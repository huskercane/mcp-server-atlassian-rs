//! Parse Jira error response bodies into typed [`McpError`] values.
//!
//! Mirrors `src/utils/transport.util.ts` from the TS Jira reference
//! (`@aashari/mcp-server-atlassian-jira`). The parser walks **every**
//! recognised envelope shape in TS order and concatenates the parts with
//! ` | ` (parts) and `; ` (within-part lists):
//!
//! 1. `errorMessages: string[]` — joined by `'; '`
//! 2. `errors` (object, field validation) — `key: value` pairs joined by `'; '`
//! 3. `message: string` — appended verbatim
//! 4. `errors` (array, legacy Atlassian format) — first entry's `title` and
//!    `detail` appended as separate parts
//! 5. `warningMessages: string[]` — prefixed `Warnings: ` then joined by `'; '`
//!
//! Final concatenation: `parts.join(' | ')`.
//!
//! Status mapping mirrors TS:
//! - 401 → `auth_invalid`, prefix `"Authentication failed. Jira API: "`
//! - 403 → `auth_invalid`, prefix `"Insufficient permissions. Jira API: "`
//!   (the previous Rust port mapped 403 to `api_error`; that broke parity
//!   with TS, which treats both as auth/permission failures)
//! - 404 → `api_error(404)`, prefix `"Resource not found. Jira API: "`
//! - 429 → `api_error(429)`, prefix `"Rate limit exceeded. Jira API: "`
//! - 5xx → `api_error(status)`, prefix `"Jira server error. Detail: "`
//! - other → `api_error(status)`, prefix `"Jira API request failed. Detail: "`

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-ok HTTP status + body to the correct [`McpError`] factory,
/// preserving the upstream payload as `original`.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message = parsed.message.unwrap_or_else(|| {
        // TS fallback: `${status} ${statusText}`. We don't have statusText
        // pre-formatted here, so emit `<status> <canonical reason>`.
        let reason = status.canonical_reason().unwrap_or("Jira API error");
        format!("{} {reason}", status.as_u16())
    });

    match status.as_u16() {
        401 => auth_invalid(format!("Authentication failed. Jira API: {message}"))
            .with_original(parsed.original),
        403 => auth_invalid(format!("Insufficient permissions. Jira API: {message}"))
            .with_status(403)
            .with_original(parsed.original),
        404 => api_error(
            format!("Resource not found. Jira API: {message}"),
            Some(404),
            parsed.original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. Jira API: {message}"),
            Some(429),
            parsed.original,
        ),
        s if s >= 500 => api_error(
            format!("Jira server error. Detail: {message}"),
            Some(s),
            parsed.original,
        ),
        s => api_error(
            format!("Jira API request failed. Detail: {message}"),
            Some(s),
            parsed.original,
        ),
    }
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

    // Walk every recognised envelope shape in TS order and accumulate
    // human-readable parts. Multiple shapes can coexist (a single 400
    // response can carry both `errors` field validation and a top-level
    // `message`), so this is additive rather than first-match-wins.
    let mut parts: Vec<String> = Vec::new();

    if let Some(arr) = parsed.get("errorMessages").and_then(Value::as_array) {
        let joined = join_string_array(arr, "; ");
        if !joined.is_empty() {
            parts.push(joined);
        }
    }

    if let Some(obj) = parsed.get("errors").and_then(Value::as_object)
        && !obj.is_empty()
    {
        let mut field_pairs: Vec<String> = Vec::new();
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                field_pairs.push(format!("{k}: {s}"));
            } else {
                // Non-string field value (rare). Best-effort stringify so the
                // user still sees something useful.
                field_pairs.push(format!("{k}: {v}"));
            }
        }
        if !field_pairs.is_empty() {
            parts.push(field_pairs.join("; "));
        }
    }

    if let Some(message) = parsed.get("message").and_then(Value::as_str)
        && !message.is_empty()
    {
        parts.push(message.to_owned());
    }

    if let Some(arr) = parsed.get("errors").and_then(Value::as_array)
        && let Some(first) = arr.first().and_then(Value::as_object)
    {
        // Legacy Atlassian `errors[0]` shape: emit title and detail as
        // separate parts (TS pushes them independently, so the final
        // `' | '` join naturally separates them).
        if let Some(title) = first.get("title").and_then(Value::as_str) {
            parts.push(title.to_owned());
        }
        if let Some(detail) = first.get("detail").and_then(Value::as_str) {
            parts.push(detail.to_owned());
        }
    }

    if let Some(arr) = parsed.get("warningMessages").and_then(Value::as_array) {
        let joined = join_string_array(arr, "; ");
        if !joined.is_empty() {
            parts.push(format!("Warnings: {joined}"));
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

fn join_string_array(arr: &[Value], sep: &str) -> String {
    arr.iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(sep)
}

// Extension helpers to fluently attach extra fields to an existing
// McpError (avoids repeating the constructor pattern in `classify`).
trait WithMutators {
    fn with_original(self, original: Option<OriginalError>) -> Self;
    fn with_status(self, status: u16) -> Self;
}

impl WithMutators for McpError {
    fn with_original(mut self, original: Option<OriginalError>) -> Self {
        self.original = original;
        self
    }
    fn with_status(mut self, status: u16) -> Self {
        self.status_code = Some(status);
        self
    }
}
