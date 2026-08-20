//! In-process tests for `creds set/get/rm/migrate` handlers.
//!
//! These exercise the credential migration algorithm against an
//! [`InMemoryKeychain`] and a tempdir-backed `configs.json`. Subprocess
//! tests for clap wiring live in `tests/binary_tests.rs` — `assert_cmd`
//! spawns a separate process so an in-memory backend in the parent can't
//! reach the child, which is why the migrate logic is tested in-process.

use std::path::Path;
use std::sync::Mutex;

use mcp_server_atlassian::auth::keychain::{KeychainBackend, KeychainError, KeychainResult};
use mcp_server_atlassian::auth::{InMemoryKeychain, SecretKind};
use mcp_server_atlassian::cli::creds::{self, MigrateSkip};
use mcp_server_atlassian::config::{VENDOR_BITBUCKET, VENDOR_JIRA, VENDOR_NINJAONE};
use serde_json::{Value, json};
use tempfile::TempDir;

// ---- helpers -------------------------------------------------------------

fn write_config(path: &Path, body: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(body).unwrap()).unwrap();
}

fn read_config(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Pluck `root[section].environments[key]` as a string, if present.
fn env_value(root: &Value, section: &str, key: &str) -> Option<String> {
    root.get(section)?
        .get("environments")?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

fn make_path(dir: &TempDir, name: &str) -> std::path::PathBuf {
    dir.path().join(name)
}

// ---- migrate happy path --------------------------------------------------

#[test]
fn migrate_happy_path_moves_token_and_rewrites_file() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "real-plaintext-token",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();

    let outcome = creds::migrate_with(&kc, &path, false).unwrap();

    assert_eq!(outcome.migrated.len(), 1);
    assert_eq!(outcome.migrated[0].kind, SecretKind::ApiToken);
    assert_eq!(outcome.migrated[0].vendor, VENDOR_BITBUCKET);
    assert_eq!(outcome.migrated[0].principal, "alice@example.com");
    assert_eq!(
        kc.get(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com")
            .unwrap()
            .as_deref(),
        Some("real-plaintext-token")
    );
    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_USER_EMAIL"),
        Some("alice@example.com".into())
    );
    let bak = outcome.backup_path.unwrap();
    let original = serde_json::to_vec_pretty(&json!({
        "bitbucket": { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  "real-plaintext-token",
        }},
    }))
    .unwrap();
    let bak_bytes = std::fs::read(&bak).unwrap();
    assert_eq!(bak_bytes, original);
}

#[test]
fn migrate_app_password_kind_works() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_BITBUCKET_USERNAME":     "bobby",
                "ATLASSIAN_BITBUCKET_APP_PASSWORD": "secret-app-pw",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    let outcome = creds::migrate_with(&kc, &path, false).unwrap();

    assert_eq!(outcome.migrated.len(), 1);
    assert_eq!(outcome.migrated[0].kind, SecretKind::AppPassword);
    assert_eq!(outcome.migrated[0].vendor, VENDOR_BITBUCKET);
    assert_eq!(
        kc.get(SecretKind::AppPassword, VENDOR_BITBUCKET, "bobby")
            .unwrap()
            .as_deref(),
        Some("secret-app-pw")
    );
    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_BITBUCKET_APP_PASSWORD"),
        Some("keychain".into())
    );
}

#[test]
fn migrate_handles_both_kinds_in_one_run() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL":             "alice@example.com",
                "ATLASSIAN_API_TOKEN":              "api-tok",
                "ATLASSIAN_BITBUCKET_USERNAME":     "bobby",
                "ATLASSIAN_BITBUCKET_APP_PASSWORD": "app-pw",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    let outcome = creds::migrate_with(&kc, &path, false).unwrap();

    assert_eq!(outcome.migrated.len(), 2);
    assert_eq!(kc.len(), 2);
    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_BITBUCKET_APP_PASSWORD"),
        Some("keychain".into())
    );
}

// ---- per-vendor tokens ---------------------------------------------------

