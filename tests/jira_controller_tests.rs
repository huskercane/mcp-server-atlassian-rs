//! Controller-pipeline tests for the Jira vendor. Mirrors the structure of
//! `controller_tests.rs` (which exercises Bitbucket) but verifies the
//! Jira-specific behaviours: paths pass through verbatim (no `/2.0`
//! prefix), the canonical Jira error envelope is parsed correctly, and
//! the `ATLASSIAN_SITE_NAME` lookup is bypassed when a base URL override
//! is in place.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::api::{HandleContext, handle_request};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::format::OutputFormat;
use mcp_server_atlassian::tools::args::QueryParams;
use mcp_server_atlassian::transport::{HttpMethod, build_client};
use mcp_server_atlassian::vendor::jira::JiraVendor;
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
    vendor: &'a JiraVendor,
) -> HandleContext<'a> {
    HandleContext::new(client, config, vendor)
}

#[tokio::test]
async fn get_pipeline_passes_jira_path_through_unchanged() {
    // Jira paths include the API version segment (`/rest/api/3/...`); the
    // vendor must NOT auto-prepend `/2.0` like Bitbucket does. Wiremock
    // matches on the exact path the server receives.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .and(header("authorization", "Basic YWxpY2VAZXhhbXBsZS5jb206dG9r"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"accountId": "abc-123", "displayName": "Alice"})),
        )
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = JiraVendor::with_base_url(server.uri());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/rest/api/3/myself",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("\"accountId\": \"abc-123\""));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn search_jql_query_params_are_appended() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/search/jql"))
        .and(query_param("jql", "project=PROJ"))
        .and(query_param("maxResults", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [
                {"key": "PROJ-1", "fields": {"summary": "First"}},
                {"key": "PROJ-2", "fields": {"summary": "Second"}}
            ]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = JiraVendor::with_base_url(server.uri());

    let mut qp = QueryParams::new();
    qp.insert("jql".into(), "project=PROJ".into());
    qp.insert("maxResults".into(), "5".into());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/rest/api/3/search/jql",
        Some(&qp),
        None,
        Some("issues[*].key"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    // JMESPath filter should reduce to just the keys.
    assert!(resp.content.contains("PROJ-1"));
    assert!(resp.content.contains("PROJ-2"));
    assert!(!resp.content.contains("First"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn post_create_issue_forwards_body_and_returns_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .and(body_json(json!({
            "fields": {
                "project": {"key": "PROJ"},
                "summary": "New issue",
                "issuetype": {"name": "Task"}
            }
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"id": "10001", "key": "PROJ-42", "self": "..."})),
        )
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = JiraVendor::with_base_url(server.uri());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Post,
        "/rest/api/3/issue",
        None,
        Some(json!({
            "fields": {
                "project": {"key": "PROJ"},
                "summary": "New issue",
                "issuetype": {"name": "Task"}
            }
        })),
        Some("{key: key, id: id}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("PROJ-42"));
    assert!(resp.content.contains("10001"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn delete_204_returns_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = JiraVendor::with_base_url(server.uri());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Delete,
        "/rest/api/3/issue/PROJ-1",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("{}") || resp.content.is_empty());
    assert!(resp.raw_response_path.is_none());
}

#[tokio::test]
async fn jira_envelope_404_surfaces_error_messages() {
    // Verifies the controller propagates the Jira-specific error envelope
    // up through the transport layer's vendor.classify_error hook.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/MISSING-1"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "errorMessages": ["Issue does not exist or you do not have permission to see it."],
            "errors": {}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = JiraVendor::with_base_url(server.uri());

    let err = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/rest/api/3/issue/MISSING-1",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Issue does not exist"));
}

#[tokio::test]
async fn jira_field_validation_errors_are_concatenated() {
    // 400 with a populated `errors` map (typical for create-issue
    // validation failures).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errorMessages": [],
            "errors": {"summary": "Summary is required."}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = JiraVendor::with_base_url(server.uri());

    let err = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Post,
        "/rest/api/3/issue",
        None,
        Some(json!({"fields": {}})),
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, Some(400));
    assert!(err.message.contains("summary: Summary is required."));
}

#[tokio::test]
async fn missing_site_name_surfaces_at_tool_call_time_not_at_construction() {
    // Critical guarantee: a Bitbucket-only deployment must not crash at
    // server boot just because Jira isn't configured. Construction of
    // `JiraVendor::new()` is infallible; the `ATLASSIAN_SITE_NAME`
    // lookup only happens inside the per-request transport call.
    let vendor = JiraVendor::new(); // would-be production constructor — no panic
    let client = build_client().unwrap();
    // Caller has Atlassian creds but never set ATLASSIAN_SITE_NAME.
    let config = Config::from_map(creds());

    let err = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/rest/api/3/myself",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("ATLASSIAN_SITE_NAME"));
}
