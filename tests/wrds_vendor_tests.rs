#![cfg(feature = "wrds")]
#![allow(clippy::doc_markdown)]

//! Tests for the WRDS (PostgreSQL) vendor.
//!
//! WRDS is the one vendor with no HTTP surface, so there is no wiremock harness:
//! a full query needs a live Postgres. What we *can* test without a database is
//! the part that runs before any socket is opened — credential resolution from
//! the `wrds` config section and the row-limit guard — plus an opt-in live smoke
//! test that runs only when real WRDS credentials are present in the environment
//! (inert in CI).

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::wrds::{
    DEFAULT_ROW_LIMIT, MAX_ROW_LIMIT, WrdsVendor, clamp_row_limit,
};

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

// ---- row-limit guard ----

#[test]
fn clamp_row_limit_defaults_and_bounds() {
    assert_eq!(clamp_row_limit(None), DEFAULT_ROW_LIMIT);
    assert_eq!(clamp_row_limit(Some(0)), 1, "zero clamps up to 1");
    assert_eq!(clamp_row_limit(Some(500)), 500);
    assert_eq!(
        clamp_row_limit(Some(u32::MAX)),
        MAX_ROW_LIMIT,
        "oversized caps to MAX_ROW_LIMIT"
    );
}

// ---- credential resolution (runs before any network I/O) ----

#[tokio::test]
async fn missing_credentials_surface_auth_missing_before_connecting() {
    // No WRDS_USERNAME / WRDS_PASSWORD anywhere: the vendor must fail fast with
    // an AuthMissing error and never attempt a connection.
    let err = WrdsVendor::new()
        .run_sql(&empty_config(), "SELECT 1", 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(
        err.message.contains("WRDS_USERNAME"),
        "message should name the missing key: {}",
        err.message
    );
}

#[tokio::test]
async fn username_without_password_is_auth_missing_naming_password() {
    let config = config_with(&[("WRDS_USERNAME", "researcher")]);
    let err = WrdsVendor::new()
        .run_sql(&config, "SELECT 1", 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(
        err.message.contains("WRDS_PASSWORD"),
        "message should name the missing password key: {}",
        err.message
    );
}

#[tokio::test]
async fn blank_credentials_are_treated_as_missing() {
    let config = config_with(&[("WRDS_USERNAME", "   "), ("WRDS_PASSWORD", "secret")]);
    let err = WrdsVendor::new()
        .list_libraries(&config, 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

#[test]
fn wrds_and_mcp_server_wrds_sections_are_recognised() {
    // Both the `wrds` and `mcp-server-wrds` aliases must map to the canonical
    // `wrds` vendor section, and an unrelated section must not bleed in.
    use serde_json::json;
    let root = json!({
        "mcp-server-wrds": { "environments": { "WRDS_USERNAME": "researcher", "WRDS_PORT": "9737" } },
        "grafana": { "environments": { "GRAFANA_TOKEN": "glsa_x" } }
    });
    let sections =
        mcp_server_atlassian::config::extract_all_vendor_sections(&root, "mcp-server-atlassian");
    let wrds = sections.get("wrds").expect("wrds section should resolve");
    assert_eq!(
        wrds.get("WRDS_USERNAME").map(String::as_str),
        Some("researcher")
    );
    assert_eq!(wrds.get("WRDS_PORT").map(String::as_str), Some("9737"));
    assert!(!wrds.contains_key("GRAFANA_TOKEN"));
}

// ---- opt-in live smoke test ----
//
// Runs a real `SELECT 1` against WRDS only when WRDS_USERNAME and WRDS_PASSWORD
// are set in the environment. Otherwise it prints a skip note and returns, so it
// is a no-op in CI. Run it locally with:
//   WRDS_USERNAME=you WRDS_PASSWORD=... cargo test --test wrds_vendor_tests -- --nocapture
#[tokio::test]
async fn live_select_one_when_credentials_present() {
    let (Ok(user), Ok(pass)) = (
        std::env::var("WRDS_USERNAME"),
        std::env::var("WRDS_PASSWORD"),
    ) else {
        eprintln!("skipping WRDS live test: set WRDS_USERNAME and WRDS_PASSWORD to run it");
        return;
    };

    let mut pairs = vec![
        ("WRDS_USERNAME", user.as_str()),
        ("WRDS_PASSWORD", pass.as_str()),
    ];
    let host = std::env::var("WRDS_HOST").unwrap_or_default();
    if !host.is_empty() {
        pairs.push(("WRDS_HOST", host.as_str()));
    }
    let config = config_with(&pairs);

    let data = WrdsVendor::new()
        .run_sql(&config, "SELECT 1 AS one", 1)
        .await
        .expect("live WRDS SELECT 1 should succeed");
    assert_eq!(data, serde_json::json!([{ "one": 1 }]));
}
