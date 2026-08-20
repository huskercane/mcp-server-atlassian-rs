#![allow(clippy::doc_markdown)]

//! NinjaOne API vendor implementation.
//!
//! A tool call selects either the single `NINJAONE_URL` or an alias from the
//! JSON object in `NINJAONE_SERVERS`. The tool never accepts a raw base URL;
//! this prevents an MCP caller from redirecting the server-held credential to
//! an arbitrary host.
//!
//! Authentication has two shapes. A static credential (`NINJAONE_ACCESS_TOKEN`,
//! `NINJAONE_SESSION_KEY`, `NINJAONE_SESSION_COOKIE`) is read straight from
//! config. Alternatively `NINJAONE_EMAIL` + `NINJAONE_PASSWORD` let the server
//! mint its own console session key through the login exchange in [`session`],
//! which is what the `ninjaone_login` tool drives.

pub mod error;
pub mod mfa;
pub mod session;
pub mod totp;

use std::sync::Arc;

use reqwest::{Client, StatusCode};
use serde_json::Value;

use crate::auth::{Credentials, SecretKind, resolve_secret_async, vendor_secret};
use crate::config::{Config, VENDOR_NINJAONE};
use crate::error::{McpError, auth_missing, unexpected};
use crate::vendor::Vendor;
use session::{LoginOutcome, LoginRequest, SessionCache};
use totp::TotpSpec;

/// Name of the cookie the console issues on `mfa-login`
/// (`Set-Cookie: sessionKey=…;Path=/;Secure;HttpOnly;SameSite=Lax`) and reads
/// back on every private `/ws/...` call. A minted session is replayed in this
/// exact form, so the server sees what the browser sends.
const SESSION_COOKIE_NAME: &str = "sessionKey";

/// Legacy carrier for a hand-copied `NINJAONE_SESSION_KEY`: the same value in
/// a bare header rather than a cookie. Retained because it is the documented,
/// already-configured shape; new sessions use the cookie.
const SESSION_KEY_HEADER: &str = "sessionKey";

/// Where the credential a request is about to use came from. Only the
/// controller needs this, and only to decide what a `401` means: a key minted
/// by [`session`] can be evicted and re-minted, a static one cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    AccessToken,
    LoginSession,
    StaticSessionKey,
    SessionCookie,
}

/// A resolved credential plus its provenance.
#[derive(Debug, Clone)]
pub struct ResolvedAuth {
    pub credentials: Credentials,
    pub source: AuthSource,
}

#[derive(Debug, Clone, Default)]
pub struct NinjaOneVendor {
    base_url_override: Option<String>,
    server_alias: Option<String>,
    /// Session keys minted by [`SessionCache::login`], shared across every
    /// clone of this vendor (including the per-request `for_server` ones) so a
    /// single `ninjaone_login` covers all later calls.
    sessions: Arc<SessionCache>,
}

