//! Tests for the auth credential resolver.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use mcp_server_atlassian::auth::{Credentials, InMemoryKeychain, KeychainBackend, SecretKind};
use mcp_server_atlassian::config::{Config, VENDOR_BITBUCKET, VENDOR_JIRA};
use mcp_server_atlassian::error::ErrorKind;
use pretty_assertions::assert_eq;

/// Default vendor for tests that don't care about vendor scope. We pick
/// Bitbucket because it's the only vendor for which both credential
/// conventions (`AtlassianApiToken` and `BitbucketAppPassword`) resolve.
const V: &str = VENDOR_BITBUCKET;

fn cfg(entries: &[(&str, &str)]) -> Config {
    let mut m = HashMap::new();
    for (k, v) in entries {
        m.insert((*k).to_string(), (*v).to_string());
    }
    Config::from_map(m)
}

fn empty_kc() -> InMemoryKeychain {
    InMemoryKeychain::new()
}

#[test]
fn prefers_atlassian_api_token_when_both_present() {
    let c = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "user@example.com"),
        ("ATLASSIAN_API_TOKEN", "atlassian-secret"),
        ("ATLASSIAN_BITBUCKET_USERNAME", "bbuser"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "bbsecret"),
    ]);
    let creds = Credentials::resolve_with_for(&c, &empty_kc(), V)
        .unwrap()
        .unwrap();
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
    let creds = Credentials::resolve_with_for(&c, &empty_kc(), V)
        .unwrap()
        .unwrap();
    assert_eq!(
        creds,
        Credentials::BitbucketAppPassword {
            username: "bbuser".into(),
            password: "bbsecret".into(),
        }
    );
}

#[test]
fn app_password_path_only_resolves_for_bitbucket_vendor() {
    // Jira and Confluence have no concept of an app-password; runtime auth
    // must not pick one up even if the env happens to define those vars.
    let c = cfg(&[
        ("ATLASSIAN_BITBUCKET_USERNAME", "bbuser"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "bbsecret"),
    ]);
    assert!(
        Credentials::resolve_with_for(&c, &empty_kc(), VENDOR_JIRA)
            .unwrap()
            .is_none()
    );
}

#[test]
fn resolves_none_when_neither_set_is_complete() {
    let c = cfg(&[("ATLASSIAN_USER_EMAIL", "only-email@example.com")]);
    assert!(
        Credentials::resolve_with_for(&c, &empty_kc(), V)
            .unwrap()
            .is_none()
    );

    let c = cfg(&[("ATLASSIAN_BITBUCKET_USERNAME", "only-username")]);
    assert!(
        Credentials::resolve_with_for(&c, &empty_kc(), V)
            .unwrap()
            .is_none()
    );

    let c = cfg(&[]);
    assert!(
        Credentials::resolve_with_for(&c, &empty_kc(), V)
            .unwrap()
            .is_none()
    );
}

#[test]
fn rejects_empty_strings() {
    let c = cfg(&[
        ("ATLASSIAN_USER_EMAIL", ""),
        ("ATLASSIAN_API_TOKEN", "token"),
    ]);
    assert!(
        Credentials::resolve_with_for(&c, &empty_kc(), V)
            .unwrap()
            .is_none()
    );
}

#[test]
fn require_for_errors_when_missing() {
    let c = cfg(&[]);
    let err = Credentials::require_for(&c, V).unwrap_err();
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

// ---- keychain-aware resolution ----

#[test]
fn keychain_sentinel_hit_expands_to_real_token() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "keychain"),
    ]);
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::ApiToken, V, "alice@example.com", "real-token-from-os")
        .unwrap();

    let creds = Credentials::resolve_with_for(&cfg, &kc, V).unwrap().unwrap();
    assert_eq!(
        creds,
        Credentials::AtlassianApiToken {
            email: "alice@example.com".into(),
            token: "real-token-from-os".into(),
        }
    );
}

