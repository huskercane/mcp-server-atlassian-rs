//! Controller-layer integration tests. Verifies the full pipeline
//! (auth → /2.0 prefix → query params → `JMESPath` filter → output format).

use std::collections::HashMap;

use mcp_server_atlassian_bitbucket::config::Config;
use mcp_server_atlassian_bitbucket::controllers::api::{HandleContext, handle_request, normalize_path};
use mcp_server_atlassian_bitbucket::format::OutputFormat;
use mcp_server_atlassian_bitbucket::tools::args::QueryParams;
use mcp_server_atlassian_bitbucket::transport::{HttpMethod, build_client};
use mcp_server_atlassian_bitbucket::vendor::bitbucket::BitbucketVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ATLASSIAN_USER_EMAIL".into(), "alice@example.com".into());
    m.insert("ATLASSIAN_API_TOKEN".into(), "tok".into());
    m
}

fn ctx<'a>(
    client: &'a reqwest::Client,
    config: &'a Config,
    vendor: &'a BitbucketVendor,
) -> HandleContext<'a> {
    HandleContext::new(client, config, vendor)
}

#[test]
fn normalize_path_adds_leading_slash_and_v2_prefix() {
    assert_eq!(normalize_path("workspaces"), "/2.0/workspaces");
    assert_eq!(normalize_path("/workspaces"), "/2.0/workspaces");
    assert_eq!(normalize_path("/2.0/repositories"), "/2.0/repositories");
    assert_eq!(normalize_path("/2.0/custom"), "/2.0/custom");
    assert_eq!(normalize_path("repositories/a/b"), "/2.0/repositories/a/b");
}

#[tokio::test]
async fn get_pipeline_normalizes_path_and_appends_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/workspaces"))
        .and(query_param("pagelen", "25"))
        .and(query_param("page", "2"))
        .and(header("authorization", "Basic YWxpY2VAZXhhbXBsZS5jb206dG9r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [{"slug":"a"},{"slug":"b"}]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let mut qp = QueryParams::new();
    qp.insert("pagelen".into(), "25".into());
    qp.insert("page".into(), "2".into());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/workspaces",
        Some(&qp),
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("\"slug\": \"a\""));
    assert!(resp.raw_response_path.is_some());
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn jq_filter_is_applied_to_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/foo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [
                {"slug":"a","name":"alpha","size":100},
                {"slug":"b","name":"beta","size":200}
            ]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/repositories/foo",
        None,
        None,
        Some("values[*].slug"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    // Filtered output should contain only the slug strings, not sizes
    assert!(resp.content.contains("\"a\""));
    assert!(resp.content.contains("\"b\""));
    assert!(!resp.content.contains("alpha"));
    assert!(!resp.content.contains("100"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn post_body_is_forwarded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/2.0/repositories/foo/pullrequests"))
        .and(body_json(json!({
            "title": "test",
            "source": {"branch": {"name": "feature"}}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 42, "title":"test"})))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Post,
        "/repositories/foo/pullrequests",
        None,
        Some(json!({
            "title":"test",
            "source":{"branch":{"name":"feature"}}
        })),
        Some("{id: id, title: title}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("42"));
    assert!(resp.content.contains("test"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn delete_returns_empty_body_content() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/2.0/repositories/foo/refs/branches/old"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Delete,
        "/repositories/foo/refs/branches/old",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap();

    // 204 → empty body → rendered as `{}` (JSON) or empty TOON.
    assert!(resp.content.contains("{}") || resp.content.is_empty());
    assert!(resp.raw_response_path.is_none());
}

#[tokio::test]
async fn missing_credentials_produces_auth_missing_error() {
    let server = MockServer::start().await;
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = BitbucketVendor::with_base_url(server.uri());

    let err = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/workspaces",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(
        err.kind,
        mcp_server_atlassian_bitbucket::error::ErrorKind::AuthMissing
    );
}

#[tokio::test]
async fn text_plain_body_passes_through_unchanged() {
    let server = MockServer::start().await;
    let diff = "diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+new\n";
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/foo/diff/abc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(diff)
                .insert_header("content-type", "text/plain; charset=utf-8"),
        )
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/repositories/foo/diff/abc",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap();
    assert_eq!(resp.content, diff);
    assert!(resp.raw_response_path.is_none());
}
