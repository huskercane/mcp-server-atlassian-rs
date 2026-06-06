#![allow(clippy::doc_markdown)]

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::edx::EdxVendor;
use mcp_server_atlassian::vendor::edx::error::{classify, parse_error_body};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use std::collections::HashMap;

fn empty_config() -> Config {
    Config::from_map(HashMap::new())
}

#[test]
fn name_is_canonical_edx() {
    assert_eq!(EdxVendor::new().name(), "edx");
}

#[test]
fn base_url_defaults_to_courses_edx() {
    assert_eq!(
        EdxVendor::new().base_url(&empty_config()).unwrap(),
        "https://courses.edx.org"
    );
}

#[test]
fn base_url_reads_edx_api_base_from_config() {
    let mut m = HashMap::new();
    m.insert(
        "EDX_API_BASE".to_string(),
        "https://openedx.example.com".to_string(),
    );
    let config = Config::from_map(m);
    assert_eq!(
        EdxVendor::new().base_url(&config).unwrap(),
        "https://openedx.example.com"
    );
}

#[test]
fn explicit_base_url_override_wins() {
    let mut m = HashMap::new();
    m.insert(
        "EDX_API_BASE".to_string(),
        "https://openedx.example.com".to_string(),
    );
    let config = Config::from_map(m);
    assert_eq!(
        EdxVendor::with_base_url("http://localhost:1234")
            .base_url(&config)
            .unwrap(),
        "http://localhost:1234"
    );
}

#[test]
fn normalize_path_only_ensures_leading_slash() {
    let vendor = EdxVendor::new();
    assert_eq!(
        vendor.normalize_path("api/discussion/v1/threads/"),
        "/api/discussion/v1/threads/"
    );
}

#[test]
fn token_reads_from_edx_config() {
    let mut m = HashMap::new();
    m.insert("EDX_ACCESS_TOKEN".to_string(), "tok-edx".to_string());
    let config = Config::from_map(m);
    assert_eq!(EdxVendor::new().token(&config).unwrap(), "tok-edx");
}

#[test]
fn token_missing_is_auth_missing() {
    let err = EdxVendor::new().token(&empty_config()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("EDX_ACCESS_TOKEN"));
}

#[test]
fn classify_404_accepts_developer_message() {
    let err = classify(
        StatusCode::NOT_FOUND,
        r#"{"developer_message": "Course not found."}"#,
    );
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Course not found"));
}

#[test]
fn classify_403_is_auth_invalid() {
    let err = classify(StatusCode::FORBIDDEN, r#"{"detail": "Permission denied."}"#);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(403));
    assert!(err.message.contains("Insufficient permissions"));
}

#[test]
fn parse_field_errors_are_joined() {
    let parsed =
        parse_error_body(r#"{"raw_body": ["This field is required."], "title": ["Too short."]}"#);
    let message = parsed.message.unwrap();
    assert!(message.contains("raw_body: This field is required."));
    assert!(message.contains("title: Too short."));
}
