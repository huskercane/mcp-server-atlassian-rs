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

use futures::{StreamExt as _, stream};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;
use tokio::sync::Semaphore;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::{McpError, OriginalError, api_error};
use crate::format::OutputFormat;
use crate::tools::args::{CircleCiLogsArgs, QueryParams, ReadArgs, WriteArgs};
use crate::transport::HttpMethod;
use crate::transport::raw_response;
use crate::vendor::circleci::CircleCiVendor;
use crate::vendor::circleci::error;

static OUTPUT_DOWNLOADS: OnceLock<Semaphore> = OnceLock::new();

fn output_downloads() -> &'static Semaphore {
    OUTPUT_DOWNLOADS.get_or_init(|| Semaphore::new(16))
}

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
    let cancellation = tokio_util::sync::CancellationToken::new();
    let disk = crate::transport::StreamingDiskQuota::server_transaction(cancellation.clone());
    let project = LogProject::parse(&args.project_slug)?;
    let build = fetch_build_details(ctx, &token, &project, args.job_number).await?;
    let steps = collect_log_steps(ctx, &build, args, disk.clone(), cancellation).await?;

    let response = CircleCiLogsResponse {
        project_slug: args.project_slug.clone(),
        job_number: args.job_number,
        steps,
    };
    let artifact_result = write_complete_log_artifact(&response, disk).await;
    for path in response
        .steps
        .iter()
        .flat_map(|step| &step.actions)
        .filter_map(|action| action.output_path.as_deref())
    {
        let _ = raw_response::remove_artifact(path).await;
    }
    let artifact = artifact_result?;
    let preview_content = if artifact.artifact.size
        > u64::try_from(artifact.head.len() + artifact.tail.len()).unwrap_or(u64::MAX)
    {
        format!(
            "{}\n... middle omitted from inline preview ...\n{}",
            artifact.head, artifact.tail
        )
    } else {
        artifact.head.clone()
    };
    let raw_response_path = Some(artifact.artifact.path.clone());
    let content = render_log_summary(
        &response,
        &preview_content,
        raw_response_path.as_deref(),
        args,
    );

    Ok(ControllerResponse {
        content,
        raw_response_path,
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
    step_number: usize,
    name: Option<String>,
    actions: Vec<LogAction>,
}

#[derive(Debug, Serialize)]
struct LogAction {
    action_number: usize,
    name: Option<String>,
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_type: Option<String>,
    #[serde(skip)]
    output_path: Option<std::path::PathBuf>,
    #[serde(skip)]
    encoded_bytes: u64,
    #[serde(skip)]
    decoded_bytes: u64,
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
    args: &CircleCiLogsArgs,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<Vec<LogStep>, McpError> {
    let Some(steps) = build.get("steps").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut collected = Vec::with_capacity(steps.len());
    for (step_index, step) in steps.iter().enumerate() {
        let step_number = step_index + 1;
        if args
            .step_number
            .is_some_and(|selected| selected != step_number)
        {
            continue;
        }
        let actions = collect_log_actions(
            ctx,
            step,
            args.failed_only,
            disk.clone(),
            cancellation.clone(),
        )
        .await?;
        if args.failed_only && actions.is_empty() {
            continue;
        }
        collected.push(LogStep {
            step_number,
            name: optional_string(step, "name"),
            actions,
        });
    }
    Ok(collected)
}

async fn collect_log_actions(
    ctx: &CircleCiContext<'_>,
    step: &Value,
    failed_only: bool,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<Vec<LogAction>, McpError> {
    let Some(actions) = step.get("actions").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let selected: Vec<_> = actions
        .iter()
        .enumerate()
        .filter_map(|(action_index, action)| {
            let status = optional_string(action, "status");
            let exit_code = action.get("exit_code").and_then(Value::as_i64);
            let failed = status.as_deref().is_some_and(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "failed" | "failing" | "error" | "errored"
                )
            }) || exit_code.is_some_and(|code| code != 0);
            if failed_only && !failed {
                return None;
            }
            Some((action_index, action.clone(), status, exit_code))
        })
        .collect();

    let mut collected: Vec<(usize, LogAction)> = stream::iter(selected)
        .map(|(action_index, action, status, exit_code)| {
            let disk = disk.clone();
            let cancellation = cancellation.clone();
            async move {
                let output_url = action.get("output_url").and_then(Value::as_str);
                let (output_path, encoded_bytes, decoded_bytes, output_fetch_error) =
                    match output_url {
                        Some(url) if !url.is_empty() => {
                            match fetch_action_output(ctx, url, disk, cancellation).await {
                                Ok(artifact) => (
                                    Some(artifact.artifact.path),
                                    artifact.encoded_bytes,
                                    artifact.decoded_bytes,
                                    None,
                                ),
                                Err(err) => (None, 0, 0, Some(err.message)),
                            }
                        }
                        _ => (None, 0, 0, None),
                    };

                (
                    action_index,
                    LogAction {
                        action_number: action_index + 1,
                        name: optional_string(&action, "name"),
                        status,
                        exit_code,
                        action_type: optional_string(&action, "type"),
                        output_path,
                        encoded_bytes,
                        decoded_bytes,
                        output_fetch_error,
                    },
                )
            }
        })
        .buffer_unordered(8)
        .collect()
        .await;
    collected.sort_by_key(|(index, _)| *index);
    Ok(collected.into_iter().map(|(_, action)| action).collect())
}

fn render_log_summary(
    response: &CircleCiLogsResponse,
    full_content: &str,
    path: Option<&std::path::Path>,
    args: &CircleCiLogsArgs,
) -> String {
    use std::fmt::Write as _;

    const HEAD_CHARS: usize = 5_000;
    const TAIL_CHARS: usize = 30_000;
    let action_count: usize = response.steps.iter().map(|step| step.actions.len()).sum();
    let failed_count = response
        .steps
        .iter()
        .flat_map(|step| &step.actions)
        .filter(|action| {
            action.exit_code.is_some_and(|code| code != 0)
                || action.status.as_deref().is_some_and(|status| {
                    matches!(
                        status.to_ascii_lowercase().as_str(),
                        "failed" | "failing" | "error" | "errored"
                    )
                })
        })
        .count();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "CircleCI job {} ({})",
        response.job_number, response.project_slug
    );
    let _ = writeln!(
        out,
        "Selected steps: {}; actions: {}; failed actions: {}",
        response.steps.len(),
        action_count,
        failed_count
    );
    let _ = writeln!(
        out,
        "Complete selected log size: {} bytes",
        full_content.len()
    );
    if let Some(path) = path {
        let _ = writeln!(out, "Complete selected logs saved at: `{}`", path.display());
        if let Some(artifact) = raw_response::artifact_for_path(path) {
            let _ = writeln!(out, "Artifact ID: `{}`", artifact.id);
            let _ = writeln!(out, "HTTP download path: `/artifacts/{}`", artifact.id);
            out.push_str(
                "Resume through HTTP Range requests or `artifact_read` with `nextOffset`.\n",
            );
        }
    } else {
        out.push_str("Warning: the complete selected logs could not be saved.\n");
    }
    out.push('\n');

    if args.condensed {
        out.push_str("--- Condensed error context ---\n");
        out.push_str(&condense_logs(
            full_content,
            args.context_lines.unwrap_or(3).min(20),
        ));
    } else {
        out.push_str("--- Beginning of selected logs ---\n");
        out.push_str(slice_head(full_content, HEAD_CHARS));
        if full_content.len() > HEAD_CHARS + TAIL_CHARS {
            out.push_str("\n\n--- Middle omitted; use the saved file for complete logs ---\n\n");
            out.push_str("--- End of selected logs ---\n");
            out.push_str(slice_tail(full_content, TAIL_CHARS));
        }
    }
    out
}

