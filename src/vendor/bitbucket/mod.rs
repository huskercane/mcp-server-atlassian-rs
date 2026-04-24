//! Bitbucket Cloud vendor implementation.
//!
//! - Base URL is fixed at `https://api.bitbucket.org` (or a caller-supplied
//!   override via [`BitbucketVendor::with_base_url`] for tests pointing at a
//!   wiremock).
//! - Path normalisation prepends `/2.0` when the caller did not already
//!   namespace the path.
//! - Error parsing covers the four Bitbucket envelope shapes described in
//!   [`error`].

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_BITBUCKET};
use crate::error::McpError;
use crate::vendor::Vendor;

/// Default base URL for Bitbucket Cloud's REST API.
pub const DEFAULT_BASE_URL: &str = "https://api.bitbucket.org";

/// Bitbucket Cloud [`Vendor`] strategy. The base URL is captured at
/// construction so tests can point the same vendor at a local wiremock
/// without touching the [`Config`].
#[derive(Debug, Clone)]
pub struct BitbucketVendor {
    base_url: String,
}

impl BitbucketVendor {
    /// New vendor pointed at the production base URL.
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// New vendor pointed at a caller-supplied base URL. Trailing slashes
    /// are tolerated; the transport layer trims them.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for BitbucketVendor {
    fn default() -> Self {
        Self::new()
    }
}

impl Vendor for BitbucketVendor {
    fn name(&self) -> &'static str {
        VENDOR_BITBUCKET
    }

    /// Bitbucket's base URL is fixed at construction time, so this lookup
    /// is infallible. The `Config` parameter is unused (present only to
    /// satisfy the trait, which must allow vendors like Jira that resolve
    /// the URL from configuration).
    fn base_url(&self, _config: &Config) -> Result<String, McpError> {
        Ok(self.base_url.clone())
    }

    /// Bitbucket REST v2 prepends `/2.0` to every endpoint. Mirrors the TS
    /// `normalizePath` helper: ensures a leading `/` then prepends `/2.0`
    /// only when the caller did not already namespace the path.
    fn normalize_path(&self, path: &str) -> String {
        let mut out = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        if !out.starts_with("/2.0") {
            out = format!("/2.0{out}");
        }
        out
    }

    fn classify_error(&self, status: StatusCode, body: &str) -> McpError {
        error::classify(status, body)
    }
}
