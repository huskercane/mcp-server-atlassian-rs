#![allow(clippy::doc_markdown)]

//! Parse SonarQube / SonarCloud error response bodies into typed [`McpError`]
//! values.
//!
//! Sonar's Web API error envelope is `{"errors": [{"msg": "<str>"}, …]}` — an
//! array so a single response can report several problems at once; we join the
//! messages with `; `. A few older endpoints instead return `{"err_msg": …}`,
//! so that key is accepted as a fallback. We surface the human-readable text and
//! key the typed error off the HTTP status, mirroring the CircleCI/Grafana
//! classifiers.

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-2xx HTTP status + body to the correct [`McpError`] factory,
/// preserving the upstream payload as `original`.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message = parsed.message.unwrap_or_else(|| {
        let reason = status.canonical_reason().unwrap_or("SonarQube API error");
        format!("{} {reason}", status.as_u16())
    });
    let original = parsed.original;

    match status.as_u16() {
        401 => finalize(
            auth_invalid(format!("Authentication failed. SonarQube API: {message}")),
            None,
            original,
        ),
        403 => finalize(
            auth_invalid(format!(
                "Insufficient permissions. SonarQube API: {message}"
            )),
            Some(403),
            original,
        ),
        404 => api_error(
            format!("Resource not found. SonarQube API: {message}"),
            Some(404),
            original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. SonarQube API: {message}"),
            Some(429),
            original,
        ),
        s if s >= 500 => api_error(
            format!("SonarQube server error. Detail: {message}"),
            Some(s),
            original,
        ),
        s => api_error(
            format!("SonarQube API request failed. Detail: {message}"),
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

    // Sonar's canonical shape is `errors: [{msg}]`; older endpoints use
    // `err_msg`. Join multiple `errors` entries so nothing is dropped.
    let message = parsed
        .get("errors")
        .and_then(Value::as_array)
        .map(|errs| {
            errs.iter()
                .filter_map(|e| e.get("msg").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|s| !s.is_empty())
        .or_else(|| {
            parsed
                .get("err_msg")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|s| !s.is_empty());

    ParsedError {
        message,
        original: Some(OriginalError::Json(parsed)),
    }
}
