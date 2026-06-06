#![allow(clippy::doc_markdown)]

//! Parse edX / Open edX API error response bodies into typed [`McpError`]
//! values.
//!
//! The discussion endpoints are Django/DRF-shaped in practice. Common
//! envelopes include `{"detail": "..."}`, `{"developer_message": "..."}`,
//! `{"error": "..."}`, `{"message": "..."}`, and field-level validation
//! maps like `{"raw_body": ["This field is required."]}`.

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message = parsed.message.unwrap_or_else(|| {
        let reason = status.canonical_reason().unwrap_or("edX API error");
        format!("{} {reason}", status.as_u16())
    });
    let original = parsed.original;

    match status.as_u16() {
        401 => finalize(
            auth_invalid(format!("Authentication failed. edX API: {message}")),
            None,
            original,
        ),
        403 => finalize(
            auth_invalid(format!("Insufficient permissions. edX API: {message}")),
            Some(403),
            original,
        ),
        404 => api_error(
            format!("Resource not found. edX API: {message}"),
            Some(404),
            original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. edX API: {message}"),
            Some(429),
            original,
        ),
        s if s >= 500 => api_error(
            format!("edX server error. Detail: {message}"),
            Some(s),
            original,
        ),
        s => api_error(
            format!("edX API request failed. Detail: {message}"),
            Some(s),
            original,
        ),
    }
}

fn finalize(mut err: McpError, status: Option<u16>, original: Option<OriginalError>) -> McpError {
    if let Some(s) = status {
        err.status_code = Some(s);
    }
    err.original = original;
    err
}

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

    let message = extract_message(&parsed);

    ParsedError {
        message,
        original: Some(OriginalError::Json(parsed)),
    }
}

fn extract_message(value: &Value) -> Option<String> {
    if let Some(message) = ["detail", "developer_message", "error", "message"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
    {
        return Some(message.to_owned());
    }

    match value {
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .filter_map(|(key, value)| field_message(key, value))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("; "))
            }
        }
        Value::Array(values) => {
            let parts: Vec<String> = values.iter().filter_map(scalar_message).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("; "))
            }
        }
        _ => scalar_message(value),
    }
}

fn field_message(key: &str, value: &Value) -> Option<String> {
    match value {
        Value::Array(values) => {
            let parts: Vec<String> = values.iter().filter_map(scalar_message).collect();
            if parts.is_empty() {
                None
            } else {
                Some(format!("{key}: {}", parts.join(", ")))
            }
        }
        Value::String(s) if !s.is_empty() => Some(format!("{key}: {s}")),
        _ => None,
    }
}

fn scalar_message(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}
