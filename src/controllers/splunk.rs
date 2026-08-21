#![allow(clippy::doc_markdown)]

//! Splunk search and saved-search controller path.

use futures::{StreamExt, stream::FuturesUnordered};
use reqwest::Client;
use reqwest::header::AUTHORIZATION;
use serde_json::{Map, Value};

use crate::auth::Credentials;
use crate::config::Config;
use crate::constants::data_limits::MAX_STREAMED_ARTIFACT_SIZE;
use crate::controllers::api::{
    ControllerResponse, HandleContext, dispatch_form_with_creds, dispatch_with_creds,
};
use crate::error::{McpError, api_error};
use crate::format::OutputFormat;
use crate::tools::args::{
    QueryParams, SplunkCreateJobArgs, SplunkJobResultsArgs, SplunkListSavedSearchesArgs,
    SplunkSearchArgs,
};
use crate::transport::HttpMethod;
use crate::transport::RequestOptions;
use crate::vendor::splunk::{
    SAVED_SEARCHES_PATH, SEARCH_EXPORT_PATH, SEARCH_JOBS_PATH, SplunkVendor,
};

pub struct SplunkContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a SplunkVendor,
}

impl<'a> SplunkContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a SplunkVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

pub async fn search(
    ctx: &SplunkContext<'_>,
    args: &SplunkSearchArgs,
) -> Result<ControllerResponse, McpError> {
    if let Some(count) = args
        .time_partitions
        .map(usize::from)
        .filter(|count| (2..=crate::constants::data_limits::MAX_TIME_PARTITIONS).contains(count))
        && is_safely_partitionable_spl(&args.search)
        && let (Some(start), Some(end)) = (
            args.earliest_time
                .as_deref()
                .and_then(crate::ingestion::parse_splunk_bound_ns),
            args.latest_time
                .as_deref()
                .and_then(crate::ingestion::parse_splunk_bound_ns),
        )
        && start < end
    {
        return search_partitioned(ctx, args, start, end, count).await;
    }
    search_single(
        ctx,
        args,
        None,
        std::sync::Arc::new(crate::transport::StreamingDiskQuota::new(
            MAX_STREAMED_ARTIFACT_SIZE,
        )),
        MAX_STREAMED_ARTIFACT_SIZE,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
}

fn is_safely_partitionable_spl(search: &str) -> bool {
    let trimmed = search.trim();
    trimmed.starts_with("search ")
        && !trimmed.contains('|')
        && !trimmed.contains(" earliest=")
        && !trimmed.contains(" latest=")
}

async fn search_single(
    ctx: &SplunkContext<'_>,
    args: &SplunkSearchArgs,
    aggregate: Option<std::sync::Arc<crate::transport::StreamingAggregateQuota>>,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    canonical_max_bytes: u64,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<ControllerResponse, McpError> {
    let creds = credentials(ctx).await?;
    let mut form = search_form(
        &args.search,
        args.earliest_time.as_deref(),
        args.latest_time.as_deref(),
        args.max_time,
    );
    form.insert("output_mode".into(), "json_rows".into());

    let mut policy = crate::transport::StreamingPolicy::new(
        MAX_STREAMED_ARTIFACT_SIZE,
        MAX_STREAMED_ARTIFACT_SIZE,
    );
    policy.aggregate = aggregate;
    policy.disk = Some(disk.clone());
    policy.cancellation = cancellation.clone();
    let artifact = crate::transport::fetch_streamed_artifact_with_policy(
        ctx.vendor,
        &creds,
        ctx.config,
        SEARCH_EXPORT_PATH,
        RequestOptions {
            method: Some(HttpMethod::Post),
            form: Some(form),
            ..RequestOptions::default()
        },
        "splunk-search",
        "json",
        "application/json",
        policy,
    )
    .await?;
    let result = normalize_streamed_response(
        &artifact,
        "splunk-search",
        args.jq.as_deref(),
        canonical_max_bytes,
        disk,
        &cancellation,
    )
    .await;
    let _ = crate::transport::raw_response::remove_artifact(&artifact.artifact.path).await;
    result
}

#[allow(clippy::too_many_lines)] // Acquisition, drain, and cleanup form one transaction.
async fn search_partitioned(
    ctx: &SplunkContext<'_>,
    args: &SplunkSearchArgs,
    start: i128,
    end: i128,
    count: usize,
) -> Result<ControllerResponse, McpError> {
    let intervals = crate::ingestion::try_half_open_partitions(start, end, count)
        .map_err(|message| api_error(message, Some(400), None))?;
    let aggregate = std::sync::Arc::new(crate::transport::StreamingAggregateQuota::new(
        MAX_STREAMED_ARTIFACT_SIZE,
        MAX_STREAMED_ARTIFACT_SIZE,
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let disk = std::sync::Arc::new(crate::transport::StreamingDiskQuota::new(
        MAX_STREAMED_ARTIFACT_SIZE,
    ));
    let concurrency = count.min(ctx.config.streaming_partition_concurrency());
    let mut active = FuturesUnordered::new();
    let acquire = |index: usize| {
        let interval = intervals[index].clone();
        let mut partition_args = args.clone();
        partition_args.time_partitions = None;
        partition_args.earliest_time =
            Some(crate::ingestion::splunk_epoch_seconds(interval.start_ns));
        partition_args.latest_time = Some(crate::ingestion::splunk_epoch_seconds(interval.end_ns));
        let aggregate = aggregate.clone();
        let cancellation = cancellation.clone();
        let disk = disk.clone();
        async move {
            let response = search_single(
                ctx,
                &partition_args,
                Some(aggregate),
                disk,
                MAX_STREAMED_ARTIFACT_SIZE,
                cancellation,
            )
            .await?;
            let path = response.raw_response_path.ok_or_else(|| {
                api_error(
                    "Splunk partition produced no canonical artifact",
                    Some(502),
                    None,
                )
            })?;
            Ok::<_, McpError>((index, path))
        }
    };
    let mut next = 0;
    while next < concurrency {
        active.push(acquire(next));
        next += 1;
    }
    let mut slots = vec![None; count];
    let mut first_error = None;
    while let Some(result) = active.next().await {
        match result {
            Ok((index, path)) => {
                slots[index] = Some(path);
                if first_error.is_none() && next < count {
                    active.push(acquire(next));
                    next += 1;
                }
            }
            Err(error) if first_error.is_none() => {
                cancellation.cancel();
                first_error = Some(error);
            }
            Err(_) => {}
        }
    }
    let paths = slots.into_iter().flatten().collect::<Vec<_>>();
    if let Some(error) = first_error {
        cleanup_splunk_partitions(&paths).await;
        return Err(error);
    }
    let result = splunk_final_partition_response(
        args,
        start,
        end,
        count,
        &paths,
        &intervals,
        &aggregate,
        disk,
        &cancellation,
    )
    .await;
    if result.is_err() {
        cleanup_splunk_partitions(&paths).await;
    }
    result
}

#[allow(clippy::too_many_arguments)] // Explicit lifecycle inputs keep the final commit auditable.
async fn splunk_final_partition_response(
    args: &SplunkSearchArgs,
    start: i128,
    end: i128,
    count: usize,
    paths: &[std::path::PathBuf],
    _intervals: &[crate::ingestion::QueryInterval],
    aggregate: &crate::transport::StreamingAggregateQuota,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<ControllerResponse, McpError> {
    let mut validated = Vec::with_capacity(paths.len());
    let mut retained_bytes = 0_u64;
    for (index, path) in paths.iter().enumerate() {
        let part = crate::ingestion::validate_partition(path, index, "splunk")
            .await
            .map_err(|e| {
                api_error(
                    format!("Invalid Splunk partition {index}: {e}"),
                    Some(502),
                    None,
                )
            })?;
        retained_bytes = retained_bytes
            .checked_add(part.checksum.decoded_bytes)
            .ok_or_else(|| {
                api_error(
                    "Splunk retained partition byte counter overflow",
                    Some(413),
                    None,
                )
            })?;
        validated.push(part);
    }
    let final_budget = MAX_STREAMED_ARTIFACT_SIZE
        .checked_sub(retained_bytes)
        .ok_or_else(|| {
            api_error(
                "Splunk temporary-plus-final disk counter overflow",
                Some(413),
                None,
            )
        })?;
    if retained_bytes > final_budget {
        return Err(api_error(
            "Splunk projected final plus retained partition quota exceeded",
            Some(413),
            None,
        ));
    }
    let merge = crate::ingestion::merge_partitions_cancellable_reserved(
        paths,
        crate::ingestion::RecordOrdering::Chronological,
        None,
        final_budget,
        cancellation,
        Some(disk.clone()),
    )
    .await
    .map_err(|e| api_error(format!("Cannot merge Splunk partitions: {e}"), None, None))?;
    let manifest = crate::ingestion::ArtifactManifest {
        artifact_version: crate::ingestion::ARTIFACT_VERSION,
        format: "canonical_ndjson".to_owned(),
        vendor: "splunk".to_owned(),
        query_interval: Some(crate::ingestion::QueryInterval {
            start_ns: start,
            end_ns: end,
        }),
        ordering: crate::ingestion::RecordOrdering::Chronological,
        total_records: merge.records,
        encoded_bytes: aggregate.encoded_bytes(),
        decoded_bytes: aggregate.decoded_bytes(),
        final_bytes: merge.artifact.artifact.size,
        final_sha256: merge.artifact.sha256.clone(),
        partitions: validated.into_iter().map(|part| part.checksum).collect(),
        partitions_requested: count,
        partitions_succeeded: paths.len(),
        partitions_failed: count.saturating_sub(paths.len()),
        deduplication_policy:
            "exact_cross_partition_boundary_timestamp_source_payload_labels_sha256".to_owned(),
        duplicate_count: merge.duplicates,
        global_limit: None,
        limit_reached: false,
        truncated_records: 0,
        skipped_records: 0,
        diagnostics: Vec::new(),
        completeness: crate::ingestion::Completeness::Complete,
        completeness_reason: None,
    };
    if let Err(error) =
        crate::ingestion::persist_manifest_reserved(&merge.artifact.artifact.path, &manifest, disk)
            .await
    {
        let _ =
            crate::transport::raw_response::remove_artifact(&merge.artifact.artifact.path).await;
        return Err(api_error(
            format!("Cannot commit final Splunk manifest: {error}"),
            None,
            None,
        ));
    }
    cleanup_splunk_partitions(paths).await;
    Ok(streamed_response(&merge.artifact, args.jq.as_deref()))
}

async fn cleanup_splunk_partitions(paths: &[std::path::PathBuf]) {
    for path in paths {
        let _ = crate::transport::raw_response::remove_artifact(path).await;
        let _ = tokio::fs::remove_file(path.with_extension("manifest.json")).await;
    }
}

fn streamed_response(
    artifact: &crate::transport::raw_response::StreamedArtifact,
    jq: Option<&str>,
) -> ControllerResponse {
    let mut content = format!(
        "Splunk response streamed to disk ({} bytes).\nSHA-256: `{}`\n",
        artifact.artifact.size, artifact.sha256
    );
    if jq.is_some() {
        content.push_str("JMESPath filtering is unavailable on streamed responses; filter the downloaded artifact.\n");
    }
    content.push_str("\n--- Start of response ---\n");
    content.push_str(&artifact.head);
    let preview_bytes = artifact.head.len() + artifact.tail.len();
    if artifact.artifact.size > u64::try_from(preview_bytes).unwrap_or(u64::MAX) {
        content.push_str("\n--- Middle omitted; use the artifact for the complete response ---\n");
        content.push_str(&artifact.tail);
    }
    ControllerResponse {
        content,
        raw_response_path: Some(artifact.artifact.path.clone()),
    }
}

pub async fn create_job(
    ctx: &SplunkContext<'_>,
    args: &SplunkCreateJobArgs,
) -> Result<ControllerResponse, McpError> {
    let creds = credentials(ctx).await?;
    let mut form = search_form(
        &args.search,
        args.earliest_time.as_deref(),
        args.latest_time.as_deref(),
        args.max_time,
    );
    if let Some(max_count) = args.max_count {
        form.insert("max_count".into(), max_count.to_string());
    }
    form.insert("output_mode".into(), "json".into());

    let format = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_form_with_creds(
        &handle,
        &creds,
        HttpMethod::Post,
        SEARCH_JOBS_PATH,
        None,
        form,
        args.jq.as_deref(),
        format,
    )
    .await
}

pub async fn job_results(
    ctx: &SplunkContext<'_>,
    args: &SplunkJobResultsArgs,
) -> Result<ControllerResponse, McpError> {
    validate_path_segment(&args.sid, "sid")?;
    let creds = credentials(ctx).await?;
    let mut query = QueryParams::new();
    query.insert("output_mode".into(), "json_rows".into());
    if let Some(count) = args.count {
        query.insert("count".into(), count.to_string());
    }
    if let Some(offset) = args.offset {
        query.insert("offset".into(), offset.to_string());
    }

    let path = append_query(
        &format!("/services/search/v2/jobs/{}/results", args.sid),
        &query,
    );
    let disk = std::sync::Arc::new(crate::transport::StreamingDiskQuota::new(
        MAX_STREAMED_ARTIFACT_SIZE,
    ));
    let mut policy = crate::transport::StreamingPolicy::new(
        MAX_STREAMED_ARTIFACT_SIZE,
        MAX_STREAMED_ARTIFACT_SIZE,
    );
    policy.disk = Some(disk.clone());
    let artifact = crate::transport::fetch_streamed_artifact_with_policy(
        ctx.vendor,
        &creds,
        ctx.config,
        &path,
        RequestOptions {
            method: Some(HttpMethod::Get),
            ..RequestOptions::default()
        },
        "splunk-job-results",
        "json",
        "application/json",
        policy,
    )
    .await?;
    let result = normalize_streamed_response(
        &artifact,
        "splunk-job-results",
        args.jq.as_deref(),
        MAX_STREAMED_ARTIFACT_SIZE,
        disk,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    let _ = crate::transport::raw_response::remove_artifact(&artifact.artifact.path).await;
    result
}

#[allow(clippy::too_many_lines)] // Commit and manifest sequencing is intentionally kept in one auditable transaction.
async fn normalize_streamed_response(
    input: &crate::transport::raw_response::StreamedArtifact,
    prefix: &str,
    jq: Option<&str>,
    canonical_max_bytes: u64,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<ControllerResponse, McpError> {
    let mut items = crate::ingestion::stream_splunk_json_rows(input.artifact.path.clone(), 8);
    let mut fields = None;
    let mut records = 0_u64;
    let mut writer = crate::transport::raw_response::begin_artifact(
        prefix,
        "ndjson",
        "application/x-ndjson",
        canonical_max_bytes,
    )
    .await
    .map_err(|error| {
        api_error(
            format!("Cannot create Splunk canonical artifact: {error}"),
            None,
            None,
        )
    })?;
    writer.set_disk_quota(disk.clone());
    while let Some(item) = items.recv().await {
        if cancellation.is_cancelled() {
            return Err(api_error(
                "Splunk partition normalization cancelled",
                Some(499),
                None,
            ));
        }
        match item.map_err(|error| {
            api_error(
                format!("Invalid Splunk json_rows response: {error}"),
                Some(502),
                None,
            )
        })? {
            crate::ingestion::SplunkJsonRowsItem::Fields(value) => fields = Some(value),
            crate::ingestion::SplunkJsonRowsItem::Row(row) => {
                let declarations = fields.as_deref().ok_or_else(|| {
                    api_error(
                        "Invalid Splunk json_rows response: row before fields",
                        Some(502),
                        None,
                    )
                })?;
                let record = splunk_record(declarations, row)?;
                let mut line = serde_json::to_vec(&record).map_err(|error| {
                    api_error(
                        format!("Cannot serialize Splunk canonical record: {error}"),
                        None,
                        None,
                    )
                })?;
                if line.len() > crate::constants::data_limits::MAX_STREAM_RECORD_SIZE {
                    return Err(api_error(
                        "Splunk canonical record exceeds maximum decoded record size",
                        Some(413),
                        None,
                    ));
                }
                line.push(b'\n');
                writer.write_chunk(&line).await.map_err(|error| {
                    let status = (error.kind() == std::io::ErrorKind::FileTooLarge).then_some(413);
                    api_error(
                        format!("Cannot write Splunk canonical artifact: {error}"),
                        status,
                        None,
                    )
                })?;
                records = records.checked_add(1).ok_or_else(|| {
                    api_error("Splunk canonical record counter overflow", Some(413), None)
                })?;
            }
        }
    }
    if cancellation.is_cancelled() {
        return Err(api_error(
            "Splunk partition normalization cancelled",
            Some(499),
            None,
        ));
    }
    let artifact = writer.commit().await.map_err(|error| {
        api_error(
            format!("Cannot commit Splunk canonical artifact: {error}"),
            None,
            None,
        )
    })?;
    let manifest = crate::ingestion::ArtifactManifest {
        artifact_version: crate::ingestion::ARTIFACT_VERSION,
        format: "canonical_ndjson".to_owned(),
        vendor: "splunk".to_owned(),
        query_interval: None,
        ordering: crate::ingestion::RecordOrdering::Chronological,
        total_records: records,
        encoded_bytes: input.encoded_bytes,
        decoded_bytes: input.decoded_bytes,
        final_bytes: artifact.artifact.size,
        final_sha256: artifact.sha256.clone(),
        partitions: vec![crate::ingestion::PartitionChecksum {
            index: 0,
            artifact_path: None,
            sha256: artifact.sha256.clone(),
            records,
            decoded_bytes: artifact.artifact.size,
        }],
        partitions_requested: 1,
        partitions_succeeded: 1,
        partitions_failed: 0,
        deduplication_policy: "none_single_splunk_response".to_owned(),
        duplicate_count: 0,
        global_limit: None,
        limit_reached: false,
        truncated_records: 0,
        skipped_records: 0,
        diagnostics: Vec::new(),
        completeness: crate::ingestion::Completeness::Complete,
        completeness_reason: None,
    };
    if let Err(error) =
        crate::ingestion::persist_manifest_reserved(&artifact.artifact.path, &manifest, disk).await
    {
        let _ = crate::transport::raw_response::remove_artifact(&artifact.artifact.path).await;
        return Err(api_error(
            format!("Cannot commit Splunk artifact manifest: {error}"),
            None,
            None,
        ));
    }
    Ok(streamed_response(&artifact, jq))
}

fn splunk_record(
    fields: &[String],
    row: Vec<Value>,
) -> Result<crate::ingestion::CanonicalRecord, McpError> {
    if fields.len() != row.len() {
        return Err(api_error(
            format!(
                "Invalid Splunk json_rows response: row width {} does not match {} fields",
                row.len(),
                fields.len()
            ),
            Some(502),
            None,
        ));
    }
    let mut values = Map::with_capacity(fields.len());
    for (field, value) in fields.iter().zip(row) {
        if value.is_array() || value.is_object() {
            return Err(api_error(
                format!(
                    "Invalid Splunk json_rows value shape for field `{field}`: expected scalar or null"
                ),
                Some(502),
                None,
            ));
        }
        values.insert(field.clone(), value);
    }
    let timestamp_ns = values.get("_time").map(parse_splunk_time).transpose()?;
    let mut identity = Map::new();
    for name in ["source", "sourcetype", "host", "index"] {
        if let Some(value) = values.get(name) {
            match value {
                Value::String(text) => {
                    identity.insert(name.to_owned(), Value::String(text.clone()));
                }
                Value::Null => {}
                _ => {
                    return Err(api_error(
                        format!(
                            "Invalid Splunk source identity field `{name}`: expected string or null"
                        ),
                        Some(502),
                        None,
                    ));
                }
            }
        }
    }
    let payload = match values.get("_raw") {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => serde_json::to_string(&values).map_err(|error| {
            api_error(
                format!("Cannot encode Splunk row payload: {error}"),
                None,
                None,
            )
        })?,
        Some(_) => {
            return Err(api_error(
                "Invalid Splunk `_raw`: expected string or null",
                Some(502),
                None,
            ));
        }
    };
    for name in ["_time", "_raw", "source", "sourcetype", "host", "index"] {
        values.remove(name);
    }
    Ok(crate::ingestion::CanonicalRecord {
        timestamp_ns,
        source: format!("splunk:{}", Value::Object(identity)),
        payload,
        labels: None,
        metadata: (!values.is_empty()).then_some(Value::Object(values)),
    })
}

fn parse_splunk_time(value: &Value) -> Result<i128, McpError> {
    let text = match value {
        Value::String(value) => value.as_str(),
        Value::Number(value) => return parse_epoch_seconds(&value.to_string()),
        _ => return Err(invalid_splunk_time(value)),
    };
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        return parsed
            .timestamp_nanos_opt()
            .map(i128::from)
            .ok_or_else(|| invalid_splunk_time(value));
    }
    parse_epoch_seconds(text)
}

fn parse_epoch_seconds(text: &str) -> Result<i128, McpError> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || fraction.len() > 9 || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_splunk_time(&Value::String(text.to_owned())));
    }
    let seconds = whole
        .parse::<i128>()
        .map_err(|_| invalid_splunk_time(&Value::String(text.to_owned())))?;
    let nanos = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| invalid_splunk_time(&Value::String(text.to_owned())))?
            * 10_i128.pow(u32::try_from(9 - fraction.len()).unwrap_or(0))
    };
    let negative = text.starts_with('-');
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(if negative { -nanos } else { nanos }))
        .ok_or_else(|| invalid_splunk_time(&Value::String(text.to_owned())))
}

