#![allow(clippy::doc_markdown)]

//! The vendor-secret registry, and the keychain expansion it drives.
//!
//! Coverage here is deliberately table-driven: the registry is the single
//! source of truth for runtime resolution, `creds migrate`, and the `creds
//! set` guard, so a row that is wrong (or missing) breaks all three at once
//! and silently — the secret just stays in plaintext.

use std::collections::HashMap;

use mcp_server_atlassian::auth::secrets::{self, VENDOR_SECRETS};
use mcp_server_atlassian::auth::{
    InMemoryKeychain, KeychainBackend, SecretKind, vendor_secret_with,
};
use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::error::ErrorKind;

fn cfg(entries: &[(&str, &str)]) -> Config {
    Config::from_map(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>(),
    )
}

/// Every secret-bearing config key the vendors read must be registered —
/// an unregistered key silently loses keychain support.
#[test]
fn every_known_secret_key_is_registered() {
    for key in [
        "ATLASSIAN_API_TOKEN",
        "ATLASSIAN_BITBUCKET_APP_PASSWORD",
        "ZOOM_CLIENT_SECRET",
        "SLACK_TOKEN",
        "CIRCLECI_TOKEN",
        "POSTMAN_API_KEY",
        "NEW_RELIC_API_KEY",
        "GRAFANA_TOKEN",
        "SONARQUBE_TOKEN",
        "SPLUNK_TOKEN",
        "EDX_ACCESS_TOKEN",
        "WRDS_PASSWORD",
        "NINJAONE_PASSWORD",
        "NINJAONE_TOTP_SECRET",
        "NINJAONE_ACCESS_TOKEN",
        "NINJAONE_SESSION_KEY",
        "NINJAONE_SESSION_COOKIE",
    ] {
        assert!(
            secrets::lookup_by_key(key).is_some(),
            "{key} is not in the vendor-secret registry"
        );
    }
}

/// Two rows may not claim the same slot: `(kind, vendor, principal)` is the
/// keychain address, so a collision means one secret overwrites the other.
#[test]
fn no_two_rows_share_a_slot() {
    let mut seen: Vec<(SecretKind, &str, &str)> = Vec::new();
    for secret in VENDOR_SECRETS {
        let principal = secret.principal_key.unwrap_or(secret.secret_key);
        let slot = (secret.kind, secret.vendor, principal);
        assert!(
            !seen.contains(&slot),
            "duplicate keychain slot for {}/{}",
            secret.vendor,
            secret.secret_key
        );
        seen.push(slot);
    }
}

/// A token with no account is addressed by its own key name, which is what
/// keeps NinjaOne's three carrier credentials in distinct slots.
#[test]
fn principal_less_secrets_are_addressed_by_key_name() {
    let slack = secrets::lookup("slack", "SLACK_TOKEN").unwrap();
    assert_eq!(slack.principal_key, None);
    assert_eq!(slack.principal(None), "SLACK_TOKEN");

    let ninja: Vec<&str> = secrets::for_vendor("ninjaone")
        .filter(|secret| secret.kind == SecretKind::Token)
        .map(|secret| secret.principal(None))
        .collect();
    assert_eq!(
        ninja,
        [
            "NINJAONE_ACCESS_TOKEN",
            "NINJAONE_SESSION_KEY",
            "NINJAONE_SESSION_COOKIE"
        ]
    );
}

#[test]
fn an_account_scoped_secret_uses_the_configured_principal() {
    let wrds = secrets::lookup("wrds", "WRDS_PASSWORD").unwrap();
    assert_eq!(wrds.principal_key, Some("WRDS_USERNAME"));
    assert_eq!(wrds.principal(Some("rohit")), "rohit");
    // Principal configured but blank falls back to the key name rather than
    // addressing an empty account.
    assert_eq!(wrds.principal(Some("")), "WRDS_PASSWORD");
}

#[test]
fn a_principal_less_token_expands_from_the_keychain_sentinel() {
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::Token, "slack", "SLACK_TOKEN", "xoxb-real")
        .unwrap();
    let config = cfg(&[("SLACK_TOKEN", "keychain")]);

    let resolved = vendor_secret_with(&config, &kc, "slack", "SLACK_TOKEN").unwrap();
    assert_eq!(resolved.as_deref(), Some("xoxb-real"));
}

#[test]
fn a_sentinel_without_an_entry_is_a_hard_error() {
    let kc = InMemoryKeychain::new();
    let config = cfg(&[("SLACK_TOKEN", "keychain")]);

    let error = vendor_secret_with(&config, &kc, "slack", "SLACK_TOKEN").unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("creds set"), "{}", error.message);
}

