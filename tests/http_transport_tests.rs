//! Black-box tests for the streamable-HTTP transport.
//!
//! Each test binds its own `127.0.0.1:0` listener so they can run in parallel
//! without port collisions. The server factory inside `build_app` uses the
//! default `AtlassianServer::new()`, which is infallible in the absence of
//! credentials because the MCP initialize handshake does not hit any vendor.

use std::time::Duration;

use mcp_server_atlassian::server::http::build_app;
use mcp_server_atlassian::server::session::{DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use serde_json::json;
use tokio::net::TcpListener;

const ACCEPT_BOTH: &str = "application/json, text/event-stream";
const ALLOWED_ORIGIN: &str = "http://localhost:3000";

/// Spawn the app on an ephemeral loopback port and return the base URL.
///
/// The server task is detached; the test process shuts it down on exit.
async fn spawn_app(idle_ttl: Duration, sweep_interval: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let app = build_app(idle_ttl, sweep_interval);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("axum::serve");
    });
    format!("http://{addr}")
}

fn initialize_body(id: u64) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "rust-http-test", "version": "0.0.0" }
        }
    })
}

fn mcp_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(ACCEPT, HeaderValue::from_static(ACCEPT_BOTH));
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert(ORIGIN, HeaderValue::from_static(ALLOWED_ORIGIN));
    h
}

#[tokio::test]
async fn health_endpoint_returns_plaintext_version_banner() {
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let resp = reqwest::get(format!("{base}/")).await.expect("GET /");
    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        ctype.starts_with("text/plain"),
        "unexpected content-type {ctype}"
    );
    let body = resp.text().await.expect("body");
    assert!(
        body.starts_with("Atlassian MCP Server v"),
        "unexpected banner: {body}"
    );
    assert!(body.ends_with(" is running"), "unexpected banner: {body}");
}

#[tokio::test]
async fn bad_origin_is_rejected_with_403() {
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .header(ACCEPT, ACCEPT_BOTH)
        .header(CONTENT_TYPE, "application/json")
        .header(ORIGIN, "http://evil.example.com")
        .json(&initialize_body(1))
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn missing_origin_is_allowed() {
    // Non-browser clients (curl, CLI, server-to-server) send no Origin. TS
    // short-circuits to `next()` in that case; we must do the same.
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .header(ACCEPT, ACCEPT_BOTH)
        .header(CONTENT_TYPE, "application/json")
        .json(&initialize_body(1))
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("mcp-session-id"));
}

#[tokio::test]
async fn initialize_issues_session_id_header() {
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .headers(mcp_headers())
        .json(&initialize_body(1))
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(resp.status(), StatusCode::OK);
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("Mcp-Session-Id header on initialize response");
    assert!(!session_id.is_empty(), "empty session id");

    // Body is SSE-framed; the initialize response lands on a `data:` line.
    let body = resp.text().await.expect("body");
    assert!(
        body.contains("\"jsonrpc\":\"2.0\""),
        "initialize SSE body missing JSON-RPC envelope:\n{body}"
    );
    assert!(
        body.contains("\"result\""),
        "initialize SSE body missing result:\n{body}"
    );
}

#[tokio::test]
async fn unknown_session_id_returns_404() {
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .headers(mcp_headers())
        .header("mcp-session-id", "00000000-0000-0000-0000-000000000000")
        .json(&json!({ "jsonrpc": "2.0", "method": "ping", "id": 99 }))
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn body_over_1mb_is_rejected_with_413() {
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let client = reqwest::Client::new();
    // 1 MB + 1 byte payload (still valid JSON).
    let padding: String = "a".repeat(1_000_002);
    let body = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"x\",\"params\":{{\"pad\":\"{padding}\"}}}}");
    let resp = client
        .post(format!("{base}/mcp"))
        .headers(mcp_headers())
        .body(body)
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn delete_closes_a_live_session() {
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let client = reqwest::Client::new();

    let init = client
        .post(format!("{base}/mcp"))
        .headers(mcp_headers())
        .json(&initialize_body(1))
        .send()
        .await
        .expect("initialize");
    assert_eq!(init.status(), StatusCode::OK);
    let session_id = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("session id")
        .to_owned();

    let del = client
        .delete(format!("{base}/mcp"))
        .header("mcp-session-id", &session_id)
        .header(ORIGIN, ALLOWED_ORIGIN)
        .send()
        .await
        .expect("DELETE /mcp");
    assert_eq!(del.status(), StatusCode::ACCEPTED);

    // Follow-up POST on the same session should now 404.
    let after = client
        .post(format!("{base}/mcp"))
        .headers(mcp_headers())
        .header("mcp-session-id", &session_id)
        .json(&json!({ "jsonrpc": "2.0", "method": "ping", "id": 2 }))
        .send()
        .await
        .expect("POST after delete");
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cors_preflight_mirrors_origin() {
    let base = spawn_app(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL).await;
    let client = reqwest::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("{base}/mcp"))
        .header(ORIGIN, ALLOWED_ORIGIN)
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "content-type, mcp-session-id",
        )
        .send()
        .await
        .expect("OPTIONS /mcp");
    assert!(resp.status().is_success(), "preflight status {}", resp.status());
    let allow_origin = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert_eq!(allow_origin, ALLOWED_ORIGIN, "ACAO should mirror Origin");
    let allow_headers = resp
        .headers()
        .get("access-control-allow-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        allow_headers.contains("mcp-session-id"),
        "mirrored ACAH missing mcp-session-id: {allow_headers}"
    );
}

#[tokio::test]
async fn idle_sessions_are_reaped_after_ttl() {
    // Tight TTL so the reaper fires within a test-acceptable window.
    let ttl = Duration::from_millis(150);
    let sweep = Duration::from_millis(40);
    let base = spawn_app(ttl, sweep).await;
    let client = reqwest::Client::new();

    let init = client
        .post(format!("{base}/mcp"))
        .headers(mcp_headers())
        .json(&initialize_body(1))
        .send()
        .await
        .expect("initialize");
    assert_eq!(init.status(), StatusCode::OK);
    let session_id = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("session id")
        .to_owned();

    // Wait well past TTL + one sweep interval.
    tokio::time::sleep(ttl + sweep * 6).await;

    let after = client
        .post(format!("{base}/mcp"))
        .headers(mcp_headers())
        .header("mcp-session-id", &session_id)
        .json(&json!({ "jsonrpc": "2.0", "method": "ping", "id": 2 }))
        .send()
        .await
        .expect("POST after reap");
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
}
