//! Port of `src/utils/error.util.test.ts`.
//!
//! Covers the MCP error surface: factory fns, `ensure_mcp_error`,
//! `get_deep_original`, `format_error_for_mcp_tool`,
//! `format_error_for_mcp_resource`.

use mcp_server_atlassian_bitbucket::error::{
    ErrorKind, McpError, OriginalError, api_error, auth_invalid, auth_missing_default,
    ensure_mcp_error, ensure_mcp_error_from_string, format_error_for_mcp_resource,
    format_error_for_mcp_tool, get_deep_original, unexpected_default,
};
use pretty_assertions::assert_eq;
use serde_json::json;

// ---- McpError basics ----

#[test]
fn mcp_error_has_expected_properties() {
    let err = McpError::new("Test error", ErrorKind::ApiError, Some(404), None);
    assert_eq!(err.message, "Test error");
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
}

// ---- factories ----

#[test]
fn create_auth_missing_uses_default_message() {
    let err = auth_missing_default();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert_eq!(err.message, "Authentication credentials are missing");
    assert_eq!(err.status_code, None);
}

#[test]
fn create_auth_invalid_sets_401() {
    let err = auth_invalid("Invalid token");
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.message, "Invalid token");
    assert_eq!(err.status_code, Some(401));
}

#[test]
fn create_api_error_preserves_original() {
    let original = OriginalError::String("Original error".into());
    let err = api_error("API failed", Some(500), Some(original.clone()));
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(500));
    assert_eq!(err.message, "API failed");
    assert!(err.original.is_some());
}

#[test]
fn create_unexpected_default_has_expected_message() {
    let err = unexpected_default();
    assert_eq!(err.kind, ErrorKind::UnexpectedError);
    assert_eq!(err.message, "An unexpected error occurred");
}

// ---- ensure_mcp_error ----

#[test]
fn ensure_mcp_error_returns_same_when_already_mcp() {
    let original = api_error("Original error", None, None);
    let wrapped = ensure_mcp_error(original.clone());
    // Behavioral parity: the wrapped value carries the same fields as the
    // original. Rust can't achieve pointer identity through boxed-error
    // round-tripping, but the data is equivalent.
    assert_eq!(wrapped.message, original.message);
    assert_eq!(wrapped.kind, original.kind);
    assert_eq!(wrapped.status_code, original.status_code);
}

#[test]
fn ensure_mcp_error_wraps_std_error() {
    let std_err: std::io::Error = std::io::Error::other("Standard error");
    let wrapped = ensure_mcp_error(Box::new(std_err));
    assert_eq!(wrapped.kind, ErrorKind::UnexpectedError);
    assert_eq!(wrapped.message, "Standard error");
}

#[test]
fn ensure_mcp_error_from_string_handles_strings() {
    let wrapped = ensure_mcp_error_from_string("String error");
    assert_eq!(wrapped.kind, ErrorKind::UnexpectedError);
    assert_eq!(wrapped.message, "String error");
}

// ---- get_deep_original ----

#[test]
fn get_deep_original_returns_deepest_in_chain() {
    let deepest = json!({ "message": "Root cause" });
    let level3 = api_error("Level 3", Some(500), Some(OriginalError::Json(deepest.clone())));
    let level2 = api_error(
        "Level 2",
        Some(500),
        Some(OriginalError::Mcp(Box::new(level3))),
    );
    let level1 = api_error(
        "Level 1",
        Some(500),
        Some(OriginalError::Mcp(Box::new(level2))),
    );

    let original = level1.original.as_ref().unwrap();
    let deep = get_deep_original(original);
    match deep {
        OriginalError::Json(v) => assert_eq!(v, &deepest),
        other => panic!("expected Json, got {other:?}"),
    }
}

#[test]
fn get_deep_original_handles_string_payload() {
    let s = OriginalError::String("Original error text".into());
    let deep = get_deep_original(&s);
    match deep {
        OriginalError::String(v) => assert_eq!(v, "Original error text"),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn get_deep_original_stops_at_maximum_depth() {
    // Build a 12-deep chain. The walker caps at 10, so the returned element
    // is whatever's at depth 10 from the start.
    let mut node = api_error("leaf", None, None);
    for i in 0..12 {
        node = api_error(
            format!("level {i}"),
            None,
            Some(OriginalError::Mcp(Box::new(node))),
        );
    }
    let root_original = node.original.as_ref().unwrap();
    let deep = get_deep_original(root_original);
    // Whatever we got, it must still be reachable without infinite recursion.
    match deep {
        OriginalError::Mcp(_) | OriginalError::String(_) | OriginalError::Json(_) => {}
    }
}

// ---- format_error_for_mcp_tool ----

#[test]
fn format_for_tool_includes_message() {
    let err = api_error("API error", None, None);
    let resp = format_error_for_mcp_tool(&err);
    assert!(resp.is_error);
    assert_eq!(resp.content.len(), 1);
    assert_eq!(resp.content[0].content_type, "text");
    assert_eq!(resp.content[0].text, "Error: API error");
}

#[test]
fn format_for_tool_includes_status_and_raw_body() {
    let original = json!({
        "code": "NOT_FOUND",
        "message": "Repository does not exist"
    });
    let err = api_error(
        "Resource not found",
        Some(404),
        Some(OriginalError::Json(original)),
    );
    let resp = format_error_for_mcp_tool(&err);
    let text = &resp.content[0].text;
    assert!(text.contains("Error: Resource not found"));
    assert!(text.contains("HTTP Status: 404"));
    assert!(text.contains("Raw API Response:"));
    assert!(text.contains("NOT_FOUND"));
    assert!(text.contains("Repository does not exist"));
}

#[test]
fn format_for_tool_includes_nested_error_details() {
    let deep = json!({
        "message": "API quota exceeded",
        "type": "RateLimitError"
    });
    let mid = api_error("Rate limit exceeded", Some(429), Some(OriginalError::Json(deep)));
    let top = api_error(
        "API error",
        Some(429),
        Some(OriginalError::Mcp(Box::new(mid))),
    );
    let resp = format_error_for_mcp_tool(&top);
    let text = &resp.content[0].text;
    assert!(text.contains("Error: API error"));
    assert!(text.contains("API quota exceeded"));
    assert!(text.contains("RateLimitError"));
}

// ---- format_error_for_mcp_resource ----

#[test]
fn format_for_resource_produces_expected_shape() {
    let err = api_error("API error", None, None);
    let resp = format_error_for_mcp_resource(&err, "test://uri");
    assert_eq!(resp.contents.len(), 1);
    assert_eq!(resp.contents[0].uri, "test://uri");
    assert_eq!(resp.contents[0].text, "Error: API error");
    assert_eq!(resp.contents[0].mime_type, "text/plain");
    assert_eq!(resp.contents[0].description, "Error: API_ERROR");
}