#[test]
fn migrate_writes_independent_keychain_entries_per_vendor() {
    // The user's actual case: three vendors, three different tokens, all
    // under the same email principal. Each vendor section migrates into
    // its own scoped keychain slot — no cross-vendor disagreement error.
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "bb-token",
            }},
            "jira": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "jira-token",
            }},
            "confluence": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "conf-token",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    let outcome = creds::migrate_with(&kc, &path, false).unwrap();

    assert_eq!(outcome.migrated.len(), 3);
    assert_eq!(
        kc.get(SecretKind::ApiToken, "bitbucket", "alice@example.com")
            .unwrap()
            .as_deref(),
        Some("bb-token")
    );
    assert_eq!(
        kc.get(SecretKind::ApiToken, "jira", "alice@example.com")
            .unwrap()
            .as_deref(),
        Some("jira-token")
    );
    assert_eq!(
        kc.get(SecretKind::ApiToken, "confluence", "alice@example.com")
            .unwrap()
            .as_deref(),
        Some("conf-token")
    );

    // Each section's secret is replaced with the sentinel — but only its own.
    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
    assert_eq!(
        env_value(&after, "jira", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
    assert_eq!(
        env_value(&after, "confluence", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
}

// ---- idempotency / sentinel verification ---------------------------------

#[test]
fn migrate_is_idempotent_when_already_migrated() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "keychain",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    kc.set(
        SecretKind::ApiToken,
        VENDOR_BITBUCKET,
        "alice@example.com",
        "stored-token",
    )
    .unwrap();

    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert!(outcome.migrated.is_empty());
    assert!(
        outcome
            .skipped
            .iter()
            .any(|s| matches!(s, MigrateSkip::AlreadyMigrated { .. }))
    );
    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
    assert!(outcome.backup_path.is_none());
}

#[test]
fn migrate_sentinel_with_empty_keychain_entry_is_hard_error() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "keychain",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    kc.set(
        SecretKind::ApiToken,
        VENDOR_BITBUCKET,
        "alice@example.com",
        "",
    )
    .unwrap();

    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(
        err.message.contains("empty"),
        "expected empty-entry message, got: {}",
        err.message
    );
}

#[test]
fn migrate_sentinel_without_keychain_entry_is_hard_error() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original_body = json!({
        "bitbucket": { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  "keychain",
        }},
    });
    write_config(&path, &original_body);
    let kc = InMemoryKeychain::new();

    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("no keychain entry"), "{}", err.message);
    assert_eq!(read_config(&path), original_body);
    assert!(!path.with_extension("json.bak").exists());
}

// ---- alias inspection / canonical-vendor conflicts -----------------------

#[test]
fn migrate_alias_agreement_rewrites_all_alias_copies() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket":           { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "shared-tok",
            }},
            "atlassian-bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "shared-tok",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    creds::migrate_with(&kc, &path, false).unwrap();

    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
    assert_eq!(
        env_value(&after, "atlassian-bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
}

#[test]
fn migrate_alias_conflict_two_plaintext_values_is_hard_error() {
    // Two ALIASES of the same canonical vendor disagreeing is still bad —
    // it's a copy-paste mistake within one product, not a per-product
    // choice. Cross-canonical-vendor disagreement is allowed (covered by
    // `migrate_writes_independent_keychain_entries_per_vendor`).
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original = json!({
        "bitbucket":           { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  "tok-A",
        }},
        "atlassian-bitbucket": { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  "tok-B",
        }},
    });
    write_config(&path, &original);
    let kc = InMemoryKeychain::new();

    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("alias conflict"), "{}", err.message);
    assert!(kc.is_empty(), "keychain modified despite conflict error");
    assert_eq!(read_config(&path), original, "file modified despite error");
}

#[test]
fn migrate_alias_conflict_sentinel_vs_plaintext_is_hard_error() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original = json!({
        "bitbucket":           { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  "keychain",
        }},
        "atlassian-bitbucket": { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  "leftover-plaintext",
        }},
    });
    write_config(&path, &original);
    let kc = InMemoryKeychain::new();
    kc.set(
        SecretKind::ApiToken,
        VENDOR_BITBUCKET,
        "alice@example.com",
        "stored",
    )
    .unwrap();

    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("alias conflict"), "{}", err.message);
    assert_eq!(read_config(&path), original);
}

