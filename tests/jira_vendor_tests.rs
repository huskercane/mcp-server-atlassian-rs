//! Unit tests for `JiraVendor`. Covers base-URL resolution (env + override +
//! missing), path normalisation (passthrough, no `/2.0`), and the Jira error
//! envelope classifier.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::{ErrorKind, OriginalError};
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::jira::JiraVendor;
use mcp_server_atlassian::vendor::jira::error::{classify, parse_error_body};
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

// ---- classify_error: status code mapping (TS Jira parity) ----
//
// Message prefixes mirror the TS Jira server's `transport.util.ts`. The
// 403 case in particular is auth_invalid, not api_error — TS treats both
// authentication failure and permission denial as auth/permission errors.

#[test]
fn classify_401_maps_to_auth_invalid_with_ts_prefix() {
    let body = r#"{"errorMessages":["Login required"],"errors":{}}"#;
    let err = classify(StatusCode::UNAUTHORIZED, body);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    // The auth_invalid factory itself sets status_code=Some(401); 401
    // responses inherit that default verbatim.
    assert_eq!(err.status_code, Some(401));
    assert!(
        err.message.starts_with("Authentication failed. Jira API: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("Login required"));
}

#[test]
fn classify_403_maps_to_auth_invalid_per_ts_parity() {
    // The previous Rust port mapped 403 to api_error; that broke parity.
    // TS createAuthInvalidError is used for both 401 and 403 to signal
    // "auth/permission failure" as a single category. We override the
    // factory's default 401 status so the actual HTTP status is preserved
    // for callers that branch on status_code.
    let body = r#"{"errorMessages":["You do not have permission."],"errors":{}}"#;
    let err = classify(StatusCode::FORBIDDEN, body);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(403));
    assert!(
        err.message.starts_with("Insufficient permissions. Jira API: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("You do not have permission."));
}

#[test]
fn classify_404_uses_resource_not_found_prefix() {
    let body = r#"{"errorMessages":["Issue does not exist or you do not have permission to see it."],"errors":{}}"#;
    let err = classify(StatusCode::NOT_FOUND, body);
    assert_eq!(err.status_code, Some(404));
    assert!(
        err.message.starts_with("Resource not found. Jira API: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("Issue does not exist"));
}

#[test]
fn classify_429_uses_rate_limit_prefix() {
    let err = classify(StatusCode::TOO_MANY_REQUESTS, r#"{"message":"slow down"}"#);
    assert_eq!(err.status_code, Some(429));
    assert!(
        err.message.starts_with("Rate limit exceeded. Jira API: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("slow down"));
}

#[test]
fn classify_5xx_uses_jira_server_error_prefix() {
    let err = classify(StatusCode::SERVICE_UNAVAILABLE, "");
    assert_eq!(err.status_code, Some(503));
    assert!(
        err.message.starts_with("Jira server error. Detail: "),
        "got: {}",
        err.message
    );
}

#[test]
fn classify_other_status_uses_request_failed_prefix() {
    let err = classify(StatusCode::BAD_REQUEST, r#"{"message":"bad input"}"#);
    assert_eq!(err.status_code, Some(400));
    assert!(
        err.message.starts_with("Jira API request failed. Detail: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("bad input"));
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
fn parse_multiple_error_messages_join_with_semicolon() {
    // TS joins errorMessages array with '; ' (within the part), not ' | '.
    let body = r#"{"errorMessages":["First problem","Second problem"]}"#;
    let parsed = parse_error_body(body);
    assert_eq!(
        parsed.message.as_deref(),
        Some("First problem; Second problem")
    );
}

#[test]
fn parse_canonical_envelope_with_field_errors_only() {
    let body = r#"{"errorMessages":[],"errors":{"summary":"Summary is required.","priority":"Invalid priority."}}"#;
    let parsed = parse_error_body(body);
    let msg = parsed.message.expect("message present");
    assert!(msg.contains("summary: Summary is required."));
    assert!(msg.contains("priority: Invalid priority."));
    // Within-part separator is "; ", not " | ".
    assert!(msg.contains("; "));
    assert!(!msg.contains(" | "));
}

#[test]
fn parse_canonical_envelope_combines_messages_and_field_errors_with_pipe_separator() {
    // TS joins TOP-LEVEL parts with " | " — errorMessages and field
    // errors are separate parts.
    let body = r#"{"errorMessages":["Some context"],"errors":{"summary":"Required."}}"#;
    let parsed = parse_error_body(body);
    let msg = parsed.message.expect("message present");
    assert_eq!(msg, "Some context | summary: Required.");
}

#[test]
fn parse_array_style_errors_extracts_title_and_detail() {
    // Legacy Atlassian shape: `errors` is an array, not an object. Each
    // entry can carry `title` and `detail`; TS pushes them as separate
    // parts to the join list.
    let body = r#"{"errors":[{"status":400,"code":"INVALID_REQUEST_PARAMETER","title":"Invalid parameter","detail":"`jql` is required"}]}"#;
    let parsed = parse_error_body(body);
    let msg = parsed.message.expect("message present");
    assert_eq!(msg, "Invalid parameter | `jql` is required");
}

#[test]
fn parse_warning_messages_get_warnings_prefix_and_join() {
    // Warnings appear after error parts and get a "Warnings: " prefix.
    let body = r#"{"errorMessages":["Real error"],"warningMessages":["heads up","also this"]}"#;
    let parsed = parse_error_body(body);
    let msg = parsed.message.expect("message present");
    assert_eq!(msg, "Real error | Warnings: heads up; also this");
}

#[test]
fn parse_all_envelope_shapes_combined() {
    // The full TS aggregation: errorMessages -> field errors -> message
    // -> array errors title -> array errors detail -> warnings. Note
    // that the same `errors` key is checked as both object AND array;
    // they are mutually exclusive at runtime so realistic payloads carry
    // one shape, not both. This test exercises every recognised slot
    // independently.
    let body = r#"{
        "errorMessages": ["msg1", "msg2"],
        "message": "flat message",
        "warningMessages": ["w1"]
    }"#;
    let parsed = parse_error_body(body);
    let msg = parsed.message.expect("message present");
    assert_eq!(msg, "msg1; msg2 | flat message | Warnings: w1");
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
fn parse_unrecognised_json_keeps_payload_as_original_with_no_message() {
    // Shapes we don't model surface no message but keep the JSON as
    // `original` so the LLM sees the raw payload via the error formatter.
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