fn invalid_splunk_time(value: &Value) -> McpError {
    api_error(
        format!(
            "Invalid Splunk `_time`: expected RFC3339 or Unix epoch seconds with at most nanosecond precision, got {value}"
        ),
        Some(502),
        None,
    )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod normalization_tests {
    use super::*;

    #[test]
    fn maps_timestamp_source_payload_and_metadata() {
        let fields = [
            "_time",
            "_raw",
            "source",
            "sourcetype",
            "host",
            "index",
            "level",
        ]
        .map(str::to_owned);
        let record = splunk_record(
            &fields,
            vec![
                Value::String("1712345678.123456789".into()),
                Value::String("héllo".into()),
                Value::String("/var/log/app".into()),
                Value::String("app:json".into()),
                Value::String("api-1".into()),
                Value::String("main".into()),
                Value::String("error".into()),
            ],
        )
        .unwrap();
        assert_eq!(record.timestamp_ns, Some(1_712_345_678_123_456_789));
        assert_eq!(record.payload, "héllo");
        assert_eq!(
            record.source,
            "splunk:{\"source\":\"/var/log/app\",\"sourcetype\":\"app:json\",\"host\":\"api-1\",\"index\":\"main\"}"
        );
        assert_eq!(
            record.metadata.unwrap(),
            serde_json::json!({"level":"error"})
        );
    }

    #[test]
    fn rejects_width_invalid_time_and_nested_values() {
        assert!(
            splunk_record(&["host".into()], Vec::new())
                .unwrap_err()
                .message
                .contains("row width")
        );
        assert!(
            splunk_record(&["_time".into()], vec![Value::String("yesterday".into())])
                .unwrap_err()
                .message
                .contains("Invalid Splunk `_time`")
        );
        assert!(
            splunk_record(&["field".into()], vec![serde_json::json!({"nested":true})])
                .unwrap_err()
                .message
                .contains("value shape")
        );
    }

    #[test]
    fn timestamp_precision_is_conservative() {
        assert_eq!(parse_epoch_seconds("-0.5").unwrap(), -500_000_000);
        assert!(parse_epoch_seconds("1.1234567890").is_err());
    }

    #[test]
    fn partition_eligibility_rejects_transforming_and_embedded_time_searches() {
        assert!(is_safely_partitionable_spl("search index=main error"));
        assert!(!is_safely_partitionable_spl("search index=main | head 10"));
        assert!(!is_safely_partitionable_spl(
            "search index=main earliest=-1h"
        ));
        assert!(!is_safely_partitionable_spl("stats count"));
    }
}

fn append_query(path: &str, query: &QueryParams) -> String {
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query.iter())
        .finish();
    format!("{path}?{encoded}")
}