impl NinjaOneVendor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
            server_alias: None,
            sessions: Arc::new(SessionCache::new()),
        }
    }

    /// Return a request-scoped vendor selecting a configured server alias.
    #[must_use]
    pub fn for_server(&self, server_alias: Option<&str>) -> Self {
        Self {
            base_url_override: self.base_url_override.clone(),
            server_alias: server_alias.map(str::to_owned),
            sessions: Arc::clone(&self.sessions),
        }
    }

    /// Resolve a **statically configured** authentication carrier. Public API
    /// access tokens take precedence, followed by API session keys and an
    /// explicit browser cookie for the private `/ws/...` endpoints.
    ///
    /// This is the config-only view: it never consults the login session
    /// cache, so it stays synchronous for diagnostics and tests. The request
    /// path uses [`resolve_auth`](Self::resolve_auth).
    pub fn credentials(&self, config: &Config) -> Result<Credentials, McpError> {
        Self::static_credentials(config)
            .map(|resolved| resolved.credentials)
            .ok_or_else(|| missing_auth_error(config))
    }

    /// Config-only view of the three carrier credentials. Synchronous, so it
    /// cannot consult the keychain; [`static_credentials_async`] is the
    /// request-path form.
    fn static_credentials(config: &Config) -> Option<ResolvedAuth> {
        if let Some(token) = non_blank(config, "NINJAONE_ACCESS_TOKEN") {
            return Some(carrier(AuthSource::AccessToken, token));
        }
        if let Some(key) = non_blank(config, "NINJAONE_SESSION_KEY") {
            return Some(carrier(AuthSource::StaticSessionKey, key));
        }
        non_blank(config, "NINJAONE_SESSION_COOKIE")
            .map(|cookie| carrier(AuthSource::SessionCookie, cookie))
    }

    /// Keychain-aware form of [`static_credentials`], in the same priority
    /// order. Each key is registered, so `"keychain"` expands for all three.
    async fn static_credentials_async(config: &Config) -> Result<Option<ResolvedAuth>, McpError> {
        for (source, key) in [
            (AuthSource::AccessToken, "NINJAONE_ACCESS_TOKEN"),
            (AuthSource::StaticSessionKey, "NINJAONE_SESSION_KEY"),
            (AuthSource::SessionCookie, "NINJAONE_SESSION_COOKIE"),
        ] {
            if let Some(value) = vendor_secret(config, VENDOR_NINJAONE, key).await? {
                return Ok(Some(carrier(source, &value)));
            }
        }
        Ok(None)
    }

    /// Request-path credential resolution, including the in-memory session
    /// key minted by [`login`](Self::login).
    ///
    /// A minted key outranks `NINJAONE_SESSION_KEY`: an operator who just ran
    /// `ninjaone_login` means to use that session, and a stale hand-copied key
    /// left in config must not silently win. `NINJAONE_ACCESS_TOKEN` still
    /// comes first — it targets the public `/v2/...` API, a different surface
    /// from console sessions.
    pub async fn resolve_auth(&self, config: &Config) -> Result<ResolvedAuth, McpError> {
        if let Some(token) = vendor_secret(config, VENDOR_NINJAONE, "NINJAONE_ACCESS_TOKEN").await?
        {
            return Ok(carrier(AuthSource::AccessToken, &token));
        }
        if let Some(key) = self.cached_session_key(config).await {
            return Ok(ResolvedAuth {
                credentials: session_cookie_credentials(&key),
                source: AuthSource::LoginSession,
            });
        }
        Self::static_credentials_async(config)
            .await?
            .ok_or_else(|| missing_auth_error(config))
    }

    /// Perform the console login exchange for the configured principal and
    /// cache the resulting session key against this vendor's base URL.
    ///
    /// `mfa_code` is the one-time code for an MFA account; it is the only
    /// caller-supplied input, and only because it is short-lived and useless
    /// on its own. Email and password always come from server-held config.
    pub async fn login(
        &self,
        client: &Client,
        config: &Config,
        mfa_code: Option<&str>,
        recaptcha_token: Option<&str>,
    ) -> Result<LoginOutcome, McpError> {
        let base_url = self.base_url(config)?;
        // Password comes through the shared keychain-aware resolver, so
        // NINJAONE_PASSWORD="keychain" behaves exactly like the Atlassian keys.
        let (email, password) = resolve_secret_async(
            config,
            VENDOR_NINJAONE,
            SecretKind::Password,
            "NINJAONE_EMAIL",
            "NINJAONE_PASSWORD",
        )
        .await?
        .ok_or_else(missing_login_creds)?;
        // Resolved before the exchange starts: a vault CLI can take seconds,
        // and a TOTP code is only valid for a 30-second window, so fetching it
        // late would race its own expiry.
        let code = self.mfa_code(config, mfa_code).await?;
        self.sessions
            .login(
                client,
                LoginRequest {
                    base_url: &base_url,
                    email: &email,
                    password: &password,
                    mfa_code: code.as_deref(),
                    recaptcha_token,
                },
            )
            .await
    }

    /// Drop the cached session key for this vendor's base URL. Called after a
    /// `401` so the stale key is not replayed on the next tool call.
    pub async fn invalidate_session(&self, config: &Config) {
        if let (Ok(base_url), Some(email)) =
            (self.base_url(config), non_blank(config, "NINJAONE_EMAIL"))
        {
            self.sessions.invalidate(&base_url, email).await;
        }
    }

    async fn cached_session_key(&self, config: &Config) -> Option<String> {
        let email = non_blank(config, "NINJAONE_EMAIL")?;
        let base_url = self.base_url(config).ok()?;
        self.sessions.get(&base_url, email).await
    }

    fn configured_alias_url(config: &Config, alias: &str) -> Result<String, McpError> {
        ServerEntry::parse(config, alias)?.base_url()
    }

    /// Resolve the one-time code for a login, in priority order: the explicit
    /// tool argument, then a configured vault command, then a configured TOTP
    /// seed.
    ///
    /// Each source is consulted only if the ones before it produced nothing.
    /// That laziness is deliberate for the seed: its keychain fallback would
    /// otherwise fire on every login even for operators who use a command,
    /// which on macOS can mean a spurious authorization prompt.
    ///
    /// A per-alias `"totpCommand"` / `"totpSecret"` wins over the top-level
    /// `NINJAONE_TOTP_COMMAND` / `NINJAONE_TOTP_SECRET`: one person routinely
    /// holds several NinjaOne accounts with different roles across
    /// environments, and each needs its own entry.
    async fn mfa_code(
        &self,
        config: &Config,
        explicit: Option<&str>,
    ) -> Result<Option<String>, McpError> {
        if let Some(code) = explicit.map(str::trim).filter(|code| !code.is_empty()) {
            return Ok(Some(code.to_owned()));
        }

        let entry = match self.server_alias.as_deref() {
            Some(alias) => ServerEntry::parse(config, alias)?,
            None => ServerEntry::default(),
        };

        if let Some(command) = entry
            .totp_command
            .or_else(|| non_blank(config, "NINJAONE_TOTP_COMMAND").map(str::to_owned))
        {
            return mfa::run_command(&command).await.map(Some);
        }

        let secret = match entry.totp_secret {
            Some(secret) => Some(secret),
            None => resolve_secret_async(
                config,
                VENDOR_NINJAONE,
                SecretKind::TotpSecret,
                "NINJAONE_EMAIL",
                "NINJAONE_TOTP_SECRET",
            )
            .await?
            .map(|(_, secret)| secret),
        };

        secret
            .map(|secret| TotpSpec::parse(&secret)?.current_code())
            .transpose()
    }
}

