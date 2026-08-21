#![allow(clippy::doc_markdown)]

//! Grafana controller path.
//!
//! Two read tools sit on Grafana's HTTP API, both authenticated with a static
//! service-account token (`GRAFANA_TOKEN`) injected as `Authorization: Bearer`
//! via [`Credentials::Bearer`]:
//!
//! - [`query_logs`] — runs a LogQL query against a Loki datasource through
//!   Grafana's datasource proxy (`GET .../api/datasources/proxy/uid/{uid}/loki/api/v1/query_range`).
//! - [`list_datasources`] — `GET /api/datasources`, used to discover the Loki
//!   datasource UID that `query_logs` needs.
//!
//! Everything after auth — base-URL resolution, query encoding, transport,
//! error classification, output rendering, raw-response persistence, and
//! JMESPath filtering — is the same code the other vendors use.

use futures::{StreamExt, stream::FuturesUnordered};
use reqwest::Client;

use crate::auth::Credentials;
use crate::config::Config;
use crate::constants::data_limits::MAX_STREAMED_ARTIFACT_SIZE;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::{GrafanaListDatasourcesArgs, GrafanaQueryLogsArgs, QueryParams};
use crate::transport::{HttpMethod, RequestOptions};
use crate::vendor::grafana::{
    DATASOURCE_PROXY_PREFIX, DATASOURCES_PATH, GrafanaVendor, LOKI_QUERY_RANGE_PATH,
};

/// Grafana-specific request context. Carries the concrete [`GrafanaVendor`]
/// (not a `&dyn Vendor`) so the token read can be driven, plus the shared
/// client and config.
pub struct GrafanaContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a GrafanaVendor,
}

impl<'a> GrafanaContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a GrafanaVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Run a LogQL query against a Loki datasource via Grafana's datasource proxy.
/// Resolves the token, builds the proxy path for the caller-supplied datasource
/// UID, and forwards the LogQL plus optional range/limit knobs as query params.
/// Kept as an `async fn` — there is a `?` on the token resolution before the
/// dispatch await, so the single-tail-await `impl Future` optimisation does not
/// apply.
pub async fn query_logs(
    ctx: &GrafanaContext<'_>,
    args: &GrafanaQueryLogsArgs,
) -> Result<ControllerResponse, McpError> {
    if let Some(count) = usable_partition_count(args.time_partitions)
        && let (Some(start), Some(end)) = (
            args.start
                .as_deref()
                .and_then(crate::ingestion::parse_loki_bound_ns),
            args.end
                .as_deref()
                .and_then(crate::ingestion::parse_loki_bound_ns),
        )
        && start < end
        && args.limit.is_some()
    {
        return query_logs_partitioned(ctx, args, start, end, count).await;
    }
    let cancellation = tokio_util::sync::CancellationToken::new();
    let disk = crate::transport::StreamingDiskQuota::server_transaction(cancellation.clone());
    query_logs_single(
        ctx,
        args,
        None,
        disk,
        MAX_STREAMED_ARTIFACT_SIZE,
        cancellation,
    )
    .await
}

fn usable_partition_count(value: Option<u8>) -> Option<usize> {
    value
        .map(usize::from)
        .filter(|count| (2..=crate::constants::data_limits::MAX_TIME_PARTITIONS).contains(count))
}