pub async fn list_saved_searches(
    ctx: &SplunkContext<'_>,
    args: &SplunkListSavedSearchesArgs,
) -> Result<ControllerResponse, McpError> {
    let creds = credentials(ctx).await?;
    let mut query = QueryParams::new();
    query.insert("output_mode".into(), "json".into());
    if let Some(search) = &args.search {
        query.insert("search".into(), search.clone());
    }
    if let Some(count) = args.count {
        query.insert("count".into(), count.to_string());
    }
    if let Some(offset) = args.offset {
        query.insert("offset".into(), offset.to_string());
    }

    let format = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        SAVED_SEARCHES_PATH,
        Some(&query),
        None,
        args.jq.as_deref(),
        format,
    )
    .await
}

async fn credentials(ctx: &SplunkContext<'_>) -> Result<Credentials, McpError> {
    let token = ctx.vendor.token(ctx.config).await?;
    let scheme = ctx.vendor.auth_scheme(ctx.config);
    Ok(Credentials::ApiKeyHeader {
        header_name: AUTHORIZATION.as_str().to_owned(),
        key: format!("{scheme} {token}"),
    })
}

fn search_form(
    search: &str,
    earliest_time: Option<&str>,
    latest_time: Option<&str>,
    max_time: Option<u32>,
) -> QueryParams {
    let mut form = QueryParams::new();
    form.insert("search".into(), search.to_owned());
    if let Some(earliest_time) = earliest_time {
        form.insert("earliest_time".into(), earliest_time.to_owned());
    }
    if let Some(latest_time) = latest_time {
        form.insert("latest_time".into(), latest_time.to_owned());
    }
    if let Some(max_time) = max_time {
        form.insert("max_time".into(), max_time.to_string());
    }
    form
}

fn validate_path_segment(value: &str, name: &str) -> Result<(), McpError> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'?' | b'#'))
    {
        return Err(api_error(
            format!("Invalid Splunk {name}: expected a non-empty path segment"),
            Some(400),
            None,
        ));
    }
    Ok(())
}
