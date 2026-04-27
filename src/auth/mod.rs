//! Credential resolution and HTTP Basic auth header construction.
//!
//! Mirrors the logic in TS `transport.util.ts` (credential selection block)
//! and the README-documented env var names. Two conventions are supported,
//! with the Atlassian API token taking priority when both sets are present.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::config::Config;
use crate::error::{McpError, auth_missing};

pub mod keychain;

pub use keychain::{InMemoryKeychain, KeychainBackend, KeychainError, OsKeychain, SecretKind};

/// Resolved Bitbucket credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credentials {
    /// `ATLASSIAN_USER_EMAIL` + `ATLASSIAN_API_TOKEN`.
    /// Shared across Jira/Confluence/Bitbucket; preferred when present.
    AtlassianApiToken { email: String, token: String },

    /// `ATLASSIAN_BITBUCKET_USERNAME` + `ATLASSIAN_BITBUCKET_APP_PASSWORD`.
    /// Bitbucket-specific fallback.
    BitbucketAppPassword { username: String, password: String },
}

/// Process-wide [`OsKeychain`] instance. Reused across all
/// [`Credentials::resolve`] / [`Credentials::require`] calls so the
/// per-(kind, principal) breadcrumb dedup state survives between requests.
fn os_keychain() -> &'static OsKeychain {
    static KC: std::sync::OnceLock<OsKeychain> = std::sync::OnceLock::new();
    KC.get_or_init(OsKeychain::new)
}

impl Credentials {
    /// Resolve credentials from a [`Config`], expanding any `"keychain"`
    /// sentinel against the OS keychain. Returns `None` when neither
    /// convention is fully populated; the caller decides whether this is an
    /// error (server boot) or allowed (CLI help, version, etc.).
    ///
    /// **Error suppression**: sentinel-miss and backend-unavailable surface
    /// as `None` from this method (with `tracing::error!` / `tracing::warn!`
    /// breadcrumbs). Callers that want the specific error message — e.g.
    /// "keychain entry not found for kind=api-token, principal=alice@x" —
    /// should call [`Credentials::require`] or [`Credentials::resolve_with`]
    /// instead.
    pub fn resolve(config: &Config) -> Option<Self> {
        match Self::resolve_with(config, os_keychain()) {
            Ok(opt) => opt,
            Err(err) => {
                tracing::error!(error = %err, "keychain-aware credential resolution failed");
                None
            }
        }
    }

    /// Same as [`resolve`] but errors with [`auth_missing`] when no credentials
    /// are present, and propagates keychain-specific errors verbatim
    /// (sentinel without an entry, backend unreachable for an explicit
    /// sentinel, etc.). Matches TS boot behavior.
    ///
    /// **Async paths must use [`require_async`] instead.** This function
    /// performs synchronous OS keychain reads, which can block on user
    /// prompts (macOS ACL grants) or D-Bus round-trips (Linux), and
    /// would freeze a Tokio worker thread.
    pub fn require(config: &Config) -> Result<Self, McpError> {
        Self::resolve_with(config, os_keychain())?.ok_or_else(|| {
            auth_missing(
                "Authentication credentials are missing. Set ATLASSIAN_USER_EMAIL + \
                 ATLASSIAN_API_TOKEN, or ATLASSIAN_BITBUCKET_USERNAME + \
                 ATLASSIAN_BITBUCKET_APP_PASSWORD.",
            )
        })
    }

    /// Async wrapper around [`require`] for use inside Tokio tasks. The
    /// OS keychain backends (Keychain Services / Credential Manager /
    /// Secret Service) expose synchronous APIs and can block — first-use
    /// ACL prompts on macOS, D-Bus round-trips on Linux — so calling
    /// [`require`] directly from an async handler would freeze a Tokio
    /// worker thread.
    ///
    /// This is the entry point every async server / controller path
    /// should use.
    pub async fn require_async(config: &Config) -> Result<Self, McpError> {
        let cfg = config.clone();
        tokio::task::spawn_blocking(move || Self::require(&cfg))
            .await
            .map_err(|e| {
                crate::error::unexpected(
                    format!("credential resolution task panicked: {e}"),
                    None,
                )
            })?
    }

    /// Keychain-aware resolution with an injectable backend. Production
    /// callers should use [`resolve`] or [`require`]; tests pass an
    /// [`InMemoryKeychain`].
    ///
    /// Behaviour per credential kind, in priority order
    /// (`AtlassianApiToken` first, `BitbucketAppPassword` fallback):
    ///
    /// 1. Read `principal_key` and `secret_key` via `Config::get`.
    /// 2. If the secret is the literal string `"keychain"`, look up the
    ///    keychain entry; missing entry / backend error is a hard error.
    /// 3. If the secret is missing entirely (implicit fallback), look up
    ///    the keychain entry; misses fall through to the next kind.
    /// 4. Otherwise, treat the secret as plaintext and use it as-is.
    pub fn resolve_with(
        config: &Config,
        backend: &dyn KeychainBackend,
    ) -> Result<Option<Self>, McpError> {
        if let Some((email, token)) = try_resolve_kind(
            config,
            backend,
            SecretKind::ApiToken,
            "ATLASSIAN_USER_EMAIL",
            "ATLASSIAN_API_TOKEN",
        )? {
            return Ok(Some(Self::AtlassianApiToken { email, token }));
        }

        if let Some((username, password)) = try_resolve_kind(
            config,
            backend,
            SecretKind::AppPassword,
            "ATLASSIAN_BITBUCKET_USERNAME",
            "ATLASSIAN_BITBUCKET_APP_PASSWORD",
        )? {
            return Ok(Some(Self::BitbucketAppPassword { username, password }));
        }

        Ok(None)
    }

