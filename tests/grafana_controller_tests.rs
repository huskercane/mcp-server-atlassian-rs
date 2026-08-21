#![allow(clippy::doc_markdown)]

//! Controller-pipeline tests for the Grafana vendor. Exercises the full path a
//! `grafana_*` tool takes: read the static `GRAFANA_TOKEN` from config →
//! dispatch through the shared transport with an `Authorization: Bearer <token>`
//! header → classify the Grafana/Loki error envelope.
//!
//! Loki and the Grafana HTTP API are stood up on a wiremock instance, so these
//! tests need no network and no global state — the base-URL override is what
//! makes that possible.

use std::collections::HashMap;
use std::time::Duration;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::grafana::{GrafanaContext, list_datasources, query_logs};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::tools::args::{GrafanaListDatasourcesArgs, GrafanaQueryLogsArgs};
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::grafana::GrafanaVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("GRAFANA_TOKEN".into(), "glsa_tok-123".into());
    m
}

fn vendor(server: &MockServer) -> GrafanaVendor {
    GrafanaVendor::with_base_url(server.uri())
}

fn logs_args(uid: &str, query: &str) -> GrafanaQueryLogsArgs {
    GrafanaQueryLogsArgs {
        datasource_uid: uid.to_string(),
        query: query.to_string(),
        start: None,
        end: None,
        time_partitions: None,
        limit: None,
        direction: None,
        step: None,
        jq: None,
        output_format: Some(mcp_server_atlassian::tools::args::OutputFormatArg::Json),
    }
}

fn remove_artifact(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("manifest.json"));
}

