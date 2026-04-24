//! Error types for the Bitbucket MCP server.
//!
//! This module mirrors the TypeScript `error.util.ts` + `error-handler.util.ts`
//! surface so the MCP tool output text matches the reference implementation
//! byte-for-byte (inspected by existing integration tests and LLM prompts).

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

/// Error classification used internally to drive MCP error-code mapping.
///
/// Mirrors TS `enum ErrorType` in `src/utils/error.util.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    AuthMissing,
    AuthInvalid,
    ApiError,
    UnexpectedError,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthMissing => "AUTH_MISSING",
            Self::AuthInvalid => "AUTH_INVALID",
            Self::ApiError => "API_ERROR",
            Self::UnexpectedError => "UNEXPECTED_ERROR",
        }
    }
}

/// MCP-facing error code string (matches TS `McpErrorType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpErrorCode {
    AuthenticationRequired,
    NotFound,
    ValidationError,
    RateLimitExceeded,
    ApiError,
    UnexpectedError,
}

impl McpErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            Self::NotFound => "NOT_FOUND",
            Self::ValidationError => "VALIDATION_ERROR",
            Self::RateLimitExceeded => "RATE_LIMIT_EXCEEDED",
            Self::ApiError => "API_ERROR",
            Self::UnexpectedError => "UNEXPECTED_ERROR",
        }
    }
}

/// Wrapped "original" error payload, matching the TS `originalError: unknown`
/// semantics while keeping Rust's type system happy.
///
/// The Bitbucket transport layer stores either the vendor JSON body (most
/// common), a raw response string, or a nested `McpError` in this slot. The
/// MCP error formatter renders JSON with two-space indent and strings as-is.
#[derive(Debug, Clone)]
pub enum OriginalError {
    Mcp(Box<McpError>),
    Json(Value),
    String(String),
}

impl OriginalError {
    /// Render for inclusion in MCP tool error text. Matches TS behavior:
    /// objects -> pretty JSON (2-space), strings -> raw.
    pub fn render(&self) -> Option<String> {
        match self {
            Self::Json(v) => serde_json::to_string_pretty(v).ok(),
            Self::String(s) => Some(s.clone()),
            Self::Mcp(_) => None,
        }
    }
}

/// Typed application error. Wraps a message, classification, optional HTTP
/// status, and an optional original error payload (vendor body or chained
/// `McpError`).
#[derive(Debug, Clone)]
pub struct McpError {
    pub message: String,
    pub kind: ErrorKind,
    pub status_code: Option<u16>,
    pub original: Option<OriginalError>,
}

impl McpError {
    pub fn new(
        message: impl Into<String>,
        kind: ErrorKind,
        status_code: Option<u16>,
        original: Option<OriginalError>,
    ) -> Self {
        Self {
            message: message.into(),
            kind,
            status_code,
            original,
        }
    }

