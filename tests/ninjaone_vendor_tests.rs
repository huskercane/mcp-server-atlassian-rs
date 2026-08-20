use std::collections::HashMap;

use mcp_server_atlassian::auth::Credentials;
use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::vendor::Vendor;
use mcp_server_atlassian::vendor::ninjaone::NinjaOneVendor;

fn config_with(pairs: &[(&str, &str)]) -> Config {
    Config::from_map(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>(),
    )
}

#[test]
fn default_server_uses_ninjaone_url() {
    let config = config_with(&[("NINJAONE_URL", "https://tenant.example/")]);
    assert_eq!(
        NinjaOneVendor::new().base_url(&config).unwrap(),
        "https://tenant.example"
    );
}

#[test]
fn server_alias_must_come_from_configured_map() {
    let config = config_with(&[(
        "NINJAONE_SERVERS",
        r#"{"dev":"https://dev.example","qa":"https://qa.example/"}"#,
    )]);
    let vendor = NinjaOneVendor::new().for_server(Some("qa"));
    assert_eq!(vendor.base_url(&config).unwrap(), "https://qa.example");

    let error = NinjaOneVendor::new()
        .for_server(Some("evil"))
        .base_url(&config)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("Unknown NinjaOne server alias"));
}

#[test]
fn server_alias_can_apply_its_own_path_prefix() {
    let config = config_with(&[(
        "NINJAONE_SERVERS",
        r#"{
            "test":{"url":"https://test.example/root/","prefix":"/test-api/"},
            "qa":{"url":"https://qa.example","prefix":"/qa-api"}
        }"#,
    )]);

    assert_eq!(
        NinjaOneVendor::new()
            .for_server(Some("test"))
            .base_url(&config)
            .unwrap(),
        "https://test.example/root/test-api"
    );
    assert_eq!(
        NinjaOneVendor::new()
            .for_server(Some("qa"))
            .base_url(&config)
            .unwrap(),
        "https://qa.example/qa-api"
    );
}

#[test]
fn server_alias_rejects_an_invalid_prefix() {
    let config = config_with(&[(
        "NINJAONE_SERVERS",
        r#"{"test":{"url":"https://test.example","prefix":"relative"}}"#,
    )]);

    let error = NinjaOneVendor::new()
        .for_server(Some("test"))
        .base_url(&config)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnexpectedError);
    assert!(error.message.contains("prefix must be an absolute path"));
}

#[test]
fn bearer_token_has_precedence() {
    let config = config_with(&[
        ("NINJAONE_ACCESS_TOKEN", "access-token"),
        ("NINJAONE_SESSION_KEY", "session-key"),
    ]);
    assert_eq!(
        NinjaOneVendor::new().credentials(&config).unwrap(),
        Credentials::Bearer {
            token: "access-token".to_owned()
        }
    );
}

#[test]
fn browser_cookie_is_an_explicit_cookie_header() {
    let config = config_with(&[("NINJAONE_SESSION_COOKIE", "sessionKey=abc")]);
    assert_eq!(
        NinjaOneVendor::new().credentials(&config).unwrap(),
        Credentials::ApiKeyHeader {
            header_name: "Cookie".to_owned(),
            key: "sessionKey=abc".to_owned()
        }
    );
}

#[test]
fn missing_auth_is_actionable() {
    let error = NinjaOneVendor::new()
        .credentials(&Config::from_map(HashMap::new()))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("NINJAONE_ACCESS_TOKEN"));
}

/// The private `/ws/...` endpoints answer an unauthenticated call with the
/// `{"resultCode","errorMessage"}` envelope, not the `{"message"}` shape the
/// public API uses. Surfacing `errorMessage` is what turns a bare
/// "Unauthorized" into a diagnosable "Missing or empty sessionKey."
#[test]
fn console_error_envelope_surfaces_error_message() {
    let error = mcp_server_atlassian::vendor::ninjaone::error::classify(
        reqwest::StatusCode::UNAUTHORIZED,
        r#"{"resultCode":"FAILURE","errorMessage":"Missing or empty sessionKey."}"#,
    );
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("Missing or empty sessionKey."));
}