#[test]
fn migrate_distinct_emails_per_vendor_are_independent() {
    // Different email per vendor — each vendor migrates against its own
    // principal. Used to error out as "ATLASSIAN_USER_EMAIL disagrees
    // across vendor sections"; with vendor scoping, that is now valid.
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "bb@example.com",
                "ATLASSIAN_API_TOKEN":  "bb-tok",
            }},
            "jira": { "environments": {
                "ATLASSIAN_USER_EMAIL": "jira@example.com",
                "ATLASSIAN_API_TOKEN":  "jira-tok",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    creds::migrate_with(&kc, &path, false).unwrap();

    assert_eq!(
        kc.get(SecretKind::ApiToken, "bitbucket", "bb@example.com")
            .unwrap()
            .as_deref(),
        Some("bb-tok")
    );
    assert_eq!(
        kc.get(SecretKind::ApiToken, "jira", "jira@example.com")
            .unwrap()
            .as_deref(),
        Some("jira-tok")
    );
}

// ---- principal/secret edge cases -----------------------------------------

#[test]
fn migrate_secret_present_principal_missing_is_hard_error() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original = json!({
        "bitbucket": { "environments": {
            "ATLASSIAN_API_TOKEN": "stranded-plaintext",
        }},
    });
    write_config(&path, &original);
    let kc = InMemoryKeychain::new();
    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("missing"), "{}", err.message);
    assert_eq!(read_config(&path), original);
}

#[test]
fn migrate_principal_present_secret_missing_skips_with_partial() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert!(outcome.migrated.is_empty());
    assert!(outcome.skipped.iter().any(|s| matches!(
        s,
        MigrateSkip::PartiallyConfigured {
            kind: SecretKind::ApiToken,
            ..
        }
    )));
    assert!(outcome.backup_path.is_none());
}

#[test]
fn migrate_sentinel_with_principal_missing_is_hard_error() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original = json!({
        "bitbucket": { "environments": {
            "ATLASSIAN_API_TOKEN": "keychain",
        }},
    });
    write_config(&path, &original);
    let kc = InMemoryKeychain::new();
    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(
        err.message.contains("ATLASSIAN_USER_EMAIL"),
        "{}",
        err.message
    );
    assert!(kc.is_empty());
}

#[test]
fn migrate_app_password_in_jira_section_is_ignored() {
    // App-passwords are Bitbucket-only; runtime auth never reads them
    // outside the bitbucket vendor. Migrate must not write a dead
    // (jira, app-password) keychain entry just because the field happens
    // to appear in a non-Bitbucket section.
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "jira": { "environments": {
                "ATLASSIAN_BITBUCKET_USERNAME":     "bobby",
                "ATLASSIAN_BITBUCKET_APP_PASSWORD": "should-not-migrate",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert!(outcome.migrated.is_empty());
    assert!(
        kc.get(SecretKind::AppPassword, VENDOR_JIRA, "bobby")
            .unwrap()
            .is_none()
    );
}

#[test]
fn migrate_empty_secret_falls_through_as_partial() {
    // Empty plaintext secret with a principal: runtime auth treats this as
    // implicit-fallback (try keychain). Migrate has nothing to write, so
    // the candidate is reported as partial rather than an error.
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert!(outcome.migrated.is_empty());
    assert!(outcome.skipped.iter().any(|s| matches!(
        s,
        MigrateSkip::PartiallyConfigured {
            kind: SecretKind::ApiToken,
            ..
        }
    )));
}

// ---- type guard ---------------------------------------------------------

#[test]
fn migrate_non_string_secret_value_is_hard_error() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original = json!({
        "bitbucket": { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  12345,
        }},
    });
    write_config(&path, &original);
    let kc = InMemoryKeychain::new();
    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("number"), "{}", err.message);
    assert!(
        err.message.contains("ATLASSIAN_API_TOKEN"),
        "{}",
        err.message
    );
}

// ---- stale-clobber guard ------------------------------------------------

#[test]
fn migrate_stale_clobber_blocked_without_force() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original = json!({
        "bitbucket": { "environments": {
            "ATLASSIAN_USER_EMAIL": "alice@example.com",
            "ATLASSIAN_API_TOKEN":  "OLD-stale-from-file",
        }},
    });
    write_config(&path, &original);
    let kc = InMemoryKeychain::new();
    kc.set(
        SecretKind::ApiToken,
        VENDOR_BITBUCKET,
        "alice@example.com",
        "NEW-rotated-by-creds-set",
    )
    .unwrap();

    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("--force"), "{}", err.message);
    assert_eq!(
        kc.get(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com")
            .unwrap()
            .as_deref(),
        Some("NEW-rotated-by-creds-set")
    );
    assert_eq!(read_config(&path), original);
}

