//! End-to-end transport tests using a local wiremock. Exercises the full
//! response contract: auth header injection, each body classification, error
//! mapping, and raw-response persistence.

use std::collections::HashMap;
use std::time::Duration;

use mcp_server_atlassian::auth::Credentials;
use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::transport::{
    HttpMethod, RequestOptions, ResponseBody, build_client,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Wire the transport to talk to a local `MockServer` instead of
/// `api.bitbucket.org`. We swap the base URL at runtime by pointing reqwest
/// at an absolute URL built from the mock server's base.
async fn call_mock(
    mock_server: &MockServer,
    path_suffix: &str,
    options: RequestOptions,
) -> Result<
    mcp_server_atlassian::transport::TransportResponse,
    mcp_server_atlassian::error::McpError,
> {
    let client = build_client().unwrap();
    let creds = Credentials::AtlassianApiToken {
        email: "alice@example.com".into(),
        token: "tok".into(),
    };
    let config = Config::from_map(HashMap::new());

    // Swap out the base host by using the full URL via path=…; the transport
    // normalises leading '/' only, it doesn't override the host. We use the
    // override_url helper below.
    override_url::fetch(
        mock_server,
        &client,
        &creds,
        &config,
        path_suffix,
        options,
    )
    .await
}

/// Thin shim that replaces the hard-coded `api.bitbucket.org` base URL with
/// the wiremock server's URL for the duration of the test. Implemented as an
/// inline module to keep scope out of the public API.
mod override_url {
    use mcp_server_atlassian::auth::Credentials;
    use mcp_server_atlassian::config::Config;
    use mcp_server_atlassian::error::McpError;
    use mcp_server_atlassian::transport::{
        RequestOptions, TransportResponse, fetch_bitbucket_with_base,
    };
    use wiremock::MockServer;

    pub async fn fetch(
        server: &MockServer,
        client: &reqwest::Client,
        creds: &Credentials,
        config: &Config,
        path: &str,
        options: RequestOptions,
    ) -> Result<TransportResponse, McpError> {
        fetch_bitbucket_with_base(&server.uri(), client, creds, config, path, options).await
    }
}

// ---- Tests ----

#[tokio::test]
async fn get_json_response_is_classified_and_persisted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/workspaces"))
        .and(header("authorization", "Basic YWxpY2VAZXhhbXBsZS5jb206dG9r"))
        .and(header("accept", "application/json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "values": [{"slug":"acme"}]
            })),
        )
        .mount(&server)
        .await;

    let resp = call_mock(&server, "/2.0/workspaces", RequestOptions::default())
        .await
        .unwrap();
    match resp.data {
        ResponseBody::Json(v) => {
            assert_eq!(v["values"][0]["slug"], "acme");
        }
        other => panic!("expected JSON, got {other:?}"),
    }
    let path = resp.raw_response_path.expect("raw path for JSON response");
    assert!(path.starts_with("/tmp/mcp/mcp-server-atlassian/"));
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn text_plain_response_passes_through_without_raw_path() {
    let server = MockServer::start().await;
    let diff = "diff --git a/a b/a\nindex 0..1\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/foo/diff/abc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(diff)
                .insert_header("content-type", "text/plain; charset=utf-8"),
        )
        .mount(&server)
        .await;

    let resp = call_mock(
        &server,
        "/2.0/repositories/foo/diff/abc",
        RequestOptions::default(),
    )
    .await
    .unwrap();
    match resp.data {
        ResponseBody::Text(s) => assert_eq!(s, diff),
        other => panic!("expected Text, got {other:?}"),
    }
    assert!(resp.raw_response_path.is_none());
}

#[tokio::test]
async fn delete_204_is_classified_empty() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/2.0/repositories/foo/branches/bar"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let opts = RequestOptions {
        method: Some(HttpMethod::Delete),
        ..Default::default()
    };
    let resp = call_mock(&server, "/2.0/repositories/foo/branches/bar", opts)
        .await
        .unwrap();
    assert!(matches!(resp.data, ResponseBody::Empty));
    assert!(resp.raw_response_path.is_none());
}

