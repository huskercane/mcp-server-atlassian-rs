#![allow(clippy::doc_markdown)]

//! Postman API vendor implementation.
//!
//! Postman is the first vendor to authenticate with a **custom header** rather
//! than `Authorization`: it sends `X-API-Key: <POSTMAN_API_KEY>`. That is
//! carried by [`Credentials::ApiKeyHeader`](crate::auth::Credentials::ApiKeyHeader),
//! which the controller mints from config. Credential *lookup* is a plain
//! config read on [`PostmanVendor::key`]; the shared Atlassian resolver is
//! never consulted.
//!
//! Everything else is conventional:
//! - **Base URL** — fixed at `https://api.getpostman.com` (overridable for
//!   tests).
//! - **Path normalisation** — verbatim (callers pass `/collections`,
//!   `/workspaces`, `/me`), only ensuring a leading `/`.
//! - **Error parsing** — the nested `{"error": {"name", "message"}}` envelope
//!   (see [`error`]).

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_POSTMAN};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

/// HTTP header Postman expects the API key in.
pub const API_KEY_HEADER: &str = "X-API-Key";

/// Production Postman API base. Tests point [`PostmanVendor::with_base_url`] at
/// a wiremock instead.
const DEFAULT_API_BASE: &str = "https://api.getpostman.com";

/// Postman [`Vendor`] strategy.
///
/// Cheap to clone: it holds only an optional base-URL override. There is no
/// token cache — the API key is static and read from config per request.
#[derive(Debug, Clone, Default)]
pub struct PostmanVendor {
    /// Optional API base override (tests → wiremock). `None` uses
    /// [`DEFAULT_API_BASE`].
    base_url_override: Option<String>,
}

impl PostmanVendor {
    /// Production constructor. Uses the real Postman API host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the API base (tests → wiremock).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
        }
    }

    /// Resolve the API key from the `postman` config section. This is the
    /// Postman credential entry point. Errors with a clear, actionable message
    /// at tool-call time when the key is absent.
    pub fn key(&self, config: &Config) -> Result<String, McpError> {
        config
            .get_for(VENDOR_POSTMAN, "POSTMAN_API_KEY")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "POSTMAN_API_KEY is required for postman_* tools. Set a Postman API \
                     key under the `postman` section of ~/.mcp/configs.json or in the \
                     environment.",
                )
            })
    }
}

impl Vendor for PostmanVendor {
    fn name(&self) -> &'static str {
        VENDOR_POSTMAN
    }

    /// Resolve the API base. Independent of [`Config`] — Postman's base is fixed
    /// (or test-overridden); credentials, not the URL, come from config.
    fn base_url(&self, _config: &Config) -> Result<String, McpError> {
        Ok(self
            .base_url_override
            .clone()
            .unwrap_or_else(|| DEFAULT_API_BASE.to_owned()))
    }

    /// Verbatim passthrough — callers supply the full `/collections` path. We
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