#[test]
fn migrate_stale_clobber_with_force_overwrites() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "OLD-from-file",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    kc.set(
        SecretKind::ApiToken,
        VENDOR_BITBUCKET,
        "alice@example.com",
        "NEW-from-creds-set",
    )
    .unwrap();

    let outcome = creds::migrate_with(&kc, &path, true).unwrap();
    assert_eq!(outcome.migrated.len(), 1);
    assert_eq!(
        kc.get(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com")
            .unwrap()
            .as_deref(),
        Some("OLD-from-file"),
        "--force should have overwritten with the file value"
    );
}

#[test]
fn migrate_in_sync_skips_keychain_write_but_rewrites_file() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "same-token-everywhere",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    kc.set(
        SecretKind::ApiToken,
        VENDOR_BITBUCKET,
        "alice@example.com",
        "same-token-everywhere",
    )
    .unwrap();

    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert!(
        outcome
            .skipped
            .iter()
            .any(|s| matches!(s, MigrateSkip::InSync { .. }))
    );
    assert_eq!(
        kc.get(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com")
            .unwrap()
            .as_deref(),
        Some("same-token-everywhere")
    );
    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
}

// ---- unrelated sections untouched ---------------------------------------

#[test]
fn migrate_unrelated_top_level_sections_are_not_touched() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket":      { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "real-token",
            }},
            "some-other-tool": { "environments": {
                "ATLASSIAN_API_TOKEN": "this-stays-as-is",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    creds::migrate_with(&kc, &path, false).unwrap();
    let after = read_config(&path);
    assert_eq!(
        env_value(&after, "bitbucket", "ATLASSIAN_API_TOKEN"),
        Some("keychain".into())
    );
    assert_eq!(
        env_value(&after, "some-other-tool", "ATLASSIAN_API_TOKEN"),
        Some("this-stays-as-is".into())
    );
}

// ---- rollback on mid-run failure ----------------------------------------

/// Backend that wraps an `InMemoryKeychain` but fails the Nth call to `set`.
/// Used to exercise rollback when the second candidate fails after the
/// first has already written.
struct FailingOnNthSet {
    inner: InMemoryKeychain,
    fail_after: Mutex<usize>,
}

impl FailingOnNthSet {
    fn new(succeed_count: usize) -> Self {
        Self {
            inner: InMemoryKeychain::new(),
            fail_after: Mutex::new(succeed_count),
        }
    }
}

impl KeychainBackend for FailingOnNthSet {
    fn get(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
    ) -> KeychainResult<Option<String>> {
        self.inner.get(kind, vendor, principal)
    }
    fn set(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
        secret: &str,
    ) -> KeychainResult<()> {
        let mut left = self.fail_after.lock().unwrap();
        if *left == 0 {
            return Err(KeychainError::Backend("simulated mid-run failure".into()));
        }
        *left -= 1;
        self.inner.set(kind, vendor, principal, secret)
    }
    fn delete(&self, kind: SecretKind, vendor: &str, principal: &str) -> KeychainResult<()> {
        self.inner.delete(kind, vendor, principal)
    }
}

#[test]
fn migrate_rolls_back_first_kind_when_second_fails() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let original = json!({
        "bitbucket": { "environments": {
            "ATLASSIAN_USER_EMAIL":             "alice@example.com",
            "ATLASSIAN_API_TOKEN":              "api-tok",
            "ATLASSIAN_BITBUCKET_USERNAME":     "bobby",
            "ATLASSIAN_BITBUCKET_APP_PASSWORD": "app-pw",
        }},
    });
    write_config(&path, &original);

    let kc = FailingOnNthSet::new(1);
    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("simulated"), "{}", err.message);

    assert_eq!(
        kc.get(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com")
            .unwrap(),
        None,
        "first kind not rolled back"
    );
    assert_eq!(read_config(&path), original);
}

// ---- atomic replace -----------------------------------------------------

