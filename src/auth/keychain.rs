//! OS keychain integration for credential storage.
//!
//! Behaviour:
//! - Two secret kinds map to distinct keyring "service" name *prefixes* so the
//!   API token and Bitbucket app-password live in separate namespaces.
//!   Each entry is further scoped by canonical vendor (`bitbucket` / `jira`
//!   / `confluence`) so the same email principal can hold three independent
//!   tokens — one per Atlassian product. Final service strings:
//!   `mcp-server-atlassian.api-token.<vendor>` and
//!   `mcp-server-atlassian.app-password.<vendor>`.
//! - The "account" is the principal (email or username) — same string the
//!   `Credentials` enum already uses via [`crate::auth::Credentials::principal`].
//! - A single in-process [`OsKeychain`] instance owns a per-(kind, vendor,
//!   principal) breadcrumb dedup set so the `tracing::info!` provenance line
//!   fires exactly once per triple, not once per request and not once per process.
//!
//! Build matrix:
//! - `--features keychain` (default on macOS/Windows/Linux desktop): real
//!   backend wired via the `keyring` crate. Each platform pins an explicit
//!   credential-store feature in `Cargo.toml`; without one, `keyring` would
//!   fall back to a mock store that silently no-ops `set`.
//! - `--no-default-features`: the [`OsKeychain`] type still exists but every
//!   method returns [`KeychainError::Unavailable`]. Use this for headless
//!   Linux (CI / SSH / containers) where there is no desktop keyring agent.

use std::collections::HashSet;
use std::fmt;
use std::sync::Mutex;

/// Two kinds of secret. Service-name suffix follows the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// Atlassian Cloud API token (paired with `ATLASSIAN_USER_EMAIL`).
    ApiToken,
    /// Bitbucket app password (paired with `ATLASSIAN_BITBUCKET_USERNAME`).
    AppPassword,
}

impl SecretKind {
    /// Vendor-scoped keyring "service" name. Vendor must be a canonical
    /// vendor name from [`crate::config`] (`bitbucket` / `jira` /
    /// `confluence`). Distinct vendors get distinct keychain slots even when
    /// the principal (email) is the same.
    pub fn service_for(self, vendor: &str) -> String {
        let prefix = match self {
            Self::ApiToken => "mcp-server-atlassian.api-token",
            Self::AppPassword => "mcp-server-atlassian.app-password",
        };
        format!("{prefix}.{vendor}")
    }

    /// Human-readable name for CLI / error messages.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ApiToken => "api-token",
            Self::AppPassword => "app-password",
        }
    }

    /// Parse a CLI-friendly name back to a kind. Accepts kebab-case and the
    /// full env-var spellings to keep the CLI forgiving.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "api-token" | "ATLASSIAN_API_TOKEN" => Some(Self::ApiToken),
            "app-password" | "ATLASSIAN_BITBUCKET_APP_PASSWORD" => Some(Self::AppPassword),
            _ => None,
        }
    }
}

impl fmt::Display for SecretKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug)]
pub enum KeychainError {
    /// Backend is reachable but the entry is not present.
    NotFound,
    /// Backend is unavailable on this platform / in this build (e.g.
    /// keychain-off compile, headless Linux without a keyring agent).
    Unavailable(String),
    /// Generic backend failure — surface the message verbatim.
    Backend(String),
}

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "keychain entry not found"),
            Self::Unavailable(msg) => write!(f, "keychain unavailable: {msg}"),
            Self::Backend(msg) => write!(f, "keychain backend error: {msg}"),
        }
    }
}

impl std::error::Error for KeychainError {}

pub type KeychainResult<T> = Result<T, KeychainError>;

