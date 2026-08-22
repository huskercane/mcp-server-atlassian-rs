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
use reqwest::header::{AUTHORIZATION, HeaderName};

use crate::config::Config;
use crate::error::{McpError, auth_invalid, auth_missing};

pub mod keychain;
pub mod secrets;

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

    /// A pre-resolved OAuth 2.0 bearer token, emitted as
    /// `Authorization: Bearer <token>`. Unlike the two Atlassian variants,
    /// this is **never** produced by [`Self::resolve_with_for`] — the shared
    /// Atlassian resolver stays free of OAuth concerns. It is minted by a
    /// vendor that owns its own token lifecycle (currently Zoom's
    /// Server-to-Server OAuth provider, which exchanges static client
    /// credentials for a short-lived token and auto-renews it) and handed to
    /// the transport purely as an auth carrier.
    Bearer { token: String },

    /// An API key carried in a **custom request header** rather than the
    /// standard `Authorization` header — e.g. Postman's `X-API-Key: <key>`.
    /// Like [`Self::Bearer`], it is never produced by the shared Atlassian
    /// resolver; a vendor that owns this scheme mints it from config and hands
    /// it to the transport. `header_name` is the header the value is sent
    /// under (canonicalised to a [`HeaderName`] at dispatch time); `key` is the
    /// raw secret, emitted verbatim with no scheme prefix.
    ApiKeyHeader { header_name: String, key: String },
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
                crate::error::unexpected(format!("credential resolution task panicked: {e}"), None)
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

    /// Header **value** for this credential. This is the single dispatch point
    /// the transport uses for the value: Basic for the two Atlassian variants,
    /// Bearer for [`Self::Bearer`], and the raw key for [`Self::ApiKeyHeader`].
    /// The header *name* this value is sent under is given by
    /// [`auth_header_name`](Self::auth_header_name). New auth schemes should be
    /// added here rather than at the transport layer.
    pub fn auth_header(&self) -> String {
        match self {
            Self::Bearer { token } => format!("Bearer {token}"),
            // A custom-header API key has no scheme prefix — the value is the
            // bare secret (e.g. `X-API-Key: <key>`).
            Self::ApiKeyHeader { key, .. } => key.clone(),
            Self::AtlassianApiToken { .. } | Self::BitbucketAppPassword { .. } => {
                format!("Basic {}", self.basic_auth_payload())
            }
        }
    }

    /// The request header name this credential's value is sent under. Every
    /// scheme except [`Self::ApiKeyHeader`] uses the standard `Authorization`
    /// header; `ApiKeyHeader` uses its caller-supplied custom name (e.g.
    /// `X-API-Key`). Returns an [`auth_invalid`] error if a custom header name
    /// is not a syntactically valid HTTP header.
    pub fn auth_header_name(&self) -> Result<HeaderName, McpError> {
        match self {
            Self::ApiKeyHeader { header_name, .. } => {
                HeaderName::from_bytes(header_name.as_bytes())
                    .map_err(|_| auth_invalid(format!("Invalid auth header name: {header_name:?}")))
            }
            _ => Ok(AUTHORIZATION),
        }
    }

    /// `Authorization: Basic <base64>` header value.
    ///
    /// Retained as a thin, back-compatible wrapper over [`Self::auth_header`]
    /// for the two Basic variants (and existing tests). Prefer
    /// [`Self::auth_header`] in new code — it is scheme-agnostic.
    pub fn basic_auth_header(&self) -> String {
        self.auth_header()
    }

    /// Base64-encoded `user:secret` payload without the `Basic ` prefix.
    pub fn basic_auth_payload(&self) -> String {
        let raw = match self {
            Self::AtlassianApiToken { email, token } => format!("{email}:{token}"),
            Self::BitbucketAppPassword { username, password } => {
                format!("{username}:{password}")
            }
            // Bearer and ApiKeyHeader carry an opaque token/key, not a
            // `user:secret` pair, and are routed through `auth_header()` above.
            // These branches keep the match total; they are unreachable for
            // those variants in practice.
            Self::Bearer { .. } | Self::ApiKeyHeader { .. } => String::new(),
        };
        STANDARD.encode(raw.as_bytes())
    }

    /// Identifier part, useful for log lines without leaking the secret.
    /// Bearer tokens have no principal, so a neutral `"bearer"` label is
    /// returned rather than any part of the token.
    pub fn principal(&self) -> &str {
        match self {
            Self::AtlassianApiToken { email, .. } => email,
            Self::BitbucketAppPassword { username, .. } => username,
            Self::Bearer { .. } => "bearer",
            Self::ApiKeyHeader { .. } => "api-key",
        }
    }
}

