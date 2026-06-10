#![allow(clippy::doc_markdown)]

//! Controller-pipeline tests for the New Relic vendor. Exercises the full path
//! a `newrelic_query` call takes: read the static `NEW_RELIC_API_KEY` from
//! config → wrap the GraphQL document into `{query, variables}` → dispatch a
//! POST to `/graphql` with the custom `API-Key` header through the shared
//! transport → classify both the non-2xx envelope and the `200 OK` +
//! `errors`-array envelope unique to NerdGraph.
//!
//! NerdGraph is stood up on a wiremock instance, so these tests need no network
//! and no global state.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::newrelic::{NewRelicContext, query};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::tools::args::NewRelicQueryArgs;
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::newrelic::NewRelicVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("NEW_RELIC_API_KEY".into(), "NRAK-123".into());
    m
}

fn vendor(server: &MockServer) -> NewRelicVendor {
    NewRelicVendor::with_base_url(server.uri())
}

fn args(graphql: &str, variables: Option<serde_json::Value>, jq: Option<&str>) -> NewRelicQueryArgs {
    NewRelicQueryArgs {
        query: graphql.to_string(),
        variables,
        jq: jq.map(str::to_string),
        output_format: None,
    }
}

#[tokio::test]
async fn query_sends_api_key_header_and_wraps_body() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("api-key", "NRAK-123"))
        .and(body_json(json!({ "query": "{ actor { user { name } } }" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "actor": { "user": { "name": "Ada" } } }
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = NewRelicContext::new(&client, &config, &vendor);

    let resp = query(
        &ctx,
        &args(
            "{ actor { user { name } } }",
            None,
            Some("data.actor.user.name"),
        ),
    )
    .await
    .unwrap();

    assert!(resp.content.contains("Ada"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn query_forwards_variables() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_json(json!({
            "query": "query($id: Int!) { actor { account(id: $id) { name } } }",
            "variables": { "id": 42 }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "actor": { "account": { "name": "Prod" } } }
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = NewRelicContext::new(&client, &config, &vendor);

    let resp = query(
        &ctx,
        &args(
            "query($id: Int!) { actor { account(id: $id) { name } } }",
            Some(json!({ "id": 42 })),
            Some("data.actor.account.name"),
        ),
    )
    .await
    .unwrap();

    assert!(resp.content.contains("Prod"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn errors_array_in_200_body_surfaces_as_error() {
    // The defining NerdGraph quirk: HTTP 200, but a non-empty `errors` array ⇒
    // this must be an error, not a "successful" response carrying an error
    // payload.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "errors": [{ "message": "NRQL Syntax Error" }]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = NewRelicContext::new(&client, &config, &vendor);

    let err = query(&ctx, &args("{ bad }", None, None))
        .await
        .unwrap_err();

    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("NRQL Syntax Error"));
}

#[tokio::test]
async fn auth_error_class_in_200_body_is_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{
                "message": "Access denied",
                "extensions": { "errorClass": "UNAUTHENTICATED" }
            }]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = NewRelicContext::new(&client, &config, &vendor);

    let err = query(&ctx, &args("{ actor { user { name } } }", None, None))
        .await
        .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthInvalid);
}

#[tokio::test]
async fn non_2xx_surfaces_as_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = NewRelicContext::new(&client, &config, &vendor);

    let err = query(&ctx, &args("{ actor { user { name } } }", None, None))
        .await
        .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthInvalid);
}

#[tokio::test]
async fn missing_api_key_surfaces_auth_missing_at_call_time() {
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = NewRelicVendor::with_base_url("http://127.0.0.1:0");
    let ctx = NewRelicContext::new(&client, &config, &vendor);

    let err = query(&ctx, &args("{ actor { user { name } } }", None, None))
        .await
        .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("NEW_RELIC_API_KEY"));
}
