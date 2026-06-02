#![allow(clippy::doc_markdown)]

//! Slack Web API vendor implementation.
//!
//! Like Zoom and CircleCI, Slack departs from the Atlassian `ATLASSIAN_*`
//! credential model: it authenticates with a single OAuth token (`SLACK_TOKEN`,
//! typically a bot token `xoxb-…` or user token `xoxp-…`) sent as
//! `Authorization: Bearer <token>`. There is no token exchange, so credential
//! *lookup* is a plain config read on [`SlackVendor::token`].
//!
//! Two things make Slack unusual among the vendors here:
//!
//! - **`200 OK` is not success.** The Web API returns HTTP `200` for almost
//!   everything, including logical failures, and signals the real outcome in
//!   the body: `{"ok": true, …}` or `{"ok": false, "error": "<code>"}`. The
//!   status-based [`Vendor::classify_error`] therefore never fires for the
//!   common error path; [`Vendor::classify_success_json`] (see [`error`])
//!   inspects the `ok` field and reclassifies `ok: false` as a typed error.
//! - **Method-style paths.** Endpoints are "methods" like
//!   `/conversations.list` or `/chat.postMessage`, called with GET (query
//!   params) or POST (JSON body, since we send `Content-Type: application/json`
//!   with a Bearer token). Path normalisation is verbatim, like Jira/Zoom.
//!
//! Base URL is fixed at `https://slack.com/api` (overridable for tests).

pub mod error;

use reqwest::StatusCode;
use serde_json::Value;

use crate::config::{Config, VENDOR_SLACK};
use crate::error::{McpError, auth_missing};
use crate::vendor::Vendor;

/// Production Slack Web API base. Tests point [`SlackVendor::with_base_url`] at
/// a wiremock instead.
const DEFAULT_API_BASE: &str = "https://slack.com/api";

/// Slack Web API [`Vendor`] strategy.
///
/// Cheap to clone: it holds only an optional base-URL override. There is no
/// token cache — Slack's OAuth token is static and read from config per
/// request.
#[derive(Debug, Clone, Default)]
pub struct SlackVendor {
    /// Optional API base override (tests → wiremock). `None` uses
    /// [`DEFAULT_API_BASE`].
    base_url_override: Option<String>,
}

impl SlackVendor {
    /// Production constructor. Uses the real Slack API host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the API base (tests → wiremock).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url_override: Some(base_url.into()),
        }
    }

    /// Resolve the OAuth token from the `slack` config section. This is the
    /// Slack credential entry point — the shared Atlassian resolver is never
    /// consulted. Errors with a clear, actionable message at tool-call time
    /// when the token is absent.
    pub fn token(&self, config: &Config) -> Result<String, McpError> {
        config
            .get_for(VENDOR_SLACK, "SLACK_TOKEN")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                auth_missing(
                    "SLACK_TOKEN is required for slack_* tools. Set a Slack bot or user \
                     OAuth token (xoxb-… / xoxp-…) under the `slack` section of \
                     ~/.mcp/configs.json or in the environment.",
                )
            })
    }
}

impl Vendor for SlackVendor {
    fn name(&self) -> &'static str {
        VENDOR_SLACK
    }

    /// Resolve the API base. Independent of [`Config`] — Slack's base is fixed
    /// (or test-overridden); credentials, not the URL, come from config.
    fn base_url(&self, _config: &Config) -> Result<String, McpError> {
        Ok(self
            .base_url_override
            .clone()
            .unwrap_or_else(|| DEFAULT_API_BASE.to_owned()))
    }

    /// Verbatim passthrough — callers supply the method path
    /// (`/conversations.list`). We only ensure a leading `/`.
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

    /// Slack returns `200 OK` with `{"ok": false, "error": …}` for logical
    /// failures; reclassify those as typed errors. `ok: true` (or a body with
    /// no `ok` field) is taken as success.
    fn classify_success_json(&self, value: &Value) -> Option<McpError> {
        error::classify_ok_envelope(value)
    }
}
