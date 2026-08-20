#![allow(clippy::doc_markdown)]

//! Unit tests for `SonarqubeVendor`: required-config base-URL resolution (and
//! the actionable error when `SONARQUBE_URL` is absent), trailing-slash
//! trimming, verbatim path normalisation, token lookup, and the non-2xx
//! classifier (Sonar's `errors: [{msg}]` envelope). The Bearer-header dispatch
//! path and the `ceTaskId` resolution are covered end-to-end in
//! `sonarqube_controller_tests.rs`.

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::sonarqube::SonarqubeVendor;
use mcp_server_atlassian::vendor::sonarqube::error::classify;
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use std::collections::HashMap;

fn empty_config() -> Config {
    Config::from_map(HashMap::new())
}

fn config_with(pairs: &[(&str, &str)]) -> Config {
    let mut m = HashMap::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), (*v).to_string());
    }
    Config::from_map(m)
}

// ---- name ----

#[test]
fn name_is_canonical_sonarqube() {
    assert_eq!(SonarqubeVendor::new().name(), "sonarqube");
}

// ---- base_url ----

#[test]
fn base_url_reads_from_sonarqube_url_config() {
    let vendor = SonarqubeVendor::new();
    let config = config_with(&[("SONARQUBE_URL", "https://sonar.mycorp.com")]);
    assert_eq!(
        vendor.base_url(&config).unwrap(),
        "https://sonar.mycorp.com"
    );
}

#[test]
fn base_url_trims_trailing_slash() {
    let vendor = SonarqubeVendor::new();
    let config = config_with(&[("SONARQUBE_URL", "https://sonarcloud.io/")]);
    assert_eq!(vendor.base_url(&config).unwrap(), "https://sonarcloud.io");
}

#[test]
fn base_url_missing_is_auth_missing_error() {
    let err = SonarqubeVendor::new()
        .base_url(&empty_config())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("SONARQUBE_URL"));
}

#[test]
fn base_url_blank_is_treated_as_missing() {
    let config = config_with(&[("SONARQUBE_URL", "   ")]);
    let err = SonarqubeVendor::new().base_url(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

#[test]
fn base_url_with_base_override_wins_and_trims() {
    let vendor = SonarqubeVendor::with_base_url("http://localhost:9000/");
    let config = config_with(&[("SONARQUBE_URL", "https://ignored.example")]);
    assert_eq!(vendor.base_url(&config).unwrap(), "http://localhost:9000");
}

// ---- normalize_path ----

#[test]
fn normalize_path_passes_through_with_leading_slash() {
    let vendor = SonarqubeVendor::new();
    assert_eq!(
        vendor.normalize_path("/api/issues/search"),
        "/api/issues/search"
    );
}

#[test]
fn normalize_path_prepends_missing_leading_slash() {
    let vendor = SonarqubeVendor::new();
    assert_eq!(
        vendor.normalize_path("api/issues/search"),
        "/api/issues/search"
    );
}

// ---- token lookup ----

#[tokio::test]
async fn token_reads_from_sonarqube_config_section() {
    let config = config_with(&[("SONARQUBE_TOKEN", "squ_abc")]);
    assert_eq!(
        SonarqubeVendor::new().token(&config).await.unwrap(),
        "squ_abc"
    );
}

#[tokio::test]
async fn token_missing_is_auth_missing_error() {
    let err = SonarqubeVendor::new()
        .token(&empty_config())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("SONARQUBE_TOKEN"));
}

#[tokio::test]
async fn token_blank_is_treated_as_missing() {
    let config = config_with(&[("SONARQUBE_TOKEN", "   ")]);
    let err = SonarqubeVendor::new().token(&config).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

// ---- non-2xx classifier ----

#[test]
fn classify_401_is_auth_invalid() {
    let err = classify(
        StatusCode::UNAUTHORIZED,
        r#"{"errors":[{"msg":"Authentication is required"}]}"#,
    );
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("Authentication is required"));
}

#[test]
fn classify_403_is_auth_invalid_with_status() {
    let err = classify(
        StatusCode::FORBIDDEN,
        r#"{"errors":[{"msg":"Insufficient privileges"}]}"#,
    );
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(403));
}

#[test]
fn classify_404_joins_multiple_error_messages() {
    let err = classify(
        StatusCode::NOT_FOUND,
        r#"{"errors":[{"msg":"Component key 'x' not found"},{"msg":"and again"}]}"#,
    );
    assert_eq!(err.status_code, Some(404));
    assert!(
        err.message
            .contains("Component key 'x' not found; and again")
    );
}

#[test]
fn classify_accepts_legacy_err_msg_key() {
    let err = classify(
        StatusCode::BAD_REQUEST,
        r#"{"err_msg":"bad request param"}"#,
    );
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("bad request param"));
}

#[test]
fn classify_plain_text_error() {
    let err = classify(StatusCode::BAD_REQUEST, "plain text sonar failure");
    assert!(err.message.contains("plain text sonar failure"));
}

#[test]
fn classify_429_is_rate_limit() {
    let err = classify(StatusCode::TOO_MANY_REQUESTS, "");
    assert_eq!(err.status_code, Some(429));
    assert!(err.message.contains("Rate limit exceeded"));
}

#[test]
fn classify_500_is_server_error() {
    let err = classify(StatusCode::INTERNAL_SERVER_ERROR, "<html>oops</html>");
    assert_eq!(err.status_code, Some(500));
    assert!(err.message.contains("SonarQube server error"));
}