/// One entry of the `NINJAONE_SERVERS` map: either a bare URL string, or an
/// object carrying the URL plus optional per-server settings.
#[derive(Debug, Default)]
struct ServerEntry {
    url: String,
    prefix: String,
    totp_command: Option<String>,
    totp_secret: Option<String>,
}

impl ServerEntry {
    fn parse(config: &Config, alias: &str) -> Result<Self, McpError> {
        let raw = non_blank(config, "NINJAONE_SERVERS").ok_or_else(|| {
            auth_missing(format!(
                "NinjaOne server alias `{alias}` was requested, but NINJAONE_SERVERS is not configured"
            ))
        })?;
        let parsed: Value = serde_json::from_str(raw).map_err(|error| {
            unexpected(
                format!(
                    "NINJAONE_SERVERS must be a JSON object whose values are URLs or server objects: {error}"
                ),
                None,
            )
        })?;
        let entry = parsed
            .as_object()
            .and_then(|servers| servers.get(alias))
            .ok_or_else(|| {
                auth_missing(format!(
                    "Unknown NinjaOne server alias `{alias}`. Add it to NINJAONE_SERVERS."
                ))
            })?;

        match entry {
            Value::String(url) if !url.trim().is_empty() => Ok(Self {
                url: url.trim().to_owned(),
                ..Self::default()
            }),
            Value::Object(server) => {
                let url = server
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .ok_or_else(|| invalid_server_entry(alias))?;
                Ok(Self {
                    url: url.to_owned(),
                    prefix: string_field(server, "prefix", alias)?.unwrap_or_default(),
                    totp_command: string_field(server, "totpCommand", alias)?,
                    totp_secret: string_field(server, "totpSecret", alias)?,
                })
            }
            _ => Err(invalid_server_entry(alias)),
        }
    }

    fn base_url(&self) -> Result<String, McpError> {
        append_prefix(&self.url, &self.prefix)
    }
}

