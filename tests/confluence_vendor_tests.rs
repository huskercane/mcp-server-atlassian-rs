//! Unit tests for `ConfluenceVendor`. Covers base-URL resolution (env +
//! override + missing), path normalisation (passthrough, no `/2.0`), and
//! the Confluence error envelope classifier.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::{ErrorKind, OriginalError};
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::confluence::ConfluenceVendor;
use mcp_server_atlassian::vendor::confluence::error::{classify, parse_error_body};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use serde_json::json;
use tempfile::TempDir;

fn cfg(entries: &[(&str, &str)]) -> Config {
    let mut m = HashMap::new();
    for (k, v) in entries {
        m.insert((*k).to_string(), (*v).to_string());
    }
    Config::from_map(m)
}

// ---- name ----

#[test]
fn name_is_canonical_confluence() {
    assert_eq!(ConfluenceVendor::new().name(), "confluence");
}

// ---- base_url ----

#[test]
fn base_url_from_site_name_env() {
    let vendor = ConfluenceVendor::new();
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "mycompany")]);
    let url = vendor.base_url(&config).unwrap();
    assert_eq!(url, "https://mycompany.atlassian.net");
}

#[test]
fn base_url_trims_whitespace_around_site_name() {
    let vendor = ConfluenceVendor::new();
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "  mycompany  ")]);
    let url = vendor.base_url(&config).unwrap();
    assert_eq!(url, "https://mycompany.atlassian.net");
}

#[test]
fn base_url_missing_site_returns_auth_missing() {
    // Critical guarantee: a Bitbucket-only deployment must never crash at
    // server boot just because Confluence isn't configured. The error
    // only shows up when a `conf_*` tool is actually invoked.
    let vendor = ConfluenceVendor::new();
    let config = cfg(&[]);
    let err = vendor.base_url(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("ATLASSIAN_SITE_NAME"));
    // Message should call out the conf_* surface so the user knows which
    // section of configs.json to populate.
    assert!(err.message.contains("conf_*"));
}

#[test]
fn base_url_falls_back_to_jira_section_when_confluence_section_absent() {
    // Migration ergonomics: a user upgrading from Jira-only to a
    // Jira+Confluence setup should not be forced to duplicate
    // ATLASSIAN_SITE_NAME under a new `confluence` section. We assert
    // here at the vendor layer (not just the lower-level config helper)
    // because base_url() is what production code actually invokes.
    let dir = TempDir::new().unwrap();
    let global_path = dir.path().join("configs.json");
    std::fs::write(
        &global_path,
        serde_json::to_string(&json!({
            "jira": { "environments": { "ATLASSIAN_SITE_NAME": "shared-site" } }
        }))
        .unwrap(),
    )
    .unwrap();
    let config =
        Config::load_from_sources(Some(&global_path), None, &HashMap::new());

    let vendor = ConfluenceVendor::new();
    assert_eq!(
        vendor.base_url(&config).unwrap(),
        "https://shared-site.atlassian.net"
    );
}

#[test]
fn base_url_does_not_fall_back_to_bitbucket_section() {
    // The fallback list is intentionally narrow. A site-name accidentally
    // (or maliciously) placed under the `bitbucket` section must not
    // resolve for Confluence — the user gets the same auth_missing error
    // they would have gotten with no config at all.
    let dir = TempDir::new().unwrap();
    let global_path = dir.path().join("configs.json");
    std::fs::write(
        &global_path,
        serde_json::to_string(&json!({
            "bitbucket": { "environments": { "ATLASSIAN_SITE_NAME": "should-not-leak" } }
        }))
        .unwrap(),
    )
    .unwrap();
    let config =
        Config::load_from_sources(Some(&global_path), None, &HashMap::new());

    let vendor = ConfluenceVendor::new();
    let err = vendor.base_url(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

#[test]
fn base_url_empty_site_name_is_treated_as_missing() {
    let vendor = ConfluenceVendor::new();
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "   ")]);
    let err = vendor.base_url(&config).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

#[test]
fn with_base_url_skips_env_lookup_entirely() {
    let vendor = ConfluenceVendor::with_base_url("http://127.0.0.1:54321");
    let config = cfg(&[("ATLASSIAN_SITE_NAME", "should-not-be-used")]);
    let url = vendor.base_url(&config).unwrap();
    assert_eq!(url, "http://127.0.0.1:54321");
}

#[test]
fn with_base_url_resolves_without_any_config() {
    let vendor = ConfluenceVendor::with_base_url("http://localhost:8080");
    let url = vendor.base_url(&Config::default()).unwrap();
    assert_eq!(url, "http://localhost:8080");
}

// ---- normalize_path ----

#[test]
fn normalize_path_adds_leading_slash_only() {
    let vendor = ConfluenceVendor::new();
    assert_eq!(
        vendor.normalize_path("wiki/api/v2/spaces"),
        "/wiki/api/v2/spaces"
    );
    assert_eq!(
        vendor.normalize_path("/wiki/api/v2/spaces"),
        "/wiki/api/v2/spaces"
    );
}

#[test]
fn normalize_path_does_not_prepend_v2_like_bitbucket() {
    let vendor = ConfluenceVendor::new();
    // /wiki/api/v2/... and /wiki/rest/api/... must pass through verbatim.
    assert_eq!(
        vendor.normalize_path("/wiki/api/v2/pages/123"),
        "/wiki/api/v2/pages/123"
    );
    assert_eq!(
        vendor.normalize_path("/wiki/rest/api/search"),
        "/wiki/rest/api/search"
    );
}