/// Cross-platform keychain abstraction. Implementations:
/// - [`OsKeychain`]: production, wraps the `keyring` crate.
/// - [`InMemoryKeychain`]: test-only fake.
///
/// Every operation is scoped by `(kind, vendor, principal)`. `vendor` is a
/// canonical name from [`crate::config`] — Atlassian Cloud users may hold
/// three independent tokens (one per product) under the same email, so
/// vendor is part of the identity, not a fallback hint.
pub trait KeychainBackend: Send + Sync {
    fn get(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
    ) -> KeychainResult<Option<String>>;
    fn set(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
        secret: &str,
    ) -> KeychainResult<()>;
    fn delete(&self, kind: SecretKind, vendor: &str, principal: &str) -> KeychainResult<()>;

    /// Record that we successfully resolved `(kind, vendor, principal)` from
    /// the keychain so that subsequent hits within the same process don't
    /// re-emit the provenance breadcrumb. Default impl is a no-op so test
    /// fakes can ignore it.
    fn note_breadcrumb(&self, _kind: SecretKind, _vendor: &str, _principal: &str) -> bool {
        false
    }

    /// Record that the implicit fallback path observed a backend failure
    /// for `(kind, vendor, principal)`. Used to dedupe `warn!` lines:
    /// backend outage on a per-request resolve path would otherwise fire
    /// on every tool invocation. Returns `true` the first time the triple
    /// is seen (caller should emit `warn!`); `false` thereafter (caller
    /// stays silent or emits `debug!`).
    fn note_implicit_failure(&self, _kind: SecretKind, _vendor: &str, _principal: &str) -> bool {
        false
    }
}

/// Real OS keychain — macOS Keychain Services, Windows Credential Manager,
/// or Linux Secret Service depending on platform.
pub struct OsKeychain {
    /// Set of `(kind, vendor, principal)` triples whose first hit has
    /// already been logged this process. Used by [`note_breadcrumb`] to
    /// dedupe the `tracing::info!(source = "keychain", ...)` line.
    seen: Mutex<HashSet<(SecretKind, String, String)>>,
    /// Set of `(kind, vendor, principal)` triples whose first implicit
    /// fallback backend failure has already been warned about. Without
    /// this, every request that hits a missing-secret + flaky-keychain
    /// config would emit a `warn!` per call.
    seen_failures: Mutex<HashSet<(SecretKind, String, String)>>,
}

impl OsKeychain {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
            seen_failures: Mutex::new(HashSet::new()),
        }
    }
}

impl Default for OsKeychain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "keychain")]
mod backend {
    use super::{KeychainError, KeychainResult, SecretKind};

    fn entry(kind: SecretKind, vendor: &str, principal: &str) -> KeychainResult<keyring::Entry> {
        keyring::Entry::new(&kind.service_for(vendor), principal)
            .map_err(|e| KeychainError::Backend(e.to_string()))
    }

    pub fn get(
        kind: SecretKind,
        vendor: &str,
        principal: &str,
    ) -> KeychainResult<Option<String>> {
        match entry(kind, vendor, principal)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeychainError::Backend(e.to_string())),
        }
    }

    pub fn set(
        kind: SecretKind,
        vendor: &str,
        principal: &str,
        secret: &str,
    ) -> KeychainResult<()> {
        entry(kind, vendor, principal)?
            .set_password(secret)
            .map_err(|e| KeychainError::Backend(e.to_string()))
    }

    pub fn delete(kind: SecretKind, vendor: &str, principal: &str) -> KeychainResult<()> {
        match entry(kind, vendor, principal)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Err(KeychainError::NotFound),
            Err(e) => Err(KeychainError::Backend(e.to_string())),
        }
    }
}

#[cfg(not(feature = "keychain"))]
mod backend {
    use super::{KeychainError, KeychainResult, SecretKind};

    fn unavailable<T>() -> KeychainResult<T> {
        Err(KeychainError::Unavailable(
            "binary built with --no-default-features (keychain disabled)".into(),
        ))
    }

    pub fn get(_: SecretKind, _: &str, _: &str) -> KeychainResult<Option<String>> {
        unavailable()
    }
    pub fn set(_: SecretKind, _: &str, _: &str, _: &str) -> KeychainResult<()> {
        unavailable()
    }
    pub fn delete(_: SecretKind, _: &str, _: &str) -> KeychainResult<()> {
        unavailable()
    }
}

