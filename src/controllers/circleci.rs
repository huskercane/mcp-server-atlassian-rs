#![allow(clippy::doc_markdown)]

//! CircleCI controller path.
//!
//! CircleCI does not use the shared [`Credentials::require_for_async`]
//! resolver: it reads its static personal API token from config (via
//! [`CircleCiVendor::token`](crate::vendor::circleci::CircleCiVendor::token))
//! and injects a [`Credentials::Bearer`] into the shared dispatch path
//! ([`dispatch_with_creds`]) — the scheme CircleCI's v2 API recommends.
//! Everything after auth — path normalisation, query encoding, transport,
//! error classification, output rendering — is the same code the Atlassian
//! vendors use.

use reqwest::Client;
use serde::Serialize;
use serde_json::Value;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::{McpError, OriginalError, api_error};
use crate::format::{OutputFormat, render_serializable};
use crate::tools::args::{CircleCiLogsArgs, QueryParams, ReadArgs, WriteArgs};
use crate::transport::HttpMethod;
use crate::vendor::circleci::CircleCiVendor;
use crate::vendor::circleci::error;

/// CircleCI-specific request context. Carries the concrete [`CircleCiVendor`]
/// (not a `&dyn Vendor`) so the token read can be driven, plus the shared
/// client and config.
pub struct CircleCiContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a CircleCiVendor,
}

impl<'a> CircleCiContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a CircleCiVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Resolve the API token, then dispatch the request. Kept as an `async fn` —
/// there is a `?` on the token resolution before the dispatch await, so the
/// single-tail-await `impl Future` optimisation does not apply.
pub async fn handle_request(
    ctx: &CircleCiContext<'_>,
    method: HttpMethod,
    path: &str,
    query_params: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config).await?;
    let creds = Credentials::Bearer { token };
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        method,
        path,
        query_params,
        body,
        jq,
        output_format,
    )
    .await
}

/// Read-shaped convenience wrapper (no body).
pub async fn handle_read(
    ctx: &CircleCiContext<'_>,
    method: HttpMethod,
    args: &ReadArgs,
) -> Result<ControllerResponse, McpError> {
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    handle_request(
        ctx,
        method,
        &args.path,
        args.query_params.as_ref(),
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// Write-shaped convenience wrapper (POST / PUT / PATCH).
pub async fn handle_write(
    ctx: &CircleCiContext<'_>,
    method: HttpMethod,
    args: &WriteArgs,
) -> Result<ControllerResponse, McpError> {
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    handle_request(
        ctx,
        method,
        &args.path,
        args.query_params.as_ref(),
        Some(args.body.clone()),
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// Fetch raw CircleCI step logs for a completed or running job.
///
/// CircleCI's v2 API returns workflow/job metadata, but not the raw action
/// output. The build details endpoint that exposes per-action `output_url`s is
/// on the older API surface, so this is intentionally a dedicated CircleCI
/// operation rather than another `circleci_get` path.
pub async fn handle_logs(
    ctx: &CircleCiContext<'_>,
    args: &CircleCiLogsArgs,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config).await?;
    let project = LogProject::parse(&args.project_slug)?;
    let build = fetch_build_details(ctx, &token, &project, args.job_number).await?;
    let steps = collect_log_steps(ctx, &build).await?;

    let response = CircleCiLogsResponse {
        project_slug: args.project_slug.clone(),
        job_number: args.job_number,
        steps,
    };
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);

    Ok(ControllerResponse {
        content: render_serializable(&response, fmt),
        raw_response_path: None,
    })
}

struct LogProject<'a> {
    vcs: &'a str,
    org: &'a str,
    repo: &'a str,
}

impl<'a> LogProject<'a> {
    fn parse(project_slug: &'a str) -> Result<Self, McpError> {
        let parts: Vec<&str> = project_slug.split('/').collect();
        if parts.len() != 3 {
            return Err(api_error(
                "CircleCI projectSlug must be in the form gh/org/repo or bb/org/repo",
                None,
                None,
            ));
        }

        let vcs = match parts[0] {
            "gh" | "github" => "github",
            "bb" | "bitbucket" => "bitbucket",
            other => {
                return Err(api_error(
                    format!(
                        "CircleCI logs only support GitHub/Bitbucket slugs; unsupported VCS prefix: {other}"
                    ),
                    None,
                    None,
                ));
            }
        };

        if parts[1].is_empty() || parts[2].is_empty() {
            return Err(api_error(
                "CircleCI projectSlug must include non-empty org and repo segments",
                None,
                None,
            ));
        }

        Ok(Self {
            vcs,
            org: parts[1],
            repo: parts[2],
        })
    }
}

#[derive(Debug, Serialize)]
struct CircleCiLogsResponse {
    project_slug: String,
    job_number: u64,
    steps: Vec<LogStep>,
}

#[derive(Debug, Serialize)]
struct LogStep {
    name: Option<String>,
    actions: Vec<LogAction>,
}

#[derive(Debug, Serialize)]
struct LogAction {
    name: Option<String>,
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_fetch_error: Option<String>,
}

async fn fetch_build_details(
    ctx: &CircleCiContext<'_>,
    token: &str,
    project: &LogProject<'_>,
    job_number: u64,
) -> Result<Value, McpError> {
    let base = ctx.vendor.log_base_url();
    let mut url = reqwest::Url::parse(&format!(
        "{}/project/{}/{}/{}/{}",
        base.trim_end_matches('/'),
        project.vcs,
        project.org,
        project.repo,
        job_number
    ))
    .map_err(|err| api_error(format!("Invalid CircleCI log API URL: {err}"), None, None))?;
    url.query_pairs_mut().append_pair("circle-token", token);

    let response = ctx.client.get(url).send().await.map_err(|err| {
        api_error(
            format!("CircleCI log API request failed: {err}"),
            None,
            None,
        )
    })?;
    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(error::classify(status, &body_text));
    }

    serde_json::from_str(&body_text).map_err(|err| {
        api_error(
            format!("CircleCI log API returned invalid JSON: {err}"),
            None,
            Some(OriginalError::String(body_text)),
        )
    })
}