    /// `Authorization: Basic <base64>` header value.
    pub fn basic_auth_header(&self) -> String {
        format!("Basic {}", self.basic_auth_payload())
    }

    /// Base64-encoded `user:secret` payload without the `Basic ` prefix.
    pub fn basic_auth_payload(&self) -> String {
        let raw = match self {
            Self::AtlassianApiToken { email, token } => format!("{email}:{token}"),
            Self::BitbucketAppPassword { username, password } => {
                format!("{username}:{password}")
            }
        };
        STANDARD.encode(raw.as_bytes())
    }

    /// Identifier part (email or username), useful for log lines without
    /// leaking the secret.
    pub fn principal(&self) -> &str {
        match self {
            Self::AtlassianApiToken { email, .. } => email,
            Self::BitbucketAppPassword { username, .. } => username,
        }
    }
}

/// Resolve one credential kind from config + keychain. Returns
/// `Ok(Some((principal, secret)))` on success, `Ok(None)` to fall through
/// to the next kind, and `Err(McpError)` for explicit sentinel
/// misconfiguration that the caller must surface.
fn try_resolve_kind(
    config: &Config,
    backend: &dyn KeychainBackend,
    kind: SecretKind,
    principal_key: &str,
    secret_key: &str,
) -> Result<Option<(String, String)>, McpError> {
    let principal = match config.get(principal_key) {
        Some(p) if !p.is_empty() => p,
        _ => {
            // Principal absent. If the secret is an explicit sentinel,
            // that's a misconfiguration — error out so the user sees it.
            // Otherwise just fall through to the next kind.
            if config.get(secret_key) == Some("keychain") {
                return Err(auth_missing(format!(
                    "config sets {secret_key}=\"keychain\" but {principal_key} is missing"
                )));
            }
            return Ok(None);
        }
    };

    match config.get(secret_key) {
        // Explicit sentinel — user opted in, miss is a hard error.
        Some("keychain") => match backend.get(kind, principal) {
            Ok(Some(s)) if !s.is_empty() => {
                if backend.note_breadcrumb(kind, principal) {
                    tracing::info!(
                        source = "keychain",
                        kind = %kind,
                        principal = principal,
                        "resolved credential (sentinel)"
                    );
                }
                Ok(Some((principal.to_owned(), s)))
            }
            Ok(_) => {
                tracing::error!(
                    kind = %kind,
                    principal = principal,
                    "config sets {secret_key}=\"keychain\" but no entry exists"
                );
                Err(auth_missing(format!(
                    "config sets {secret_key}=\"keychain\" but no keychain entry \
                     exists for kind={kind}, principal={principal}. Run \
                     `mcp-atlassian creds set --kind {kind} --principal {principal}` \
                     or remove the sentinel."
                )))
            }
            Err(e) => {
                tracing::error!(
                    kind = %kind,
                    principal = principal,
                    error = %e,
                    "keychain lookup failed for sentinel"
                );
                Err(auth_missing(format!(
                    "keychain lookup failed for kind={kind}, principal={principal}: {e}"
                )))
            }
        },
        // Plaintext secret — use as-is.
        Some(s) if !s.is_empty() => Ok(Some((principal.to_owned(), s.to_owned()))),
        // Empty plaintext is treated as missing for fall-through.
        Some(_) => Ok(None),
        // Implicit fallback — secret absent; try keychain, miss is fine.
        None => match backend.get(kind, principal) {
            Ok(Some(s)) if !s.is_empty() => {
                if backend.note_breadcrumb(kind, principal) {
                    tracing::info!(
                        source = "keychain",
                        kind = %kind,
                        principal = principal,
                        "resolved credential (implicit)"
                    );
                }
                Ok(Some((principal.to_owned(), s)))
            }
            Ok(_) => {
                tracing::debug!(
                    kind = %kind,
                    principal = principal,
                    "implicit keychain miss; falling through"
                );
                Ok(None)
            }
            Err(e) => {
                // Per-(kind, principal) dedup so a flaky/missing keyring
                // agent doesn't spam warnings on every request. First
                // failure observed warns; subsequent failures stay at
                // debug level so RUST_LOG=debug still surfaces them but
                // INFO-level operators don't see noise.
                if backend.note_implicit_failure(kind, principal) {
                    tracing::warn!(
                        kind = %kind,
                        principal = principal,
                        error = %e,
                        "keychain backend unavailable for implicit lookup"
                    );
                } else {
                    tracing::debug!(
                        kind = %kind,
                        principal = principal,
                        error = %e,
                        "keychain backend unavailable (deduped warn)"
                    );
                }
                Ok(None)
            }
        },
    }
}
