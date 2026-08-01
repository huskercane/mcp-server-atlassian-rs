#![allow(clippy::doc_markdown)]

//! Splunk management REST API vendor implementation.

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_SPLUNK};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

pub const SEARCH_EXPORT_PATH: &str = "/services/search/v2/jobs/export";
pub const SEARCH_JOBS_PATH: &str = "/services/search/jobs";
pub const SAVED_SEARCHES_PATH: &str = "/services/saved/searches";

#[derive(Debug, Clone, Default)]
pub struct SplunkVendor {
    base_url_override: Option<String>,
}

impl SplunkVendor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
        }
    }

    pub fn token(&self, config: &Config) -> Result<String, McpError> {
        config
            .get_for(VENDOR_SPLUNK, "SPLUNK_TOKEN")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "SPLUNK_TOKEN is required for splunk_* tools. Set a Splunk \
                     authentication token under the `splunk` section of \
                     ~/.mcp/configs.json or in the environment.",
                )
            })
    }

    /// Modern Splunk authentication tokens are JWTs and use `Bearer`.
    /// `splunk` remains available for legacy session keys.
    pub fn auth_scheme<'a>(&self, config: &'a Config) -> &'a str {
        config
            .get_for(VENDOR_SPLUNK, "SPLUNK_AUTH_SCHEME")
            .map(str::trim)
            .filter(|value| value.eq_ignore_ascii_case("splunk"))
            .map_or("Bearer", |_| "Splunk")
    }
}

impl Vendor for SplunkVendor {
    fn name(&self) -> &'static str {
        VENDOR_SPLUNK
    }

    fn base_url(&self, config: &Config) -> Result<String, McpError> {
        if let Some(base) = &self.base_url_override {
            return Ok(base.trim_end_matches('/').to_owned());
        }
        let url = config
            .get_for(VENDOR_SPLUNK, "SPLUNK_URL")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                auth_missing(
                    "SPLUNK_URL is required for splunk_* tools. Set it to the Splunk \
                     management API base URL (for example https://splunk.example.com:8089) \
                     under the `splunk` section of ~/.mcp/configs.json or in the environment.",
                )
            })?;
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
