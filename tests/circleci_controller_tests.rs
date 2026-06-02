#![allow(clippy::doc_markdown)]

//! Controller-pipeline tests for the `CircleCI` vendor. Exercises the full path
//! a `circleci_*` tool takes: read the static `CIRCLECI_TOKEN` from config →
//! dispatch the request with a `Authorization: Bearer <token>` header through
//! the shared transport → classify the `CircleCI` error envelope.
//!
//! The REST API is stood up on a wiremock instance, so these tests need no
//! network and no global state — the base-URL override is what makes that
//! possible.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::circleci::{CircleCiContext, handle_request};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::format::OutputFormat;
use mcp_server_atlassian::transport::{HttpMethod, build_client};
use mcp_server_atlassian::vendor::circleci::CircleCiVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("CIRCLECI_TOKEN".into(), "tok-123".into());
    m
}

fn vendor(server: &MockServer) -> CircleCiVendor {
    CircleCiVendor::with_base_url(server.uri())
}

#[tokio::test]
async fn get_sends_bearer_token_and_filters_response() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/project/gh/acme/web/pipeline"))
        .and(header("authorization", "Bearer tok-123"))
        .and(query_param("branch", "main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": "p1", "state": "created", "number": 42},
                {"id": "p2", "state": "errored", "number": 41}
            ],
            "next_page_token": null
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = CircleCiContext::new(&client, &config, &vendor);

    let mut qp = mcp_server_atlassian::tools::args::QueryParams::new();
    qp.insert("branch".into(), "main".into());

    let resp = handle_request(
        &ctx,
        HttpMethod::Get,
        "/project/gh/acme/web/pipeline",
        Some(&qp),
        None,
        Some("items[*].{id: id, state: state}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("p1"));
    assert!(resp.content.contains("created"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn post_forwards_body_and_returns_created_pipeline() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/project/gh/acme/web/pipeline"))
        .and(header("authorization", "Bearer tok-123"))
        .and(body_json(json!({ "branch": "main" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "pipe-99",
            "state": "pending",
            "number": 100
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = CircleCiContext::new(&client, &config, &vendor);

    let resp = handle_request(
        &ctx,
        HttpMethod::Post,
        "/project/gh/acme/web/pipeline",
        None,
        Some(json!({ "branch": "main" })),
        Some("{id: id, state: state}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("pipe-99"));
    assert!(resp.content.contains("pending"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn api_404_surfaces_circleci_error_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pipeline/does-not-exist"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "Pipeline not found"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = CircleCiContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/pipeline/does-not-exist",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Pipeline not found"));
}

#[tokio::test]
async fn missing_token_surfaces_auth_missing_at_call_time() {
    // A deployment without CircleCI configured must not crash; the error
    // appears only when a `circleci_*` tool is actually invoked, and before
    // any network call.
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = CircleCiVendor::with_base_url("http://127.0.0.1:0");
    let ctx = CircleCiContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/me",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("CIRCLECI_TOKEN"));
}
