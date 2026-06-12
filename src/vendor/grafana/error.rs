#![allow(clippy::doc_markdown)]

//! Parse Grafana / Loki error response bodies into typed [`McpError`] values.
//!
//! Two envelope shapes reach us through the datasource proxy:
//!
//! - **Grafana HTTP API** — `{"message": "<str>"}` (e.g. a missing datasource,
//!   a bad token, or a permission failure).
//! - **Loki** (proxied) — `{"status": "error", "error": "<str>"}` for LogQL
//!   parse/validation errors, or, on some paths, a plain-text body.
//!
//! We surface the human-readable text and key the typed error off the HTTP
//! status, mirroring the CircleCI/Jira/Zoom classifiers. Loki returns a normal
//! non-2xx status (commonly `400`) for a bad LogQL query, so there is no
//! "`200 OK` with an error" quirk to handle — `classify_success_json` is left at
//! its default no-op.

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-2xx HTTP status + body to the correct [`McpError`] factory,
/// preserving the upstream payload as `original`.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message = parsed.message.unwrap_or_else(|| {
        let reason = status.canonical_reason().unwrap_or("Grafana API error");
        format!("{} {reason}", status.as_u16())
    });
    let original = parsed.original;

    match status.as_u16() {
        401 => finalize(
            auth_invalid(format!("Authentication failed. Grafana API: {message}")),
            None,
            original,
        ),
        403 => finalize(
            auth_invalid(format!("Insufficient permissions. Grafana API: {message}")),
            Some(403),
            original,
        ),
        404 => api_error(
            format!("Resource not found. Grafana API: {message}"),
            Some(404),
            original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. Grafana API: {message}"),
            Some(429),
            original,
        ),
        s if s >= 500 => api_error(
            format!("Grafana server error. Detail: {message}"),
            Some(s),
            original,
        ),
        s => api_error(
            format!("Grafana API request failed. Detail: {message}"),
            Some(s),
            original,
        ),
    }
}

/// Attach the HTTP status and original payload to an already-built error.
fn finalize(mut err: McpError, status: Option<u16>, original: Option<OriginalError>) -> McpError {
    if let Some(s) = status {
        err.status_code = Some(s);
    }
    err.original = original;
    err
}

/// Narrow-view parse result. `original` is what gets attached to the
/// `McpError` so downstream MCP consumers see the vendor payload.
#[derive(Debug, Default)]
pub struct ParsedError {
    pub message: Option<String>,
    pub original: Option<OriginalError>,
}

pub fn parse_error_body(body_text: &str) -> ParsedError {
    let trimmed = body_text.trim();
    if trimmed.is_empty() {
        return ParsedError::default();
    }

    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        // Loki sometimes returns a plain-text error body.
        return ParsedError {
            message: Some(trimmed.to_owned()),
            original: Some(OriginalError::String(body_text.to_owned())),
        };
    }

    let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
        return ParsedError {
            message: Some(trimmed.to_owned()),
            original: Some(OriginalError::String(body_text.to_owned())),
        };
    };

    // Grafana uses `message`; proxied Loki uses `error`. Prefer `message`.
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| parsed.get("error").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    ParsedError {
        message,
        original: Some(OriginalError::Json(parsed)),
    }
}