// ---- classify_error: status code mapping (TS Confluence parity) ----
//
// Confluence's status mapping is deliberately *different* from Jira's:
// 403 stays an ApiError (not auth_invalid), preserving the TS behaviour
// where Confluence treats 403 as a permissions-style API error.

#[test]
fn classify_401_maps_to_auth_invalid_with_ts_prefix() {
    let body = r#"{"message":"Login required"}"#;
    let err = classify(StatusCode::UNAUTHORIZED, body);
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.status_code, Some(401));
    assert!(
        err.message
            .starts_with("Authentication failed. Confluence API: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("Login required"));
}

#[test]
fn classify_403_stays_api_error_unlike_jira() {
    // Intentional asymmetry with the Jira classifier — Confluence keeps
    // 403 as ApiError per TS reference.
    let body = r#"{"message":"You do not have permission."}"#;
    let err = classify(StatusCode::FORBIDDEN, body);
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(403));
    assert!(
        err.message.starts_with("Access denied. Confluence API: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("You do not have permission."));
}

#[test]
fn classify_404_uses_resource_not_found_prefix() {
    let body = r#"{"title":"Not Found","status":404,"detail":"Page not found"}"#;
    let err = classify(StatusCode::NOT_FOUND, body);
    assert_eq!(err.status_code, Some(404));
    assert!(
        err.message
            .starts_with("Resource not found. Confluence API: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("Not Found"));
    assert!(err.message.contains("Page not found"));
}

#[test]
fn classify_429_uses_rate_limit_prefix() {
    let err = classify(StatusCode::TOO_MANY_REQUESTS, r#"{"message":"slow down"}"#);
    assert_eq!(err.status_code, Some(429));
    assert!(
        err.message
            .starts_with("Rate limit exceeded. Confluence API: "),
        "got: {}",
        err.message
    );
}

#[test]
fn classify_5xx_uses_service_error_prefix() {
    let err = classify(StatusCode::SERVICE_UNAVAILABLE, "");
    assert_eq!(err.status_code, Some(503));
    assert!(
        err.message
            .starts_with("Confluence service error. Detail: "),
        "got: {}",
        err.message
    );
}

#[test]
fn classify_other_status_uses_request_failed_prefix() {
    let err = classify(StatusCode::BAD_REQUEST, r#"{"message":"bad input"}"#);
    assert_eq!(err.status_code, Some(400));
    assert!(
        err.message
            .starts_with("Confluence API request failed. Detail: "),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("bad input"));
}

// ---- parse_error_body: envelope shapes ----

#[test]
fn parse_v2_problem_details_combines_title_and_detail() {
    let body = r#"{"title":"Bad Request","status":400,"detail":"`spaceId` is required"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(
        parsed.message.as_deref(),
        Some("Bad Request: `spaceId` is required")
    );
}

#[test]
fn parse_v2_title_only_when_no_detail() {
    let body = r#"{"title":"Conflict","status":409}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("Conflict"));
}

#[test]
fn parse_v2_skips_duplicate_detail_when_already_in_title() {
    // Defensive: TS check is `!errorMessage.includes(detail)`, so a title
    // that already contains the detail string must not be doubled-up.
    let body = r#"{"title":"Bad Request: foo is required","detail":"foo is required"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("Bad Request: foo is required"));
}

#[test]
fn parse_message_with_reason_appends_reason() {
    let body = r#"{"message":"Could not authenticate","reason":"token expired"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(
        parsed.message.as_deref(),
        Some("Could not authenticate: token expired")
    );
}

#[test]
fn parse_flat_message_only() {
    let body = r#"{"message":"Something broke"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("Something broke"));
}

#[test]
fn parse_graphql_errors_array_joins_first_three() {
    let body = r#"{"errors":[{"message":"e1"},{"message":"e2"},{"message":"e3"},{"message":"e4"},{"message":"e5"}]}"#;
    let parsed = parse_error_body(body);
    assert_eq!(
        parsed.message.as_deref(),
        Some("e1; e2; e3; and 2 more errors")
    );
}

#[test]
fn parse_graphql_errors_under_three_no_overflow_suffix() {
    let body = r#"{"errors":[{"message":"only one"}]}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("only one"));
}

#[test]
fn parse_graphql_errors_falls_back_to_title_when_no_message() {
    let body = r#"{"errors":[{"title":"Invalid query"}]}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("Invalid query"));
}

#[test]
fn parse_jira_style_error_messages_array_join() {
    let body = r#"{"errorMessages":["one","two"]}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("one; two"));
}

#[test]
fn parse_message_wins_over_status_code_branch() {
    // TS parity: the `message` branch is checked before the
    // `statusCode + message` fallback, so a payload that carries both
    // resolves to the message verbatim — the statusCode prefix never
    // appears in this case. Earlier versions of this Rust port prefixed
    // unconditionally inside the message branch and broke parity with
    // the TS reference at src/utils/transport.util.ts.
    let body = r#"{"statusCode":418,"message":"I'm a teapot"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("I'm a teapot"));
}

#[test]
fn parse_status_code_with_message_only_when_no_title_or_reason() {
    // The terminal TS `else if` branch only fires for a payload whose
    // *only* useful fields are `statusCode` and `message` AND the
    // `message` branch above missed for some other reason (in practice
    // this means the body needs `statusCode` plus `message` but with
    // `message` empty/absent at the top level — which can't actually
    // round-trip through the TS chain, so this branch is essentially
    // defensive). We exercise it via the normalised path: a payload
    // that names `statusCode` and `message` resolves through the
    // earlier `message` branch (covered by the test above), so this
    // test instead documents that a `statusCode`-only payload with no
    // message resolves to `None`.
    let body = r#"{"statusCode":418}"#;
    let parsed = parse_error_body(body);
    assert!(parsed.message.is_none());
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
