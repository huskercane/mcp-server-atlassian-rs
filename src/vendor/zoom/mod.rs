//! Zoom Cloud vendor implementation.
//!
//! Zoom departs from the Atlassian vendors in one way that matters: it does
//! **not** share the `ATLASSIAN_*` credential model. It authenticates with
//! Server-to-Server OAuth — static client credentials
//! (`ZOOM_ACCOUNT_ID` + `ZOOM_CLIENT_ID` + `ZOOM_CLIENT_SECRET`) exchanged for
//! a short-lived bearer that this vendor caches and auto-renews (see
//! [`token`]). Credential *lookup* lives on [`ZoomVendor::bearer`] rather than
//! in the shared [`Credentials`](crate::auth::Credentials) resolver, so the
//! Atlassian auth path stays free of OAuth concerns.
//!
//! Everything else is conventional:
//! - **Base URL** — fixed at `https://api.zoom.us/v2` (overridable for tests).
//! - **Path normalisation** — verbatim (callers pass `/users/me/meetings`),
//!   only ensuring a leading `/`, like the Jira vendor.
//! - **Error parsing** — the `{code, message}` REST envelope (see [`error`]).

pub mod error;
pub mod token;

use std::sync::Arc;

use reqwest::{Client, StatusCode};

use crate::config::{Config, VENDOR_ZOOM};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;
use token::TokenCache;

/// Production Zoom REST base. Tests point [`ZoomVendor::with_urls`] at a
/// wiremock instead.
const DEFAULT_API_BASE: &str = "https://api.zoom.us/v2";

/// Production Zoom OAuth token endpoint. Note this is the `zoom.us` host, not
/// `api.zoom.us` — the token exchange does not go through the API base URL.
const DEFAULT_TOKEN_URL: &str = "https://zoom.us/oauth/token";

/// Zoom Cloud [`Vendor`] strategy.
///
/// Cheap to clone: the token cache is shared via [`Arc`] so all clones of a
/// vendor instance (e.g. the one held in `ServerState` and any borrow of it)
/// see the same cached bearer.
#[derive(Debug, Clone, Default)]
pub struct ZoomVendor {
    /// Optional API base override (tests → wiremock). `None` uses
    /// [`DEFAULT_API_BASE`].
    base_url_override: Option<String>,
    /// Optional OAuth token-endpoint override (tests → wiremock). `None` uses
    /// [`DEFAULT_TOKEN_URL`]. Separate from the API base so token-exchange
    /// tests need no network and no global state.
    token_url_override: Option<String>,
    /// Per-instance token cache. Shared across clones via `Arc`.
    cache: Arc<TokenCache>,
}

impl ZoomVendor {
    /// Production constructor. Uses the real Zoom API + OAuth hosts and a
    /// fresh, empty token cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override only the API base (tests). The OAuth host stays the real one
    /// — most tests should prefer [`Self::with_urls`].
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
            ..Self::default()
        }
    }

    /// Override both the API base and the OAuth token endpoint (tests). Lets a
    /// wiremock stand in for the entire Zoom surface with zero network access.
    pub fn with_urls(base_url: impl Into<String>, token_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
            token_url_override: Some(token_url.into()),
            cache: Arc::new(TokenCache::new()),
        }
    }

    fn token_url(&self) -> &str {
        self.token_url_override
            .as_deref()
            .unwrap_or(DEFAULT_TOKEN_URL)
    }

    /// Resolve a valid bearer for the configured Server-to-Server OAuth app,
    /// exchanging and caching as needed. This is the Zoom credential entry
    /// point — the shared Atlassian resolver is never consulted.
    pub async fn bearer(&self, client: &Client, config: &Config) -> Result<String, McpError> {
        let account_id = require_cred(config, "ZOOM_ACCOUNT_ID")?;
        let client_id = require_cred(config, "ZOOM_CLIENT_ID")?;
        let client_secret = require_cred(config, "ZOOM_CLIENT_SECRET")?;
        self.cache
            .bearer(
                client,
                &account_id,
                &client_id,
                &client_secret,
                self.token_url(),
            )
            .await
    }
}

/// Read a required Zoom credential from the `zoom` config section, erroring
/// with a clear, actionable message at tool-call time when it is absent.
fn require_cred(config: &Config, key: &str) -> Result<String, McpError> {
    config
        .get_for(VENDOR_ZOOM, key)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            auth_missing(format!(
                "{key} is required for zoom_* tools. Set the Server-to-Server OAuth \
                 credentials (ZOOM_ACCOUNT_ID + ZOOM_CLIENT_ID + ZOOM_CLIENT_SECRET) under \
                 the `zoom` section of ~/.mcp/configs.json or in the environment."
            ))
        })
}

impl Vendor for ZoomVendor {
    fn name(&self) -> &'static str {
        VENDOR_ZOOM
    }

    /// Resolve the API base. Independent of [`Config`] — Zoom's base is fixed
    /// (or test-overridden); credentials, not the URL, come from config.
    fn base_url(&self, _config: &Config) -> Result<String, McpError> {
        Ok(self
            .base_url_override
            .clone()
            .unwrap_or_else(|| DEFAULT_API_BASE.to_owned()))
    }

    /// Verbatim passthrough — callers supply the full `/users/...` path. We
    /// only ensure a leading `/`.
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