impl KeychainBackend for OsKeychain {
    fn get(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
    ) -> KeychainResult<Option<String>> {
        backend::get(kind, vendor, principal)
    }

    fn set(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
        secret: &str,
    ) -> KeychainResult<()> {
        backend::set(kind, vendor, principal, secret)
    }

    fn delete(&self, kind: SecretKind, vendor: &str, principal: &str) -> KeychainResult<()> {
        backend::delete(kind, vendor, principal)
    }

    fn note_breadcrumb(&self, kind: SecretKind, vendor: &str, principal: &str) -> bool {
        let mut guard = match self.seen.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert((kind, vendor.to_owned(), principal.to_owned()))
    }

    fn note_implicit_failure(&self, kind: SecretKind, vendor: &str, principal: &str) -> bool {
        let mut guard = match self.seen_failures.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert((kind, vendor.to_owned(), principal.to_owned()))
    }
}

/// In-memory test fake. Public so integration tests in `tests/` can use it
/// without a feature flag — it never touches the OS keychain.
#[derive(Debug, Default)]
pub struct InMemoryKeychain {
    inner: Mutex<std::collections::HashMap<(SecretKind, String, String), String>>,
}

impl InMemoryKeychain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent `get` / `set` / `delete` return
    /// `KeychainError::Backend(reason)`. Used to test rollback paths.
    pub fn with_failure(reason: &str) -> FailingKeychain {
        FailingKeychain {
            reason: reason.to_owned(),
        }
    }

    /// Test helper — count of stored entries.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl KeychainBackend for InMemoryKeychain {
    fn get(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
    ) -> KeychainResult<Option<String>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(kind, vendor.to_owned(), principal.to_owned()))
            .cloned())
    }

    fn set(
        &self,
        kind: SecretKind,
        vendor: &str,
        principal: &str,
        secret: &str,
    ) -> KeychainResult<()> {
        self.inner.lock().unwrap().insert(
            (kind, vendor.to_owned(), principal.to_owned()),
            secret.to_owned(),
        );
        Ok(())
    }

    fn delete(&self, kind: SecretKind, vendor: &str, principal: &str) -> KeychainResult<()> {
        match self
            .inner
            .lock()
            .unwrap()
            .remove(&(kind, vendor.to_owned(), principal.to_owned()))
        {
            Some(_) => Ok(()),
            None => Err(KeychainError::NotFound),
        }
    }
}

/// Backend whose every operation fails with the supplied message. Used
/// only by tests to exercise rollback paths.
pub struct FailingKeychain {
    reason: String,
}

