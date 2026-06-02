//! Controller-pipeline tests for the Zoom vendor. Exercises the full path a
//! `zoom_*` tool takes: resolve Server-to-Server OAuth credentials → exchange
//! them for a bearer (cached + auto-renewed) → dispatch the request with that
//! bearer through the shared transport → classify the Zoom error envelope.
//!
//! Both the OAuth token endpoint and the REST API are stood up on a single
//! wiremock instance (different paths), so these tests need no network and no
//! global state — the token-URL override is what makes that possible.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::zoom::{ZoomContext, handle_request};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::format::OutputFormat;
use mcp_server_atlassian::transport::{HttpMethod, build_client};
use mcp_server_atlassian::vendor::zoom::ZoomVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ZOOM_ACCOUNT_ID".into(), "acct-1".into());
    m.insert("ZOOM_CLIENT_ID".into(), "client-1".into());
    m.insert("ZOOM_CLIENT_SECRET".into(), "secret-1".into());
    m
}

/// Expected `Authorization: Basic …` header on the token exchange, for
/// `client-1:secret-1`.
fn expected_basic() -> String {
    format!("Basic {}", STANDARD.encode(b"client-1:secret-1"))
}

/// Build a token-endpoint mock that issues `token` with the given lifetime.
/// The caller mounts it (and may add `.expect(n)`).
fn token_mock(token: &str, expires_in: u64) -> Mock {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("authorization", expected_basic().as_str()))
        .and(body_string_contains("grant_type=account_credentials"))
        .and(body_string_contains("account_id=acct-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token,
            "token_type": "bearer",
            "expires_in": expires_in,
        })))
}

fn vendor(server: &MockServer) -> ZoomVendor {
    ZoomVendor::with_urls(server.uri(), format!("{}/oauth/token", server.uri()))
}

