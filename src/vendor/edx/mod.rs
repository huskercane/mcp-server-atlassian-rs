#![allow(clippy::doc_markdown)]

//! edX / Open edX discussion API vendor implementation.
//!
//! edX discussion APIs live under the LMS host (edx.org uses
//! `https://courses.edx.org`) at `/api/discussion/v1/...`. Open edX instances
//! expose the same path shape on their own LMS base, so the base URL is
//! configurable with `EDX_API_BASE`.
//!
//! Authentication is a static bearer token read from `EDX_ACCESS_TOKEN`. This
//! keeps the first implementation aligned with the existing non-Atlassian
//! providers in this crate; an OAuth token exchange can be layered in later if
//! the user's edX access requires client credentials.

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_EDX};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

const DEFAULT_API_BASE: &str = "https://courses.edx.org";

#[derive(Debug, Clone, Default)]
pub struct EdxVendor {
    base_url_override: Option<String>,
}

impl EdxVendor {
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
            .get_for(VENDOR_EDX, "EDX_ACCESS_TOKEN")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "EDX_ACCESS_TOKEN is required for edx_discussion_* tools. Set an edX \
                     bearer token under the `edx` section of ~/.mcp/configs.json or in \
                     the environment.",
                )
            })
    }
}

impl Vendor for EdxVendor {
    fn name(&self) -> &'static str {
        VENDOR_EDX
    }

    fn base_url(&self, config: &Config) -> Result<String, McpError> {
        Ok(self
            .base_url_override
            .clone()
            .or_else(|| {
                config
                    .get_for(VENDOR_EDX, "EDX_API_BASE")
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| DEFAULT_API_BASE.to_owned()))
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