impl KeychainBackend for FailingKeychain {
    fn get(&self, _: SecretKind, _: &str, _: &str) -> KeychainResult<Option<String>> {
        Err(KeychainError::Backend(self.reason.clone()))
    }
    fn set(&self, _: SecretKind, _: &str, _: &str, _: &str) -> KeychainResult<()> {
        Err(KeychainError::Backend(self.reason.clone()))
    }
    fn delete(&self, _: SecretKind, _: &str, _: &str) -> KeychainResult<()> {
        Err(KeychainError::Backend(self.reason.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_distinct_per_kind_and_vendor() {
        assert_ne!(
            SecretKind::ApiToken.service_for("bitbucket"),
            SecretKind::AppPassword.service_for("bitbucket")
        );
        assert_ne!(
            SecretKind::ApiToken.service_for("bitbucket"),
            SecretKind::ApiToken.service_for("jira")
        );
        assert_eq!(
            SecretKind::ApiToken.service_for("jira"),
            "mcp-server-atlassian.api-token.jira"
        );
    }

    #[test]
    fn parse_accepts_kebab_and_envvar_forms() {
        assert_eq!(SecretKind::parse("api-token"), Some(SecretKind::ApiToken));
        assert_eq!(
            SecretKind::parse("ATLASSIAN_API_TOKEN"),
            Some(SecretKind::ApiToken)
        );
        assert_eq!(SecretKind::parse("app-password"), Some(SecretKind::AppPassword));
        assert_eq!(
            SecretKind::parse("ATLASSIAN_BITBUCKET_APP_PASSWORD"),
            Some(SecretKind::AppPassword)
        );
        assert_eq!(SecretKind::parse("nonsense"), None);
    }

    #[test]
    fn in_memory_roundtrip() {
        let kc = InMemoryKeychain::new();
        assert!(kc.is_empty());
        kc.set(SecretKind::ApiToken, "bitbucket", "alice@example.com", "secret-1")
            .unwrap();
        assert_eq!(
            kc.get(SecretKind::ApiToken, "bitbucket", "alice@example.com")
                .unwrap()
                .as_deref(),
            Some("secret-1")
        );
        assert_eq!(kc.len(), 1);
        kc.delete(SecretKind::ApiToken, "bitbucket", "alice@example.com")
            .unwrap();
        assert!(kc.is_empty());
    }

    #[test]
    fn in_memory_distinguishes_kinds() {
        let kc = InMemoryKeychain::new();
        kc.set(
            SecretKind::ApiToken,
            "bitbucket",
            "same@example.com",
            "token-value",
        )
        .unwrap();
        kc.set(
            SecretKind::AppPassword,
            "bitbucket",
            "same@example.com",
            "password-value",
        )
        .unwrap();
        assert_eq!(
            kc.get(SecretKind::ApiToken, "bitbucket", "same@example.com")
                .unwrap()
                .as_deref(),
            Some("token-value")
        );
        assert_eq!(
            kc.get(SecretKind::AppPassword, "bitbucket", "same@example.com")
                .unwrap()
                .as_deref(),
            Some("password-value")
        );
    }

    #[test]
    fn in_memory_distinguishes_vendors_for_same_principal() {
        let kc = InMemoryKeychain::new();
        kc.set(SecretKind::ApiToken, "bitbucket", "same@example.com", "bb-token")
            .unwrap();
        kc.set(SecretKind::ApiToken, "jira", "same@example.com", "jira-token")
            .unwrap();
        assert_eq!(
            kc.get(SecretKind::ApiToken, "bitbucket", "same@example.com")
                .unwrap()
                .as_deref(),
            Some("bb-token")
        );
        assert_eq!(
            kc.get(SecretKind::ApiToken, "jira", "same@example.com")
                .unwrap()
                .as_deref(),
            Some("jira-token")
        );
    }

    #[test]
    fn os_keychain_breadcrumb_dedupes_per_triple() {
        let kc = OsKeychain::new();
        assert!(kc.note_breadcrumb(SecretKind::ApiToken, "bitbucket", "a@x"));
        assert!(!kc.note_breadcrumb(SecretKind::ApiToken, "bitbucket", "a@x"));
        assert!(kc.note_breadcrumb(SecretKind::ApiToken, "bitbucket", "b@x"));
        assert!(kc.note_breadcrumb(SecretKind::ApiToken, "jira", "a@x"));
        assert!(kc.note_breadcrumb(SecretKind::AppPassword, "bitbucket", "a@x"));
    }

    #[test]
    fn delete_missing_returns_not_found() {
        let kc = InMemoryKeychain::new();
        let err = kc
            .delete(SecretKind::ApiToken, "bitbucket", "nope@x")
            .unwrap_err();
        match err {
            KeychainError::NotFound => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn failing_backend_propagates_reason() {
        let kc = InMemoryKeychain::with_failure("dbus down");
        let err = kc
            .set(SecretKind::ApiToken, "bitbucket", "p", "s")
            .unwrap_err();
        match err {
            KeychainError::Backend(msg) => assert!(msg.contains("dbus down")),
            other => panic!("expected Backend, got {other:?}"),
        }
    }
}
