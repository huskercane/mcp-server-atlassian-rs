#![allow(clippy::doc_markdown)]

//! SonarQube / SonarCloud controller path.
//!
//! Three read tools sit on Sonar's Web API, all authenticated with a static user
//! token (`SONARQUBE_TOKEN`) injected as `Authorization: Bearer` via
//! [`Credentials::Bearer`]:
//!
//! - [`quality_gate`] — `GET /api/qualitygates/project_status`: the failing
//!   gate conditions (metric, threshold, actual value) for an analysis, branch,
//!   or PR. This is the "why did Sonar fail this build" call. It accepts a
//!   scanner `ceTaskId` (as printed in the CI log's `report-task.txt`) and
//!   resolves it to the underlying `analysisId` via
//!   [`CE_TASK_PATH`](crate::vendor::sonarqube::CE_TASK_PATH) first.
//! - [`search_issues`] — `GET /api/issues/search`: the individual offending
//!   bugs / vulnerabilities / code smells (file, line, rule), scoped to a
//!   project and optionally a branch/PR.
//! - [`get`] — a generic `GET /api/...` passthrough for the long tail
//!   (`/api/measures/component`, `/api/projects/search`, `/api/hotspots/search`,
//!   raw `/api/ce/task`, …).
//!
//! Everything after auth — base-URL resolution, query encoding, transport, error
//! classification, output rendering, raw-response persistence, and JMESPath
//! filtering — is the same code the other vendors use.

use reqwest::Client;
use serde_json::Value;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::{McpError, OriginalError, api_error};
use crate::format::OutputFormat;
use crate::tools::args::{
    QueryParams, ReadArgs, SonarqubeQualityGateArgs, SonarqubeSearchIssuesArgs,
};
use crate::transport::HttpMethod;
use crate::vendor::Vendor;
use crate::vendor::sonarqube::{
    CE_TASK_PATH, ISSUES_SEARCH_PATH, QUALITY_GATE_PATH, SonarqubeVendor, error,
};

/// SonarQube-specific request context. Carries the concrete [`SonarqubeVendor`]
/// (not a `&dyn Vendor`) so the token read and the `ce/task` pre-fetch can be
/// driven, plus the shared client and config.
pub struct SonarqubeContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a SonarqubeVendor,
}

