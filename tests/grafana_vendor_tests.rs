#![allow(clippy::doc_markdown)]

//! Unit tests for `GrafanaVendor`: required-config base-URL resolution (and the
//! actionable error when `GRAFANA_URL` is absent), trailing-slash trimming,
//! verbatim path normalisation, token lookup, and the non-2xx classifier. The
//! Bearer-header dispatch path is covered end-to-end in
//! `grafana_controller_tests.rs`.

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::grafana::GrafanaVendor;
use mcp_server_atlassian::vendor::grafana::error::classify;
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
fn name_is_canonical_grafana() {
    assert_eq!(GrafanaVendor::new().name(), "grafana");
}

// ---- base_url ----

#[test]
fn base_url_reads_from_grafana_url_config() {
    let vendor = GrafanaVendor::new();
    let config = config_with(&[("GRAFANA_URL", "https://myorg.grafana.net")]);
    assert_eq!(
        vendor.base_url(&config).unwrap(),
        "https://myorg.grafana.net"
    );
}

#[test]
fn base_url_trims_trailing_slash() {
    let vendor = GrafanaVendor::new();
    let config = config_with(&[("GRAFANA_URL", "http://localhost:3000/")]);
    assert_eq!(vendor.base_url(&config).unwrap(), "http://localhost:3000");
}

#[test]
fn base_url_missing_is_auth_missing_error() {
    let err = GrafanaVendor::new().base_url(&empty_config()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("GRAFANA_URL"));
}

#[test]
fn base_url_blank_is_treated_as_missing() {
    let config = config_with(&[("GRAFANA_URL", "   ")]);
    let err = GrafanaVendor::new().base_url(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

#[test]
fn base_url_with_base_override_wins_and_trims() {
    let vendor = GrafanaVendor::with_base_url("http://localhost:9999/");
    let config = config_with(&[("GRAFANA_URL", "https://ignored.example")]);
    assert_eq!(vendor.base_url(&config).unwrap(), "http://localhost:9999");
}

// ---- normalize_path ----

#[test]
fn normalize_path_passes_through_with_leading_slash() {
    let vendor = GrafanaVendor::new();
    assert_eq!(
        vendor.normalize_path("/api/datasources"),
        "/api/datasources"
    );
}

#[test]
fn normalize_path_prepends_missing_leading_slash() {
    let vendor = GrafanaVendor::new();
    assert_eq!(vendor.normalize_path("api/datasources"), "/api/datasources");
}

// ---- token lookup ----

#[test]
fn token_reads_from_grafana_config_section() {
    let config = config_with(&[("GRAFANA_TOKEN", "glsa_abc")]);
    assert_eq!(GrafanaVendor::new().token(&config).unwrap(), "glsa_abc");
}

#[test]
fn token_missing_is_auth_missing_error() {
    let err = GrafanaVendor::new().token(&empty_config()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("GRAFANA_TOKEN"));
}

#[test]
fn token_blank_is_treated_as_missing() {
    let config = config_with(&[("GRAFANA_TOKEN", "   ")]);
    let err = GrafanaVendor::new().token(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

// ---- non-2xx classifier ----

#[test]
fn classify_401_is_auth_invalid() {
    let err = classify(StatusCode::UNAUTHORIZED, r#"{"message":"Unauthorized"}"#);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("Unauthorized"));
}

#[test]
fn classify_403_is_auth_invalid_with_status() {
    let err = classify(StatusCode::FORBIDDEN, r#"{"message":"Forbidden"}"#);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(403));
}

#[test]
fn classify_404_surfaces_grafana_message() {
    let err = classify(
        StatusCode::NOT_FOUND,
        r#"{"message":"Data source not found"}"#,
    );
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Data source not found"));
}

#[test]
fn classify_loki_400_uses_error_key() {
    // A bad LogQL query comes back from Loki as 400 with `{"error": …}`.
    let body = r#"{"status":"error","error":"parse error: unexpected IDENTIFIER"}"#;
    let err = classify(StatusCode::BAD_REQUEST, body);
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("parse error"));
}

#[test]
fn classify_loki_plain_text_error() {
    let err = classify(StatusCode::BAD_REQUEST, "plain text loki failure");
    assert!(err.message.contains("plain text loki failure"));
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
    assert!(err.message.contains("Grafana server error"));
}
