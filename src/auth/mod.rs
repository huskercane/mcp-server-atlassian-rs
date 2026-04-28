//! Credential resolution and HTTP Basic auth header construction.
//!
//! Every credential is resolved per-vendor. The same email principal may
//! hold three independent Atlassian Cloud API tokens (one each for Jira,
//! Confluence, Bitbucket) — that is the supported model, not a quirk —
//! so vendor scope is part of the identity, not a fallback hint.
//!
//! Two conventions are supported per vendor, with the Atlassian API token
//! taking priority when both sets are present:
//! - `ATLASSIAN_USER_EMAIL` + `ATLASSIAN_API_TOKEN`
//! - `ATLASSIAN_BITBUCKET_USERNAME` + `ATLASSIAN_BITBUCKET_APP_PASSWORD`
//!
//! Config lookups go through [`Config::get_for`] so a credential defined in
//! one vendor section never leaks into another vendor's resolution.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::config::Config;
use crate::error::{McpError, auth_missing};

pub mod keychain;

pub use keychain::{InMemoryKeychain, KeychainBackend, KeychainError, OsKeychain, SecretKind};

/// Resolved Atlassian credentials, scoped to a single vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credentials {
    /// `ATLASSIAN_USER_EMAIL` + `ATLASSIAN_API_TOKEN`.
    /// Available for every vendor (Jira / Confluence / Bitbucket).
    AtlassianApiToken { email: String, token: String },

    /// `ATLASSIAN_BITBUCKET_USERNAME` + `ATLASSIAN_BITBUCKET_APP_PASSWORD`.
    /// Bitbucket-specific fallback; resolution returns this only when the
    /// vendor is `bitbucket`.
    BitbucketAppPassword { username: String, password: String },
}

/// Process-wide [`OsKeychain`] instance. Reused across all credential
/// resolution calls so the per-(kind, vendor, principal) breadcrumb dedup
/// state survives between requests.
fn os_keychain() -> &'static OsKeychain {
    static KC: std::sync::OnceLock<OsKeychain> = std::sync::OnceLock::new();
    KC.get_or_init(OsKeychain::new)
}

impl Credentials {
    /// Resolve credentials for `vendor` from a [`Config`], expanding any
    /// `"keychain"` sentinel against the OS keychain. Errors with
    /// [`auth_missing`] when no credentials are present, and propagates
    /// keychain-specific errors verbatim (sentinel without an entry,
    /// backend unreachable for an explicit sentinel, etc.).
    ///
    /// **Async paths must use [`require_for_async`](Self::require_for_async)
    /// instead.** This function performs synchronous OS keychain reads,
    /// which can block on user prompts (macOS ACL grants) or D-Bus
    /// round-trips (Linux), and would freeze a Tokio worker thread.
    ///
    /// Synchronous; intended for diagnostics, CLI bootstrap, and tests.
    pub fn require_for(config: &Config, vendor: &str) -> Result<Self, McpError> {
        Self::resolve_with_for(config, os_keychain(), vendor)?.ok_or_else(|| {
            auth_missing(format!(
                "Authentication credentials are missing for vendor `{vendor}`. Set \
                 ATLASSIAN_USER_EMAIL + ATLASSIAN_API_TOKEN in the `{vendor}` section, \
                 or (Bitbucket only) ATLASSIAN_BITBUCKET_USERNAME + \
                 ATLASSIAN_BITBUCKET_APP_PASSWORD."
            ))
        })
    }

    /// Async wrapper around [`require_for`](Self::require_for) for use
    /// inside Tokio tasks. The OS keychain backends (Keychain Services /
    /// Credential Manager / Secret Service) expose synchronous APIs and can
    /// block — first-use ACL prompts on macOS, D-Bus round-trips on Linux —
    /// so calling [`require_for`](Self::require_for) directly from an async
    /// handler would freeze a Tokio worker thread.
    ///
    /// This is the entry point every async server / controller path uses.
    pub async fn require_for_async(config: &Config, vendor: &str) -> Result<Self, McpError> {
        let cfg = config.clone();
        let vendor = vendor.to_owned();
        tokio::task::spawn_blocking(move || Self::require_for(&cfg, &vendor))
            .await
            .map_err(|e| {
                crate::error::unexpected(
                    format!("credential resolution task panicked: {e}"),
                    None,
                )
            })?
    }

