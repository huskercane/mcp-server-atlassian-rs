//! Controller-pipeline tests for the Confluence vendor. Mirrors the
//! structure of `jira_controller_tests.rs`: paths pass through verbatim
//! (no `/2.0` prefix), the canonical Confluence error envelope is parsed
//! correctly, and the `ATLASSIAN_SITE_NAME` lookup is bypassed when a
//! base URL override is in place.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::api::{HandleContext, handle_request};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::format::OutputFormat;
use mcp_server_atlassian::tools::args::QueryParams;
use mcp_server_atlassian::transport::{HttpMethod, build_client};
use mcp_server_atlassian::vendor::confluence::ConfluenceVendor;
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
    vendor: &'a ConfluenceVendor,
) -> HandleContext<'a> {
    HandleContext::new(client, config, vendor)
}

#[tokio::test]
async fn get_pipeline_passes_confluence_path_through_unchanged() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wiki/api/v2/spaces"))
        .and(header("authorization", "Basic YWxpY2VAZXhhbXBsZS5jb206dG9r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"id": "1", "key": "DEV", "name": "Engineering"}
            ]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = ConfluenceVendor::with_base_url(server.uri());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/wiki/api/v2/spaces",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("\"id\": \"1\""));
    assert!(resp.content.contains("Engineering"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn cql_search_query_params_are_appended() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wiki/rest/api/search"))
        .and(query_param("cql", "type=page AND space=DEV"))
        .and(query_param("limit", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"id": "p1", "title": "Setup"},
                {"id": "p2", "title": "Onboarding"}
            ]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = ConfluenceVendor::with_base_url(server.uri());

    let mut qp = QueryParams::new();
    qp.insert("cql".into(), "type=page AND space=DEV".into());
    qp.insert("limit".into(), "5".into());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/wiki/rest/api/search",
        Some(&qp),
        None,
        Some("results[*].id"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("p1"));
    assert!(resp.content.contains("p2"));
    assert!(!resp.content.contains("Setup"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn post_create_page_forwards_body_and_returns_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/wiki/api/v2/pages"))
        .and(body_json(json!({
            "spaceId": "123",
            "status": "current",
            "title": "Hello",
            "body": {"representation": "storage", "value": "<p>Hi</p>"}
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "999", "title": "Hello", "status": "current"})),
        )
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = ConfluenceVendor::with_base_url(server.uri());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Post,
        "/wiki/api/v2/pages",
        None,
        Some(json!({
            "spaceId": "123",
            "status": "current",
            "title": "Hello",
            "body": {"representation": "storage", "value": "<p>Hi</p>"}
        })),
        Some("{id: id, title: title}"),
        OutputFormat::Json,
    )
    .await
    .unwrap();

    assert!(resp.content.contains("\"id\": \"999\""));
    assert!(resp.content.contains("Hello"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn delete_204_returns_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/wiki/api/v2/pages/999"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = ConfluenceVendor::with_base_url(server.uri());

    let resp = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Delete,
        "/wiki/api/v2/pages/999",
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
async fn confluence_envelope_404_surfaces_title_and_detail() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wiki/api/v2/pages/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "title": "Not Found",
            "status": 404,
            "detail": "Page does not exist"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = ConfluenceVendor::with_base_url(server.uri());

    let err = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/wiki/api/v2/pages/missing",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Not Found"));
    assert!(err.message.contains("Page does not exist"));
}

#[tokio::test]
async fn confluence_403_is_api_error_not_auth_invalid() {
    // Confirms the controller surfaces the TS asymmetry: Confluence 403
    // is an ApiError, unlike Jira's auth_invalid mapping.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wiki/api/v2/spaces/restricted"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "message": "User does not have access to this space"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = ConfluenceVendor::with_base_url(server.uri());

    let err = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/wiki/api/v2/spaces/restricted",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(403));
    assert!(err.message.contains("Access denied"));
}

#[tokio::test]
async fn missing_site_name_surfaces_at_tool_call_time_not_at_construction() {
    let vendor = ConfluenceVendor::new();
    let client = build_client().unwrap();
    let config = Config::from_map(creds());

    let err = handle_request(
        &ctx(&client, &config, &vendor),
        HttpMethod::Get,
        "/wiki/api/v2/spaces",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("ATLASSIAN_SITE_NAME"));
    assert!(err.message.contains("conf_*"));
}
