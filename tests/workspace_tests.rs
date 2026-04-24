//! Default workspace resolver tests. Uses wiremock for the API path and
//! a caller-supplied `Config` for the env path.

use std::collections::HashMap;

use mcp_server_atlassian_bitbucket::config::Config;
use mcp_server_atlassian_bitbucket::controllers::api::HandleContext;
use mcp_server_atlassian_bitbucket::transport::build_client;
use mcp_server_atlassian_bitbucket::vendor::bitbucket::BitbucketVendor;
use mcp_server_atlassian_bitbucket::workspace::{reset_cache, resolve_default_workspace};
use pretty_assertions::assert_eq;
use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds_only() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ATLASSIAN_USER_EMAIL".into(), "alice@example.com".into());
    m.insert("ATLASSIAN_API_TOKEN".into(), "tok".into());
    m
}

#[tokio::test]
#[serial]
async fn env_override_short_circuits_api_call() {
    reset_cache();
    let server = MockServer::start().await;
    // No mocks registered → any API call would 404 and fail the test.

    let mut env = creds_only();
    env.insert("BITBUCKET_DEFAULT_WORKSPACE".into(), "acme".into());
    let config = Config::from_map(env);
    let client = build_client().unwrap();
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let ctx = HandleContext::new(&client, &config, &vendor);

    let slug = resolve_default_workspace(&ctx).await;
    assert_eq!(slug.as_deref(), Some("acme"));
}

#[tokio::test]
#[serial]
async fn api_fallback_returns_first_workspace() {
    reset_cache();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/user/permissions/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [
                {"workspace": {"slug": "first-team"}},
                {"workspace": {"slug": "second-team"}}
            ]
        })))
        .mount(&server)
        .await;

    let config = Config::from_map(creds_only());
    let client = build_client().unwrap();
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let ctx = HandleContext::new(&client, &config, &vendor);

    let slug = resolve_default_workspace(&ctx).await;
    assert_eq!(slug.as_deref(), Some("first-team"));
}

#[tokio::test]
#[serial]
async fn empty_api_response_resolves_to_none() {
    reset_cache();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/user/permissions/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values": []})))
        .mount(&server)
        .await;

    let config = Config::from_map(creds_only());
    let client = build_client().unwrap();
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let ctx = HandleContext::new(&client, &config, &vendor);

    let slug = resolve_default_workspace(&ctx).await;
    assert_eq!(slug, None);
}

#[tokio::test]
#[serial]
async fn cache_prevents_second_api_call() {
    reset_cache();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/user/permissions/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [{"workspace": {"slug": "cached-team"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config = Config::from_map(creds_only());
    let client = build_client().unwrap();
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let ctx = HandleContext::new(&client, &config, &vendor);

    let first = resolve_default_workspace(&ctx).await;
    let second = resolve_default_workspace(&ctx).await;
    assert_eq!(first.as_deref(), Some("cached-team"));
    assert_eq!(second.as_deref(), Some("cached-team"));
    // The MockServer verifies `.expect(1)` on drop.
}
