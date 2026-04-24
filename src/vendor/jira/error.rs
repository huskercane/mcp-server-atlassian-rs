//! Parse Jira error response bodies into typed [`McpError`] values.
//!
//! Jira REST API v3 error envelopes:
//! 1. Canonical: `{"errorMessages": ["..."], "errors": {"field": "msg"}}`
//!    Either side may be empty; field validation errors live in `errors`,
//!    higher-level failures (auth/permission/not-found) in `errorMessages`.
//! 2. Fallback: `{"message": "..."}` — used by some auth endpoints.
//! 3. OAuth-style: `{"error": "...", "error_description": "..."}` — caught
//!    via the nested-error branch.
//!
//! When none match, the raw body text is surfaced via
//! [`OriginalError::String`] so the MCP error formatter can expose it.

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
            .unwrap_or("Jira API error")
            .to_owned()
    } else {
        message_suffix.to_owned()
    };

    match status.as_u16() {
        401 => auth_invalid(format!("Jira API: Authentication failed - {message_suffix}"))
            .with_original(parsed.original),
        403 => api_error(
            format!("Jira API: Permission denied - {message_suffix}"),
            Some(403),
            parsed.original,
        ),
        404 => api_error(
            format!("Jira API: Resource not found - {message_suffix}"),
            Some(404),
            parsed.original,
        ),
        429 => api_error(
            format!("Jira API: Rate limit exceeded - {message_suffix}"),
            Some(429),
            parsed.original,
        ),
        s if s >= 500 => api_error(
            format!("Jira API: Service error - {message_suffix}"),
            Some(s),
            parsed.original,
        ),
        s => api_error(
            format!("Jira API Error: {message_suffix}"),
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

    if let Some(result) = parse_jira_envelope(&parsed) {
        return result;
    }
    if let Some(result) = parse_oauth_error(&parsed) {
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

/// `{"errorMessages": ["..."], "errors": {"field": "msg"}}` — the canonical
/// Jira envelope. Either side may be empty; the parser concatenates both
/// into a single "; "-delimited message so the human sees everything.
fn parse_jira_envelope(parsed: &Value) -> Option<ParsedError> {
    let obj = parsed.as_object()?;
    let messages = obj.get("errorMessages").and_then(Value::as_array);
    let errors = obj.get("errors").and_then(Value::as_object);
    if messages.is_none() && errors.is_none() {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = messages {
        for v in arr {
            if let Some(s) = v.as_str() {
                parts.push(s.to_owned());
            }
        }
    }
    if let Some(field_errors) = errors {
        for (field, value) in field_errors {
            if let Some(s) = value.as_str() {
                parts.push(format!("{field}: {s}"));
            }
        }
    }

    let message = if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    };

    Some(ParsedError {
        message,
        original: Some(OriginalError::Json(parsed.clone())),
    })
}

/// OAuth-style: `{"error": "...", "error_description": "..."}`.
fn parse_oauth_error(parsed: &Value) -> Option<ParsedError> {
    let obj = parsed.as_object()?;
    let code = obj.get("error").and_then(Value::as_str)?;
    let description = obj
        .get("error_description")
        .and_then(Value::as_str)
        .unwrap_or(code)
        .to_owned();
    Some(ParsedError {
        message: Some(description),
        original: Some(OriginalError::Json(parsed.clone())),
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
