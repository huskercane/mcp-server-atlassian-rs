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
    let expected = format!("Basic {}", STANDARD.encode(b"alice@example.com:s3cret"));
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
fn auth_header_bearer_emits_bearer_scheme() {
    // Zoom's resolved token uses the Bearer scheme, not Basic — and the token
    // is passed through verbatim (no base64).
    let creds = Credentials::Bearer {
        token: "abc.def.ghi".into(),
    };
    assert_eq!(creds.auth_header(), "Bearer abc.def.ghi");
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
    let c = Credentials::Bearer {
        token: "abc.def.ghi".into(),
    };
    assert_eq!(a.principal(), "alice@example.com");
    assert_eq!(b.principal(), "bob");
    // Bearer has no principal and must never leak the token.
    assert_eq!(c.principal(), "bearer");
}

// ---- keychain-aware resolution ----

#[test]
fn keychain_sentinel_hit_expands_to_real_token() {
    let cfg = cfg(&[
        ("ATLASSIAN_USER_EMAIL", "alice@example.com"),
        ("ATLASSIAN_API_TOKEN", "keychain"),
    ]);
    let kc = InMemoryKeychain::new();
    kc.set(
        SecretKind::ApiToken,
        V,
        "alice@example.com",
        "real-token-from-os",
    )
    .unwrap();

    let creds = Credentials::resolve_with_for(&cfg, &kc, V)
        .unwrap()
        .unwrap();
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
    kc.set(
        SecretKind::ApiToken,
        VENDOR_BITBUCKET,
        "alice@example.com",
        "bb-tok",
    )
    .unwrap();
    kc.set(
        SecretKind::ApiToken,
        VENDOR_JIRA,
        "alice@example.com",
        "jira-tok",
    )
    .unwrap();

    let bb = Credentials::resolve_with_for(&cfg, &kc, VENDOR_BITBUCKET)
        .unwrap()
        .unwrap();
    let jira = Credentials::resolve_with_for(&cfg, &kc, VENDOR_JIRA)
        .unwrap()
        .unwrap();
    match bb {
        Credentials::AtlassianApiToken { token, .. } => assert_eq!(token, "bb-tok"),
        other => panic!("unexpected: {other:?}"),
    }
    match jira {
        Credentials::AtlassianApiToken { token, .. } => assert_eq!(token, "jira-tok"),
        other => panic!("unexpected: {other:?}"),
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
    kc.set(
        SecretKind::ApiToken,
        V,
        "alice@example.com",
        "from-implicit",
    )
    .unwrap();

    let creds = Credentials::resolve_with_for(&cfg, &kc, V)
        .unwrap()
        .unwrap();
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
    let creds = Credentials::resolve_with_for(&cfg, &kc, V)
        .unwrap()
        .unwrap();
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

    let creds = Credentials::resolve_with_for(&cfg, &kc, V)
        .unwrap()
        .unwrap();
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

    let creds = Credentials::resolve_with_for(&cfg, &kc, V)
        .unwrap()
        .unwrap();
    match creds {
        Credentials::AtlassianApiToken { token, .. } => {
            assert_eq!(token, "plaintext-from-config");
        }
        other => {
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
    let creds = Credentials::resolve_with_for(&cfg, &kc, V)
        .unwrap()
        .unwrap();
    match creds {
        Credentials::BitbucketAppPassword { .. } => {}
        other => {
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
    let creds = Credentials::resolve_with_for(&cfg, &kc, V)
        .unwrap()
        .unwrap();
    match creds {
        Credentials::BitbucketAppPassword { .. } => {}
        other => {
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
        !err.message
            .contains("Authentication credentials are missing"),
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
        fn note_implicit_failure(&self, kind: SecretKind, vendor: &str, principal: &str) -> bool {
            let inserted =
                self.seen
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
    assert_eq!(
        warns, 1,
        "expected exactly one warn-worthy event, got {warns}"
    );
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

// ---------------------------------------------------------------------------
// Vendor-owned secrets (NinjaOne console login)
//
// These go through `resolve_secret_for` rather than the Atlassian resolver,
// but must honour identical sentinel semantics: an explicit `"keychain"` miss
// is a hard error, an absent key falls back silently, and plaintext wins.
// ---------------------------------------------------------------------------

const NINJA: &str = mcp_server_atlassian::config::VENDOR_NINJAONE;

fn ninja_password(
    config: &Config,
    kc: &InMemoryKeychain,
) -> Result<Option<(String, String)>, mcp_server_atlassian::error::McpError> {
    mcp_server_atlassian::auth::resolve_secret_for(
        config,
        kc,
        NINJA,
        SecretKind::Password,
        "NINJAONE_EMAIL",
        "NINJAONE_PASSWORD",
    )
}

#[test]
fn ninjaone_password_resolves_from_the_keychain_sentinel() {
    let kc = empty_kc();
    kc.set(SecretKind::Password, NINJA, "tech@example.com", "s3cret")
        .unwrap();
    let config = cfg(&[
        ("NINJAONE_EMAIL", "tech@example.com"),
        ("NINJAONE_PASSWORD", "keychain"),
    ]);

    let (principal, secret) = ninja_password(&config, &kc).unwrap().unwrap();
    assert_eq!(principal, "tech@example.com");
    assert_eq!(secret, "s3cret");
}

/// The entry is scoped by vendor, so a password filed under another vendor is
/// not visible here — that is what keeps `--vendor` meaningful.
#[test]
fn ninjaone_password_does_not_read_another_vendors_slot() {
    let kc = empty_kc();
    kc.set(
        SecretKind::Password,
        VENDOR_JIRA,
        "tech@example.com",
        "wrong",
    )
    .unwrap();
    let config = cfg(&[
        ("NINJAONE_EMAIL", "tech@example.com"),
        ("NINJAONE_PASSWORD", "keychain"),
    ]);

    let error = ninja_password(&config, &kc).unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("no keychain"), "{}", error.message);
}

#[test]
fn ninjaone_password_sentinel_without_an_entry_is_a_hard_error() {
    let kc = empty_kc();
    let config = cfg(&[
        ("NINJAONE_EMAIL", "tech@example.com"),
        ("NINJAONE_PASSWORD", "keychain"),
    ]);

    let error = ninja_password(&config, &kc).unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    // The message must name the command that fixes it.
    assert!(error.message.contains("creds set"), "{}", error.message);
    assert!(error.message.contains("password"), "{}", error.message);
}

/// No `NINJAONE_PASSWORD` at all: the keychain is consulted anyway, so an
/// operator can store the secret and omit the key entirely.
#[test]
fn ninjaone_password_falls_back_to_the_keychain_implicitly() {
    let kc = empty_kc();
    kc.set(SecretKind::Password, NINJA, "tech@example.com", "s3cret")
        .unwrap();
    let config = cfg(&[("NINJAONE_EMAIL", "tech@example.com")]);

    let (_, secret) = ninja_password(&config, &kc).unwrap().unwrap();
    assert_eq!(secret, "s3cret");
}

#[test]
fn a_plaintext_ninjaone_password_still_wins() {
    let kc = empty_kc();
    kc.set(
        SecretKind::Password,
        NINJA,
        "tech@example.com",
        "from-keychain",
    )
    .unwrap();
    let config = cfg(&[
        ("NINJAONE_EMAIL", "tech@example.com"),
        ("NINJAONE_PASSWORD", "from-config"),
    ]);

    let (_, secret) = ninja_password(&config, &kc).unwrap().unwrap();
    assert_eq!(secret, "from-config");
}

#[test]
fn the_totp_seed_uses_its_own_keychain_slot() {
    let kc = empty_kc();
    kc.set(SecretKind::Password, NINJA, "tech@example.com", "s3cret")
        .unwrap();
    kc.set(
        SecretKind::TotpSecret,
        NINJA,
        "tech@example.com",
        "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQ",
    )
    .unwrap();
    let config = cfg(&[
        ("NINJAONE_EMAIL", "tech@example.com"),
        ("NINJAONE_PASSWORD", "keychain"),
        ("NINJAONE_TOTP_SECRET", "keychain"),
    ]);

    let (_, password) = ninja_password(&config, &kc).unwrap().unwrap();
    let (_, seed) = mcp_server_atlassian::auth::resolve_secret_for(
        &config,
        &kc,
        NINJA,
        SecretKind::TotpSecret,
        "NINJAONE_EMAIL",
        "NINJAONE_TOTP_SECRET",
    )
    .unwrap()
    .unwrap();

    assert_eq!(password, "s3cret");
    assert!(seed.starts_with("otpauth://"));
}