fn slice_head(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn slice_tail(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn condense_logs(text: &str, context: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if [
            "failed",
            "failure",
            "error",
            "exception",
            "assert",
            "panic",
            "exit code",
            "tests completed",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            let start = index.saturating_sub(context);
            let end = (index + context + 1).min(lines.len());
            keep[start..end].fill(true);
        }
    }
    let mut out = String::new();
    let mut omitted = false;
    for (index, line) in lines.iter().enumerate() {
        if keep[index] {
            if omitted && !out.is_empty() {
                out.push_str("...\n");
            }
            out.push_str(line);
            out.push('\n');
            omitted = false;
        } else {
            omitted = true;
        }
    }
    if out.is_empty() {
        "No error-like lines matched; use the saved complete log.\n".to_owned()
    } else {
        out
    }
}

async fn fetch_action_output(
    ctx: &CircleCiContext<'_>,
    output_url: &str,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<raw_response::StreamedArtifact, McpError> {
    let _ = ctx;
    let _permit = output_downloads().acquire().await.map_err(|err| {
        api_error(
            format!("CircleCI output concurrency limiter closed: {err}"),
            None,
            None,
        )
    })?;
    let mut policy = crate::transport::StreamingPolicy::new(
        crate::constants::data_limits::MAX_STREAMED_ARTIFACT_SIZE,
        crate::constants::data_limits::MAX_STREAMED_ARTIFACT_SIZE,
    );
    policy.disk = Some(disk);
    policy.cancellation = cancellation;
    crate::transport::fetch_streamed_url(
        output_url,
        "circleci-action",
        "json",
        "application/json",
        policy,
    )
    .await
}

#[allow(clippy::too_many_lines)]
async fn write_complete_log_artifact(
    response: &CircleCiLogsResponse,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
) -> Result<raw_response::StreamedArtifact, McpError> {
    let mut writer = raw_response::begin_artifact(
        &format!("circleci-job-{}", response.job_number),
        "ndjson",
        "application/x-ndjson",
        crate::constants::data_limits::MAX_STREAMED_ARTIFACT_SIZE,
    )
    .await
    .map_err(|err| {
        api_error(
            format!("Cannot create CircleCI log artifact: {err}"),
            None,
            None,
        )
    })?;
    writer.set_disk_quota(&disk);
    let mut sequence = 0_u64;
    for step in &response.steps {
        for action in &step.actions {
            if let Some(path) = &action.output_path {
                let mut items =
                    crate::ingestion::stream_json_array::<CircleOutputItem>(path.clone(), 8);
                while let Some(item) = items.recv().await {
                    let item = item.map_err(|err| {
                        api_error(
                            format!("Cannot normalize CircleCI output: {err}"),
                            Some(502),
                            None,
                        )
                    })?;
                    let timestamp_ns = item.time.as_deref().and_then(parse_rfc3339_ns);
                    for raw_line in item.message.split_inclusive('\n') {
                        sequence += 1;
                        let without_lf = raw_line.strip_suffix('\n').unwrap_or(raw_line);
                        let payload = without_lf.strip_suffix('\r').unwrap_or(without_lf);
                        let record = crate::ingestion::CanonicalRecord {
                            timestamp_ns,
                            source: format!(
                                "circleci:{}/{}",
                                response.project_slug, response.job_number
                            ),
                            payload: payload.to_owned(),
                            labels: None,
                            metadata: Some(serde_json::json!({
                                "project": response.project_slug, "job": response.job_number,
                                "step": step.step_number, "step_name": step.name,
                                "action": action.action_number, "action_name": action.name,
                                "sequence": sequence
                            })),
                        };
                        let mut encoded = serde_json::to_vec(&record).map_err(|err| {
                            api_error(
                                format!("Cannot serialize CircleCI record: {err}"),
                                None,
                                None,
                            )
                        })?;
                        if encoded.len() > crate::constants::data_limits::MAX_STREAM_RECORD_SIZE {
                            return Err(api_error(
                                "CircleCI canonical record exceeds maximum decoded record size",
                                Some(413),
                                None,
                            ));
                        }
                        encoded.push(b'\n');
                        writer
                            .write_chunk(&encoded)
                            .await
                            .map_err(stream_write_error)?;
                    }
                }
            }
        }
    }
    let artifact = writer.commit().await.map_err(|err| {
        api_error(
            format!("Cannot commit CircleCI log artifact: {err}"),
            None,
            None,
        )
    })?;
    let requested = response
        .steps
        .iter()
        .flat_map(|step| &step.actions)
        .filter(|action| action.output_path.is_some() || action.output_fetch_error.is_some())
        .count();
    let failed = response
        .steps
        .iter()
        .flat_map(|step| &step.actions)
        .filter(|action| action.output_fetch_error.is_some())
        .count();
    let manifest = crate::ingestion::ArtifactManifest {
        artifact_version: crate::ingestion::ARTIFACT_VERSION,
        format: "canonical_ndjson".to_owned(),
        vendor: "circleci".to_owned(),
        query_interval: None,
        ordering: crate::ingestion::RecordOrdering::Chronological,
        total_records: sequence,
        encoded_bytes: response
            .steps
            .iter()
            .flat_map(|step| &step.actions)
            .map(|action| action.encoded_bytes)
            .sum(),
        decoded_bytes: response
            .steps
            .iter()
            .flat_map(|step| &step.actions)
            .map(|action| action.decoded_bytes)
            .sum(),
        final_bytes: artifact.artifact.size,
        final_sha256: artifact.sha256.clone(),
        partitions: vec![crate::ingestion::PartitionChecksum {
            index: 0,
            artifact_path: None,
            sha256: artifact.sha256.clone(),
            records: sequence,
            decoded_bytes: artifact.artifact.size,
        }],
        partitions_requested: requested,
        partitions_succeeded: requested.saturating_sub(failed),
        partitions_failed: failed,
        deduplication_policy: "none_single_circleci_job".to_owned(),
        duplicate_count: 0,
        global_limit: None,
        limit_reached: false,
        truncated_records: 0,
        skipped_records: 0,
        diagnostics: (failed != 0)
            .then(|| format!("{failed} CircleCI action output downloads failed"))
            .into_iter()
            .collect(),
        completeness: if failed == 0 {
            crate::ingestion::Completeness::Complete
        } else {
            crate::ingestion::Completeness::Partial
        },
        completeness_reason: (failed != 0)
            .then(|| "one or more action output downloads failed".to_owned()),
    };
    if let Err(err) =
        crate::ingestion::persist_manifest_reserved(&artifact.artifact.path, &manifest, disk).await
    {
        let _ = raw_response::remove_artifact(&artifact.artifact.path).await;
        return Err(api_error(
            format!("Cannot commit CircleCI artifact manifest: {err}"),
            None,
            None,
        ));
    }
    Ok(artifact)
}

#[derive(Deserialize)]
struct CircleOutputItem {
    message: String,
    #[serde(default)]
    time: Option<String>,
}

fn parse_rfc3339_ns(value: &str) -> Option<i128> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|time| time.timestamp_nanos_opt())
        .map(i128::from)
}

#[allow(clippy::needless_pass_by_value)]
fn stream_write_error(err: std::io::Error) -> McpError {
    let status = (err.kind() == std::io::ErrorKind::FileTooLarge).then_some(413);
    api_error(
        format!("Cannot write CircleCI log artifact: {err}"),
        status,
        None,
    )
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