#[test]
fn keychain_sentinel_per_vendor_isolation() {
    // The same email may have a different token per vendor. Resolving for
    // jira must NOT pick up the bitbucket-scoped entry — that would defeat
    // the entire point of vendor scope.
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "keychain"),
    ]);
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com", "bb-tok")
        .unwrap();
    kc.set(SecretKind::ApiToken, VENDOR_JIRA, "alice@example.com", "jira-tok")
        .unwrap();

    let bb = Credentials::resolve_with_for(&cfg, &kc, VENDOR_BITBUCKET)
        .unwrap()
        .unwrap();
    let jira = Credentials::resolve_with_for(&cfg, &kc, VENDOR_JIRA)
        .unwrap()
        .unwrap();
    match bb {
        Credentials::AtlassianApiToken { token, .. } => assert_eq!(token, "bb-tok"),
        other @ Credentials::BitbucketAppPassword { .. } => panic!("unexpected: {other:?}"),
    }
    match jira {
        Credentials::AtlassianApiToken { token, .. } => assert_eq!(token, "jira-tok"),
        other @ Credentials::BitbucketAppPassword { .. } => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn keychain_sentinel_miss_is_hard_error() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "keychain"),
    ]);
    let kc = InMemoryKeychain::new(); // empty — sentinel set but no entry
    let err = Credentials::resolve_with_for(&cfg, &kc, V).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("no keychain entry"), "{}", err.message);
}

#[test]
fn keychain_sentinel_with_missing_principal_is_hard_error() {
    let cfg = cfg(&[("ATLASSIAN_API_TOKEN", "keychain")]); // email missing
    let kc = empty_kc();
    let err = Credentials::resolve_with_for(&cfg, &kc, V).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(
        err.message.contains("ATLASSIAN_USER_EMAIL"),
        "{}",
        err.message
    );
}

#[test]
fn keychain_implicit_fallback_hit_expands_when_secret_absent() {
    let cfg = cfg(&[("ATLASSIAN_USER_EMAIL", "alice@example.com")]);
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::ApiToken, V, "alice@example.com", "from-implicit")
        .unwrap();

    let creds = Credentials::resolve_with_for(&cfg, &kc, V).unwrap().unwrap();
    assert_eq!(
        creds,
        Credentials::AtlassianApiToken {
            email: "alice@example.com".into(),
            token: "from-implicit".into(),
        }
    );
}

#[test]
fn keychain_implicit_miss_falls_through_to_next_kind() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        // no API token entry in keychain
        ("ATLASSIAN_BITBUCKET_USERNAME", "bb-fallback"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "bb-secret"),
    ]);
    let kc = empty_kc();
    let creds = Credentials::resolve_with_for(&cfg, &kc, V).unwrap().unwrap();
    assert_eq!(
        creds,
        Credentials::BitbucketAppPassword {
            username: "bb-fallback".into(),
            password: "bb-secret".into(),
        }
    );
}

#[test]
fn keychain_implicit_miss_on_both_kinds_returns_none() {
    let cfg = cfg(&[("ATLASSIAN_USER_EMAIL", "alice@example.com")]);
    let kc = empty_kc();
    assert!(
        Credentials::resolve_with_for(&cfg, &kc, V)
            .unwrap()
            .is_none()
    );
}

#[test]
fn keychain_sentinel_works_for_app_password_kind() {
    let cfg = cfg(&[
        ("ATLASSIAN_BITBUCKET_USERNAME", "bobby"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "keychain"),
    ]);
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::AppPassword, V, "bobby", "real-app-password")
        .unwrap();

    let creds = Credentials::resolve_with_for(&cfg, &kc, V).unwrap().unwrap();
    assert_eq!(
        creds,
        Credentials::BitbucketAppPassword {
            username: "bobby".into(),
            password: "real-app-password".into(),
        }
    );
}

#[test]
fn plaintext_secret_takes_priority_over_keychain_lookup() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "plaintext-from-config"),
    ]);
    let kc = InMemoryKeychain::new();
    kc.set(SecretKind::ApiToken, V, "alice@example.com", "ignored")
        .unwrap();

    let creds = Credentials::resolve_with_for(&cfg, &kc, V).unwrap().unwrap();
    match creds {
        Credentials::AtlassianApiToken { token, .. } => {
            assert_eq!(token, "plaintext-from-config");
        }
        other @ Credentials::BitbucketAppPassword { .. } => {
            panic!("expected api token kind, got {other:?}")
        }
    }
}

#[test]
fn empty_plaintext_secret_falls_through() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", ""), // empty: not sentinel, not usable
        ("ATLASSIAN_BITBUCKET_USERNAME", "bb"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "bb-pass"),
    ]);
    let kc = empty_kc();
    let creds = Credentials::resolve_with_for(&cfg, &kc, V).unwrap().unwrap();
    match creds {
        Credentials::BitbucketAppPassword { .. } => {}
        other @ Credentials::AtlassianApiToken { .. } => {
            panic!("expected fallback to app password, got {other:?}")
        }
    }
}

