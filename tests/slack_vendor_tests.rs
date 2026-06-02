#![allow(clippy::doc_markdown)]

//! Unit tests for `SlackVendor`: base-URL resolution, verbatim path
//! normalisation, token lookup, the non-2xx classifier, and — the part unique
//! to Slack — the `{"ok": false}` success-body classifier. The Bearer-auth
//! dispatch path is covered end-to-end in `slack_controller_tests.rs`.

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::slack::SlackVendor;
use mcp_server_atlassian::vendor::slack::error::{classify, classify_ok_envelope};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use serde_json::json;
use std::collections::HashMap;

fn empty_config() -> Config {
    Config::from_map(HashMap::new())
}

// ---- name ----

#[test]
fn name_is_canonical_slack() {
    assert_eq!(SlackVendor::new().name(), "slack");
}

// ---- base_url ----

#[test]
fn base_url_defaults_to_slack_api() {
    let vendor = SlackVendor::new();
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "https://slack.com/api"
    );
}

#[test]
fn base_url_override_is_returned_verbatim() {
    let vendor = SlackVendor::with_base_url("http://localhost:9999");
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "http://localhost:9999"
    );
}

// ---- normalize_path ----

#[test]
fn normalize_path_passes_through_with_leading_slash() {
    let vendor = SlackVendor::new();
    assert_eq!(
        vendor.normalize_path("/conversations.list"),
        "/conversations.list"
    );
}

#[test]
fn normalize_path_prepends_missing_leading_slash() {
    let vendor = SlackVendor::new();
    assert_eq!(vendor.normalize_path("auth.test"), "/auth.test");
}

// ---- token lookup ----

#[test]
fn token_reads_from_slack_config_section() {
    let mut m = HashMap::new();
    m.insert("SLACK_TOKEN".to_string(), "xoxb-abc".to_string());
    let config = Config::from_map(m);
    assert_eq!(SlackVendor::new().token(&config).unwrap(), "xoxb-abc");
}

#[test]
fn token_missing_is_auth_missing_error() {
    let err = SlackVendor::new().token(&empty_config()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("SLACK_TOKEN"));
}

#[test]
fn token_blank_is_treated_as_missing() {
    let mut m = HashMap::new();
    m.insert("SLACK_TOKEN".to_string(), "   ".to_string());
    let config = Config::from_map(m);
    let err = SlackVendor::new().token(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

// ---- non-2xx classifier ----

#[test]
fn classify_429_is_rate_limit() {
    let err = classify(StatusCode::TOO_MANY_REQUESTS, "ratelimited");
    assert_eq!(err.status_code, Some(429));
    assert!(err.message.contains("Rate limit exceeded"));
}

#[test]
fn classify_500_is_server_error() {
    let err = classify(StatusCode::INTERNAL_SERVER_ERROR, "<html>oops</html>");
    assert_eq!(err.status_code, Some(500));
    assert!(err.message.contains("Slack server error"));
}

#[test]
fn classify_empty_body_falls_back_to_status_reason() {
    let err = classify(StatusCode::BAD_GATEWAY, "");
    assert_eq!(err.status_code, Some(502));
    assert!(err.message.contains("502"));
}

// ---- ok:false success-body classifier ----

#[test]
fn ok_true_is_not_an_error() {
    let v = json!({"ok": true, "channels": []});
    assert!(classify_ok_envelope(&v).is_none());
}

#[test]
fn missing_ok_field_is_treated_as_success() {
    // Some endpoints return bare payloads without an `ok` field.
    let v = json!({"some": "payload"});
    assert!(classify_ok_envelope(&v).is_none());
}

#[test]
fn non_object_body_is_not_an_error() {
    let v = json!(["a", "b"]);
    assert!(classify_ok_envelope(&v).is_none());
}

#[test]
fn ok_false_invalid_auth_is_auth_invalid() {
    let v = json!({"ok": false, "error": "invalid_auth"});
    let err = classify_ok_envelope(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("invalid_auth"));
}

#[test]
fn ok_false_ratelimited_is_429() {
    let v = json!({"ok": false, "error": "ratelimited"});
    let err = classify_ok_envelope(&v).unwrap();
    assert_eq!(err.status_code, Some(429));
}

#[test]
fn ok_false_channel_not_found_is_404() {
    let v = json!({"ok": false, "error": "channel_not_found"});
    let err = classify_ok_envelope(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
}

#[test]
fn ok_false_generic_is_api_error() {
    let v = json!({"ok": false, "error": "something_bad"});
    let err = classify_ok_envelope(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("something_bad"));
}

#[test]
fn ok_false_without_error_code_still_errors() {
    let v = json!({"ok": false});
    let err = classify_ok_envelope(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("unknown_error"));
}