async fn collect_log_steps(
    ctx: &CircleCiContext<'_>,
    build: &Value,
) -> Result<Vec<LogStep>, McpError> {
    let Some(steps) = build.get("steps").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut collected = Vec::with_capacity(steps.len());
    for step in steps {
        let actions = collect_log_actions(ctx, step).await?;
        collected.push(LogStep {
            name: optional_string(step, "name"),
            actions,
        });
    }
    Ok(collected)
}

async fn collect_log_actions(
    ctx: &CircleCiContext<'_>,
    step: &Value,
) -> Result<Vec<LogAction>, McpError> {
    let Some(actions) = step.get("actions").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut collected = Vec::with_capacity(actions.len());
    for action in actions {
        let output_url = action.get("output_url").and_then(Value::as_str);
        let (output, output_fetch_error) = match output_url {
            Some(url) if !url.is_empty() => match fetch_action_output(ctx, url).await {
                Ok(text) => (Some(text), None),
                Err(err) => (None, Some(err.message)),
            },
            _ => (None, None),
        };

        collected.push(LogAction {
            name: optional_string(action, "name"),
            status: optional_string(action, "status"),
            action_type: optional_string(action, "type"),
            output,
            output_fetch_error,
        });
    }
    Ok(collected)
}

async fn fetch_action_output(
    ctx: &CircleCiContext<'_>,
    output_url: &str,
) -> Result<String, McpError> {
    let response = ctx.client.get(output_url).send().await.map_err(|err| {
        api_error(
            format!("CircleCI step output request failed: {err}"),
            None,
            None,
        )
    })?;
    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(api_error(
            format!(
                "CircleCI step output request failed with status {}",
                status.as_u16()
            ),
            Some(status.as_u16()),
            Some(OriginalError::String(body_text)),
        ));
    }

    Ok(flatten_output_body(&body_text))
}

fn flatten_output_body(body_text: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body_text) else {
        return body_text.to_owned();
    };

    let Some(items) = value.as_array() else {
        return body_text.to_owned();
    };

    let mut output = String::new();
    for item in items {
        if let Some(message) = item.get("message").and_then(Value::as_str) {
            output.push_str(message);
            if !message.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    output
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
