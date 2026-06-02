#![allow(clippy::doc_markdown)]

//! Controller-pipeline tests for the Slack vendor. Exercises the full path a
//! `slack_*` tool takes: read the static `SLACK_TOKEN` from config → dispatch
//! with an `Authorization: Bearer <token>` header through the shared transport
//! → classify both the non-2xx envelope and the `200 OK`/`{"ok": false}`
//! envelope unique to Slack.
//!
//! The Web API is stood up on a wiremock instance, so these tests need no
//! network and no global state.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::slack::{SlackContext, handle_request};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::format::OutputFormat;
use mcp_server_atlassian::transport::{HttpMethod, build_client};
use mcp_server_atlassian::vendor::slack::SlackVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("SLACK_TOKEN".into(), "xoxb-123".into());
    m
}

fn vendor(server: &MockServer) -> SlackVendor {
    SlackVendor::with_base_url(server.uri())
}

#[tokio::test]
async fn get_sends_bearer_token_and_filters_response() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .and(header("authorization", "Bearer xoxb-123"))
        .and(query_param("types", "public_channel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [
                {"id": "C1", "name": "general"},
                {"id": "C2", "name": "random"}
            ],
            "response_metadata": {"next_cursor": ""}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SlackContext::new(&client, &config, &vendor);

    let mut qp = mcp_server_atlassian::tools::args::QueryParams::new();
    qp.insert("types".into(), "public_channel".into());

    let resp = handle_request(
        &ctx,
        HttpMethod::Get,
        "/conversations.list",
        Some(&qp),
        None,
        Some("channels[*].{id: id, name: name}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("general"));
    assert!(resp.content.contains("C1"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn post_forwards_body_and_returns_message() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(header("authorization", "Bearer xoxb-123"))
        .and(body_json(json!({ "channel": "C1", "text": "hi" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channel": "C1",
            "ts": "1700000000.000100",
            "message": {"text": "hi"}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SlackContext::new(&client, &config, &vendor);

    let resp = handle_request(
        &ctx,
        HttpMethod::Post,
        "/chat.postMessage",
        None,
        Some(json!({ "channel": "C1", "text": "hi" })),
        Some("{channel: channel, ts: ts}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("C1"));
    assert!(resp.content.contains("1700000000"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn ok_false_in_200_body_surfaces_as_error() {
    // The defining Slack quirk: HTTP 200, but `ok: false` ⇒ this must be an
    // error, not a "successful" response carrying an error payload.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": false,
            "error": "channel_not_found"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SlackContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Post,
        "/chat.postMessage",
        None,
        Some(json!({ "channel": "nope", "text": "hi" })),
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("channel_not_found"));
}

#[tokio::test]
async fn ok_false_invalid_auth_in_200_body_is_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth.test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": false,
            "error": "invalid_auth"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SlackContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/auth.test",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("invalid_auth"));
}

#[tokio::test]
async fn missing_token_surfaces_auth_missing_at_call_time() {
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = SlackVendor::with_base_url("http://127.0.0.1:0");
    let ctx = SlackContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/auth.test",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("SLACK_TOKEN"));
}
