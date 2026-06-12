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

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::grafana::{GrafanaContext, list_datasources, query_logs};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::tools::args::{GrafanaListDatasourcesArgs, GrafanaQueryLogsArgs};
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::grafana::GrafanaVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
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
        limit: None,
        direction: None,
        step: None,
        jq: None,
        output_format: Some(mcp_server_atlassian::tools::args::OutputFormatArg::Json),
    }
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
        let _ = std::fs::remove_file(p);
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
    assert!(resp.content.contains("success"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
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
        let _ = std::fs::remove_file(p);
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