async fn query_logs_single(
    ctx: &GrafanaContext<'_>,
    args: &GrafanaQueryLogsArgs,
    aggregate: Option<std::sync::Arc<crate::transport::StreamingAggregateQuota>>,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    canonical_max_bytes: u64,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config).await?;
    let creds = Credentials::Bearer { token };

    // datasource UIDs are `[a-zA-Z0-9_-]`, so direct interpolation is safe.
    let path = format!(
        "{DATASOURCE_PROXY_PREFIX}/{uid}{LOKI_QUERY_RANGE_PATH}",
        uid = args.datasource_uid,
    );

    let mut qp: QueryParams = QueryParams::new();
    qp.insert("query".into(), args.query.clone());
    if let Some(start) = &args.start {
        qp.insert("start".into(), start.clone());
    }
    if let Some(end) = &args.end {
        qp.insert("end".into(), end.clone());
    }
    if let Some(limit) = args.limit {
        qp.insert("limit".into(), limit.to_string());
    }
    if let Some(direction) = &args.direction {
        qp.insert("direction".into(), direction.clone());
    }
    if let Some(step) = &args.step {
        qp.insert("step".into(), step.clone());
    }

    let normalized = append_query(&path, &qp);
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
        &normalized,
        RequestOptions {
            method: Some(HttpMethod::Get),
            ..RequestOptions::default()
        },
        "grafana-loki",
        "json",
        "application/json",
        policy,
    )
    .await?;
    let ordering = if args.direction.as_deref().unwrap_or("backward") == "backward" {
        crate::ingestion::RecordOrdering::ReverseChronological
    } else {
        crate::ingestion::RecordOrdering::Chronological
    };
    let result = normalize_loki_response(
        &artifact,
        args.jq.as_deref(),
        canonical_max_bytes,
        disk,
        ordering,
        &cancellation,
    )
    .await;
    let _ = crate::transport::raw_response::remove_artifact(&artifact.artifact.path).await;
    result
}

