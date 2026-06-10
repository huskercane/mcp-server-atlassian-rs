#![allow(clippy::doc_markdown)]

//! New Relic NerdGraph (GraphQL) vendor implementation.
//!
//! New Relic's primary API is **NerdGraph**, a single GraphQL endpoint
//! (`POST https://api.newrelic.com/graphql`) through which NRQL queries, entity
//! search, dashboards, and alerts are all driven. That makes it the odd vendor
//! out here: instead of the five generic REST verbs the other vendors expose,
//! New Relic exposes one `newrelic_query` tool that posts a GraphQL document
//! (plus optional variables) to the fixed endpoint.
//!
//! Authentication is a **User API key** carried in the custom `API-Key` header
//! (not `Authorization`) — the same [`Credentials::ApiKeyHeader`] carrier
//! Postman uses (see [`crate::auth::Credentials::ApiKeyHeader`]). Credential
//! lookup is a plain config read on [`NewRelicVendor::api_key`].
//!
//! Two NerdGraph traits shape the rest:
//! - **Region split.** US accounts live on `https://api.newrelic.com`; EU
//!   accounts on `https://api.eu.newrelic.com`. Selected via `NEW_RELIC_REGION`
//!   (`us` default / `eu`), with `NEW_RELIC_API_BASE` as an explicit override
//!   (tests / future regions).
//! - **`200 OK` is not success.** GraphQL returns HTTP `200` with a top-level
//!   `errors` array for query/validation/auth failures, so the status-based
//!   [`Vendor::classify_error`] never fires for them;
//!   [`Vendor::classify_success_json`] (see [`error`]) inspects `errors` and
//!   reclassifies a non-empty array as a typed error.

pub mod error;

use reqwest::StatusCode;
use serde_json::Value;

use crate::config::{Config, VENDOR_NEWRELIC};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

/// HTTP header New Relic expects the User API key in.
pub const API_KEY_HEADER: &str = "API-Key";

/// The single NerdGraph endpoint path. Every `newrelic_query` call posts here.
pub const GRAPHQL_PATH: &str = "/graphql";

/// US NerdGraph base (default region).
const US_API_BASE: &str = "https://api.newrelic.com";

/// EU NerdGraph base — EU-region accounts must use this host or NerdGraph
/// rejects the key.
const EU_API_BASE: &str = "https://api.eu.newrelic.com";

/// New Relic NerdGraph [`Vendor`] strategy.
///
/// Cheap to clone: it holds only an optional base-URL override. There is no
/// token cache — the User API key is static and read from config per request.
#[derive(Debug, Clone, Default)]
pub struct NewRelicVendor {
    /// Optional API base override (tests → wiremock). `None` resolves the
    /// region from config.
    base_url_override: Option<String>,
}

impl NewRelicVendor {
    /// Production constructor. Resolves the region from config at request time.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the API base (tests → wiremock).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
        }
    }

    /// Resolve the User API key from the `newrelic` config section. This is the
    /// New Relic credential entry point — the shared Atlassian resolver is never
    /// consulted. Errors with a clear, actionable message at tool-call time when
    /// the key is absent.
    pub fn api_key(&self, config: &Config) -> Result<String, McpError> {
        config
            .get_for(VENDOR_NEWRELIC, "NEW_RELIC_API_KEY")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "NEW_RELIC_API_KEY is required for newrelic_query. Set a New Relic \
                     User API key under the `newrelic` section of ~/.mcp/configs.json or \
                     in the environment.",
                )
            })
    }
}

impl Vendor for NewRelicVendor {
    fn name(&self) -> &'static str {
        VENDOR_NEWRELIC
    }

    /// Resolve the NerdGraph base. Priority: explicit `with_base_url` (tests) →
    /// `NEW_RELIC_API_BASE` override → `NEW_RELIC_REGION` (`eu` → EU host) →
    /// US default.
    fn base_url(&self, config: &Config) -> Result<String, McpError> {
        if let Some(base) = &self.base_url_override {
            return Ok(base.clone());
        }
        if let Some(base) = config
            .get_for(VENDOR_NEWRELIC, "NEW_RELIC_API_BASE")
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Ok(base.to_owned());
        }
        let region = config
            .get_for(VENDOR_NEWRELIC, "NEW_RELIC_REGION")
            .map_or("us", str::trim);
        Ok(if region.eq_ignore_ascii_case("eu") {
            EU_API_BASE.to_owned()
        } else {
            US_API_BASE.to_owned()
        })
    }

    /// Verbatim passthrough — the controller always supplies [`GRAPHQL_PATH`].
    /// We only ensure a leading `/`, matching the other single-host vendors.
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

    /// NerdGraph returns `200 OK` with a top-level `errors` array for query,
    /// validation, and most auth failures; reclassify a non-empty array as a
    /// typed error. An absent or empty array is taken as success.
    fn classify_success_json(&self, value: &Value) -> Option<McpError> {
        error::classify_graphql_errors(value)
    }
}
