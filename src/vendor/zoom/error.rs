//! Parse Zoom API error response bodies into typed [`McpError`] values.
//!
//! Zoom's REST error envelope is `{"code": <int>, "message": "<str>"}`, with
//! an optional `errors` array for field validation on `300 Validation Failed`
//! responses (`[{"field": "...", "message": "..."}]`). The `code` is Zoom's
//! own numeric error code, distinct from the HTTP status; we surface the
//! human-readable `message` (plus any field errors) and key the typed error
//! off the HTTP status, mirroring the Jira/Confluence classifiers.
//!
//! Note: token-exchange (OAuth) errors use a *different* envelope
//! (`{"reason", "error"}`) and are handled in [`super::token`], not here.

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-ok HTTP status + body to the correct [`McpError`] factory,
/// preserving the upstream payload as `original`.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message = parsed.message.unwrap_or_else(|| {
        let reason = status.canonical_reason().unwrap_or("Zoom API error");
        format!("{} {reason}", status.as_u16())
    });
    let original = parsed.original;

    match status.as_u16() {
        401 => finalize(
            auth_invalid(format!("Authentication failed. Zoom API: {message}")),
            None,
            original,
        ),
        403 => finalize(
            auth_invalid(format!("Insufficient permissions. Zoom API: {message}")),
            Some(403),
            original,
        ),
        404 => api_error(
            format!("Resource not found. Zoom API: {message}"),
            Some(404),
            original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. Zoom API: {message}"),
            Some(429),
            original,
        ),
        s if s >= 500 => api_error(
            format!("Zoom server error. Detail: {message}"),
            Some(s),
            original,
        ),
        s => api_error(
            format!("Zoom API request failed. Detail: {message}"),
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

    let mut parts: Vec<String> = Vec::new();

    if let Some(message) = parsed.get("message").and_then(Value::as_str)
        && !message.is_empty()
    {
        parts.push(message.to_owned());
    }

    // Field-validation detail on `300 Validation Failed` (and similar).
    if let Some(arr) = parsed.get("errors").and_then(Value::as_array) {
        for entry in arr {
            let Some(msg) = entry.get("message").and_then(Value::as_str) else {
                continue;
            };
            match entry.get("field").and_then(Value::as_str) {
                Some(field) => parts.push(format!("{field}: {msg}")),
                None => parts.push(msg.to_owned()),
            }
        }
    }

    let message = if parts.is_empty() {
        None
    } else {
        Some(parts.join(" | "))
    };

    ParsedError {
        message,
        original: Some(OriginalError::Json(parsed)),
    }
}
