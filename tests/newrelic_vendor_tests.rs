#![allow(clippy::doc_markdown)]

//! Unit tests for `NewRelicVendor`: region-aware base-URL resolution, verbatim
//! path normalisation, API-key lookup, the non-2xx classifier, and — the part
//! unique to NerdGraph — the `200 OK` + `errors`-array success-body classifier.
//! The custom-header dispatch path is covered end-to-end in
//! `newrelic_controller_tests.rs`.

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::newrelic::NewRelicVendor;
use mcp_server_atlassian::vendor::newrelic::error::{classify, classify_graphql_errors};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use serde_json::json;
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
fn name_is_canonical_newrelic() {
    assert_eq!(NewRelicVendor::new().name(), "newrelic");
}

// ---- base_url ----

#[test]
fn base_url_defaults_to_us_region() {
    let vendor = NewRelicVendor::new();
    assert_eq!(
        vendor.base_url(&empty_config()).unwrap(),
        "https://api.newrelic.com"
    );
}

#[test]
fn base_url_eu_region_uses_eu_host() {
    let vendor = NewRelicVendor::new();
    let config = config_with(&[("NEW_RELIC_REGION", "eu")]);
    assert_eq!(
        vendor.base_url(&config).unwrap(),
        "https://api.eu.newrelic.com"
    );
}

#[test]
fn base_url_region_is_case_insensitive() {
    let vendor = NewRelicVendor::new();
    let config = config_with(&[("NEW_RELIC_REGION", "EU")]);
    assert_eq!(
        vendor.base_url(&config).unwrap(),
        "https://api.eu.newrelic.com"
    );
}

#[test]
fn base_url_explicit_base_overrides_region() {
    let vendor = NewRelicVendor::new();
    let config = config_with(&[
        ("NEW_RELIC_REGION", "eu"),
        ("NEW_RELIC_API_BASE", "https://nerdgraph.internal.example"),
    ]);
    assert_eq!(
        vendor.base_url(&config).unwrap(),
        "https://nerdgraph.internal.example"
    );
}

#[test]
fn base_url_with_base_override_wins_over_config() {
    let vendor = NewRelicVendor::with_base_url("http://localhost:9999");
    let config = config_with(&[("NEW_RELIC_API_BASE", "https://ignored.example")]);
    assert_eq!(vendor.base_url(&config).unwrap(), "http://localhost:9999");
}

// ---- normalize_path ----

#[test]
fn normalize_path_passes_through_with_leading_slash() {
    let vendor = NewRelicVendor::new();
    assert_eq!(vendor.normalize_path("/graphql"), "/graphql");
}

#[test]
fn normalize_path_prepends_missing_leading_slash() {
    let vendor = NewRelicVendor::new();
    assert_eq!(vendor.normalize_path("graphql"), "/graphql");
}

// ---- api key lookup ----

#[test]
fn api_key_reads_from_newrelic_config_section() {
    let config = config_with(&[("NEW_RELIC_API_KEY", "NRAK-abc")]);
    assert_eq!(NewRelicVendor::new().api_key(&config).unwrap(), "NRAK-abc");
}

#[test]
fn api_key_missing_is_auth_missing_error() {
    let err = NewRelicVendor::new().api_key(&empty_config()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("NEW_RELIC_API_KEY"));
}

#[test]
fn api_key_blank_is_treated_as_missing() {
    let config = config_with(&[("NEW_RELIC_API_KEY", "   ")]);
    let err = NewRelicVendor::new().api_key(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

// ---- non-2xx classifier ----

#[test]
fn classify_401_is_auth_invalid() {
    let err = classify(StatusCode::UNAUTHORIZED, r#"{"error":"Invalid API key"}"#);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert!(err.message.contains("Invalid API key"));
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
    assert!(err.message.contains("New Relic server error"));
}

#[test]
fn classify_non_2xx_extracts_graphql_errors_array() {
    let body = r#"{"errors":[{"message":"boom"}]}"#;
    let err = classify(StatusCode::BAD_REQUEST, body);
    assert!(err.message.contains("boom"));
}

// ---- errors-array success-body classifier ----

#[test]
fn no_errors_field_is_success() {
    let v = json!({"data": {"actor": {"user": {"name": "Ada"}}}});
    assert!(classify_graphql_errors(&v).is_none());
}

#[test]
fn empty_errors_array_is_success() {
    let v = json!({"data": {"actor": {}}, "errors": []});
    assert!(classify_graphql_errors(&v).is_none());
}

#[test]
fn partial_null_data_without_errors_is_success() {
    // GraphQL may null a field without reporting an error; that is still a
    // successful response, not a failure.
    let v = json!({"data": {"actor": {"account": null}}});
    assert!(classify_graphql_errors(&v).is_none());
}

#[test]
fn non_empty_errors_is_api_error_with_joined_messages() {
    let v = json!({
        "data": null,
        "errors": [
            {"message": "first problem"},
            {"message": "second problem"}
        ]
    });
    let err = classify_graphql_errors(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("first problem"));
    assert!(err.message.contains("second problem"));
}

#[test]
fn auth_error_class_becomes_auth_invalid() {
    let v = json!({
        "errors": [
            {"message": "denied", "extensions": {"errorClass": "UNAUTHENTICATED"}}
        ]
    });
    let err = classify_graphql_errors(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
}

#[test]
fn auth_message_heuristic_becomes_auth_invalid() {
    let v = json!({"errors": [{"message": "The API key provided is invalid"}]});
    let err = classify_graphql_errors(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
}

#[test]
fn errors_without_message_falls_back_to_error_class() {
    let v = json!({"errors": [{"extensions": {"errorClass": "VALIDATION_ERROR"}}]});
    let err = classify_graphql_errors(&v).unwrap();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("VALIDATION_ERROR"));
}

#[test]
fn errors_is_not_an_array_is_treated_as_success() {
    // Defensive: a non-array `errors` value should not be treated as an error.
    let v = json!({"errors": "weird"});
    assert!(classify_graphql_errors(&v).is_none());
}
