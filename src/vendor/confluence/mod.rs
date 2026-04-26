//! Confluence Cloud vendor implementation.
//!
//! - Base URL is derived from `ATLASSIAN_SITE_NAME` (resolved per-request,
//!   never at server construction). Tests can bypass the env lookup with
//!   [`ConfluenceVendor::with_base_url`].
//! - Path normalisation only ensures a leading `/`; Confluence callers pass
//!   the full path (e.g. `/wiki/api/v2/spaces`, `/wiki/rest/api/search`).
//! - Error parsing handles the canonical Confluence v2 envelope plus the
//!   legacy `message` / `errorMessages` / `errors[]` shapes (see [`error`]).

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_CONFLUENCE, VENDOR_JIRA};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

/// Confluence Cloud [`Vendor`] strategy.
///
/// Two construction paths:
/// - [`ConfluenceVendor::new`] (production) defers base-URL resolution to
///   [`Vendor::base_url`], which reads `ATLASSIAN_SITE_NAME` from the
///   per-request [`Config`]. Missing site name surfaces as a tool-call
///   error, never a startup failure.
/// - [`ConfluenceVendor::with_base_url`] (tests) pins an absolute URL —
///   typically a wiremock — and skips the env lookup entirely.
#[derive(Debug, Clone, Default)]
pub struct ConfluenceVendor {
    base_url_override: Option<String>,
}

impl ConfluenceVendor {
    /// New vendor that derives its base URL from `ATLASSIAN_SITE_NAME` at
    /// request time. Construction itself is infallible.
    pub fn new() -> Self {
        Self {
            base_url_override: None,
        }
    }

    /// New vendor pinned to a caller-supplied base URL. Used by tests to
    /// point the same vendor at a wiremock without touching the
    /// [`Config`].
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
        }
    }
}

impl Vendor for ConfluenceVendor {
    fn name(&self) -> &'static str {
        VENDOR_CONFLUENCE
    }

    /// Resolve the absolute base URL. When constructed via
    /// [`Self::with_base_url`], returns the pinned override. Otherwise
    /// looks up `ATLASSIAN_SITE_NAME` (vendor-scoped to `confluence`,
    /// with fallback to the shared overlay) and builds
    /// `https://{site}.atlassian.net`. An empty or missing value surfaces
    /// as [`crate::error::auth_missing`] so the user sees a clear
    /// configuration error at tool-call time.
    fn base_url(&self, config: &Config) -> Result<String, McpError> {
        if let Some(base) = &self.base_url_override {
            return Ok(base.clone());
        }
        // Site name is shared with Jira (same Atlassian site), so a user
        // with `ATLASSIAN_SITE_NAME` only under the `jira` section still
        // gets a working `conf_*` surface. The fallback is explicit to
        // keep unrelated vendors (Bitbucket) out of the lookup.
        let raw = config
            .get_for_with_fallback(VENDOR_CONFLUENCE, &[VENDOR_JIRA], "ATLASSIAN_SITE_NAME")
            .ok_or_else(|| {
                auth_missing(
                    "ATLASSIAN_SITE_NAME is required for conf_* tools. Set the env var \
                     (e.g. `mycompany` for mycompany.atlassian.net) or add it under the \
                     `confluence` (or `jira`) section of ~/.mcp/configs.json.",
                )
            })?;
        let site = raw.trim();
        if site.is_empty() {
            return Err(auth_missing("ATLASSIAN_SITE_NAME is set but empty."));
        }
        Ok(format!("https://{site}.atlassian.net"))
    }

    /// Confluence paths pass through verbatim — callers supply the full
    /// `/wiki/api/v2/...` or `/wiki/rest/api/...` path. We only ensure a
    /// leading `/`.
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