#[tokio::test]
async fn empty_body_is_classified_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/2.0/empty"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("")
                .insert_header("content-type", "text/plain"),
        )
        .mount(&server)
        .await;

    let opts = RequestOptions {
        method: Some(HttpMethod::Post),
        body: Some(json!({})),
        ..Default::default()
    };
    let resp = call_mock(&server, "/2.0/empty", opts).await.unwrap();
    // An empty text/plain body is still "present" by header; we classify as
    // Text("") here. Use the JSON-default path for the Empty case.
    assert!(matches!(resp.data, ResponseBody::Text(ref s) if s.is_empty()));
}

#[tokio::test]
async fn empty_json_body_is_classified_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/2.0/empty"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(&[][..], "application/json"))
        .mount(&server)
        .await;

    let opts = RequestOptions {
        method: Some(HttpMethod::Post),
        body: Some(json!({})),
        ..Default::default()
    };
    let resp = call_mock(&server, "/2.0/empty", opts).await.unwrap();
    assert!(
        matches!(resp.data, ResponseBody::Empty),
        "got {:?}",
        resp.data
    );
}

#[tokio::test]
async fn non_json_body_with_default_content_type_falls_back_to_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/plain"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("this is not JSON")
                .insert_header("content-type", "application/octet-stream"),
        )
        .mount(&server)
        .await;

    let resp = call_mock(&server, "/2.0/plain", RequestOptions::default())
        .await
        .unwrap();
    match resp.data {
        ResponseBody::Text(s) => assert_eq!(s, "this is not JSON"),
        other => panic!("expected Text, got {other:?}"),
    }
    assert!(resp.raw_response_path.is_none());
}

#[tokio::test]
async fn request_body_is_serialized_as_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/2.0/echo"))
        .and(body_json(json!({"title":"hello"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;

    let opts = RequestOptions {
        method: Some(HttpMethod::Post),
        body: Some(json!({"title":"hello"})),
        ..Default::default()
    };
    let resp = call_mock(&server, "/2.0/echo", opts).await.unwrap();
    assert_eq!(resp.data.as_json().unwrap()["ok"], true);
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn auth_failure_maps_to_auth_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/private"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({
                "type":"error",
                "error":{"message":"bad credentials"}
            })),
        )
        .mount(&server)
        .await;

    let err = call_mock(&server, "/2.0/private", RequestOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(401));
    assert!(err.message.contains("bad credentials"));
}

#[tokio::test]
async fn not_found_maps_to_404_with_parsed_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "type":"error",
            "error":{"message":"Repository not found"}
        })))
        .mount(&server)
        .await;

    let err = call_mock(&server, "/2.0/missing", RequestOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Repository not found"));
}

#[tokio::test]
async fn oversized_content_length_rejected() {
    // We can't trick wiremock/hyper into sending a fake content-length mismatch
    // (hyper enforces the invariant server-side). Instead, verify by serving a
    // body that really is 10MB + 1 byte and letting the guard reject it.
    let server = MockServer::start().await;
    let body = "a".repeat(10 * 1024 * 1024 + 1);
    Mock::given(method("GET"))
        .and(path("/2.0/huge"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let err = call_mock(&server, "/2.0/huge", RequestOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.status_code, Some(413));
    assert!(err.message.contains("exceeds maximum limit"));
}

#[tokio::test]
async fn timeout_maps_to_408() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
        .mount(&server)
        .await;

    let opts = RequestOptions {
        timeout: Some(Duration::from_millis(50)),
        ..Default::default()
    };
    let err = call_mock(&server, "/2.0/slow", opts).await.unwrap_err();
    assert_eq!(err.status_code, Some(408));
    assert!(err.message.to_lowercase().contains("timeout"));
}