fn canonical_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn artifact_manifest(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path.with_extension("manifest.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn query_logs_proxies_logql_with_bearer_and_params() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki-prod/loki/api/v1/query_range",
        ))
        .and(header("authorization", "Bearer glsa_tok-123"))
        .and(query_param("query", "{app=\"api\"} |= \"error\""))
        .and(query_param("limit", "50"))
        .and(query_param("direction", "backward"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {
                "resultType": "streams",
                "result": [
                    {
                        "stream": {"app": "api", "level": "error"},
                        "values": [["1700000000000000000", "boom happened"]]
                    }
                ]
            }
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = GrafanaContext::new(&client, &config, &vendor);

    let mut args = logs_args("loki-prod", "{app=\"api\"} |= \"error\"");
    args.limit = Some(50);
    args.direction = Some("backward".into());
    args.jq = Some("data.result[*].values[*][1]".into());

    let resp = query_logs(&ctx, &args).await.unwrap();
    assert!(resp.content.contains("boom happened"));
    if let Some(p) = resp.raw_response_path {
        let bytes = std::fs::read(&p).unwrap();
        let line: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(line["timestamp_ns"], json!(1_700_000_000_000_000_000_u64));
        assert_eq!(line["payload"], "boom happened");
        assert_eq!(line["labels"], json!({"app":"api","level":"error"}));
        assert_eq!(line["source"], "loki:{\"app\":\"api\"}");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(p.with_extension("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["completeness"], "complete");
        assert_eq!(manifest["total_records"], 1);
        assert_eq!(manifest["encoded_bytes"], manifest["decoded_bytes"]);
        assert_eq!(
            manifest["final_sha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
        remove_artifact(&p);
    }
}

#[tokio::test]
async fn query_logs_omits_unset_optional_params() {
    let server = MockServer::start().await;

    // Only `query` is sent when start/end/limit/direction/step are unset.
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki-prod/loki/api/v1/query_range",
        ))
        .and(query_param("query", "{job=\"app\"}"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {"resultType": "streams", "result": []}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = GrafanaContext::new(&client, &config, &vendor);

    let resp = query_logs(&ctx, &logs_args("loki-prod", "{job=\"app\"}"))
        .await
        .unwrap();
    assert!(resp.content.contains("Grafana/Loki canonical"));
    if let Some(p) = resp.raw_response_path {
        assert!(std::fs::read(&p).unwrap().is_empty());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(p.with_extension("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["completeness"], "complete");
        assert_eq!(manifest["total_records"], 0);
        remove_artifact(&p);
    }
}

#[tokio::test]
async fn query_logs_partitions_exact_half_open_nanosecond_bounds() {
    let server = MockServer::start().await;
    for (start, end) in [("100", "105"), ("105", "110")] {
        Mock::given(method("GET"))
            .and(path(
                "/api/datasources/proxy/uid/loki-prod/loki/api/v1/query_range",
            ))
            .and(query_param("start", start))
            .and(query_param("end", end))
            .and(query_param("limit", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"status":"success","data":{"resultType":"streams","result":[]}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = build_client().unwrap();
    let vendor = GrafanaVendor::with_base_url(server.uri());
    let config = Config::from_map(creds());
    let mut args = logs_args("loki-prod", "{app=\"api\"}");
    args.start = Some("100".into());
    args.end = Some("110".into());
    args.limit = Some(10);
    args.direction = Some("forward".into());
    args.time_partitions = Some(2);
    let response = query_logs(&GrafanaContext::new(&client, &config, &vendor), &args)
        .await
        .unwrap();
    let artifact_path = response.raw_response_path.unwrap();
    assert!(std::fs::read(&artifact_path).unwrap().is_empty());
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(artifact_path.with_extension("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["completeness"], "complete");
    assert_eq!(manifest["query_interval"]["start_ns"], 100);
    assert_eq!(manifest["query_interval"]["end_ns"], 110);
    assert_eq!(manifest["partitions"].as_array().unwrap().len(), 2);
    remove_artifact(&artifact_path);
}

#[tokio::test]
async fn query_logs_merges_out_of_order_forward_partitions_deterministically() {
    let server = MockServer::start().await;
    let endpoint = "/api/datasources/proxy/uid/loki-prod/loki/api/v1/query_range";
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("start", "100"))
        .and(query_param("end", "105"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(150))
                .set_body_json(
                    json!({"status":"success","data":{"resultType":"streams","result":[{
                        "stream":{"app":"api"},
                        "values":[["101","first"],["105","boundary"]]
                    }]}}),
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("start", "105"))
        .and(query_param("end", "110"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status":"success","data":{"resultType":"streams","result":[{
                "stream":{"app":"api"},
                "values":[["105","boundary"],["105","same timestamp, distinct"],["108","last"]]
            }]}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let mut args = logs_args("loki-prod", "{app=\"api\"}");
    args.start = Some("100".into());
    args.end = Some("110".into());
    args.limit = Some(10);
    args.direction = Some("forward".into());
    args.time_partitions = Some(2);
    let response = query_logs(&GrafanaContext::new(&client, &config, &vendor), &args)
        .await
        .unwrap();
    let path = response.raw_response_path.unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let records = canonical_lines(&path);
    assert_eq!(
        records
            .iter()
            .map(|record| record["payload"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "boundary", "same timestamp, distinct", "last"]
    );
    let manifest = artifact_manifest(&path);
    assert_eq!(manifest["completeness"], "complete");
    assert_eq!(manifest["total_records"], 4);
    assert_eq!(manifest["duplicate_count"], 1);
    assert_eq!(manifest["limit_reached"], false);
    assert_eq!(manifest["partitions"][0]["index"], 0);
    assert_eq!(manifest["partitions"][1]["index"], 1);
    assert_eq!(manifest["final_bytes"], bytes.len());
    assert_eq!(
        manifest["final_sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert!(manifest["encoded_bytes"].as_u64().unwrap() > 0);
    assert_eq!(manifest["encoded_bytes"], manifest["decoded_bytes"]);
    remove_artifact(&path);
}

#[tokio::test]
async fn query_logs_applies_backward_limit_once_after_ordered_merge() {
    let server = MockServer::start().await;
    let endpoint = "/api/datasources/proxy/uid/loki-prod/loki/api/v1/query_range";
    for (start, end, values) in [
        ("100", "105", json!([["104", "four"], ["101", "one"]])),
        ("105", "110", json!([["109", "nine"], ["106", "six"]])),
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .and(query_param("start", start))
            .and(query_param("end", end))
            .and(query_param("limit", "3"))
            .and(query_param("direction", "backward"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"resultType":"streams","result":[{
                    "stream":{"app":"api"}, "values": values
                }]}
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let mut args = logs_args("loki-prod", "{app=\"api\"}");
    args.start = Some("100".into());
    args.end = Some("110".into());
    args.limit = Some(3);
    args.direction = Some("backward".into());
    args.time_partitions = Some(2);
    let response = query_logs(&GrafanaContext::new(&client, &config, &vendor), &args)
        .await
        .unwrap();
    let path = response.raw_response_path.unwrap();
    let records = canonical_lines(&path);
    assert_eq!(
        records
            .iter()
            .map(|record| record["payload"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["nine", "six", "four"]
    );
    let manifest = artifact_manifest(&path);
    assert_eq!(manifest["ordering"], "reverse_chronological");
    assert_eq!(manifest["total_records"], 3);
    assert_eq!(manifest["global_limit"], 3);
    assert_eq!(manifest["limit_reached"], true);
    assert_eq!(manifest["completeness"], "partial");
    remove_artifact(&path);
}

#[tokio::test]
async fn list_datasources_sends_bearer_and_filters_loki() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/datasources"))
        .and(header("authorization", "Bearer glsa_tok-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "uid": "prom-1", "name": "Prometheus", "type": "prometheus"},
            {"id": 2, "uid": "loki-prod", "name": "Loki", "type": "loki"}
        ])))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = GrafanaContext::new(&client, &config, &vendor);

    let args = GrafanaListDatasourcesArgs {
        jq: Some("[?type=='loki'].{name: name, uid: uid}".into()),
        output_format: Some(mcp_server_atlassian::tools::args::OutputFormatArg::Json),
    };

    let resp = list_datasources(&ctx, &args).await.unwrap();
    assert!(resp.content.contains("loki-prod"));
    assert!(!resp.content.contains("prom-1"));
    if let Some(p) = resp.raw_response_path {
        remove_artifact(&p);
    }
}

#[tokio::test]
async fn bad_logql_surfaces_loki_error_envelope() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki-prod/loki/api/v1/query_range",
        ))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "status": "error",
            "error": "parse error at line 1: unexpected IDENTIFIER"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = GrafanaContext::new(&client, &config, &vendor);

    let err = query_logs(&ctx, &logs_args("loki-prod", "not valid logql"))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(400));
    assert!(err.message.contains("parse error"));
}

#[tokio::test]
async fn missing_token_surfaces_auth_missing_at_call_time() {
    // A deployment without Grafana configured must not crash; the error appears
    // only when a `grafana_*` tool is actually invoked, before any network call.
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = GrafanaVendor::with_base_url("http://127.0.0.1:0");
    let ctx = GrafanaContext::new(&client, &config, &vendor);

    let err = query_logs(&ctx, &logs_args("loki-prod", "{app=\"api\"}"))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("GRAFANA_TOKEN"));
}
