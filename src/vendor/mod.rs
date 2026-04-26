//! Vendor abstraction for Atlassian product APIs.
//!
//! This crate exposes generic HTTP wrapper tools (GET/POST/PUT/PATCH/DELETE)
//! over multiple Atlassian REST APIs that share auth, transport, and output
//! conventions but differ in:
//!
//! - **Base URL** — fixed for Bitbucket, derived from `ATLASSIAN_SITE_NAME`
//!   for Jira, etc.
//! - **Path normalisation** — Bitbucket auto-prepends `/2.0`; Jira passes the
//!   caller-supplied path through verbatim (`/rest/api/3/...`).
//! - **Error response parsing** — each product has its own error envelope
//!   shape(s).
//!
//! The [`Vendor`] trait captures exactly those three concerns. Everything
//! else (auth header, request building, body classification, raw-response
//! persistence, `JMESPath` filtering, output rendering, pagination,
//! truncation) lives in vendor-neutral layers and is shared.
//!
//! ## Lazy base-URL resolution
//!
//! [`Vendor::base_url`] takes the [`Config`] and may fail
//! ([`McpError`]). It is only invoked inside per-request transport calls,
//! never at server construction. This is critical: a Bitbucket-only user
//! must be able to start the server even when no Jira site name is
//! configured. Jira tools surface a clear "missing `ATLASSIAN_SITE_NAME`"
//! error at tool-call time rather than crashing the process at boot.

pub mod bitbucket;
pub mod confluence;
pub mod jira;

use reqwest::StatusCode;

use crate::config::Config;
use crate::error::McpError;

/// Per-product strategy object. Implementations are typically zero-cost
/// value types (e.g. unit structs or single-field wrappers) constructed
/// once per server and borrowed by reference for the lifetime of each
/// request.
pub trait Vendor: Send + Sync {
    /// Canonical vendor name. Use the `VENDOR_*` constants from
    /// [`crate::config`] to keep call-site keys typo-free.
    fn name(&self) -> &'static str;

    /// Resolve the absolute base URL for API calls. Called per request, not
    /// at server construction; an `Err` here surfaces as a tool-call error
    /// rather than a startup failure.
    fn base_url(&self, config: &Config) -> Result<String, McpError>;

    /// Apply vendor-specific path normalisation to a caller-supplied path.
    /// Bitbucket auto-prepends `/2.0`; Jira just ensures a leading `/`.
    fn normalize_path(&self, path: &str) -> String;

    /// Convert a non-2xx response (status + body text) into the typed
    /// [`McpError`] for this vendor's error envelope shape(s).
    fn classify_error(&self, status: StatusCode, body: &str) -> McpError;
}
