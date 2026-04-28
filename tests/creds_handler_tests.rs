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
use mcp_server_atlassian::config::{VENDOR_BITBUCKET, VENDOR_JIRA};
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
    kc.set(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com", "stored-token")
        .unwrap();

    let outcome = creds::migrate_with(&kc, &path, false).unwrap();
    assert!(outcome.migrated.is_empty());
    assert!(outcome
        .skipped
        .iter()
        .any(|s| matches!(s, MigrateSkip::AlreadyMigrated { .. })));
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
    kc.set(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com", "")
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
    kc.set(SecretKind::ApiToken, VENDOR_BITBUCKET, "alice@example.com", "stored")
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
    assert!(err.message.contains("ATLASSIAN_API_TOKEN"), "{}", err.message);
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
    assert!(outcome
        .skipped
        .iter()
        .any(|s| matches!(s, MigrateSkip::InSync { .. })));
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
    assert!(err.message.contains("nothing to migrate"), "{}", err.message);
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
