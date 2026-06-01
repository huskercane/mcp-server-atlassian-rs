//! Unit tests for `ZoomVendor`: base-URL resolution (default + override),
//! verbatim path normalisation, and the Zoom REST error-envelope classifier.
//! The OAuth token lifecycle is covered end-to-end in
//! `zoom_controller_tests.rs`.

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::{ErrorKind, OriginalError};
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::zoom::ZoomVendor;
use mcp_server_atlassian::vendor::zoom::error::{classify, parse_error_body};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use std::collections::HashMap;

fn empty_config() -> Config {
    Config::from_map(HashMap::new())
}

// ---- name ----

#[test]
fn name_is_canonical_zoom() {
    assert_eq!(ZoomVendor::new().name(), "zoom");
}

// ---- base_url ----

#[test]
fn base_url_defaults_to_zoom_api_v2() {
    // Independent of config — Zoom's base is fixed; only credentials come
    // from config.
    let vendor = ZoomVendor::new();
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "https://api.zoom.us/v2"
    );
}

#[test]
fn base_url_override_is_returned_verbatim() {
    let vendor = ZoomVendor::with_base_url("http://localhost:1234");
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "http://localhost:1234"
    );
}

// ---- normalize_path ----

#[test]
fn normalize_path_passes_through_with_leading_slash() {
    let vendor = ZoomVendor::new();
    assert_eq!(
        vendor.normalize_path("/users/me/meetings"),
        "/users/me/meetings"
    );
}

#[test]
fn normalize_path_prepends_missing_leading_slash() {
    let vendor = ZoomVendor::new();
    assert_eq!(
        vendor.normalize_path("users/me/meetings"),
        "/users/me/meetings"
    );
}

#[test]
fn normalize_path_does_not_prepend_api_version() {
    // Unlike Bitbucket (`/2.0`), Zoom callers supply paths relative to the
    // `/v2` base, so no version segment is injected.
    let vendor = ZoomVendor::new();
    assert!(!vendor.normalize_path("/meetings/123").contains("/v2"));
}

// ---- error classifier ----

#[test]
fn classify_401_is_auth_invalid() {
    let err = classify(
        StatusCode::UNAUTHORIZED,
        r#"{"code": 124, "message": "Invalid access token."}"#,
    );
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("Authentication failed"));
    assert!(err.message.contains("Invalid access token"));
}

#[test]
fn classify_403_is_auth_invalid_with_status() {
    let err = classify(
        StatusCode::FORBIDDEN,
        r#"{"code": 200, "message": "Insufficient privileges."}"#,
    );
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(403));
    assert!(err.message.contains("Insufficient permissions"));
}

#[test]
fn classify_404_is_api_error_with_message() {
    let err = classify(
        StatusCode::NOT_FOUND,
        r#"{"code": 3001, "message": "Meeting does not exist: 999."}"#,
    );
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Meeting does not exist"));
}

#[test]
fn classify_429_is_rate_limit() {
    let err = classify(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"code": 429, "message": "You have reached the maximum per-second rate limit."}"#,
    );
    assert_eq!(err.status_code, Some(429));
    assert!(err.message.contains("Rate limit exceeded"));
}

#[test]
fn classify_500_is_server_error() {
    let err = classify(StatusCode::INTERNAL_SERVER_ERROR, r#"{"message": "boom"}"#);
    assert_eq!(err.status_code, Some(500));
    assert!(err.message.contains("Zoom server error"));
}

#[test]
fn parse_concatenates_field_validation_errors() {
    // `300 Validation Failed` carries an `errors` array with field detail.
    let parsed = parse_error_body(
        r#"{
            "code": 300,
            "message": "Validation Failed.",
            "errors": [
                {"field": "start_time", "message": "Invalid date-time format."},
                {"field": "duration", "message": "must be a positive integer."}
            ]
        }"#,
    );
    let message = parsed.message.unwrap();
    assert!(message.contains("Validation Failed."));
    assert!(message.contains("start_time: Invalid date-time format."));
    assert!(message.contains("duration: must be a positive integer."));
}

#[test]
fn parse_non_json_body_is_passed_through_as_string() {
    let parsed = parse_error_body("upstream proxy error");
    assert_eq!(parsed.message.as_deref(), Some("upstream proxy error"));
    assert!(matches!(parsed.original, Some(OriginalError::String(_))));
}

#[test]
fn classify_empty_body_falls_back_to_status_reason() {
    let err = classify(StatusCode::BAD_GATEWAY, "");
    assert_eq!(err.status_code, Some(502));
    // Canonical reason for 502 is "Bad Gateway".
    assert!(err.message.contains("502"));
}