#[test]
fn keychain_backend_error_on_sentinel_is_hard_error() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "keychain"),
    ]);
    let kc = InMemoryKeychain::with_failure("dbus down");
    let err = Credentials::resolve_with_for(&cfg, &kc, V).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("dbus down"), "{}", err.message);
}

#[test]
fn keychain_backend_error_on_implicit_falls_through() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        // no token at all → triggers implicit lookup
        ("ATLASSIAN_BITBUCKET_USERNAME", "bb"),
        ("ATLASSIAN_BITBUCKET_APP_PASSWORD", "bb-pass"),
    ]);
    let kc = InMemoryKeychain::with_failure("kc down");
    let creds = Credentials::resolve_with_for(&cfg, &kc, V).unwrap().unwrap();
    match creds {
        Credentials::BitbucketAppPassword { .. } => {}
        other @ Credentials::AtlassianApiToken { .. } => {
            panic!("expected app password fallback, got {other:?}")
        }
    }
}

#[test]
fn require_propagates_keychain_specific_errors() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "keychain"),
    ]);
    let kc = empty_kc();
    let err = Credentials::resolve_with_for(&cfg, &kc, V).unwrap_err();
    assert!(
        !err.message.contains("Authentication credentials are missing"),
        "got generic message instead of keychain-specific: {}",
        err.message
    );
}

#[test]
fn implicit_failure_breadcrumb_dedupes_per_triple() {
    // Backend that fails get() but tracks how many times note_implicit_failure
    // returned true (i.e. how many `warn!`s would fire).
    use mcp_server_atlassian::auth::keychain::{KeychainError, KeychainResult};
    use std::sync::Mutex;

    struct CountingFailingBackend {
        warn_calls: Mutex<usize>,
        seen: Mutex<std::collections::HashSet<(SecretKind, String, String)>>,
    }
    impl KeychainBackend for CountingFailingBackend {
        fn get(&self, _: SecretKind, _: &str, _: &str) -> KeychainResult<Option<String>> {
            Err(KeychainError::Backend("simulated".into()))
        }
        fn set(&self, _: SecretKind, _: &str, _: &str, _: &str) -> KeychainResult<()> {
            unreachable!()
        }
        fn delete(&self, _: SecretKind, _: &str, _: &str) -> KeychainResult<()> {
            unreachable!()
        }
        fn note_implicit_failure(
            &self,
            kind: SecretKind,
            vendor: &str,
            principal: &str,
        ) -> bool {
            let inserted = self
                .seen
                .lock()
                .unwrap()
                .insert((kind, vendor.to_owned(), principal.to_owned()));
            if inserted {
                *self.warn_calls.lock().unwrap() += 1;
            }
            inserted
        }
    }

    let cfg = cfg(&[("ATLASSIAN_USER_EMAIL", "alice@example.com")]);
    let backend = CountingFailingBackend {
        warn_calls: Mutex::new(0),
        seen: Mutex::new(std::collections::HashSet::new()),
    };

    // Three calls for the same (kind, vendor, principal) → only one warn.
    let _ = Credentials::resolve_with_for(&cfg, &backend, V);
    let _ = Credentials::resolve_with_for(&cfg, &backend, V);
    let _ = Credentials::resolve_with_for(&cfg, &backend, V);

    let warns = *backend.warn_calls.lock().unwrap();
    assert_eq!(warns, 1, "expected exactly one warn-worthy event, got {warns}");
}

#[tokio::test]
async fn require_for_async_runs_off_the_runtime() {
    // Keychain reads are synchronous and can block (macOS ACL prompt,
    // libsecret D-Bus round-trip). `require_for_async` must offload to a
    // blocking task so a Tokio worker isn't held hostage.
    let good = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "plaintext"),
    ]);
    let creds = Credentials::require_for_async(&good, V).await.unwrap();
    assert_eq!(
        creds,
        Credentials::AtlassianApiToken {
            email: "alice@example.com".into(),
            token: "plaintext".into(),
        }
    );

    // Errors from the inner sync path round-trip through .await.
    let bad = cfg(&[]);
    let err = Credentials::require_for_async(&bad, V).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
}
