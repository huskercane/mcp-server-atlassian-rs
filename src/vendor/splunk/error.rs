//! Splunk REST API error classification.

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = serde_json::from_str::<Value>(body_text).ok();
    let message = parsed
        .as_ref()
        .and_then(extract_message)
        .or_else(|| {
            let trimmed = body_text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
        .unwrap_or_else(|| {
            let reason = status.canonical_reason().unwrap_or("Splunk API error");
            format!("{} {reason}", status.as_u16())
        });
    let original = parsed
        .map(OriginalError::Json)
        .or_else(|| (!body_text.is_empty()).then(|| OriginalError::String(body_text.to_owned())));

    match status.as_u16() {
        401 => with_original(
            auth_invalid(format!("Authentication failed. Splunk API: {message}")),
            original,
        ),
        403 => {
            let mut err = auth_invalid(format!("Insufficient permissions. Splunk API: {message}"));
            err.status_code = Some(403);
            err.original = original;
            err
        }
        404 => api_error(
            format!("Resource not found. Splunk API: {message}"),
            Some(404),
            original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. Splunk API: {message}"),
            Some(429),
            original,
        ),
        code if code >= 500 => api_error(
            format!("Splunk server error. Detail: {message}"),
            Some(code),
            original,
        ),
        code => api_error(
            format!("Splunk API request failed. Detail: {message}"),
            Some(code),
            original,
        ),
    }
}

fn extract_message(value: &Value) -> Option<String> {
    value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("text"))
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .or_else(|| value.get("error").and_then(Value::as_str))
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
}

fn with_original(mut err: McpError, original: Option<OriginalError>) -> McpError {
    err.original = original;
    err
}
