#![allow(clippy::doc_markdown)]

//! Grafana HTTP API vendor implementation.
//!
//! Grafana is the odd observability vendor here: it is a query/visualization
//! layer, not a log store. "Reading logs from Grafana" means running a **LogQL**
//! query against a **Loki** datasource *through Grafana's datasource proxy* —
//! `GET {base}/api/datasources/proxy/uid/{uid}/loki/api/v1/query_range`. The
//! proxy path keeps Grafana's auth and datasource model in charge, and works
//! identically for self-hosted Grafana and Grafana Cloud (only the base URL and
//! token differ).
//!
//! Authentication is a **service-account token** (or legacy API key) carried as
//! `Authorization: Bearer <token>` — the same [`Credentials::Bearer`] carrier
//! CircleCI uses (see [`crate::auth::Credentials::Bearer`]). Credential lookup
//! is a plain config read on [`GrafanaVendor::token`].
//!
//! Unlike the fixed-host vendors, Grafana's **base URL is required config**
//! (`GRAFANA_URL`) — there is no sensible default for a self-hosted-or-cloud
//! split — so [`Vendor::base_url`] fails with a clear, actionable
//! [`auth_missing`] error at tool-call time when it is absent (mirroring how
//! Jira surfaces a missing `ATLASSIAN_SITE_NAME`).

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_GRAFANA};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

/// Datasource-proxy path prefix. Callers append `/{uid}/<loki-path>`.
pub const DATASOURCE_PROXY_PREFIX: &str = "/api/datasources/proxy/uid";

/// Loki range-query path, appended after the datasource UID in the proxy URL.
pub const LOKI_QUERY_RANGE_PATH: &str = "/loki/api/v1/query_range";

/// Lists configured datasources (used to discover Loki datasource UIDs).
pub const DATASOURCES_PATH: &str = "/api/datasources";

/// Grafana HTTP API [`Vendor`] strategy.
///
/// Cheap to clone: it holds only an optional base-URL override. There is no
/// token cache — the service-account token is static and read from config per
/// request.
#[derive(Debug, Clone, Default)]
pub struct GrafanaVendor {
    /// Optional API base override (tests → wiremock). `None` resolves
    /// `GRAFANA_URL` from config.
    base_url_override: Option<String>,
}

impl GrafanaVendor {
    /// Production constructor. Resolves `GRAFANA_URL` from config at request
    /// time.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the API base (tests → wiremock).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
        }
    }

    /// Resolve the service-account token (or legacy API key) from the `grafana`
    /// config section. This is the Grafana credential entry point — the shared
    /// Atlassian resolver is never consulted. Errors with a clear, actionable
    /// message at tool-call time when the token is absent.
    pub fn token(&self, config: &Config) -> Result<String, McpError> {
        config
            .get_for(VENDOR_GRAFANA, "GRAFANA_TOKEN")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "GRAFANA_TOKEN is required for grafana_* tools. Set a Grafana \
                     service-account token (or API key) under the `grafana` section of \
                     ~/.mcp/configs.json or in the environment.",
                )
            })
    }
}

impl Vendor for GrafanaVendor {
    fn name(&self) -> &'static str {
        VENDOR_GRAFANA
    }

    /// Resolve the Grafana base. Priority: explicit `with_base_url` (tests) →
    /// `GRAFANA_URL` config. A trailing slash is trimmed so the appended path
    /// never produces a double slash. Errors (not panics) when `GRAFANA_URL` is
    /// absent so a deployment without Grafana still boots.
    fn base_url(&self, config: &Config) -> Result<String, McpError> {
        if let Some(base) = &self.base_url_override {
            return Ok(base.trim_end_matches('/').to_owned());
        }
        let url = config
            .get_for(VENDOR_GRAFANA, "GRAFANA_URL")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                auth_missing(
                    "GRAFANA_URL is required for grafana_* tools. Set it to your Grafana \
                     base URL (e.g. https://myorg.grafana.net or http://localhost:3000) \
                     under the `grafana` section of ~/.mcp/configs.json or in the \
                     environment.",
                )
            })?;
        Ok(url.trim_end_matches('/').to_owned())
    }

    /// Verbatim passthrough — the controller builds the full proxy path. We only
    /// ensure a leading `/`, matching the other single-host vendors.
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
