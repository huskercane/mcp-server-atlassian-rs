#![allow(clippy::doc_markdown)]

//! CircleCI v2 vendor implementation.
//!
//! Like Zoom, CircleCI departs from the Atlassian vendors in how it
//! authenticates: it does **not** share the `ATLASSIAN_*` credential model.
//! CircleCI uses a single personal API token (`CIRCLECI_TOKEN`) which its v2
//! API documents as best sent via `Authorization: Bearer <token>`. There is
//! no token exchange — unlike Zoom, the token is used verbatim — so credential
//! *lookup* is a plain config read on [`CircleCiVendor::token`] rather than
//! anything in the shared [`Credentials`](crate::auth::Credentials) resolver.
//!
//! Everything else is conventional:
//! - **Base URL** — fixed at `https://circleci.com/api/v2` (overridable for
//!   tests).
//! - **Path normalisation** — verbatim (callers pass
//!   `/project/{slug}/pipeline`), only ensuring a leading `/`, like the Jira
//!   and Zoom vendors.
//! - **Error parsing** — the `{message}` / `{error}` REST envelope (see
//!   [`error`]).

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_CIRCLECI};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

/// Production CircleCI REST base. Tests point [`CircleCiVendor::with_base_url`]
/// at a wiremock instead.
const DEFAULT_API_BASE: &str = "https://circleci.com/api/v2";

/// CircleCI v2 [`Vendor`] strategy.
///
/// Cheap to clone: it holds only an optional base-URL override. Unlike
/// [`ZoomVendor`](crate::vendor::zoom::ZoomVendor) there is no token cache —
/// CircleCI's API token is static and read from config per request.
#[derive(Debug, Clone, Default)]
pub struct CircleCiVendor {
    /// Optional API base override (tests → wiremock). `None` uses
    /// [`DEFAULT_API_BASE`].
    base_url_override: Option<String>,
}

impl CircleCiVendor {
    /// Production constructor. Uses the real CircleCI API host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the API base (tests → wiremock).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
        }
    }

    /// Resolve the personal API token from the `circleci` config section. This
    /// is the CircleCI credential entry point — the shared Atlassian resolver
    /// is never consulted. Errors with a clear, actionable message at
    /// tool-call time when the token is absent.
    pub fn token(&self, config: &Config) -> Result<String, McpError> {
        config
            .get_for(VENDOR_CIRCLECI, "CIRCLECI_TOKEN")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "CIRCLECI_TOKEN is required for circleci_* tools. Set a CircleCI \
                     personal API token under the `circleci` section of \
                     ~/.mcp/configs.json or in the environment.",
                )
            })
    }
}

impl Vendor for CircleCiVendor {
    fn name(&self) -> &'static str {
        VENDOR_CIRCLECI
    }

    /// Resolve the API base. Independent of [`Config`] — CircleCI's base is
    /// fixed (or test-overridden); credentials, not the URL, come from config.
    fn base_url(&self, _config: &Config) -> Result<String, McpError> {
        Ok(self
            .base_url_override
            .clone()
            .unwrap_or_else(|| DEFAULT_API_BASE.to_owned()))
    }

    /// Verbatim passthrough — callers supply the full `/project/...` path. We
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