#[test]
fn migrate_atomic_replace_produces_valid_json_on_disk() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "bitbucket": { "environments": {
                "ATLASSIAN_USER_EMAIL": "alice@example.com",
                "ATLASSIAN_API_TOKEN":  "tok",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();
    creds::migrate_with(&kc, &path, false).unwrap();

    let raw = std::fs::read(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&raw).expect("rewritten file is valid JSON");
    assert_eq!(
        parsed
            .get("bitbucket")
            .unwrap()
            .get("environments")
            .unwrap()
            .get("ATLASSIAN_API_TOKEN")
            .unwrap()
            .as_str(),
        Some("keychain")
    );
}

#[test]
fn migrate_errors_when_file_does_not_exist() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "nonexistent.json");
    let kc = InMemoryKeychain::new();
    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(
        err.message.contains("nothing to migrate"),
        "{}",
        err.message
    );
}

#[test]
fn migrate_errors_on_invalid_json() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    std::fs::write(&path, b"this is not json").unwrap();
    let kc = InMemoryKeychain::new();
    let err = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(err.message.contains("not valid JSON"), "{}", err.message);
}

// ---- migrate across the full vendor registry ------------------------------

/// Before the registry existed, `migrate` walked a hardcoded Atlassian table
/// and silently left every other vendor's secret in plaintext. This locks the
/// sweep: a principal-less token, an account-scoped password, and a
/// client-secret paired with a client id all move in one run.
#[test]
fn migrate_moves_non_atlassian_vendor_secrets() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &json!({
            "slack":    { "environments": { "SLACK_TOKEN": "xoxb-real" }},
            "circleci": { "environments": { "CIRCLECI_TOKEN": "circle-real" }},
            "zoom":     { "environments": {
                "ZOOM_ACCOUNT_ID":  "acct-1",
                "ZOOM_CLIENT_ID":     "client-1",
                "ZOOM_CLIENT_SECRET": "zoom-real",
            }},
            "wrds":     { "environments": {
                "WRDS_USERNAME": "rohit",
                "WRDS_PASSWORD": "wrds-real",
            }},
            "ninjaone": { "environments": {
                "NINJAONE_EMAIL":       "tech@example.com",
                "NINJAONE_PASSWORD":    "ninja-real",
                "NINJAONE_SESSION_KEY": "session-real",
            }},
        }),
    );
    let kc = InMemoryKeychain::new();

    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert_eq!(outcome.migrated.len(), 6, "{:?}", outcome.migrated);

    // Principal-less tokens are filed under their own key name.
    assert_eq!(
        kc.get(SecretKind::Token, "slack", "SLACK_TOKEN")
            .unwrap()
            .as_deref(),
        Some("xoxb-real")
    );
    assert_eq!(
        kc.get(SecretKind::Token, "circleci", "CIRCLECI_TOKEN")
            .unwrap()
            .as_deref(),
        Some("circle-real")
    );
    assert_eq!(
        kc.get(SecretKind::Token, "ninjaone", "NINJAONE_SESSION_KEY")
            .unwrap()
            .as_deref(),
        Some("session-real")
    );
    // Account-scoped secrets follow their principal.
    assert_eq!(
        kc.get(SecretKind::Token, "zoom", "client-1")
            .unwrap()
            .as_deref(),
        Some("zoom-real")
    );
    assert_eq!(
        kc.get(SecretKind::Password, "wrds", "rohit")
            .unwrap()
            .as_deref(),
        Some("wrds-real")
    );
    assert_eq!(
        kc.get(SecretKind::Password, "ninjaone", "tech@example.com")
            .unwrap()
            .as_deref(),
        Some("ninja-real")
    );

    // Every migrated key is replaced by the sentinel; identifiers are not.
    let rewritten: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        rewritten["slack"]["environments"]["SLACK_TOKEN"],
        "keychain"
    );
    assert_eq!(
        rewritten["zoom"]["environments"]["ZOOM_CLIENT_SECRET"],
        "keychain"
    );
    assert_eq!(
        rewritten["zoom"]["environments"]["ZOOM_CLIENT_ID"],
        "client-1"
    );
    assert_eq!(rewritten["wrds"]["environments"]["WRDS_USERNAME"], "rohit");
    assert_eq!(
        rewritten["ninjaone"]["environments"]["NINJAONE_EMAIL"],
        "tech@example.com"
    );
}

