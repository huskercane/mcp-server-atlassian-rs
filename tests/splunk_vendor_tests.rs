use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::splunk::SplunkVendor;
use mcp_server_atlassian::vendor::splunk::error::classify;
use pretty_assertions::assert_eq;
use reqwest::StatusCode;

fn config_with(pairs: &[(&str, &str)]) -> Config {
    Config::from_map(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    )
}

#[tokio::test]
async fn resolves_name_url_token_and_auth_scheme() {
    let vendor = SplunkVendor::new();
    let config = config_with(&[
        ("SPLUNK_URL", "https://splunk.example.com:8089/"),
        ("SPLUNK_TOKEN", "ey-token"),
    ]);

    assert_eq!(vendor.name(), "splunk");
    assert_eq!(
        vendor.base_url(&config).unwrap(),
        "https://splunk.example.com:8089"
    );
    assert_eq!(vendor.token(&config).await.unwrap(), "ey-token");
    assert_eq!(vendor.auth_scheme(&config), "Bearer");
}

#[test]
fn legacy_splunk_auth_scheme_is_supported() {
    let config = config_with(&[("SPLUNK_AUTH_SCHEME", "splunk")]);
    assert_eq!(SplunkVendor::new().auth_scheme(&config), "Splunk");
}

#[tokio::test]
async fn missing_configuration_fails_at_call_time() {
    let config = Config::from_map(HashMap::new());
    let vendor = SplunkVendor::new();

    let url_error = vendor.base_url(&config).unwrap_err();
    assert_eq!(url_error.kind, ErrorKind::AuthMissing);
    assert!(url_error.message.contains("SPLUNK_URL"));

    let token_error = vendor.token(&config).await.unwrap_err();
    assert_eq!(token_error.kind, ErrorKind::AuthMissing);
    assert!(token_error.message.contains("SPLUNK_TOKEN"));
}

#[test]
fn splunk_config_aliases_are_vendor_scoped() {
    let root = serde_json::json!({
        "mcp-server-splunk": {
            "environments": {
                "SPLUNK_URL": "https://splunk.example.com:8089",
                "SPLUNK_TOKEN": "scoped-token"
            }
        },
        "grafana": {
            "environments": {
                "GRAFANA_TOKEN": "unrelated"
            }
        }
    });
    let sections =
        mcp_server_atlassian::config::extract_all_vendor_sections(&root, "mcp-server-atlassian");
    let splunk = sections
        .get("splunk")
        .expect("Splunk alias should resolve to the canonical section");

    assert_eq!(
        splunk.get("SPLUNK_TOKEN").map(String::as_str),
        Some("scoped-token")
    );
    assert!(!splunk.contains_key("GRAFANA_TOKEN"));
}

#[test]
fn classifier_extracts_splunk_messages() {
    let error = classify(
        StatusCode::BAD_REQUEST,
        r#"{"messages":[{"type":"ERROR","text":"Unknown search command"}]}"#,
    );
    assert_eq!(error.kind, ErrorKind::ApiError);
    assert_eq!(error.status_code, Some(400));
    assert!(error.message.contains("Unknown search command"));
}

#[test]
fn classifier_maps_authentication_errors() {
    let error = classify(StatusCode::UNAUTHORIZED, "Token is invalid");
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("Token is invalid"));
}