    /// Public MCP-facing error code derived from kind + status.
    /// Matches TS constructor logic in `McpError`.
    pub fn mcp_code(&self) -> McpErrorCode {
        match self.kind {
            ErrorKind::AuthMissing | ErrorKind::AuthInvalid => {
                McpErrorCode::AuthenticationRequired
            }
            ErrorKind::ApiError => match self.status_code {
                Some(404) => McpErrorCode::NotFound,
                Some(429) => McpErrorCode::RateLimitExceeded,
                _ => McpErrorCode::ApiError,
            },
            ErrorKind::UnexpectedError => McpErrorCode::UnexpectedError,
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for McpError {}

// ---------- factory fns (match TS create*Error helpers) ----------

pub fn auth_missing(message: impl Into<String>) -> McpError {
    McpError::new(message, ErrorKind::AuthMissing, None, None)
}

pub fn auth_missing_default() -> McpError {
    auth_missing("Authentication credentials are missing")
}

pub fn auth_invalid(message: impl Into<String>) -> McpError {
    McpError::new(message, ErrorKind::AuthInvalid, Some(401), None)
}

pub fn auth_invalid_default() -> McpError {
    auth_invalid("Authentication credentials are invalid")
}

pub fn api_error(
    message: impl Into<String>,
    status_code: Option<u16>,
    original: Option<OriginalError>,
) -> McpError {
    McpError::new(message, ErrorKind::ApiError, status_code, original)
}

pub fn unexpected(message: impl Into<String>, original: Option<OriginalError>) -> McpError {
    McpError::new(message, ErrorKind::UnexpectedError, None, original)
}

pub fn unexpected_default() -> McpError {
    unexpected("An unexpected error occurred", None)
}

/// Coerce any `std::error::Error` into an `McpError`. Matches TS
/// `ensureMcpError`: if already our type, returns as-is; otherwise wraps as
/// `UnexpectedError` and preserves the original message.
pub fn ensure_mcp_error<E: Into<Box<dyn std::error::Error + Send + Sync>>>(err: E) -> McpError {
    let boxed: Box<dyn std::error::Error + Send + Sync> = err.into();
    if let Some(existing) = boxed.downcast_ref::<McpError>() {
        return existing.clone();
    }
    let msg = boxed.to_string();
    unexpected(msg.clone(), Some(OriginalError::String(msg)))
}

/// String-source variant so callers that hold `&str`/`String` don't have to
/// box manually. Matches TS fallback `createUnexpectedError(String(error))`.
pub fn ensure_mcp_error_from_string(s: impl Into<String>) -> McpError {
    let s = s.into();
    unexpected(s, None)
}

/// Walk the `original` chain as long as it stays `McpError`, returning the
/// deepest reachable payload. Terminates at a non-`McpError` payload or when
/// the chain runs out. Matches TS `getDeepOriginalError` with `maxDepth = 10`.
pub fn get_deep_original(root: &OriginalError) -> &OriginalError {
    let mut current = root;
    for _ in 0..10 {
        let OriginalError::Mcp(inner) = current else {
            return current;
        };
        let Some(next) = inner.original.as_ref() else {
            return current;
        };
        current = next;
    }
    current
}

// ---------- MCP tool/resource output formatters ----------

/// Shape of an MCP tool error response. Matches the over-the-wire JSON of the
/// TS MCP SDK (`content: [{type, text}], isError: true`).
#[derive(Debug, Clone, Serialize)]
pub struct ToolErrorResponse {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: &'static str,
    pub text: String,
}

/// Format an error for an MCP tool response with the full vendor payload
/// inlined. Matches TS `formatErrorForMcpTool` byte-for-byte:
///
/// - Always prefixes `Error: <message>`
/// - Appends `HTTP Status: <code>` when present
/// - Appends `Raw API Response:\n<pretty JSON | raw string>` when a distinct
///   deep original is available
pub fn format_error_for_mcp_tool(err: &McpError) -> ToolErrorResponse {
    let mut text = format!("Error: {}", err.message);

    if let Some(status) = err.status_code {
        // `write!` into String is infallible.
        let _ = write!(text, "\nHTTP Status: {status}");
    }

    if let Some(original) = err.original.as_ref() {
        let deep = get_deep_original(original);
        let body = match deep {
            OriginalError::Json(v) => {
                // Skip if it stringifies to the same text as the message.
                let rendered = serde_json::to_string_pretty(v).unwrap_or_default();
                if rendered == err.message {
                    None
                } else {
                    Some(rendered)
                }
            }
            OriginalError::String(s) if s != &err.message => Some(s.clone()),
            OriginalError::String(_) => None,
            OriginalError::Mcp(inner) => {
                // The chain terminated in an McpError with no .original.
                // TS behavior: JSON.stringify stringifies an Error's enumerable
                // fields; the most useful fallback is the nested message.
                if inner.message == err.message {
                    None
                } else {
                    Some(inner.message.clone())
                }
            }
        };

        if let Some(body) = body {
            text.push_str("\n\nRaw API Response:\n");
            text.push_str(&body);
        }
    }

    ToolErrorResponse {
        content: vec![ToolContent {
            content_type: "text",
            text,
        }],
        is_error: true,
    }
}

/// MCP resource error shape. Matches TS `formatErrorForMcpResource`.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceErrorResponse {
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceContent {
    pub uri: String,
    pub text: String,
    #[serde(rename = "mimeType")]
    pub mime_type: &'static str,
    pub description: String,
}

/// Render a human-readable multi-line error message suitable for CLI stderr
/// output. Matches TS `handleCliError` without the process-exit side effect.
pub fn format_cli_error(err: &McpError) -> String {
    use std::fmt::Write as _;

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Error: {}", err.message));
    if let Some(status) = err.status_code {
        lines.push(format!("HTTP Status: {status}"));
    }
    lines.push("---".to_owned());

    match err.kind {
        ErrorKind::AuthMissing => {
            lines.push(
                "Tip: Make sure to set up your Atlassian credentials in the configuration file or environment variables:"
                    .to_owned(),
            );
            lines.push(
                "- ATLASSIAN_SITE_NAME, ATLASSIAN_USER_EMAIL, and ATLASSIAN_API_TOKEN; or"
                    .to_owned(),
            );
            lines.push(
                "- ATLASSIAN_BITBUCKET_USERNAME and ATLASSIAN_BITBUCKET_APP_PASSWORD".to_owned(),
            );
        }
        ErrorKind::AuthInvalid => {
            lines.push(
                "Tip: Check that your Atlassian API token or app password is correct and has not expired."
                    .to_owned(),
            );
            lines.push(
                "Also verify that the configured user has access to the requested resource."
                    .to_owned(),
            );
        }
        ErrorKind::ApiError if err.status_code == Some(429) => {
            lines.push(
                "Tip: You may have exceeded your Bitbucket API rate limits. Try again later."
                    .to_owned(),
            );
        }
        _ => {}
    }

    if let Some(original) = err.original.as_ref() {
        let deep = get_deep_original(original);
        lines.push("Bitbucket API Error:".to_owned());
        lines.push("```".to_owned());
        match deep {
            OriginalError::Json(value) => match extract_vendor_error(value) {
                Some(lines_from_vendor) => lines.extend(lines_from_vendor),
                None => lines.push(serde_json::to_string_pretty(value).unwrap_or_default()),
            },
            OriginalError::String(s) => lines.push(s.trim().to_owned()),
            OriginalError::Mcp(inner) => {
                let _ = write!(lines.last_mut().unwrap(), "{}", inner.message);
            }
        }
        lines.push("```".to_owned());
    }

    if std::env::var("DEBUG").ok().is_none_or(|v| !v.contains("mcp:")) {
        lines.push(
            "For more detailed error information, run with DEBUG=mcp:* environment variable."
                .to_owned(),
        );
    }

    lines.join("\n")
}

fn extract_vendor_error(value: &Value) -> Option<Vec<String>> {
    let obj = value.as_object()?;
    if let Some(err_obj) = obj.get("error").and_then(Value::as_object) {
        let mut out = Vec::new();
        let message = err_obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Unknown error");
        out.push(format!("Message: {message}"));
        if let Some(detail) = err_obj.get("detail").and_then(Value::as_str) {
            out.push(format!("Detail: {detail}"));
        }
        return Some(out);
    }
    if let Some(message) = obj.get("message").and_then(Value::as_str) {
        return Some(vec![message.to_owned()]);
    }
    None
}

pub fn format_error_for_mcp_resource(err: &McpError, uri: impl Into<String>) -> ResourceErrorResponse {
    ResourceErrorResponse {
        contents: vec![ResourceContent {
            uri: uri.into(),
            text: format!("Error: {}", err.message),
            mime_type: "text/plain",
            description: format!("Error: {}", err.kind.as_str()),
        }],
    }
}

// ---------- error-handler.util.ts port (controller-level classification) ----------

/// Standard error codes for controller-level classification. Matches TS
/// `ErrorCode` enum in `error-handler.util.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotFound,
    InvalidCursor,
    AccessDenied,
    ValidationError,
    UnexpectedError,
    NetworkError,
    RateLimitError,
    PrivateIpError,
    ReservedRangeError,
}

/// Context for controller error handling. Matches TS `ErrorContext`.
#[derive(Debug, Clone, Default)]
pub struct ErrorContext {
    pub entity_type: Option<String>,
    pub operation: Option<String>,
    pub source: Option<String>,
    pub entity_id: Option<EntityId>,
    pub additional_info: Option<Value>,
}

#[derive(Debug, Clone)]
pub enum EntityId {
    Single(String),
    Map(Vec<(String, String)>),
}

impl EntityId {
    pub fn display(&self) -> String {
        match self {
            Self::Single(s) => s.clone(),
            Self::Map(pairs) => pairs
                .iter()
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
                .join("/"),
        }
    }
}

/// Detected error type + mapped HTTP status. Matches TS `detectErrorType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectedError {
    pub code: ErrorCode,
    pub status_code: u16,
}

/// Classify an error for the controller layer. Inspects the message text,
/// explicit status code, and (when present) the Bitbucket-shaped original
/// payload (`{error:{message,detail}}`, `{type:"error",status}`, `{errors:[…]}`).
#[allow(clippy::too_many_lines)]
pub fn detect_error_type(err: &McpError, _ctx: &ErrorContext) -> DetectedError {
    let msg = err.message.to_lowercase();
    let status = err.status_code;

    // Pull request ID validation errors
    if err.message.contains("Invalid pull request ID")
        || err.message.contains("Pull request ID must be a positive integer")
    {
        return DetectedError {
            code: ErrorCode::ValidationError,
            status_code: 400,
        };
    }

    // Network error detection
    if is_network_error_text(&msg) {
        return DetectedError {
            code: ErrorCode::NetworkError,
            status_code: 500,
        };
    }

    // Rate limiting on message or explicit 429
    if msg.contains("rate limit") || msg.contains("too many requests") || status == Some(429) {
        return DetectedError {
            code: ErrorCode::RateLimitError,
            status_code: 429,
        };
    }

    // Bitbucket-shaped vendor payloads
    if let Some(original) = err.original.as_ref()
        && let OriginalError::Json(root) = get_deep_original(original)
        && let Some(detected) = classify_bitbucket_error(root)
    {
        return detected;
    }

    // Not found fallback
    if msg.contains("not found") || msg.contains("does not exist") || status == Some(404) {
        return DetectedError {
            code: ErrorCode::NotFound,
            status_code: 404,
        };
    }

    // Access denied fallback
    if msg.contains("access")
        || msg.contains("permission")
        || msg.contains("authorize")
        || msg.contains("authentication")
        || status == Some(401)
        || status == Some(403)
    {
        return DetectedError {
            code: ErrorCode::AccessDenied,
            status_code: status.unwrap_or(403),
        };
    }

    // Invalid cursor
    if (msg.contains("cursor") || msg.contains("startat") || msg.contains("page"))
        && (msg.contains("invalid") || msg.contains("not valid"))
    {
        return DetectedError {
            code: ErrorCode::InvalidCursor,
            status_code: 400,
        };
    }

    // Validation
    if msg.contains("validation")
        || msg.contains("invalid")
        || msg.contains("required")
        || status == Some(400)
        || status == Some(422)
    {
        return DetectedError {
            code: ErrorCode::ValidationError,
            status_code: status.unwrap_or(400),
        };
    }

    DetectedError {
        code: ErrorCode::UnexpectedError,
        status_code: status.unwrap_or(500),
    }
}

fn is_network_error_text(msg: &str) -> bool {
    msg.contains("network error")
        || msg.contains("fetch failed")
        || msg.contains("econnrefused")
        || msg.contains("enotfound")
        || msg.contains("failed to fetch")
        || msg.contains("network request failed")
}

fn classify_bitbucket_error(root: &Value) -> Option<DetectedError> {
    let obj = root.as_object()?;

    if let Some(err) = obj.get("error").and_then(Value::as_object)
        && let Some(d) = classify_error_object(err)
    {
        return Some(d);
    }

    if obj.get("type").and_then(Value::as_str) == Some("error")
        && let Some(status) = obj.get("status").and_then(Value::as_u64)
        && let Ok(status) = u16::try_from(status)
    {
        return map_status_to_error(status);
    }

    if let Some(arr) = obj.get("errors").and_then(Value::as_array)
        && let Some(first) = arr.first().and_then(Value::as_object)
        && let Some(d) = classify_error_array_entry(first)
    {
        return Some(d);
    }

    None
}

fn classify_error_object(err: &serde_json::Map<String, Value>) -> Option<DetectedError> {
    let err_msg = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();

    if contains_any(
        &err_msg,
        &[
            "repository not found",
            "does not exist",
            "no such resource",
            "not found",
        ],
    ) {
        return Some(DetectedError {
            code: ErrorCode::NotFound,
            status_code: 404,
        });
    }
    if contains_any(
        &err_msg,
        &[
            "access",
            "permission",
            "credentials",
            "unauthorized",
            "forbidden",
            "authentication",
        ],
    ) {
        return Some(DetectedError {
            code: ErrorCode::AccessDenied,
            status_code: 403,
        });
    }
    if err_msg.contains("invalid")
        || (err_msg.contains("parameter") && err_msg.contains("error"))
        || err_msg.contains("input")
        || err_msg.contains("validation")
        || err_msg.contains("required field")
        || err_msg.contains("bad request")
    {
        return Some(DetectedError {
            code: ErrorCode::ValidationError,
            status_code: 400,
        });
    }
    if contains_any(&err_msg, &["rate limit", "too many requests", "throttled"]) {
        return Some(DetectedError {
            code: ErrorCode::RateLimitError,
            status_code: 429,
        });
    }
    None
}

fn classify_error_array_entry(first: &serde_json::Map<String, Value>) -> Option<DetectedError> {
    if let Some(status) = first.get("status").and_then(Value::as_u64)
        && let Ok(status) = u16::try_from(status)
        && let Some(d) = map_status_to_error(status)
    {
        return Some(d);
    }
    let text = first
        .get("title")
        .or_else(|| first.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    if text.contains("not found") {
        return Some(DetectedError {
            code: ErrorCode::NotFound,
            status_code: 404,
        });
    }
    if text.contains("access") || text.contains("permission") {
        return Some(DetectedError {
            code: ErrorCode::AccessDenied,
            status_code: 403,
        });
    }
    if text.contains("invalid") || text.contains("required") {
        return Some(DetectedError {
            code: ErrorCode::ValidationError,
            status_code: 400,
        });
    }
    if text.contains("rate limit") || text.contains("too many requests") {
        return Some(DetectedError {
            code: ErrorCode::RateLimitError,
            status_code: 429,
        });
    }
    None
}

fn map_status_to_error(status: u16) -> Option<DetectedError> {
    match status {
        404 => Some(DetectedError {
            code: ErrorCode::NotFound,
            status_code: 404,
        }),
        401 | 403 => Some(DetectedError {
            code: ErrorCode::AccessDenied,
            status_code: status,
        }),
        400 => Some(DetectedError {
            code: ErrorCode::ValidationError,
            status_code: 400,
        }),
        429 => Some(DetectedError {
            code: ErrorCode::RateLimitError,
            status_code: 429,
        }),
        _ => None,
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}
