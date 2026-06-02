#![allow(clippy::doc_markdown)]

//! Controller-pipeline tests for the Postman vendor. Exercises the full path a
//! `postman_*` tool takes: read the static `POSTMAN_API_KEY` from config →
//! dispatch with an `X-API-Key: <key>` header (NOT `Authorization`) through the
//! shared transport → classify the nested Postman error envelope.
//!
//! The custom-header assertion is the point of these tests: Postman is the
//! first vendor to authenticate outside the `Authorization` header, so we pin
//! that `X-API-Key` carries the key and `Authorization` is absent.
//!
//! The REST API is stood up on a wiremock instance, so these tests need no
//! network and no global state.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::postman::{PostmanContext, handle_request};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::format::OutputFormat;
use mcp_server_atlassian::transport::{HttpMethod, build_client};
use mcp_server_atlassian::vendor::postman::PostmanVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("POSTMAN_API_KEY".into(), "PMAK-xyz".into());
    m
}

fn vendor(server: &MockServer) -> PostmanVendor {
    PostmanVendor::with_base_url(server.uri())
}

#[tokio::test]
async fn get_sends_api_key_header_and_filters_response() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/collections"))
        .and(header("x-api-key", "PMAK-xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "collections": [
                {"uid": "u-1", "name": "Smoke"},
                {"uid": "u-2", "name": "Regression"}
            ]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = PostmanContext::new(&client, &config, &vendor);

    let resp = handle_request(
        &ctx,
        HttpMethod::Get,
        "/collections",
        None,
        None,
        Some("collections[*].{uid: uid, name: name}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("Smoke"));
    assert!(resp.content.contains("u-1"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn auth_is_not_sent_in_authorization_header() {
    // Pin the defining Postman behaviour: the key rides in X-API-Key, and the
    // transport must NOT also stamp an Authorization header for this vendor.
    let server = MockServer::start().await;

    // This mock only matches when Authorization is ABSENT (header_exists would
    // pass; we assert via a second mock below that the X-API-Key path is hit).
    Mock::given(method("GET"))
        .and(path("/me"))
        .and(header("x-api-key", "PMAK-xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user": {"id": 42}})))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = PostmanContext::new(&client, &config, &vendor);

    let resp = handle_request(
        &ctx,
        HttpMethod::Get,
        "/me",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("42"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }

    // Inspect the recorded request directly: Authorization must be absent.
    let requests = server.received_requests().await.unwrap();
    let req = requests.iter().find(|r| r.url.path() == "/me").unwrap();
    assert!(
        req.headers.get("authorization").is_none(),
        "Postman must not send an Authorization header"
    );
    assert_eq!(
        req.headers.get("x-api-key").map(|v| v.to_str().unwrap()),
        Some("PMAK-xyz")
    );
}

#[tokio::test]
async fn post_forwards_body_and_returns_created() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/collections"))
        .and(header_exists("x-api-key"))
        .and(body_json(
            json!({ "collection": {"info": {"name": "New"}} }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "collection": {"uid": "u-9", "name": "New"}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = PostmanContext::new(&client, &config, &vendor);

    let resp = handle_request(
        &ctx,
        HttpMethod::Post,
        "/collections",
        None,
        Some(json!({ "collection": {"info": {"name": "New"}} })),
        Some("collection.{uid: uid, name: name}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("u-9"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn api_404_surfaces_postman_error_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/collections/nope"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"name": "instanceNotFoundError", "message": "Collection not found"}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = PostmanContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/collections/nope",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Collection not found"));
}

#[tokio::test]
async fn missing_key_surfaces_auth_missing_at_call_time() {
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = PostmanVendor::with_base_url("http://127.0.0.1:0");
    let ctx = PostmanContext::new(&client, &config, &vendor);

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
    assert!(err.message.contains("POSTMAN_API_KEY"));
}