/// The DB blob holds passwords but is not a single secret, so migrate must
/// leave it exactly as it found it rather than storing the whole document.
#[test]
fn migrate_leaves_the_database_environment_blob_alone() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let blob = r#"{"qa5":{"centralHost":"h","divisionHosts":{},"username":"u","password":"p"}}"#;
    write_config(
        &path,
        &json!({
            "ninjaone": { "environments": { "NINJAONE_DB_ENVIRONMENTS": blob }},
        }),
    );
    let kc = InMemoryKeychain::new();

    creds::migrate_with(&kc, &path, false).unwrap();

    let rewritten: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        rewritten["ninjaone"]["environments"]["NINJAONE_DB_ENVIRONMENTS"],
        blob
    );
    assert!(kc.is_empty());
}

// ---- per-server NinjaOne accounts ----------------------------------------
//
// A NinjaOne account is the access boundary, so an operator holds one per
// environment and configures each on its own `NINJAONE_SERVERS` entry. Those
// credentials sit inside a JSON document that is itself one config string,
// which is the only place migrate has to walk into rather than read off a key.

/// Pull `NINJAONE_SERVERS` back out of the rewritten file and parse it.
fn servers_map(root: &Value, section: &str) -> Value {
    let raw = env_value(root, section, "NINJAONE_SERVERS").expect("NINJAONE_SERVERS is present");
    serde_json::from_str(&raw).expect("NINJAONE_SERVERS is valid JSON")
}

fn ninjaone_config(servers: &Value, extra: &[(&str, &str)]) -> Value {
    let mut env = serde_json::Map::new();
    env.insert(
        "NINJAONE_SERVERS".to_owned(),
        Value::String(serde_json::to_string(servers).unwrap()),
    );
    for (key, value) in extra {
        env.insert((*key).to_owned(), Value::String((*value).to_owned()));
    }
    json!({ "ninjaone": { "environments": Value::Object(env) } })
}

#[test]
fn migrate_moves_each_server_entrys_own_credentials() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    write_config(
        &path,
        &ninjaone_config(
            &json!({
                "qa4-1": {
                    "url": "https://qa4.example",
                    "prefix": "/swb/s1",
                    "email": "qa4@example.com",
                    "password": "qa4-plaintext",
                    "totpSecret": "GEZDGNBVGY3TQOJQ",
                },
                "qa5": {
                    "url": "https://qa5.example",
                    "email": "qa5@example.com",
                    "password": "qa5-plaintext",
                },
                "prod": "https://app.ninjarmm.com",
            }),
            &[],
        ),
    );
    let kc = InMemoryKeychain::new();

    let outcome = creds::migrate_with(&kc, &path, false).unwrap();

    // Each account got its own slot, keyed by the entry's own email.
    assert_eq!(
        kc.get(SecretKind::Password, VENDOR_NINJAONE, "qa4@example.com")
            .unwrap()
            .as_deref(),
        Some("qa4-plaintext")
    );
    assert_eq!(
        kc.get(SecretKind::TotpSecret, VENDOR_NINJAONE, "qa4@example.com")
            .unwrap()
            .as_deref(),
        Some("GEZDGNBVGY3TQOJQ")
    );
    assert_eq!(
        kc.get(SecretKind::Password, VENDOR_NINJAONE, "qa5@example.com")
            .unwrap()
            .as_deref(),
        Some("qa5-plaintext")
    );
    assert_eq!(outcome.migrated.len(), 3);
    // The record says which line moved: one account can be configured in more
    // than one place, so the principal alone does not identify it.
    assert!(
        outcome
            .migrated
            .iter()
            .any(|record| record.site == "NINJAONE_SERVERS[\"qa4-1\"].totpSecret"),
        "sites: {:?}",
        outcome.migrated.iter().map(|r| &r.site).collect::<Vec<_>>()
    );

    // The file keeps everything that is not a secret, and holds sentinels
    // where the secrets were.
    let servers = servers_map(&read_config(&path), "ninjaone");
    assert_eq!(servers["qa4-1"]["password"], json!("keychain"));
    assert_eq!(servers["qa4-1"]["totpSecret"], json!("keychain"));
    assert_eq!(servers["qa4-1"]["email"], json!("qa4@example.com"));
    assert_eq!(servers["qa4-1"]["prefix"], json!("/swb/s1"));
    assert_eq!(servers["qa5"]["password"], json!("keychain"));
    assert_eq!(servers["prod"], json!("https://app.ninjarmm.com"));
    // No plaintext survives anywhere in the file.
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(!rewritten.contains("qa4-plaintext"));
    assert!(!rewritten.contains("qa5-plaintext"));
}

