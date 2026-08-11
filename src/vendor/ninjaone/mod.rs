#![allow(clippy::doc_markdown)]

//! NinjaOne API vendor implementation.
//!
//! A tool call selects either the single `NINJAONE_URL` or an alias from the
//! JSON object in `NINJAONE_SERVERS`. The tool never accepts a raw base URL;
//! this prevents an MCP caller from redirecting the server-held credential to
//! an arbitrary host.

pub mod error;

use reqwest::StatusCode;
use serde_json::Value;

use crate::auth::Credentials;
use crate::config::{Config, VENDOR_NINJAONE};
use crate::error::{McpError, auth_missing, unexpected};
use crate::vendor::Vendor;

#[derive(Debug, Clone, Default)]
pub struct NinjaOneVendor {
    base_url_override: Option<String>,
    server_alias: Option<String>,
}

impl NinjaOneVendor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
            server_alias: None,
        }
    }

    /// Return a request-scoped vendor selecting a configured server alias.
    #[must_use]
    pub fn for_server(&self, server_alias: Option<&str>) -> Self {
        Self {
            base_url_override: self.base_url_override.clone(),
            server_alias: server_alias.map(str::to_owned),
        }
    }

    /// Resolve the configured authentication carrier. Public API access tokens
    /// take precedence, followed by API session keys and an explicit browser
    /// cookie for the private `/ws/...` endpoints shown in the user's sample.
    pub fn credentials(&self, config: &Config) -> Result<Credentials, McpError> {
        if let Some(token) = non_blank(config, "NINJAONE_ACCESS_TOKEN") {
            return Ok(Credentials::Bearer {
                token: token.to_owned(),
            });
        }
        if let Some(key) = non_blank(config, "NINJAONE_SESSION_KEY") {
            return Ok(Credentials::ApiKeyHeader {
                header_name: "sessionKey".to_owned(),
                key: key.to_owned(),
            });
        }
        if let Some(cookie) = non_blank(config, "NINJAONE_SESSION_COOKIE") {
            return Ok(Credentials::ApiKeyHeader {
                header_name: "Cookie".to_owned(),
                key: cookie.to_owned(),
            });
        }
        Err(auth_missing(
            "NinjaOne authentication is required for ninjaone_* tools. Set one of \
             NINJAONE_ACCESS_TOKEN, NINJAONE_SESSION_KEY, or NINJAONE_SESSION_COOKIE \
             under the `ninjaone` section of ~/.mcp/configs.json or in the environment.",
        ))
    }

    fn configured_alias_url(config: &Config, alias: &str) -> Result<String, McpError> {
        let raw = non_blank(config, "NINJAONE_SERVERS").ok_or_else(|| {
            auth_missing(format!(
                "NinjaOne server alias `{alias}` was requested, but NINJAONE_SERVERS is not configured"
            ))
        })?;
        let parsed: Value = serde_json::from_str(raw).map_err(|error| {
            unexpected(
                format!("NINJAONE_SERVERS must be a JSON object of alias-to-URL strings: {error}"),
                None,
            )
        })?;
        parsed
            .as_object()
            .and_then(|servers| servers.get(alias))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(format!(
                    "Unknown NinjaOne server alias `{alias}`. Add it to NINJAONE_SERVERS."
                ))
            })
    }
}

fn non_blank<'a>(config: &'a Config, key: &str) -> Option<&'a str> {
    config
        .get_for(VENDOR_NINJAONE, key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
