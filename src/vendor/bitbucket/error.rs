//! Parse Bitbucket error response bodies into typed [`McpError`] values.
//!
//! Mirrors the non-ok branch of TS `fetchAtlassian`. Four response shapes are
//! recognised:
//! 1. `{"type":"error","error":{"message":"…","detail":"…"}}` (canonical)
//! 2. `{"error":{"message":"…"}}` (legacy)
//! 3. `{"errors":[{"status":…,"title":"…",…}]}` (Atlassian-shared)
//! 4. `{"message":"…"}` (fallback)
//!
//! When none match, the raw body text is surfaced via
//! [`OriginalError::String`] so the MCP error formatter can expose it.

use std::fmt::Write as _;

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-ok HTTP status + body to the correct [`McpError`] factory,
/// preserving the upstream payload as `original`.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message_suffix = parsed.message.as_deref().unwrap_or(body_text);
    let message_suffix = if message_suffix.is_empty() {
        status
            .canonical_reason()
            .unwrap_or("Bitbucket API error")
            .to_owned()
    } else {
        message_suffix.to_owned()
    };

    match status.as_u16() {
        401 => auth_invalid(format!(
            "Bitbucket API: Authentication failed - {message_suffix}"
        ))
        .with_original(parsed.original),
        403 => api_error(
            format!("Bitbucket API: Permission denied - {message_suffix}"),
            Some(403),
            parsed.original,
        ),
        404 => api_error(
            format!("Bitbucket API: Resource not found - {message_suffix}"),
            Some(404),
            parsed.original,
        ),
        429 => api_error(
            format!("Bitbucket API: Rate limit exceeded - {message_suffix}"),
            Some(429),
            parsed.original,
        ),
        s if s >= 500 => api_error(
            format!("Bitbucket API: Service error - {message_suffix}"),
            Some(s),
            parsed.original,
        ),
        s => api_error(
            format!("Bitbucket API Error: {message_suffix}"),
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

    if let Some(result) = parse_type_error(&parsed) {
        return result;
    }
    if let Some(result) = parse_nested_error(&parsed) {
        return result;
    }
    if let Some(result) = parse_errors_array(&parsed) {
        return result;
    }
    if let Some(result) = parse_flat_message(&parsed) {
        return result;
    }

    ParsedError {
        message: None,
        original: Some(OriginalError::Json(parsed)),
    }
}

fn parse_type_error(parsed: &Value) -> Option<ParsedError> {
    if parsed.get("type").and_then(Value::as_str) != Some("error") {
        return None;
    }
    let err_obj = parsed.get("error")?.as_object()?;
    let message = err_obj.get("message").and_then(Value::as_str)?.to_owned();
    let mut composed = message.clone();
    if let Some(detail) = err_obj.get("detail").and_then(Value::as_str) {
        let _ = write!(composed, " Detail: {detail}");
    }
    Some(ParsedError {
        message: Some(composed),
        original: Some(OriginalError::Json(Value::Object(err_obj.clone()))),
    })
}

fn parse_nested_error(parsed: &Value) -> Option<ParsedError> {
    let err_obj = parsed.get("error")?.as_object()?;
    let message = err_obj.get("message").and_then(Value::as_str)?.to_owned();
    Some(ParsedError {
        message: Some(message),
        original: Some(OriginalError::Json(Value::Object(err_obj.clone()))),
    })
}

fn parse_errors_array(parsed: &Value) -> Option<ParsedError> {
    let arr = parsed.get("errors")?.as_array()?;
    let first = arr.first()?.as_object()?;
    let title = first
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| first.get("message").and_then(Value::as_str))?
        .to_owned();
    Some(ParsedError {
        message: Some(title),
        original: Some(OriginalError::Json(Value::Object(first.clone()))),
    })
}

fn parse_flat_message(parsed: &Value) -> Option<ParsedError> {
    let obj = parsed.as_object()?;
    let message = obj.get("message").and_then(Value::as_str)?.to_owned();
    Some(ParsedError {
        message: Some(message),
        original: Some(OriginalError::Json(parsed.clone())),
    })
}

// Extension helper to fluently attach an original payload to an existing
// McpError (avoids repeating the constructor pattern in `classify`).
trait WithOriginal {
    fn with_original(self, original: Option<OriginalError>) -> Self;
}

impl WithOriginal for McpError {
    fn with_original(mut self, original: Option<OriginalError>) -> Self {
        self.original = original;
        self
    }
}
