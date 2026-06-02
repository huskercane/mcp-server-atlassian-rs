#![allow(clippy::doc_markdown)]

//! Parse Slack Web API failures into typed [`McpError`] values.
//!
//! Slack has two distinct failure channels:
//!
//! 1. **Transport-level (non-2xx).** Rare for the Web API, but the edge can
//!    return `429 Too Many Requests` (with a `Retry-After` header), `5xx`, or
//!    an HTML error page. [`classify`] handles these the same way the
//!    Jira/CircleCI classifiers do — keyed off the HTTP status.
//! 2. **Application-level (`200 OK` with `{"ok": false}`).** This is the
//!    *common* error path. [`classify_ok_envelope`] inspects a successful JSON
//!    body and, when `ok` is `false`, turns the short `error` code (e.g.
//!    `channel_not_found`, `invalid_auth`, `ratelimited`) into a typed error.

use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-2xx HTTP status + body to the correct [`McpError`] factory. Slack
/// non-2xx bodies are frequently *not* JSON (plain `ratelimited`, or an HTML
/// page from the edge), so we surface the trimmed body text as the detail.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let trimmed = body_text.trim();
    let detail = if trimmed.is_empty() {
        let reason = status.canonical_reason().unwrap_or("Slack API error");
        format!("{} {reason}", status.as_u16())
    } else {
        trimmed.to_owned()
    };
    let original = (!trimmed.is_empty()).then(|| OriginalError::String(body_text.to_owned()));

    match status.as_u16() {
        401 => {
            let mut err = auth_invalid(format!("Authentication failed. Slack API: {detail}"));
            err.original = original;
            err
        }
        403 => {
            let mut err = auth_invalid(format!("Insufficient permissions. Slack API: {detail}"));
            err.status_code = Some(403);
            err.original = original;
            err
        }
        429 => api_error(
            format!("Rate limit exceeded. Slack API: {detail}"),
            Some(429),
            original,
        ),
        s if s >= 500 => api_error(
            format!("Slack server error. Detail: {detail}"),
            Some(s),
            original,
        ),
        s => api_error(
            format!("Slack API request failed. Detail: {detail}"),
            Some(s),
            original,
        ),
    }
}

/// Inspect a 2xx JSON body for Slack's `{"ok": false, "error": "<code>"}`
/// envelope. Returns `None` when `ok` is `true`, when the `ok` field is absent
/// (some endpoints return bare payloads), or when the body is not an object.
pub fn classify_ok_envelope(value: &Value) -> Option<McpError> {
    let obj = value.as_object()?;
    // Only act when `ok` is explicitly `false`. Absent `ok` ⇒ treat as success.
    if obj.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }

    let code = obj
        .get("error")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown_error");
    let original = Some(OriginalError::Json(value.clone()));

    Some(match code {
        // Auth / scope failures — surface as an auth error so the MCP code maps
        // to AUTHENTICATION_REQUIRED.
        "invalid_auth"
        | "not_authed"
        | "account_inactive"
        | "token_revoked"
        | "token_expired"
        | "no_permission"
        | "missing_scope"
        | "not_allowed_token_type"
        | "ekm_access_denied" => {
            let mut err = auth_invalid(format!("Authentication failed. Slack API error: {code}"));
            err.original = original;
            err
        }
        "ratelimited" => api_error(
            format!("Rate limit exceeded. Slack API error: {code}"),
            Some(429),
            original,
        ),
        // `channel_not_found`, `users_not_found`, `message_not_found`, … and the
        // bare `not_found`.
        c if c == "not_found" || c.ends_with("_not_found") => api_error(
            format!("Resource not found. Slack API error: {code}"),
            Some(404),
            original,
        ),
        _ => api_error(format!("Slack API error: {code}"), None, original),
    })
}