#[test]
fn plaintext_still_wins_over_a_stored_entry() {
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::Token, "slack", "SLACK_TOKEN", "from-keychain")
        .unwrap();
    let config = cfg(&[("SLACK_TOKEN", "from-config")]);

    let resolved = vendor_secret_with(&config, &kc, "slack", "SLACK_TOKEN").unwrap();
    assert_eq!(resolved.as_deref(), Some("from-config"));
}

/// Omitting the key entirely still consults the keychain, so an operator can
/// store the secret and delete the field.
#[test]
fn an_absent_key_falls_back_to_the_keychain() {
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::Token, "grafana", "GRAFANA_TOKEN", "glsa_x")
        .unwrap();

    let resolved = vendor_secret_with(&cfg(&[]), &kc, "grafana", "GRAFANA_TOKEN").unwrap();
    assert_eq!(resolved.as_deref(), Some("glsa_x"));
}

/// Regression: the per-vendor wrappers used to trim before this resolver
/// existed. A whitespace-only value must read as unset, not as a credential
/// made of spaces, and a copy-pasted trailing newline must not travel.
#[test]
fn whitespace_is_trimmed_and_blank_reads_as_missing() {
    let kc = InMemoryKeychain::new();

    let blank =
        vendor_secret_with(&cfg(&[("SLACK_TOKEN", "   ")]), &kc, "slack", "SLACK_TOKEN").unwrap();
    assert_eq!(blank, None);

    let padded = vendor_secret_with(
        &cfg(&[("SLACK_TOKEN", "  xoxb-padded\n")]),
        &kc,
        "slack",
        "SLACK_TOKEN",
    )
    .unwrap();
    assert_eq!(padded.as_deref(), Some("xoxb-padded"));

    // The sentinel is matched after trimming too.
    kc.set(SecretKind::Token, "slack", "SLACK_TOKEN", "xoxb-real")
        .unwrap();
    let sentinel = vendor_secret_with(
        &cfg(&[("SLACK_TOKEN", " keychain ")]),
        &kc,
        "slack",
        "SLACK_TOKEN",
    )
    .unwrap();
    assert_eq!(sentinel.as_deref(), Some("xoxb-real"));
}

/// Entries are vendor-scoped, so a token stored for one vendor is invisible
/// to another even under the same kind and principal.
#[test]
fn slots_do_not_leak_across_vendors() {
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::Token, "slack", "SLACK_TOKEN", "xoxb-real")
        .unwrap();

    let resolved = vendor_secret_with(&cfg(&[]), &kc, "circleci", "CIRCLECI_TOKEN").unwrap();
    assert_eq!(resolved, None);
}

/// An unregistered key has no slot to expand from, so it is read as plaintext
/// only — inventing a slot would file secrets where nothing looks for them.
#[test]
fn an_unregistered_key_is_plaintext_only() {
    let kc = InMemoryKeychain::new();
    let config = cfg(&[("SPLUNK_AUTH_SCHEME", "splunk")]);

    let resolved = vendor_secret_with(&config, &kc, "splunk", "SPLUNK_AUTH_SCHEME").unwrap();
    assert_eq!(resolved.as_deref(), Some("splunk"));
    // Even the sentinel stays literal: nothing registered it.
    let literal = vendor_secret_with(
        &cfg(&[("SPLUNK_AUTH_SCHEME", "keychain")]),
        &kc,
        "splunk",
        "SPLUNK_AUTH_SCHEME",
    )
    .unwrap();
    assert_eq!(literal.as_deref(), Some("keychain"));
}

#[test]
fn the_cli_guard_follows_the_registry() {
    assert!(secrets::kind_supported_by(SecretKind::Token, "slack"));
    assert!(secrets::kind_supported_by(SecretKind::Password, "wrds"));
    assert!(secrets::kind_supported_by(
        SecretKind::TotpSecret,
        "ninjaone"
    ));
    assert!(secrets::kind_supported_by(
        SecretKind::AppPassword,
        "bitbucket"
    ));

    assert!(!secrets::kind_supported_by(SecretKind::AppPassword, "jira"));
    assert!(!secrets::kind_supported_by(SecretKind::TotpSecret, "slack"));
    assert!(!secrets::kind_supported_by(SecretKind::Token, "jira"));
}

/// The DB environment blob is intentionally out of scope — it is a JSON
/// document with several passwords inside, not a single secret.
#[test]
fn the_database_environment_blob_is_not_registered() {
    assert!(secrets::lookup_by_key("NINJAONE_DB_ENVIRONMENTS").is_none());
}
