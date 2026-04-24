//! Unit tests for `JiraVendor`. Covers base-URL resolution (env + override +
//! missing), path normalisation (passthrough, no `/2.0`), and the Jira error
//! envelope classifier.

use std::collections::HashMap;

use mcp_server_atlassian_bitbucket::config::Config;
use mcp_server_atlassian_bitbucket::error::{ErrorKind, OriginalError};
use mcp_server_atlassian_bitbucket::vendor::Vendor;
use mcp_server_atlassian_bitbucket::vendor::jira::JiraVendor;
use mcp_server_atlassian_bitbucket::vendor::jira::error::{classify, parse_error_body};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use serde_json::json;

fn cfg(entries: &[(&str, &str)]) -> Config {
    let mut m = HashMap::new();
    for (k, v) in entries {
        m.insert((*k).to_string(), (*v).to_string());
    }
    Config::from_map(m)
}

// ---- name ----

#[test]
fn name_is_canonical_jira() {
    assert_eq!(JiraVendor::new().name(), "jira");
}

// ---- base_url ----

#[test]
fn base_url_from_site_name_env() {
    let vendor = JiraVendor::new();
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "mycompany")]);
    let url = vendor.base_url(&config).unwrap();
    assert_eq!(url, "https://mycompany.atlassian.net");
}

#[test]
fn base_url_trims_whitespace_around_site_name() {
    let vendor = JiraVendor::new();
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "  mycompany  ")]);
    let url = vendor.base_url(&config).unwrap();
    assert_eq!(url, "https://mycompany.atlassian.net");
}

#[test]
fn base_url_missing_site_returns_auth_missing() {
    // The crucial guarantee: a Bitbucket-only deployment must never crash
    // at server boot just because Jira isn't configured. The error only
    // shows up when a `jira_*` tool is actually invoked.
    let vendor = JiraVendor::new();
    let config = cfg(&[]);
    let err = vendor.base_url(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("ATLASSIAN_SITE_NAME"));
}

#[test]
fn base_url_empty_site_name_is_treated_as_missing() {
    let vendor = JiraVendor::new();
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "   ")]);
    let err = vendor.base_url(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

#[test]
fn with_base_url_skips_env_lookup_entirely() {
    // Tests typically construct the vendor with a wiremock URL. The env
    // path must not be consulted at all in that case — even if the env
    // var is set, the override wins.
    let vendor = JiraVendor::with_base_url("http://127.0.0.1:54321");
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "should-not-be-used")]);
    let url = vendor.base_url(&config).unwrap();
    assert_eq!(url, "http://127.0.0.1:54321");
}

#[test]
fn with_base_url_resolves_without_any_config() {
    let vendor = JiraVendor::with_base_url("http://localhost:8080");
    let url = vendor.base_url(&Config::default()).unwrap();
    assert_eq!(url, "http://localhost:8080");
}

// ---- normalize_path ----

#[test]
fn normalize_path_adds_leading_slash_only() {
    let vendor = JiraVendor::new();
    assert_eq!(vendor.normalize_path("rest/api/3/myself"), "/rest/api/3/myself");
    assert_eq!(vendor.normalize_path("/rest/api/3/myself"), "/rest/api/3/myself");
}

#[test]
fn normalize_path_does_not_prepend_v2_like_bitbucket() {
    // Sanity: Bitbucket prepends `/2.0`. Jira must NOT — paths like
    // `/rest/api/3/...` and `/rest/agile/1.0/...` need to pass through
    // verbatim.
    let vendor = JiraVendor::new();
    assert_eq!(
        vendor.normalize_path("/rest/api/3/search/jql"),
        "/rest/api/3/search/jql"
    );
    assert_eq!(
        vendor.normalize_path("/rest/agile/1.0/board"),
        "/rest/agile/1.0/board"
    );
}

// ---- classify_error: status code mapping ----

#[test]
fn classify_401_maps_to_auth_invalid() {
    let body = r#"{"errorMessages":["Login required"],"errors":{}}"#;
    let err = classify(StatusCode::UNAUTHORIZED, body);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(401));
    assert!(err.message.contains("Login required"));
    assert!(err.message.contains("Authentication failed"));
}