#[test]
fn migrate_files_an_entry_without_an_email_under_the_top_level_account() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    // No `email` on the entry: it logs in as the top-level account, so that is
    // the account its password belongs to.
    write_config(
        &path,
        &ninjaone_config(
            &json!({ "qa": { "url": "https://qa.example", "password": "shared-plaintext" } }),
            &[("NINJAONE_EMAIL", "shared@example.com")],
        ),
    );
    let kc = InMemoryKeychain::new();

    creds::migrate_with(&kc, &path, false).unwrap();

    assert_eq!(
        kc.get(SecretKind::Password, VENDOR_NINJAONE, "shared@example.com")
            .unwrap()
            .as_deref(),
        Some("shared-plaintext")
    );
    let servers = servers_map(&read_config(&path), "ninjaone");
    assert_eq!(servers["qa"]["password"], json!("keychain"));
}

#[test]
fn migrate_refuses_a_server_secret_with_no_account_to_file_it_under() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let config = ninjaone_config(
        &json!({ "qa": { "url": "https://qa.example", "password": "orphan-plaintext" } }),
        &[],
    );
    write_config(&path, &config);
    let kc = InMemoryKeychain::new();

    let error = creds::migrate_with(&kc, &path, false).unwrap_err();

    assert!(
        error.message.contains("NINJAONE_SERVERS[\"qa\"].password"),
        "unhelpful message: {}",
        error.message
    );
    assert!(error.message.contains("email"));
    // Nothing filed, nothing rewritten: a secret in a slot nothing reads is
    // worse than the plaintext it replaced.
    assert_eq!(read_config(&path), config);
}

#[test]
fn migrate_rejects_two_entries_that_disagree_about_one_account() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let config = ninjaone_config(
        &json!({
            "qa4-1": {
                "url": "https://qa4.example",
                "email": "same@example.com",
                "password": "one-value",
            },
            "qa5": {
                "url": "https://qa5.example",
                "email": "same@example.com",
                "password": "another-value",
            },
        }),
        &[],
    );
    write_config(&path, &config);
    let kc = InMemoryKeychain::new();

    let error = creds::migrate_with(&kc, &path, false).unwrap_err();

    // Both culprits are named; the keychain holds one value per account, so
    // the second write would otherwise silently win.
    assert!(
        error
            .message
            .contains("NINJAONE_SERVERS[\"qa4-1\"].password")
            && error.message.contains("NINJAONE_SERVERS[\"qa5\"].password"),
        "unhelpful message: {}",
        error.message
    );
    // Redacted, and no plaintext in the error.
    assert!(!error.message.contains("one-value"));
    assert!(
        kc.get(SecretKind::Password, VENDOR_NINJAONE, "same@example.com")
            .unwrap()
            .is_none()
    );
    assert_eq!(read_config(&path), config);
}

#[test]
fn migrate_verifies_a_per_server_sentinel_against_the_keychain() {
    let dir = TempDir::new().unwrap();
    let path = make_path(&dir, "configs.json");
    let config = ninjaone_config(
        &json!({
            "qa4-1": {
                "url": "https://qa4.example",
                "email": "qa4@example.com",
                "password": "keychain",
            },
        }),
        &[],
    );
    write_config(&path, &config);
    let kc = InMemoryKeychain::new();

    // A sentinel with nothing behind it is a hard error naming the fix — the
    // runtime would fail the same way on the next login.
    let error = creds::migrate_with(&kc, &path, false).unwrap_err();
    assert!(error.message.contains("creds set"), "{}", error.message);
    assert!(error.message.contains("qa4@example.com"));
    assert_eq!(read_config(&path), config);

    // With the entry present it is a no-op, and the file is left alone.
    kc.set(
        SecretKind::Password,
        VENDOR_NINJAONE,
        "qa4@example.com",
        "already-there",
    )
    .unwrap();
    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert!(outcome.migrated.is_empty());
    assert!(
        outcome
            .skipped
            .iter()
            .any(|skip| matches!(skip, MigrateSkip::AlreadyMigrated { .. }))
    );
    assert_eq!(read_config(&path), config);
}
