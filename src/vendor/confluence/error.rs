//! Parse Confluence error response bodies into typed [`McpError`] values.
//!
//! Mirrors `src/utils/transport.util.ts` from the TS Confluence reference
//! (`@aashari/mcp-server-atlassian-confluence`). The parser walks each
//! recognised envelope shape in TS order, taking the **first** one that
//! produces a non-empty message:
//!
//! 1. `title: string` (Confluence v2 problem-details) — appended with
//!    `: detail` when `detail` is also present and not already in title.
//! 2. `message: string` (older API / generic error) — appended with
//!    `: reason` when `reason` is also present and not already in message.
//! 3. `errors: [{message|title, ...}]` — joins up to three entries with
//!    `'; '`; appends `; and N more errors` when there are more than three.
//! 4. `errorMessages: string[]` (Jira-style legacy) — joined with `'; '`.
//! 5. `statusCode + message` fallback — `"<statusCode>: <message>"`. Only
//!    reached when the earlier `message` branch did not fire (which means
//!    the body has `statusCode` but no `message`/`title`/`errors`/
//!    `errorMessages`). The TS reference puts this at the very end of an
//!    `if/else if` chain, so a payload that already carries `message`
//!    resolves to the message verbatim — never with the statusCode prefix.
//!
//! Status mapping mirrors TS:
//! - 401 → `auth_invalid`, prefix `"Authentication failed. Confluence API: "`
//! - 403 → `api_error(403)`, prefix `"Access denied. Confluence API: "`
//!   (Confluence treats 403 as a permissions-style API error, **not**
//!   `auth_invalid` like Jira does — keep that asymmetry intentional.)
//! - 404 → `api_error(404)`, prefix `"Resource not found. Confluence API: "`
//! - 429 → `api_error(429)`, prefix `"Rate limit exceeded. Confluence API: "`
//! - 5xx → `api_error(status)`, prefix `"Confluence service error. Detail: "`
//! - other → `api_error(status)`, prefix `"Confluence API request failed. Detail: "`

use std::fmt::Write as _;

use reqwest::StatusCode;
use serde_json::Value;


use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a non-ok HTTP status + body to the correct [`McpError`] factory,
/// preserving the upstream payload as `original`.
pub fn classify(status: StatusCode, body_text: &str) -> McpError {
    let parsed = parse_error_body(body_text);
    let message = parsed.message.unwrap_or_else(|| {
        let reason = status.canonical_reason().unwrap_or("Confluence API error");
        format!("{} {reason}", status.as_u16())
    });

    match status.as_u16() {
        401 => auth_invalid(format!("Authentication failed. Confluence API: {message}"))
            .with_original(parsed.original),
        403 => api_error(
            format!("Access denied. Confluence API: {message}"),
            Some(403),
            parsed.original,
        ),
        404 => api_error(
            format!("Resource not found. Confluence API: {message}"),
            Some(404),
            parsed.original,
        ),
        429 => api_error(
            format!("Rate limit exceeded. Confluence API: {message}"),
            Some(429),
            parsed.original,
        ),
        s if s >= 500 => api_error(
            format!("Confluence service error. Detail: {message}"),
            Some(s),
            parsed.original,
        ),
        s => api_error(
            format!("Confluence API request failed. Detail: {message}"),
            Some(s),
            parsed.original,
        ),
    }
}

/// Narrow-view parse result.
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

    // First-match-wins. The TS implementation chains `if/else if`
    // through the recognised shapes; the first one that yields a
    // non-empty message wins.
    let message = extract_message(&parsed);

    ParsedError {
        message,
        original: Some(OriginalError::Json(parsed)),
    }
}

fn extract_message(parsed: &Value) -> Option<String> {
    // 1. Confluence v2 problem-details: { title, status, detail }
    if let Some(title) = parsed.get("title").and_then(Value::as_str)
        && !title.is_empty()
    {
        let mut msg = title.to_owned();
        if let Some(detail) = parsed.get("detail").and_then(Value::as_str)
            && !detail.is_empty()
            && !msg.contains(detail)
        {
            msg.push_str(": ");
            msg.push_str(detail);
        }
        return Some(msg);
    }

    // 2. Older API / generic: { message, reason? }
    if let Some(message) = parsed.get("message").and_then(Value::as_str)
        && !message.is_empty()
    {
        let mut msg = message.to_owned();
        if let Some(reason) = parsed.get("reason").and_then(Value::as_str)
            && !reason.is_empty()
            && !msg.contains(reason)
        {
            msg.push_str(": ");
            msg.push_str(reason);
        }
        return Some(msg);
    }

    // 3. GraphQL-style: { errors: [{ message, ... }] }
    if let Some(arr) = parsed.get("errors").and_then(Value::as_array)
        && !arr.is_empty()
    {
        let entries: Vec<String> = arr
            .iter()
            .take(3)
            .filter_map(|e| {
                let obj = e.as_object()?;
                obj.get("message")
                    .and_then(Value::as_str)
                    .or_else(|| obj.get("title").and_then(Value::as_str))
                    .map(str::to_owned)
            })
            .collect();
        if !entries.is_empty() {
            let mut joined = entries.join("; ");
            if arr.len() > 3 {
                let extra = arr.len() - 3;
                let _ = write!(joined, "; and {extra} more errors");
            }
            return Some(joined);
        }
    }

    // 4. Jira-style legacy: { errorMessages: [...] }
    if let Some(arr) = parsed.get("errorMessages").and_then(Value::as_array)
        && !arr.is_empty()
    {
        let joined: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
        if !joined.is_empty() {
            return Some(joined.join("; "));
        }
    }

    // 5. statusCode + message fallback. Only reachable when all earlier
    //    branches missed — i.e. no `title`, no `message` (the message
    //    branch above would have won), no usable `errors`/`errorMessages`.
    //    The TS chain makes this the terminal `else if`; we mirror that
    //    here with the same shape so a payload carrying both `statusCode`
    //    and `message` resolves to the bare `message` (it never reaches
    //    this branch). A payload like `{statusCode: 418}` alone falls
    //    through to `None` since there is no `message` to format.
    if let Some(status_code) = parsed.get("statusCode").and_then(Value::as_u64)
        && let Some(message) = parsed.get("message").and_then(Value::as_str)
        && !message.is_empty()
    {
        return Some(format!("{status_code}: {message}"));
    }

    None
}

trait WithMutators {
    fn with_original(self, original: Option<OriginalError>) -> Self;
}

impl WithMutators for McpError {
    fn with_original(mut self, original: Option<OriginalError>) -> Self {
        self.original = original;
        self
    }
}