#[test]
fn classify_403_maps_to_api_error_with_403() {
    let body = r#"{"errorMessages":["You do not have permission."],"errors":{}}"#;
    let err = classify(StatusCode::FORBIDDEN, body);
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(403));
    assert!(err.message.contains("Permission denied"));
}

#[test]
fn classify_404_maps_to_api_error_with_404() {
    let body = r#"{"errorMessages":["Issue does not exist or you do not have permission to see it."],"errors":{}}"#;
    let err = classify(StatusCode::NOT_FOUND, body);
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Issue does not exist"));
}

#[test]
fn classify_429_maps_to_rate_limit_message() {
    let err = classify(StatusCode::TOO_MANY_REQUESTS, r#"{"message":"slow down"}"#);
    assert_eq!(err.status_code, Some(429));
    assert!(err.message.contains("Rate limit exceeded"));
    assert!(err.message.contains("slow down"));
}

#[test]
fn classify_5xx_maps_to_service_error() {
    let err = classify(StatusCode::SERVICE_UNAVAILABLE, "");
    assert_eq!(err.status_code, Some(503));
    assert!(err.message.contains("Service error"));
}

// ---- parse_error_body: envelope shapes ----

#[test]
fn parse_canonical_envelope_with_messages_only() {
    let body = r#"{"errorMessages":["Issue does not exist"],"errors":{}}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("Issue does not exist"));
    matches_json(parsed.original.as_ref(), &json!({
        "errorMessages": ["Issue does not exist"],
        "errors": {}
    }));
}

#[test]
fn parse_canonical_envelope_with_field_errors_only() {
    // Field validation errors (most often 400 from POST /issue) live in
    // the `errors` object keyed by field name. Concatenated as
    // "field: message" so the user sees what's wrong.
    let body = r#"{"errorMessages":[],"errors":{"summary":"Summary is required.","priority":"Invalid priority."}}"#;
    let parsed = parse_error_body(body);
    let msg = parsed.message.expect("message present");
    // BTreeMap ordering of `errors` keys is preserved by serde_json's
    // default object iteration; we assert both substrings instead of an
    // exact match to stay robust.
    assert!(msg.contains("summary: Summary is required."));
    assert!(msg.contains("priority: Invalid priority."));
}

#[test]
fn parse_canonical_envelope_combines_messages_and_field_errors() {
    let body = r#"{"errorMessages":["Some context"],"errors":{"summary":"Required."}}"#;
    let parsed = parse_error_body(body);
    let msg = parsed.message.expect("message present");
    assert!(msg.contains("Some context"));
    assert!(msg.contains("summary: Required."));
}

#[test]
fn parse_oauth_style_error() {
    let body = r#"{"error":"invalid_token","error_description":"The access token is invalid"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("The access token is invalid"));
}

#[test]
fn parse_flat_message_fallback() {
    let body = r#"{"message":"Something went wrong"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("Something went wrong"));
}

#[test]
fn parse_non_json_body_passes_through_as_string() {
    let body = "<html>502 Bad Gateway</html>";
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some(body));
    assert!(matches!(parsed.original, Some(OriginalError::String(_))));
}

#[test]
fn parse_empty_body_yields_default() {
    let parsed = parse_error_body("");
    assert!(parsed.message.is_none());
    assert!(parsed.original.is_none());
}

#[test]
fn parse_unrecognised_json_keeps_payload_as_original() {
    // Some endpoints return shapes we don't model (e.g. arrays, unknown
    // keys). Surface the parsed JSON as `original` so the LLM sees the
    // raw payload even when we couldn't extract a message.
    let body = r#"{"unknownField":"nope"}"#;
    let parsed = parse_error_body(body);
    assert!(parsed.message.is_none());
    matches_json(parsed.original.as_ref(), &json!({"unknownField":"nope"}));
}

// ---- helpers ----

fn matches_json(original: Option<&OriginalError>, expected: &serde_json::Value) {
    match original {
        Some(OriginalError::Json(v)) => assert_eq!(v, expected),
        other => panic!("expected JSON original, got {other:?}"),
    }
}