    /// Keychain-aware resolution with an injectable backend. Production
    /// callers should use [`require_for`](Self::require_for) or
    /// [`require_for_async`](Self::require_for_async); tests pass an
    /// [`InMemoryKeychain`].
    ///
    /// Behaviour per credential kind, in priority order
    /// (`AtlassianApiToken` first, `BitbucketAppPassword` only for the
    /// `bitbucket` vendor):
    ///
    /// 1. Read `principal_key` and `secret_key` via [`Config::get_for`] for
    ///    the requested vendor.
    /// 2. If the secret is the literal string `"keychain"`, look up the
    ///    keychain entry under `(kind, vendor, principal)`; missing entry /
    ///    backend error is a hard error.
    /// 3. If the secret is missing entirely (implicit fallback), look up
    ///    the keychain entry under `(kind, vendor, principal)`; misses
    ///    fall through to the next kind.
    /// 4. Otherwise, treat the secret as plaintext and use it as-is.
    pub fn resolve_with_for(
        config: &Config,
        backend: &dyn KeychainBackend,
        vendor: &str,
    ) -> Result<Option<Self>, McpError> {
        if let Some((email, token)) = try_resolve_kind(
            config,
            backend,
            vendor,
            SecretKind::ApiToken,
            "ATLASSIAN_USER_EMAIL",
            "ATLASSIAN_API_TOKEN",
        )? {
            return Ok(Some(Self::AtlassianApiToken { email, token }));
        }

        if vendor == crate::config::VENDOR_BITBUCKET
            && let Some((username, password)) = try_resolve_kind(
                config,
                backend,
                vendor,
                SecretKind::AppPassword,
                "ATLASSIAN_BITBUCKET_USERNAME",
                "ATLASSIAN_BITBUCKET_APP_PASSWORD",
            )?
        {
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

/// Resolve one credential kind from config + keychain, scoped to `vendor`.
/// Returns `Ok(Some((principal, secret)))` on success, `Ok(None)` to fall
/// through to the next kind, and `Err(McpError)` for explicit sentinel
/// misconfiguration that the caller must surface.
fn try_resolve_kind(
    config: &Config,
    backend: &dyn KeychainBackend,
    vendor: &str,
    kind: SecretKind,
    principal_key: &str,
    secret_key: &str,
) -> Result<Option<(String, String)>, McpError> {
    let principal = match config.get_for(vendor, principal_key) {
        Some(p) if !p.is_empty() => p,
        _ => {
            // Principal absent. If the secret is an explicit sentinel,
            // that's a misconfiguration — error out so the user sees it.
            // Otherwise just fall through to the next kind.
            if config.get_for(vendor, secret_key) == Some("keychain") {
                return Err(auth_missing(format!(
                    "vendor `{vendor}` sets {secret_key}=\"keychain\" but \
                     {principal_key} is missing"
                )));
            }
            return Ok(None);
        }
    };

    match config.get_for(vendor, secret_key) {
        // Explicit sentinel — user opted in, miss is a hard error.
        Some("keychain") => match backend.get(kind, vendor, principal) {
            Ok(Some(s)) if !s.is_empty() => {
                if backend.note_breadcrumb(kind, vendor, principal) {
                    tracing::info!(
                        source = "keychain",
                        kind = %kind,
                        vendor = vendor,
                        principal = principal,
                        "resolved credential (sentinel)"
                    );
                }
                Ok(Some((principal.to_owned(), s)))
            }
            Ok(_) => {
                tracing::error!(
                    kind = %kind,
                    vendor = vendor,
                    principal = principal,
                    "vendor `{vendor}` sets {secret_key}=\"keychain\" but no entry exists"
                );
                Err(auth_missing(format!(
                    "vendor `{vendor}` sets {secret_key}=\"keychain\" but no keychain \
                     entry exists for kind={kind}, vendor={vendor}, principal={principal}. \
                     Run `mcp-atlassian creds set --kind {kind} --vendor {vendor} \
                     --principal {principal}` or remove the sentinel."
                )))
            }
            Err(e) => {
                tracing::error!(
                    kind = %kind,
                    vendor = vendor,
                    principal = principal,
                    error = %e,
                    "keychain lookup failed for sentinel"
                );
                Err(auth_missing(format!(
                    "keychain lookup failed for kind={kind}, vendor={vendor}, \
                     principal={principal}: {e}"
                )))
            }
        },
        // Plaintext secret — use as-is.
        Some(s) if !s.is_empty() => Ok(Some((principal.to_owned(), s.to_owned()))),
        // Empty plaintext is treated as missing for fall-through.
        Some(_) => Ok(None),
        // Implicit fallback — secret absent; try keychain, miss is fine.
        None => match backend.get(kind, vendor, principal) {
            Ok(Some(s)) if !s.is_empty() => {
                if backend.note_breadcrumb(kind, vendor, principal) {
                    tracing::info!(
                        source = "keychain",
                        kind = %kind,
                        vendor = vendor,
                        principal = principal,
                        "resolved credential (implicit)"
                    );
                }
                Ok(Some((principal.to_owned(), s)))
            }
            Ok(_) => {
                tracing::debug!(
                    kind = %kind,
                    vendor = vendor,
                    principal = principal,
                    "implicit keychain miss; falling through"
                );
                Ok(None)
            }
            Err(e) => {
                if backend.note_implicit_failure(kind, vendor, principal) {
                    tracing::warn!(
                        kind = %kind,
                        vendor = vendor,
                        principal = principal,
                        error = %e,
                        "keychain backend unavailable for implicit lookup"
                    );
                } else {
                    tracing::debug!(
                        kind = %kind,
                        vendor = vendor,
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