#[tokio::test]
async fn schedule_get_exchanges_token_then_calls_api_with_bearer() {
    let server = MockServer::start().await;
    token_mock("tok-123", 3600).mount(&server).await;

    Mock::given(method("GET"))
        .and(path("/users/me/meetings"))
        .and(header("authorization", "Bearer tok-123"))
        .and(query_param("type", "scheduled"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "meetings": [
                {"id": 111, "topic": "Standup", "start_time": "2026-06-02T15:00:00Z", "join_url": "https://zoom.us/j/111"}
            ]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = ZoomContext::new(&client, &config, &vendor);

    let mut qp = mcp_server_atlassian::tools::args::QueryParams::new();
    qp.insert("type".into(), "scheduled".into());

    let resp = handle_request(
        &ctx,
        HttpMethod::Get,
        "/users/me/meetings",
        Some(&qp),
        None,
        Some("meetings[*].{id: id, topic: topic}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("Standup"));
    assert!(resp.content.contains("111"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn create_meeting_forwards_body_and_returns_start_url() {
    let server = MockServer::start().await;
    token_mock("tok-xyz", 3600).mount(&server).await;

    Mock::given(method("POST"))
        .and(path("/users/me/meetings"))
        .and(header("authorization", "Bearer tok-xyz"))
        .and(body_json(json!({
            "topic": "Project sync",
            "type": 2,
            "start_time": "2026-06-02T15:00:00Z",
            "duration": 30
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 98765,
            "join_url": "https://zoom.us/j/98765",
            "start_url": "https://zoom.us/s/98765?zak=secret"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = ZoomContext::new(&client, &config, &vendor);

    let resp = handle_request(
        &ctx,
        HttpMethod::Post,
        "/users/me/meetings",
        None,
        Some(json!({
            "topic": "Project sync",
            "type": 2,
            "start_time": "2026-06-02T15:00:00Z",
            "duration": 30
        })),
        Some("{id: id, start: start_url}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("98765"));
    assert!(resp.content.contains("https://zoom.us/s/98765?zak=secret"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn token_is_exchanged_once_and_cached_across_calls() {
    // The token mock asserts exactly one exchange even though two API calls
    // are made — proving the bearer is cached on the vendor instance.
    let server = MockServer::start().await;
    token_mock("cached-tok", 3600)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/users/me"))
        .and(header("authorization", "Bearer cached-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "me"})))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);

    for _ in 0..2 {
        let ctx = ZoomContext::new(&client, &config, &vendor);
        let resp = handle_request(
            &ctx,
            HttpMethod::Get,
            "/users/me",
            None,
            None,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert!(resp.content.contains("\"id\""));
        if let Some(p) = resp.raw_response_path {
            let _ = std::fs::remove_file(p);
        }
    }
    // `.expect(1)` is verified when `server` drops at end of test.
}

#[tokio::test]
async fn changing_credentials_invalidates_cached_token() {
    // The cache key includes the account/client identity, so a config change
    // on the same vendor instance must force a fresh exchange rather than
    // serving a token minted for the previous credentials.
    let server = MockServer::start().await;
    // Two distinct token requests (one per client id), each issued once.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("account_id=acct-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "tok-A", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("account_id=acct-2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "tok-B", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "me"})))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let vendor = vendor(&server);

    for account in ["acct-1", "acct-2"] {
        let mut map = creds();
        map.insert("ZOOM_ACCOUNT_ID".into(), account.into());
        let config = Config::from_map(map);
        let ctx = ZoomContext::new(&client, &config, &vendor);
        let resp = handle_request(
            &ctx,
            HttpMethod::Get,
            "/users/me",
            None,
            None,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        if let Some(p) = resp.raw_response_path {
            let _ = std::fs::remove_file(p);
        }
    }
    // Both `.expect(1)` token mocks verified on drop.
}

#[tokio::test]
async fn rotating_client_secret_invalidates_cached_token() {
    // The cache key fingerprints the client secret, so rotating the secret on
    // the same vendor instance must force a fresh exchange — it must never keep
    // serving a bearer minted with the old secret.
    let server = MockServer::start().await;
    let basic = |secret: &str| format!("Basic {}", STANDARD.encode(format!("client-1:{secret}")));
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("authorization", basic("secret-1").as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "tok-old", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("authorization", basic("secret-2").as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "tok-new", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "me"})))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let vendor = vendor(&server);

    for secret in ["secret-1", "secret-2"] {
        let mut map = creds();
        map.insert("ZOOM_CLIENT_SECRET".into(), secret.into());
        let config = Config::from_map(map);
        let ctx = ZoomContext::new(&client, &config, &vendor);
        let resp = handle_request(
            &ctx,
            HttpMethod::Get,
            "/users/me",
            None,
            None,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        if let Some(p) = resp.raw_response_path {
            let _ = std::fs::remove_file(p);
        }
    }
    // Both `.expect(1)` token mocks verified on drop: rotating the secret
    // forced a second exchange instead of reusing the cached bearer.
}

#[tokio::test]
async fn expired_token_is_refreshed() {
    // A 1s lifetime collapses (after the 60s skew clamp) to the 1s floor;
    // sleeping past it forces a second exchange on the next call.
    let server = MockServer::start().await;
    token_mock("short-tok", 1).expect(2).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "me"})))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);

    let call = || async {
        let ctx = ZoomContext::new(&client, &config, &vendor);
        let resp = handle_request(
            &ctx,
            HttpMethod::Get,
            "/users/me",
            None,
            None,
            None,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        if let Some(p) = resp.raw_response_path {
            let _ = std::fs::remove_file(p);
        }
    };

    call().await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    call().await;
    // `.expect(2)` verified on drop: one initial exchange + one refresh.
}

#[tokio::test]
async fn api_404_surfaces_zoom_error_envelope() {
    let server = MockServer::start().await;
    token_mock("tok-err", 3600).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/meetings/999"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "code": 3001,
            "message": "Meeting does not exist: 999."
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = ZoomContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/meetings/999",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Meeting does not exist"));
}

#[tokio::test]
async fn token_exchange_rejection_surfaces_auth_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "reason": "Invalid client_id or client_secret",
            "error": "invalid_client"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = ZoomContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/users/me",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(401));
    assert!(err.message.contains("Invalid client_id"));
}

#[tokio::test]
async fn missing_credentials_surface_auth_missing_at_call_time() {
    // A deployment without Zoom configured must not crash; the error appears
    // only when a `zoom_*` tool is actually invoked.
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = ZoomVendor::new();
    let ctx = ZoomContext::new(&client, &config, &vendor);

    let err = handle_request(
        &ctx,
        HttpMethod::Get,
        "/users/me",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("ZOOM_ACCOUNT_ID"));
}
