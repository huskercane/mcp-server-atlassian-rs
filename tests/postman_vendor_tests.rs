#![allow(clippy::doc_markdown)]

//! Unit tests for `PostmanVendor`: base-URL resolution, verbatim path
//! normalisation, API-key lookup, and the nested `{"error": {...}}` REST
//! classifier. The `X-API-Key` dispatch path is covered end-to-end in
//! `postman_controller_tests.rs`.

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::{ErrorKind, OriginalError};
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::postman::PostmanVendor;
use mcp_server_atlassian::vendor::postman::error::{classify, parse_error_body};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use std::collections::HashMap;

fn empty_config() -> Config {
    Config::from_map(HashMap::new())
}

// ---- name ----

#[test]
fn name_is_canonical_postman() {
    assert_eq!(PostmanVendor::new().name(), "postman");
}

// ---- base_url ----

#[test]
fn base_url_defaults_to_postman_api() {
    let vendor = PostmanVendor::new();
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "https://api.getpostman.com"
    );
}

#[test]
fn base_url_override_is_returned_verbatim() {
    let vendor = PostmanVendor::with_base_url("http://localhost:4321");
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "http://localhost:4321"
    );
}

// ---- normalize_path ----

#[test]
fn normalize_path_passes_through_with_leading_slash() {
    let vendor = PostmanVendor::new();
    assert_eq!(vendor.normalize_path("/collections"), "/collections");
}

#[test]
fn normalize_path_prepends_missing_leading_slash() {
    let vendor = PostmanVendor::new();
    assert_eq!(vendor.normalize_path("me"), "/me");
}

// ---- key lookup ----

#[test]
fn key_reads_from_postman_config_section() {
    let mut m = HashMap::new();
    m.insert("POSTMAN_API_KEY".to_string(), "PMAK-abc".to_string());
    let config = Config::from_map(m);
    assert_eq!(PostmanVendor::new().key(&config).unwrap(), "PMAK-abc");
}

#[test]
fn key_missing_is_auth_missing_error() {
    let err = PostmanVendor::new().key(&empty_config()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("POSTMAN_API_KEY"));
}

#[test]
fn key_blank_is_treated_as_missing() {
    let mut m = HashMap::new();
    m.insert("POSTMAN_API_KEY".to_string(), "   ".to_string());
    let config = Config::from_map(m);
    let err = PostmanVendor::new().key(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

// ---- error classifier ----

#[test]
fn classify_401_is_auth_invalid() {
    let err = classify(
        StatusCode::UNAUTHORIZED,
        r#"{"error": {"name": "AuthenticationError", "message": "Invalid API Key"}}"#,
    );
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("Authentication failed"));
    assert!(err.message.contains("Invalid API Key"));
}

#[test]
fn classify_404_is_api_error_with_message() {
    let err = classify(
        StatusCode::NOT_FOUND,
        r#"{"error": {"name": "instanceNotFoundError", "message": "Collection not found"}}"#,
    );
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Resource not found"));
    assert!(err.message.contains("Collection not found"));
}

#[test]
fn classify_429_is_rate_limit() {
    let err = classify(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"error": {"name": "rateLimitError", "message": "Too many requests"}}"#,
    );
    assert_eq!(err.status_code, Some(429));
    assert!(err.message.contains("Rate limit exceeded"));
}

#[test]
fn classify_500_is_server_error() {
    let err = classify(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error": {"message": "boom"}}"#,
    );
    assert_eq!(err.status_code, Some(500));
    assert!(err.message.contains("Postman server error"));
}

#[test]
fn parse_falls_back_to_error_name_when_message_absent() {
    let parsed = parse_error_body(r#"{"error": {"name": "paramMissingError"}}"#);
    assert_eq!(parsed.message.as_deref(), Some("paramMissingError"));
}

#[test]
fn parse_non_json_body_is_passed_through_as_string() {
    let parsed = parse_error_body("gateway timeout");
    assert_eq!(parsed.message.as_deref(), Some("gateway timeout"));
    assert!(matches!(parsed.original, Some(OriginalError::String(_))));
}

#[test]
fn classify_empty_body_falls_back_to_status_reason() {
    let err = classify(StatusCode::BAD_GATEWAY, "");
    assert_eq!(err.status_code, Some(502));
    assert!(err.message.contains("502"));
}
