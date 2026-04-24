//! Tests for the auth credential resolver.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use mcp_server_atlassian_bitbucket::auth::Credentials;
use mcp_server_atlassian_bitbucket::config::Config;
use mcp_server_atlassian_bitbucket::error::ErrorKind;
use pretty_assertions::assert_eq;

fn cfg(entries: &[(&str, &str)]) -> Config {
    let mut m = HashMap::new();
    for (k, v) in entries {
        m.insert((*k).to_string(), (*v).to_string());
    }
    Config::from_map(m)
}

#[test]
fn prefers_atlassian_api_token_when_both_present() {
    let c = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "user@example.com"),
        ("ATLASSIAN_API_TOKEN", "atlassian-secret"),
        ("ATLASSIAN_BITBUCKET_USERNAME", "bbuser"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "bbsecret"),
    ]);
    let creds = Credentials::resolve(&c).unwrap();
    assert_eq!(
        creds,
        Credentials::AtlassianApiToken {
            email: "user@example.com".into(),
            token: "atlassian-secret".into(),
        }
    );
}

#[test]
fn falls_back_to_bitbucket_app_password() {
    let c = cfg(&[
        ("ATLASSIAN_BITBUCKET_USERNAME", "bbuser"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "bbsecret"),
    ]);
    let creds = Credentials::resolve(&c).unwrap();
    assert_eq!(
        creds,
        Credentials::BitbucketAppPassword {
            username: "bbuser".into(),
            password: "bbsecret".into(),
        }
    );
}

#[test]
fn resolves_none_when_neither_set_is_complete() {
    let c = cfg(&[("ATLASSIAN_USER_EMAIL", "only-email@example.com")]);
    assert!(Credentials::resolve(&c).is_none());

    let c = cfg(&[("ATLASSIAN_BITBUCKET_USERNAME", "only-username")]);
    assert!(Credentials::resolve(&c).is_none());

    let c = cfg(&[]);
    assert!(Credentials::resolve(&c).is_none());
}

#[test]
fn rejects_empty_strings() {
    let c = cfg(&[
        ("ATLASSIAN_USER_EMAIL", ""),
        ("ATLASSIAN_API_TOKEN", "token"),
    ]);
    assert!(Credentials::resolve(&c).is_none());
}

#[test]
fn require_errors_when_missing() {
    let c = cfg(&[]);
    let err = Credentials::require(&c).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}

#[test]
fn basic_auth_header_atlassian() {
    let creds = Credentials::AtlassianApiToken {
        email: "alice@example.com".into(),
        token: "s3cret".into(),
    };
    let expected = format!(
        "Basic {}",
        STANDARD.encode(b"alice@example.com:s3cret")
    );
    assert_eq!(creds.basic_auth_header(), expected);
}

#[test]
fn basic_auth_header_bitbucket() {
    let creds = Credentials::BitbucketAppPassword {
        username: "bob".into(),
        password: "hunter2".into(),
    };
    let expected = format!("Basic {}", STANDARD.encode(b"bob:hunter2"));
    assert_eq!(creds.basic_auth_header(), expected);
}

#[test]
fn principal_returns_public_identifier() {
    let a = Credentials::AtlassianApiToken {
        email: "alice@example.com".into(),
        token: "s3cret".into(),
    };
    let b = Credentials::BitbucketAppPassword {
        username: "bob".into(),
        password: "hunter2".into(),
    };
    assert_eq!(a.principal(), "alice@example.com");
    assert_eq!(b.principal(), "bob");
}