/// Resolve one `(principal, secret)` pair from config + keychain for a vendor
/// that owns its own credential lifecycle.
///
/// This is [`try_resolve_kind`] under a public name: vendors outside the
/// shared Atlassian resolver (`NinjaOne`'s console login, for one) need the same
/// `"keychain"` sentinel semantics — explicit sentinel misses are hard errors,
/// an absent key falls back to the keychain silently — and reimplementing them
/// per vendor is how those semantics drift apart.
///
/// Backend-injectable for tests; production callers want
/// [`resolve_secret_async`].
pub fn resolve_secret_for(
    config: &Config,
    backend: &dyn KeychainBackend,
    vendor: &str,
    kind: SecretKind,
    principal_key: &str,
    secret_key: &str,
) -> Result<Option<(String, String)>, McpError> {
    try_resolve_kind(config, backend, vendor, kind, principal_key, secret_key)
}

/// Resolve a registered vendor secret from config + keychain.
///
/// Looks the key up in [`secrets::VENDOR_SECRETS`] to learn its kind and which
/// config key (if any) names the principal, then applies the same sentinel
/// rules as every other credential. An unregistered key is read as plaintext
/// only — a key nobody registered has no slot to expand from, and silently
/// inventing one would file secrets where the runtime never looks.
pub fn vendor_secret_with(
    config: &Config,
    backend: &dyn KeychainBackend,
    vendor: &str,
    secret_key: &str,
) -> Result<Option<String>, McpError> {
    let Some(registered) = secrets::lookup(vendor, secret_key) else {
        return Ok(non_blank(config, vendor, secret_key).map(str::to_owned));
    };
    let configured = registered
        .principal_key
        .and_then(|key| non_blank(config, vendor, key));
    let principal = registered.principal(configured);
    resolve_kind_with_principal(
        config,
        backend,
        vendor,
        registered.kind,
        principal,
        secret_key,
    )
    .map(|resolved| resolved.map(|(_, secret)| secret))
}

/// Async form of [`vendor_secret_with`] against the process-wide OS keychain.
/// This is what vendor credential lookups call on the request path.
pub async fn vendor_secret(
    config: &Config,
    vendor: &'static str,
    secret_key: &'static str,
) -> Result<Option<String>, McpError> {
    // Plaintext short-circuits before any thread dispatch: it is the common
    // case, needs no I/O, and paying a `spawn_blocking` round-trip per tool
    // call to read a string out of a map would be pure overhead. Only a
    // sentinel or an absent key — the two cases that actually reach the OS
    // keychain — go to the blocking pool.
    if let Some(value) = non_blank(config, vendor, secret_key)
        && value != "keychain"
    {
        return Ok(Some(value.to_owned()));
    }

    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        vendor_secret_with(&config, os_keychain(), vendor, secret_key)
    })
    .await
    .map_err(|error| {
        crate::error::unexpected(
            format!("credential resolution task panicked: {error}"),
            None,
        )
    })?
}

fn non_blank<'a>(config: &'a Config, vendor: &str, key: &str) -> Option<&'a str> {
    config
        .get_for(vendor, key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Async wrapper around [`resolve_secret_for`] using the process-wide OS
/// keychain.
///
/// The read happens on a blocking thread for the same reason
/// [`Credentials::require_for_async`] does it: the platform backends are
/// synchronous and can block on a macOS ACL prompt or a Linux D-Bus
/// round-trip, which would stall a Tokio worker.
pub async fn resolve_secret_async(
    config: &Config,
    vendor: &str,
    kind: SecretKind,
    principal_key: &'static str,
    secret_key: &'static str,
) -> Result<Option<(String, String)>, McpError> {
    let config = config.clone();
    let vendor = vendor.to_owned();
    tokio::task::spawn_blocking(move || {
        resolve_secret_for(
            &config,
            os_keychain(),
            &vendor,
            kind,
            principal_key,
            secret_key,
        )
    })
    .await
    .map_err(|error| {
        crate::error::unexpected(
            format!("credential resolution task panicked: {error}"),
            None,
        )
    })?
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
            if config.get_for(vendor, secret_key).map(str::trim) == Some("keychain") {
                return Err(auth_missing(format!(
                    "vendor `{vendor}` sets {secret_key}=\"keychain\" but \
                     {principal_key} is missing"
                )));
            }
            return Ok(None);
        }
    };
    resolve_kind_with_principal(config, backend, vendor, kind, principal, secret_key)
}

