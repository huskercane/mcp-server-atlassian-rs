//! Default workspace resolver tests. Uses wiremock for the API path and
//! a caller-supplied `Config` for the env path.
//!
//! Each test constructs its own [`WorkspaceCache`] to verify the
//! per-instance scoping invariant introduced when the cache moved off
//! the process-global `OnceLock`. Tests no longer need `#[serial]` for
//! cache-state reasons, since each cache is isolated.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::api::BitbucketContext;
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::bitbucket::BitbucketVendor;
use mcp_server_atlassian::workspace::{WorkspaceCache, resolve_default_workspace};
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds_only() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ATLASSIAN_USER_EMAIL".into(), "alice@example.com".into());
    m.insert("ATLASSIAN_API_TOKEN".into(), "tok".into());
    m
}

#[tokio::test]
async fn env_override_short_circuits_api_call() {
    let server = MockServer::start().await;
    // No mocks registered → any API call would 404 and fail the test.

    let mut env = creds_only();
    env.insert("BITBUCKET_DEFAULT_WORKSPACE".into(), "acme".into());
    let config = Config::from_map(env);
    let client = build_client().unwrap();
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);

    let slug = resolve_default_workspace(&ctx).await;
    assert_eq!(slug.as_deref(), Some("acme"));
}

#[tokio::test]
async fn api_fallback_returns_first_workspace() {
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
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);

    let slug = resolve_default_workspace(&ctx).await;
    assert_eq!(slug.as_deref(), Some("first-team"));
}

#[tokio::test]
async fn empty_api_response_resolves_to_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/user/permissions/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"values": []})))
        .mount(&server)
        .await;

    let config = Config::from_map(creds_only());
    let client = build_client().unwrap();
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);

    let slug = resolve_default_workspace(&ctx).await;
    assert_eq!(slug, None);
}

#[tokio::test]
async fn cache_prevents_second_api_call() {
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
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);

    let first = resolve_default_workspace(&ctx).await;
    let second = resolve_default_workspace(&ctx).await;
    assert_eq!(first.as_deref(), Some("cached-team"));
    assert_eq!(second.as_deref(), Some("cached-team"));
    // The MockServer verifies `.expect(1)` on drop.
}

#[tokio::test]
async fn separate_caches_do_not_leak_across_instances() {
    // Regression test for the per-instance scoping fix: two parallel
    // BitbucketContexts pointing at different mock servers must each
    // see their own workspace, not the other's. With the old global
    // OnceLock this test would have leaked the first server's slug
    // into the second context's lookups.
    let server_a = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/user/permissions/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [{"workspace": {"slug": "team-a"}}]
        })))
        .mount(&server_a)
        .await;

    let server_b = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/user/permissions/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [{"workspace": {"slug": "team-b"}}]
        })))
        .mount(&server_b)
        .await;

    let config = Config::from_map(creds_only());
    let client = build_client().unwrap();
    let vendor_a = BitbucketVendor::with_base_url(server_a.uri());
    let vendor_b = BitbucketVendor::with_base_url(server_b.uri());
    let cache_a = WorkspaceCache::new();
    let cache_b = WorkspaceCache::new();
    let ctx_a = BitbucketContext::new(&client, &config, &vendor_a, &cache_a);
    let ctx_b = BitbucketContext::new(&client, &config, &vendor_b, &cache_b);

    assert_eq!(
        resolve_default_workspace(&ctx_a).await.as_deref(),
        Some("team-a")
    );
    assert_eq!(
        resolve_default_workspace(&ctx_b).await.as_deref(),
        Some("team-b")
    );
    // And the caches are still independent on the second hit.
    assert_eq!(
        resolve_default_workspace(&ctx_a).await.as_deref(),
        Some("team-a")
    );
}

#[test]
fn workspace_cache_clear_resets_state() {
    let cache = WorkspaceCache::new();
    cache.set("first".into());
    assert_eq!(cache.get().as_deref(), Some("first"));
    cache.clear();
    assert_eq!(cache.get(), None);
}