#[allow(clippy::too_many_lines)] // Acquisition, drain, and cleanup form one transaction.
async fn query_logs_partitioned(
    ctx: &GrafanaContext<'_>,
    args: &GrafanaQueryLogsArgs,
    start: i128,
    end: i128,
    count: usize,
) -> Result<ControllerResponse, McpError> {
    let mut intervals = crate::ingestion::try_half_open_partitions(start, end, count)
        .map_err(|message| api_error(message.to_owned()))?;
    let reverse = args.direction.as_deref().unwrap_or("backward") == "backward";
    if reverse {
        intervals.reverse();
    }
    let aggregate = std::sync::Arc::new(crate::transport::StreamingAggregateQuota::new(
        MAX_STREAMED_ARTIFACT_SIZE,
        MAX_STREAMED_ARTIFACT_SIZE,
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let disk = crate::transport::StreamingDiskQuota::server_transaction(cancellation.clone());
    let concurrency = count.min(ctx.config.streaming_partition_concurrency());
    let mut active = FuturesUnordered::new();
    let acquire = |index: usize| {
        let interval = intervals[index].clone();
        let mut partition_args = args.clone();
        partition_args.time_partitions = None;
        partition_args.start = Some(interval.start_ns.to_string());
        partition_args.end = Some(interval.end_ns.to_string());
        partition_args.limit = args.limit;
        let aggregate = aggregate.clone();
        let cancellation = cancellation.clone();
        let disk = disk.clone();
        async move {
            let response = query_logs_single(
                ctx,
                &partition_args,
                Some(aggregate),
                disk,
                MAX_STREAMED_ARTIFACT_SIZE,
                cancellation,
            )
            .await?;
            let path = response.raw_response_path.ok_or_else(|| {
                api_error("Loki partition produced no canonical artifact".to_owned())
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
    let parts = slots.into_iter().flatten().collect::<Vec<_>>();
    if let Some(error) = first_error {
        cleanup_partition_paths(&parts).await;
        return Err(error);
    }
    let result = final_partition_response(
        "grafana_loki",
        args,
        start,
        end,
        &parts,
        &intervals,
        aggregate,
        disk,
        &cancellation,
    )
    .await;
    if result.is_err() {
        cleanup_partition_paths(&parts).await;
    }
    result
}

async fn cleanup_partition_paths(paths: &[std::path::PathBuf]) {
    for path in paths {
        let _ = crate::transport::raw_response::remove_artifact(path).await;
        let _ = tokio::fs::remove_file(path.with_extension("manifest.json")).await;
    }
}

#[allow(clippy::too_many_arguments)] // Explicit lifecycle inputs keep the final commit auditable.
async fn final_partition_response(
    vendor: &str,
    args: &GrafanaQueryLogsArgs,
    start: i128,
    end: i128,
    paths: &[std::path::PathBuf],
    _intervals: &[crate::ingestion::QueryInterval],
    aggregate: std::sync::Arc<crate::transport::StreamingAggregateQuota>,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<ControllerResponse, McpError> {
    let mut validated = Vec::with_capacity(paths.len());
    let mut retained_bytes = 0_u64;
    let mut upstream_limit_reached = false;
    for (index, path) in paths.iter().enumerate() {
        let part = crate::ingestion::validate_partition(path, index, vendor)
            .await
            .map_err(|e| api_error(format!("Invalid Loki partition {index}: {e}")))?;
        retained_bytes = retained_bytes
            .checked_add(part.checksum.decoded_bytes)
            .ok_or_else(|| api_error("Loki retained partition byte counter overflow".to_owned()))?;
        upstream_limit_reached |= args
            .limit
            .is_some_and(|limit| part.checksum.records >= u64::from(limit));
        validated.push(part);
    }
    let final_budget = MAX_STREAMED_ARTIFACT_SIZE
        .checked_sub(retained_bytes)
        .ok_or_else(|| api_error("Loki temporary-plus-final disk counter overflow".to_owned()))?;
    if retained_bytes > final_budget {
        return Err(api_error(
            "Loki projected final plus retained partition quota exceeded".to_owned(),
        ));
    }
    let ordering = if args.direction.as_deref().unwrap_or("backward") == "backward" {
        crate::ingestion::RecordOrdering::ReverseChronological
    } else {
        crate::ingestion::RecordOrdering::Chronological
    };
    let merge = crate::ingestion::merge_partitions_cancellable_reserved(
        paths,
        ordering,
        args.limit.map(u64::from),
        final_budget,
        cancellation,
        Some(disk.clone()),
    )
    .await
    .map_err(|e| api_error(format!("Cannot merge Loki partitions: {e}")))?;
    let limited = merge.limited || upstream_limit_reached;
    let requested = args.time_partitions.map_or(paths.len(), usize::from);
    let diagnostics = limited
        .then(|| {
            "global or upstream partition result limit reached; completeness is conservative"
                .to_owned()
        })
        .into_iter()
        .collect();
    let manifest = crate::ingestion::ArtifactManifest {
        artifact_version: crate::ingestion::ARTIFACT_VERSION,
        format: "canonical_ndjson".to_owned(),
        vendor: vendor.to_owned(),
        query_interval: Some(crate::ingestion::QueryInterval {
            start_ns: start,
            end_ns: end,
        }),
        ordering,
        total_records: merge.records,
        encoded_bytes: aggregate.encoded_bytes(),
        decoded_bytes: aggregate.decoded_bytes(),
        final_bytes: merge.artifact.artifact.size,
        final_sha256: merge.artifact.sha256.clone(),
        partitions: validated.into_iter().map(|part| part.checksum).collect(),
        partitions_requested: requested,
        partitions_succeeded: paths.len(),
        partitions_failed: requested.saturating_sub(paths.len()),
        deduplication_policy:
            "exact_cross_partition_boundary_timestamp_source_payload_labels_sha256".to_owned(),
        duplicate_count: merge.duplicates,
        global_limit: args.limit.map(u64::from),
        limit_reached: limited,
        truncated_records: 0,
        skipped_records: 0,
        diagnostics,
        completeness: if limited {
            crate::ingestion::Completeness::Partial
        } else {
            crate::ingestion::Completeness::Complete
        },
        completeness_reason: limited
            .then(|| "a result limit may have truncated the complete ordered result".to_owned()),
    };
    if let Err(error) =
        crate::ingestion::persist_manifest_reserved(&merge.artifact.artifact.path, &manifest, disk)
            .await
    {
        let _ =
            crate::transport::raw_response::remove_artifact(&merge.artifact.artifact.path).await;
        return Err(api_error(format!(
            "Cannot commit final Loki manifest: {error}"
        )));
    }
    cleanup_partition_paths(paths).await;
    Ok(streamed_response(
        "Grafana/Loki canonical",
        &merge.artifact,
        args.jq.as_deref(),
    ))
}

#[allow(clippy::too_many_lines)]
async fn normalize_loki_response(
    input: &crate::transport::raw_response::StreamedArtifact,
    jq: Option<&str>,
    canonical_max_bytes: u64,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    ordering: crate::ingestion::RecordOrdering,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<ControllerResponse, McpError> {
    let mut items = crate::ingestion::stream_loki_response(input.artifact.path.clone(), 8);
    let mut writer = crate::transport::raw_response::begin_artifact(
        "grafana-loki-canonical",
        "ndjson",
        "application/x-ndjson",
        canonical_max_bytes,
    )
    .await
    .map_err(|error| api_error(format!("Cannot create Loki canonical artifact: {error}")))?;
    writer.set_disk_quota(&disk);
    let mut records = 0_u64;
    while let Some(item) = items.recv().await {
        if cancellation.is_cancelled() {
            return Err(api_error(
                "Loki partition normalization cancelled".to_owned(),
            ));
        }
        let item =
            item.map_err(|error| api_error(format!("Invalid Loki streams response: {error}")))?;
        let record = loki_record(item)?;
        let mut line = serde_json::to_vec(&record).map_err(|error| {
            api_error(format!("Cannot serialize Loki canonical record: {error}"))
        })?;
        if line.len() > crate::constants::data_limits::MAX_STREAM_RECORD_SIZE {
            return Err(api_error(
                "Loki canonical record exceeds maximum decoded record size".to_owned(),
            ));
        }
        line.push(b'\n');
        writer
            .write_chunk(&line)
            .await
            .map_err(|error| api_error(format!("Cannot write Loki canonical artifact: {error}")))?;
        records = records
            .checked_add(1)
            .ok_or_else(|| api_error("Loki record count overflow".to_owned()))?;
    }
    if cancellation.is_cancelled() {
        return Err(api_error(
            "Loki partition normalization cancelled".to_owned(),
        ));
    }
    let artifact = writer
        .commit()
        .await
        .map_err(|error| api_error(format!("Cannot commit Loki canonical artifact: {error}")))?;
    let manifest = crate::ingestion::ArtifactManifest {
        artifact_version: crate::ingestion::ARTIFACT_VERSION,
        format: "canonical_ndjson".to_owned(),
        vendor: "grafana_loki".to_owned(),
        query_interval: None,
        ordering,
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
        deduplication_policy: "none_single_loki_response".to_owned(),
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
        return Err(api_error(format!(
            "Cannot commit Loki artifact manifest: {error}"
        )));
    }
    Ok(streamed_response("Grafana/Loki canonical", &artifact, jq))
}

fn loki_record(
    item: crate::ingestion::LokiStreamValue,
) -> Result<crate::ingestion::CanonicalRecord, McpError> {
    let timestamp = parse_loki_timestamp(&item.timestamp)?;
    // These are conventional labels documented across Loki's service
    // discovery and automatic structured-metadata integrations. Their fixed
    // precedence makes identity stable; arbitrary tenant labels never become
    // identity implicitly. If all are absent, the explicit `loki:unknown`
    // identity preserves that fact while the complete label map remains below.
    let mut identity = serde_json::Map::new();
    for name in [
        "service_name",
        "namespace",
        "job",
        "app",
        "container",
        "pod",
        "host",
        "instance",
        "filename",
    ] {
        if let Some(value) = item.labels.get(name) {
            identity.insert(name.to_owned(), value.clone());
        }
    }
    let source = if identity.is_empty() {
        "loki:unknown".to_owned()
    } else {
        format!("loki:{}", serde_json::Value::Object(identity))
    };
    Ok(crate::ingestion::CanonicalRecord {
        timestamp_ns: Some(timestamp),
        source,
        payload: item.payload,
        labels: Some(serde_json::Value::Object(item.labels)),
        metadata: None,
    })
}

fn parse_loki_timestamp(value: &str) -> Result<i128, McpError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_loki_timestamp(value));
    }
    value
        .parse::<u64>()
        .map(i128::from)
        .map_err(|_| invalid_loki_timestamp(value))
}

fn invalid_loki_timestamp(value: &str) -> McpError {
    api_error(format!(
        "Invalid Loki timestamp `{value}`: expected a non-negative base-10 integer nanosecond timestamp fitting in u64"
    ))
}

fn api_error(message: String) -> McpError {
    crate::error::api_error(message, Some(502), None)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod normalization_tests {
    use super::*;

    #[test]
    fn maps_exact_timestamp_payload_labels_and_documented_source() {
        let record = loki_record(crate::ingestion::LokiStreamValue {
            labels: serde_json::from_value(serde_json::json!({
                "service_name": "billing",
                "pod": "billing-1",
                "tenant_custom": "kept"
            }))
            .unwrap(),
            timestamp: "1712345678123456789".into(),
            payload: "héllo\nexact".into(),
        })
        .unwrap();
        assert_eq!(record.timestamp_ns, Some(1_712_345_678_123_456_789));
        assert_eq!(record.payload, "héllo\nexact");
        assert_eq!(
            record.source,
            "loki:{\"service_name\":\"billing\",\"pod\":\"billing-1\"}"
        );
        assert_eq!(
            record.labels.unwrap(),
            serde_json::json!({"pod":"billing-1","service_name":"billing","tenant_custom":"kept"})
        );
    }

    #[test]
    fn source_is_explicitly_unknown_without_conventional_labels() {
        let record = loki_record(crate::ingestion::LokiStreamValue {
            labels: serde_json::from_value(serde_json::json!({"tenant_custom":"kept"})).unwrap(),
            timestamp: "0".into(),
            payload: String::new(),
        })
        .unwrap();
        assert_eq!(record.source, "loki:unknown");
    }

    #[test]
    fn timestamp_parser_rejects_negative_fractional_ambiguous_and_overflowing_values() {
        for value in ["", "-1", "+1", "1.0", " 1", "18446744073709551616"] {
            assert!(parse_loki_timestamp(value).is_err(), "accepted {value}");
        }
        assert_eq!(
            parse_loki_timestamp("18446744073709551615").unwrap(),
            i128::from(u64::MAX)
        );
    }

    #[test]
    fn partition_count_is_narrowly_bounded() {
        assert_eq!(usable_partition_count(None), None);
        assert_eq!(usable_partition_count(Some(1)), None);
        assert_eq!(usable_partition_count(Some(2)), Some(2));
        assert_eq!(usable_partition_count(Some(16)), Some(16));
        assert_eq!(usable_partition_count(Some(17)), None);
    }
}

fn append_query(path: &str, query: &QueryParams) -> String {
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query.iter())
        .finish();
    format!("{path}?{encoded}")
}

fn streamed_response(
    vendor: &str,
    artifact: &crate::transport::raw_response::StreamedArtifact,
    jq: Option<&str>,
) -> ControllerResponse {
    let mut content = format!(
        "{vendor} response streamed to disk ({} bytes).\nSHA-256: `{}`\n",
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

/// List configured datasources so the caller can discover a Loki datasource's
/// UID. Same auth/transport path as [`query_logs`]; filtering to Loki
/// datasources is left to the caller's `jq` (e.g. `[?type=='loki']`).
pub async fn list_datasources(
    ctx: &GrafanaContext<'_>,
    args: &GrafanaListDatasourcesArgs,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config).await?;
    let creds = Credentials::Bearer { token };

    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        DATASOURCES_PATH,
        None,
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}