/// Read an optional string field, rejecting a non-string with the same
/// "malformed entry" error the URL and prefix use. A blank value is treated as
/// absent so an empty placeholder in config does not become an empty command.
fn string_field(
    server: &serde_json::Map<String, Value>,
    field: &str,
    alias: &str,
) -> Result<Option<String>, McpError> {
    server
        .get(field)
        .map(|value| value.as_str().ok_or_else(|| invalid_server_entry(alias)))
        .transpose()
        .map(|value| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

/// Build the [`ResolvedAuth`] for one of the three carrier credentials. The
/// header each rides in is a property of the source, so keeping the mapping in
/// one place stops the sync and async paths from disagreeing.
fn carrier(source: AuthSource, value: &str) -> ResolvedAuth {
    let credentials = match source {
        AuthSource::AccessToken => Credentials::Bearer {
            token: value.to_owned(),
        },
        AuthSource::StaticSessionKey => session_key_credentials(value),
        AuthSource::SessionCookie | AuthSource::LoginSession => Credentials::ApiKeyHeader {
            header_name: "Cookie".to_owned(),
            key: value.to_owned(),
        },
    };
    ResolvedAuth {
        credentials,
        source,
    }
}

fn session_key_credentials(key: &str) -> Credentials {
    Credentials::ApiKeyHeader {
        header_name: SESSION_KEY_HEADER.to_owned(),
        key: key.to_owned(),
    }
}

/// Replay a minted session key the way the browser does: as the `sessionKey`
/// cookie. The console sets it `HttpOnly` on `mfa-login` and reads it from the
/// `Cookie` header on every subsequent `/ws/...` request.
fn session_cookie_credentials(key: &str) -> Credentials {
    Credentials::ApiKeyHeader {
        header_name: "Cookie".to_owned(),
        key: format!("{SESSION_COOKIE_NAME}={key}"),
    }
}

/// Actionable "no credentials" error. The wording branches on whether login
/// credentials are configured: with them the fix is to run `ninjaone_login`
/// (or re-run it after the session expired), without them it is to set one of
/// the static keys.
fn missing_auth_error(config: &Config) -> McpError {
    if non_blank(config, "NINJAONE_EMAIL").is_some()
        && non_blank(config, "NINJAONE_PASSWORD").is_some()
    {
        auth_missing(
            "No NinjaOne session is active. Call the ninjaone_login tool (with the current \
             multi-factor code, if the account uses MFA) to mint a session key for this \
             process, or set NINJAONE_ACCESS_TOKEN / NINJAONE_SESSION_KEY.",
        )
    } else {
        auth_missing(
            "NinjaOne authentication is required for ninjaone_* tools. Set NINJAONE_EMAIL + \
             NINJAONE_PASSWORD and call ninjaone_login, or set one of NINJAONE_ACCESS_TOKEN, \
             NINJAONE_SESSION_KEY, or NINJAONE_SESSION_COOKIE under the `ninjaone` section of \
             ~/.mcp/configs.json or in the environment.",
        )
    }
}

fn missing_login_creds() -> McpError {
    auth_missing(
        "NINJAONE_EMAIL + NINJAONE_PASSWORD are required to log in to NinjaOne. Set them under \
         the `ninjaone` section of ~/.mcp/configs.json or in the environment, or store the \
         password in the OS keychain (`mcp-atlassian creds set --kind password --vendor \
         ninjaone --principal <email>`) and set NINJAONE_PASSWORD=\"keychain\".",
    )
}

fn non_blank<'a>(config: &'a Config, key: &str) -> Option<&'a str> {
    config
        .get_for(VENDOR_NINJAONE, key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn invalid_server_entry(alias: &str) -> McpError {
    unexpected(
        format!(
            "NINJAONE_SERVERS entry `{alias}` must be a URL string or an object with a non-empty `url` and optional `prefix` string"
        ),
        None,
    )
}

fn append_prefix(url: &str, prefix: &str) -> Result<String, McpError> {
    let prefix = prefix.trim();
    if prefix.is_empty() || prefix == "/" {
        return Ok(url.to_owned());
    }
    if !prefix.starts_with('/') || prefix.contains(['?', '#']) {
        return Err(unexpected(
            "NinjaOne server prefix must be an absolute path beginning with `/` and must not include a query string or fragment",
            None,
        ));
    }
    Ok(format!(
        "{}{}",
        url.trim_end_matches('/'),
        prefix.trim_end_matches('/')
    ))
}

impl Vendor for NinjaOneVendor {
    fn name(&self) -> &'static str {
        VENDOR_NINJAONE
    }

    fn base_url(&self, config: &Config) -> Result<String, McpError> {
        let url = if let Some(base) = &self.base_url_override {
            base.clone()
        } else if let Some(alias) = self.server_alias.as_deref() {
            Self::configured_alias_url(config, alias)?
        } else {
            non_blank(config, "NINJAONE_URL")
                .map(str::to_owned)
                .ok_or_else(|| {
                    auth_missing(
                        "NINJAONE_URL is required when no NinjaOne server alias is supplied. \
                         Set it under the `ninjaone` section of ~/.mcp/configs.json or in the \
                         environment.",
                    )
                })?
        };
        validate_base_url(&url)?;
        Ok(url.trim_end_matches('/').to_owned())
    }

    fn normalize_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        }
    }

    fn classify_error(&self, status: StatusCode, body: &str) -> McpError {
        error::classify(status, body)
    }
}

fn validate_base_url(url: &str) -> Result<(), McpError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| unexpected(format!("Invalid NinjaOne base URL: {error}"), None))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(unexpected(
            "NinjaOne base URL must be an absolute http:// or https:// URL",
            None,
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(unexpected(
            "NinjaOne base URL must not include a query string or fragment",
            None,
        ));
    }
    Ok(())
}
