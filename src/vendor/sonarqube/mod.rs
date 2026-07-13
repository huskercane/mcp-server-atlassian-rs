#![allow(clippy::doc_markdown)]

//! SonarQube / SonarCloud Web API vendor implementation.
//!
//! SonarQube is the CI code-quality gate: a build runs the Sonar scanner, the
//! scanner uploads an analysis, and a **quality gate** is evaluated against it.
//! This vendor reads that result back — most usefully "why did the gate fail" —
//! over Sonar's stable REST **Web API** (`{base}/api/...`).
//!
//! It follows the Grafana model, not the Atlassian one:
//! - **Base URL is required config** (`SONARQUBE_URL`) since the same binary
//!   serves self-hosted SonarQube (`https://sonar.mycorp.com`) and SonarCloud
//!   (`https://sonarcloud.io`) — there is no sensible default, so
//!   [`Vendor::base_url`] fails with a clear, actionable [`auth_missing`] error
//!   at tool-call time when it is absent (mirroring Jira's `ATLASSIAN_SITE_NAME`
//!   and Grafana's `GRAFANA_URL`).
//! - Authentication is a **user token** (`SONARQUBE_TOKEN`) carried as
//!   `Authorization: Bearer <token>` — the same [`Credentials::Bearer`] carrier
//!   CircleCI and Grafana use. Bearer is supported by SonarQube 9.9 LTS+ and
//!   SonarCloud; older servers accepting only Basic-with-token-as-username are
//!   out of scope for this pinned baseline. Credential lookup is a plain config
//!   read on [`SonarqubeVendor::token`] — the shared Atlassian resolver is never
//!   consulted.
//!
//! Everything after auth (path normalisation, query encoding, transport, output
//! rendering, JMESPath filtering) is the shared vendor-neutral machinery.

pub mod error;

use reqwest::StatusCode;

use crate::config::{Config, VENDOR_SONARQUBE};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

/// Quality-gate status endpoint: the failing conditions for an analysis /
/// branch / PR. This is the "why did Sonar fail" call.
pub const QUALITY_GATE_PATH: &str = "/api/qualitygates/project_status";

/// Issue-search endpoint: the individual bugs / vulnerabilities / code smells,
/// filterable by project, branch/PR, type and severity.
pub const ISSUES_SEARCH_PATH: &str = "/api/issues/search";

/// Compute-engine task endpoint: resolves a scanner `ceTaskId` (printed in the
/// CI log's `report-task.txt`) to the `analysisId` the quality-gate call needs.
pub const CE_TASK_PATH: &str = "/api/ce/task";

/// SonarQube / SonarCloud Web API [`Vendor`] strategy.
///
/// Cheap to clone: it holds only an optional base-URL override. There is no
/// token cache — the user token is static and read from config per request.
#[derive(Debug, Clone, Default)]
pub struct SonarqubeVendor {
    /// Optional API base override (tests → wiremock). `None` resolves
    /// `SONARQUBE_URL` from config.
    base_url_override: Option<String>,
}

impl SonarqubeVendor {
    /// Production constructor. Resolves `SONARQUBE_URL` from config at request
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

    /// Resolve the user token from the `sonarqube` config section. This is the
    /// SonarQube credential entry point — the shared Atlassian resolver is never
    /// consulted. Errors with a clear, actionable message at tool-call time when
    /// the token is absent.
    pub fn token(&self, config: &Config) -> Result<String, McpError> {
        config
            .get_for(VENDOR_SONARQUBE, "SONARQUBE_TOKEN")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "SONARQUBE_TOKEN is required for sonarqube_* tools. Set a SonarQube \
                     user token (My Account → Security → Generate Token) under the \
                     `sonarqube` section of ~/.mcp/configs.json or in the environment.",
                )
            })
    }
}

impl Vendor for SonarqubeVendor {
    fn name(&self) -> &'static str {
        VENDOR_SONARQUBE
    }

    /// Resolve the Sonar base. Priority: explicit `with_base_url` (tests) →
    /// `SONARQUBE_URL` config. A trailing slash is trimmed so the appended
    /// `/api/...` path never produces a double slash. Errors (not panics) when
    /// `SONARQUBE_URL` is absent so a deployment without Sonar still boots.
    fn base_url(&self, config: &Config) -> Result<String, McpError> {
        if let Some(base) = &self.base_url_override {
            return Ok(base.trim_end_matches('/').to_owned());
        }
        let url = config
            .get_for(VENDOR_SONARQUBE, "SONARQUBE_URL")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                auth_missing(
                    "SONARQUBE_URL is required for sonarqube_* tools. Set it to your Sonar \
                     base URL (e.g. https://sonar.mycorp.com or https://sonarcloud.io) \
                     under the `sonarqube` section of ~/.mcp/configs.json or in the \
                     environment.",
                )
            })?;
        Ok(url.trim_end_matches('/').to_owned())
    }

    /// Verbatim passthrough — callers (and the controller) supply the full
    /// `/api/...` path. We only ensure a leading `/`, matching the other
    /// single-host vendors.
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