impl<'a> SonarqubeContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a SonarqubeVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Report the quality-gate status — the failing conditions — for an analysis,
/// branch, or PR. Resolution order for identifying the analysis:
///
/// 1. `analysis_id` — used verbatim.
/// 2. `ce_task_id` — resolved to an `analysisId` via `GET /api/ce/task` first
///    (the scanner prints this task id in CI as `report-task.txt`'s `ceTaskId`).
/// 3. `project_key` (+ optional `branch` or `pull_request`) — the latest
///    analysis of that target.
///
/// Kept as an `async fn`: there are `?`s and a conditional `ce/task` await
/// before the final dispatch, so the single-tail-await `impl Future`
/// optimisation does not apply.
pub async fn quality_gate(
    ctx: &SonarqubeContext<'_>,
    args: &SonarqubeQualityGateArgs,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config)?;

    if args.branch.is_some() && args.pull_request.is_some() {
        return Err(api_error(
            "Pass either `branch` or `pullRequest`, not both — a Sonar analysis \
             targets one or the other.",
            None,
            None,
        ));
    }

    let mut qp: QueryParams = QueryParams::new();
    if let Some(analysis_id) = trimmed(args.analysis_id.as_ref()) {
        qp.insert("analysisId".into(), analysis_id.to_owned());
    } else if let Some(ce_task_id) = trimmed(args.ce_task_id.as_ref()) {
        let analysis_id = resolve_analysis_id(ctx, &token, ce_task_id).await?;
        qp.insert("analysisId".into(), analysis_id);
    } else if let Some(project_key) = trimmed(args.project_key.as_ref()) {
        qp.insert("projectKey".into(), project_key.to_owned());
        if let Some(branch) = trimmed(args.branch.as_ref()) {
            qp.insert("branch".into(), branch.to_owned());
        }
        if let Some(pr) = trimmed(args.pull_request.as_ref()) {
            qp.insert("pullRequest".into(), pr.to_owned());
        }
    } else {
        return Err(api_error(
            "One of `projectKey`, `ceTaskId`, or `analysisId` is required to \
             identify the analysis to report the quality gate for.",
            None,
            None,
        ));
    }
    if let Some(org) = trimmed(args.organization.as_ref()) {
        qp.insert("organization".into(), org.to_owned());
    }

    let creds = Credentials::Bearer { token };
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        QUALITY_GATE_PATH,
        Some(&qp),
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// Search issues (bugs / vulnerabilities / code smells) for a project,
/// optionally scoped to a branch or PR and filtered by type/severity/status.
/// Kept as an `async fn` — there is a `?` on the token resolution before the
/// dispatch await, so the single-tail-await `impl Future` optimisation does not
/// apply.
pub async fn search_issues(
    ctx: &SonarqubeContext<'_>,
    args: &SonarqubeSearchIssuesArgs,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config)?;

    if args.branch.is_some() && args.pull_request.is_some() {
        return Err(api_error(
            "Pass either `branch` or `pullRequest`, not both — a Sonar analysis \
             targets one or the other.",
            None,
            None,
        ));
    }

    let mut qp: QueryParams = QueryParams::new();
    qp.insert("componentKeys".into(), args.component_keys.clone());
    if let Some(branch) = trimmed(args.branch.as_ref()) {
        qp.insert("branch".into(), branch.to_owned());
    }
    if let Some(pr) = trimmed(args.pull_request.as_ref()) {
        qp.insert("pullRequest".into(), pr.to_owned());
    }
    if let Some(types) = trimmed(args.types.as_ref()) {
        qp.insert("types".into(), types.to_owned());
    }
    if let Some(severities) = trimmed(args.severities.as_ref()) {
        qp.insert("severities".into(), severities.to_owned());
    }
    if let Some(statuses) = trimmed(args.statuses.as_ref()) {
        qp.insert("statuses".into(), statuses.to_owned());
    }
    if let Some(resolved) = args.resolved {
        qp.insert("resolved".into(), resolved.to_string());
    }
    if let Some(ps) = args.page_size {
        qp.insert("ps".into(), ps.to_string());
    }
    if let Some(org) = trimmed(args.organization.as_ref()) {
        qp.insert("organization".into(), org.to_owned());
    }

    let creds = Credentials::Bearer { token };
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        ISSUES_SEARCH_PATH,
        Some(&qp),
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// Generic `GET /api/...` passthrough for the endpoints without a bespoke tool
/// (measures, projects search, hotspots, raw `ce/task`, …). Kept as an
/// `async fn` — there is a `?` on the token resolution before the dispatch
/// await, so the single-tail-await `impl Future` optimisation does not apply.
pub async fn get(
    ctx: &SonarqubeContext<'_>,
    args: &ReadArgs,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config)?;
    let creds = Credentials::Bearer { token };
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        &args.path,
        args.query_params.as_ref(),
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// Resolve a scanner compute-engine task id to the `analysisId` the quality-gate
/// call needs, via `GET /api/ce/task?id=<ceTaskId>`. This is the bridge from a
/// CircleCI log (which prints the `ceTaskId`) to the structured gate result.
///
/// The `analysisId` only exists once the task reaches `SUCCESS`; a task still
/// `PENDING`/`IN_PROGRESS`, or one that `FAILED`/`CANCELED` before producing an
/// analysis, has none — we surface the task status so the caller understands why.
async fn resolve_analysis_id(
    ctx: &SonarqubeContext<'_>,
    token: &str,
    ce_task_id: &str,
) -> Result<String, McpError> {
    let base = ctx.vendor.base_url(ctx.config)?;
    let mut url = reqwest::Url::parse(&format!("{base}{CE_TASK_PATH}"))
        .map_err(|err| api_error(format!("Invalid SonarQube ce/task URL: {err}"), None, None))?;
    url.query_pairs_mut().append_pair("id", ce_task_id);

    let response = ctx
        .client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|err| {
            api_error(
                format!("SonarQube ce/task request failed: {err}"),
                None,
                None,
            )
        })?;
    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(error::classify(status, &body_text));
    }

    let value: Value = serde_json::from_str(&body_text).map_err(|err| {
        api_error(
            format!("SonarQube ce/task returned invalid JSON: {err}"),
            None,
            Some(OriginalError::String(body_text.clone())),
        )
    })?;

    let task = value.get("task");
    if let Some(analysis_id) = task
        .and_then(|t| t.get("analysisId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(analysis_id.to_owned());
    }

    let task_status = task
        .and_then(|t| t.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    Err(api_error(
        format!(
            "SonarQube ce/task {ce_task_id} has no analysisId (task status: {task_status}). \
             The analysis is not finished, or it failed before producing one — pass \
             `projectKey` with `branch`/`pullRequest` instead."
        ),
        None,
        Some(OriginalError::Json(value)),
    ))
}

/// Treat an absent or whitespace-only optional string as "not provided".
fn trimmed(opt: Option<&String>) -> Option<&str> {
    opt.map(|s| s.trim()).filter(|s| !s.is_empty())
}
