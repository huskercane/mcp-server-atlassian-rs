#![allow(clippy::doc_markdown)]

//! Parse New Relic NerdGraph failures into typed [`McpError`] values.
//!
//! NerdGraph has two failure channels:
//!
//! 1. **Transport-level (non-2xx).** A missing/invalid API key, a malformed
//!    request, or an upstream outage arrives as `401`/`403`/`4xx`/`5xx`.
//!    [`classify`] keys off the HTTP status like the other REST classifiers.
//! 2. **GraphQL-level (`200 OK` with an `errors` array).** This is the *common*
//!    error path: query syntax errors, NRQL errors, and most auth/permission
//!    failures come back as HTTP `200` with `{"data": …, "errors": [ … ]}`.
//!    [`classify_graphql_errors`] inspects that array and reclassifies a
//!    non-empty one as a typed error.

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-2xx HTTP status + body to the correct [`McpError`] factory,
/// preserving the upstream payload as `original`.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message = parsed.message.unwrap_or_else(|| {
        let reason = status.canonical_reason().unwrap_or("New Relic API error");
        format!("{} {reason}", status.as_u16())
    });
    let original = parsed.original;

    match status.as_u16() {
        401 => finalize(
            auth_invalid(format!("Authentication failed. New Relic API: {message}")),
            None,
            original,
        ),
        403 => finalize(
            auth_invalid(format!(
                "Insufficient permissions. New Relic API: {message}"
            )),
            Some(403),
            original,
        ),
        404 => api_error(
            format!("Resource not found. New Relic API: {message}"),
            Some(404),
            original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. New Relic API: {message}"),
            Some(429),
            original,
        ),
        s if s >= 500 => api_error(
            format!("New Relic server error. Detail: {message}"),
            Some(s),
            original,
        ),
        s => api_error(
            format!("New Relic API request failed. Detail: {message}"),
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

/// Inspect a 2xx JSON body for NerdGraph's top-level `errors` array. Returns
/// `None` when `errors` is absent or empty (a successful query — even one whose
/// `data` is partially null). A non-empty array is reclassified: when any error
/// looks auth/permission-shaped (by `errorClass` or message), it becomes an
/// authentication error; otherwise an API error carrying the joined messages.
pub fn classify_graphql_errors(value: &Value) -> Option<McpError> {
    let errors = value.get("errors").and_then(Value::as_array)?;
    if errors.is_empty() {
        return None;
    }

    let message = join_messages(errors);
    let original = Some(OriginalError::Json(value.clone()));

    if errors.iter().any(is_auth_error) {
        let mut err = auth_invalid(format!("New Relic NerdGraph error: {message}"));
        err.original = original;
        return Some(err);
    }
    Some(api_error(
        format!("New Relic NerdGraph error: {message}"),
        None,
        original,
    ))
}

/// Join each error's `message` (falling back to its `errorClass`) into a single
/// `; `-separated string. Falls back to a sentinel when nothing is usable.
fn join_messages(errors: &[Value]) -> String {
    let parts: Vec<String> = errors
        .iter()
        .filter_map(|e| {
            e.get("message")
                .and_then(Value::as_str)
                .or_else(|| error_class(e))
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .collect();
    if parts.is_empty() {
        "unknown error".to_owned()
    } else {
        parts.join("; ")
    }
}

/// A NerdGraph error is auth-shaped when its `errorClass` (under `extensions`)
/// or its message points at an authentication/permission failure.
fn is_auth_error(err: &Value) -> bool {
    if let Some(class) = error_class(err) {
        let upper = class.to_ascii_uppercase();
        if upper.contains("UNAUTHENTICATED")
            || upper.contains("UNAUTHORIZED")
            || upper.contains("FORBIDDEN")
            || upper.contains("ACCESS_DENIED")
        {
            return true;
        }
    }
    err.get("message").and_then(Value::as_str).is_some_and(|m| {
        let lower = m.to_ascii_lowercase();
        lower.contains("api key") || lower.contains("unauthor") || lower.contains("not authorized")
    })
}

/// Read `extensions.errorClass` off a single GraphQL error object.
fn error_class(err: &Value) -> Option<&str> {
    err.get("extensions")
        .and_then(|e| e.get("errorClass"))
        .and_then(Value::as_str)
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

    // A non-2xx body may still be GraphQL-shaped (`{"errors": [...]}`), or a
    // plain `{"error": "..."}` / `{"message": "..."}` envelope.
    let message = parsed
        .get("errors")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .map(|errs| join_messages(errs))
        .or_else(|| {
            parsed
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            parsed
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|s| !s.is_empty());

    ParsedError {
        message,
        original: Some(OriginalError::Json(parsed)),
    }
}