/// The sentinel/plaintext/implicit cascade for an already-known principal.
///
/// Split out of [`try_resolve_kind`] because vendor tokens have no principal
/// config key to read — their principal is the secret's own key name (see
/// [`secrets`]) — while the resolution rules below must stay identical for
/// every credential in the process.
fn resolve_kind_with_principal(
    config: &Config,
    backend: &dyn KeychainBackend,
    vendor: &str,
    kind: SecretKind,
    principal: &str,
    secret_key: &str,
) -> Result<Option<(String, String)>, McpError> {
    resolve_configured_secret(
        backend,
        kind,
        vendor,
        principal,
        config.get_for(vendor, secret_key),
        secret_key,
    )
    .map(|secret| secret.map(|secret| (principal.to_owned(), secret)))
}

/// The same cascade for a secret whose raw value the caller already holds,
/// rather than one addressed by a config key.
///
/// `raw` is the configured value (`None` when it is absent altogether), and
/// `secret_label` is what error messages call it. For a plain config key those
/// are `config.get_for(...)` and the key name; for a credential carried
/// *inside* a config value — a per-server `NINJAONE_SERVERS` entry, say — they
/// are the field's value and a path like `NINJAONE_SERVERS["qa4-1"].password`.
/// Keeping one implementation is the point: a nested credential gets the same
/// `"keychain"` semantics as a top-level one instead of a second dialect.
pub fn resolve_configured_secret(
    backend: &dyn KeychainBackend,
    kind: SecretKind,
    vendor: &str,
    principal: &str,
    raw: Option<&str>,
    secret_label: &str,
) -> Result<Option<String>, McpError> {
    // Trim before matching. Surrounding whitespace on a secret is never
    // intentional — it is a copy-paste artefact — and a whitespace-only value
    // must read as "not set" rather than as a credential made of spaces.
    match raw.map(str::trim) {
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
                Ok(Some(s))
            }
            Ok(_) => {
                tracing::error!(
                    kind = %kind,
                    vendor = vendor,
                    principal = principal,
                    "vendor `{vendor}` sets {secret_label}=\"keychain\" but no entry exists"
                );
                Err(auth_missing(format!(
                    "vendor `{vendor}` sets {secret_label}=\"keychain\" but no keychain \
                     entry exists for kind={kind}, vendor={vendor}, principal={principal}. \
                     Run `mcp-devtools creds set --kind {kind} --vendor {vendor} \
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
        Some(s) if !s.is_empty() => Ok(Some(s.to_owned())),
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
                Ok(Some(s))
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

/// Async form of [`resolve_configured_secret`] against the process-wide OS
/// keychain, for the request path.
///
/// Plaintext short-circuits before any thread dispatch, exactly as
/// [`vendor_secret`] does: only a sentinel or an absent value actually reaches
/// the OS keychain, and those are the only cases worth a `spawn_blocking`.
pub async fn resolve_configured_secret_async(
    kind: SecretKind,
    vendor: &'static str,
    principal: String,
    raw: Option<String>,
    secret_label: String,
) -> Result<Option<String>, McpError> {
    match raw.as_deref().map(str::trim) {
        // Only a sentinel or an absent value needs the keychain; everything
        // else is decided here, off the blocking pool.
        Some("keychain") | None => {}
        Some("") => return Ok(None),
        Some(secret) => return Ok(Some(secret.to_owned())),
    }

    tokio::task::spawn_blocking(move || {
        resolve_configured_secret(
            os_keychain(),
            kind,
            vendor,
            &principal,
            raw.as_deref(),
            &secret_label,
        )
    })
    .await
    .map_err(|error| {
        crate::error::unexpected(
            format!("credential resolution task panicked: {error}"),
            None,
        )
    })?
}
