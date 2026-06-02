#![allow(clippy::doc_markdown)]

//! Unit tests for `CircleCiVendor`: base-URL resolution (default + override),
//! verbatim path normalisation, token lookup, and the CircleCI REST
//! error-envelope classifier. The Bearer-auth dispatch path is covered
//! end-to-end in `circleci_controller_tests.rs`.

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::{ErrorKind, OriginalError};
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::circleci::CircleCiVendor;
use mcp_server_atlassian::vendor::circleci::error::{classify, parse_error_body};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use std::collections::HashMap;

fn empty_config() -> Config {
    Config::from_map(HashMap::new())
}

// ---- name ----

#[test]
fn name_is_canonical_circleci() {
    assert_eq!(CircleCiVendor::new().name(), "circleci");
}

// ---- base_url ----

#[test]
fn base_url_defaults_to_circleci_api_v2() {
    // Independent of config — CircleCI's base is fixed; only the token comes
    // from config.
    let vendor = CircleCiVendor::new();
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "https://circleci.com/api/v2"
    );
}

#[test]
fn base_url_override_is_returned_verbatim() {
    let vendor = CircleCiVendor::with_base_url("http://localhost:1234");
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "http://localhost:1234"
    );
}

// ---- normalize_path ----

#[test]
fn normalize_path_passes_through_with_leading_slash() {
    let vendor = CircleCiVendor::new();
    assert_eq!(
        vendor.normalize_path("/project/gh/acme/web/pipeline"),
        "/project/gh/acme/web/pipeline"
    );
}

#[test]
fn normalize_path_prepends_missing_leading_slash() {
    let vendor = CircleCiVendor::new();
    assert_eq!(vendor.normalize_path("me"), "/me");
}

#[test]
fn normalize_path_does_not_prepend_api_version() {
    // Unlike Bitbucket (`/2.0`), CircleCI callers supply paths relative to the
    // `/v2` base, so no version segment is injected.
    let vendor = CircleCiVendor::new();
    assert!(!vendor.normalize_path("/me").contains("/v2"));
}

// ---- token lookup ----

#[test]
fn token_reads_from_circleci_config_section() {
    let mut m = HashMap::new();
    m.insert("CIRCLECI_TOKEN".to_string(), "tok-abc".to_string());
    let config = Config::from_map(m);
    assert_eq!(CircleCiVendor::new().token(&config).unwrap(), "tok-abc");
}

#[test]
fn token_missing_is_auth_missing_error() {
    let err = CircleCiVendor::new().token(&empty_config()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("CIRCLECI_TOKEN"));
}

#[test]
fn token_blank_is_treated_as_missing() {
    let mut m = HashMap::new();
    m.insert("CIRCLECI_TOKEN".to_string(), "   ".to_string());
    let config = Config::from_map(m);
    let err = CircleCiVendor::new().token(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

// ---- error classifier ----

#[test]
fn classify_401_is_auth_invalid() {
    let err = classify(
        StatusCode::UNAUTHORIZED,
        r#"{"message": "You must log in first."}"#,
    );
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("Authentication failed"));
    assert!(err.message.contains("You must log in first"));
}

#[test]
fn classify_403_is_auth_invalid_with_status() {
    let err = classify(
        StatusCode::FORBIDDEN,
        r#"{"message": "Permission denied."}"#,
    );
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(403));
    assert!(err.message.contains("Insufficient permissions"));
}

#[test]
fn classify_404_is_api_error_with_message() {
    let err = classify(
        StatusCode::NOT_FOUND,
        r#"{"message": "Pipeline not found"}"#,
    );
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Pipeline not found"));
}

#[test]
fn classify_429_is_rate_limit() {
    let err = classify(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"message": "Rate limit exceeded"}"#,
    );
    assert_eq!(err.status_code, Some(429));
    assert!(err.message.contains("Rate limit exceeded"));
}

#[test]
fn classify_500_is_server_error() {
    let err = classify(StatusCode::INTERNAL_SERVER_ERROR, r#"{"message": "boom"}"#);
    assert_eq!(err.status_code, Some(500));
    assert!(err.message.contains("CircleCI server error"));
}

#[test]
fn parse_accepts_error_key_as_alternative_to_message() {
    // Some CircleCI/gateway responses use `error` instead of `message`.
    let parsed = parse_error_body(r#"{"error": "invalid token"}"#);
    assert_eq!(parsed.message.as_deref(), Some("invalid token"));
}

#[test]
fn parse_prefers_message_over_error() {
    let parsed = parse_error_body(r#"{"message": "primary", "error": "secondary"}"#);
    assert_eq!(parsed.message.as_deref(), Some("primary"));
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
    assert!(err.message.contains("502"));
}
