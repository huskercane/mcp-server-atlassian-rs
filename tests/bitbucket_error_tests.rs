//! Port of `bitbucket-error-detection.test.ts`. Exercises the controller-level
//! classifier and, at the same time, the body parser used by transport.

use mcp_server_atlassian_bitbucket::error::{
    DetectedError, ErrorCode, ErrorContext, OriginalError, api_error, detect_error_type,
    unexpected,
};
use mcp_server_atlassian_bitbucket::transport::bitbucket_error::{classify, parse_error_body};
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use serde_json::json;

fn ctx() -> ErrorContext {
    ErrorContext::default()
}

fn detected(code: ErrorCode, status: u16) -> DetectedError {
    DetectedError {
        code,
        status_code: status,
    }
}

// ---- Classic `{ error: { message, detail } }` shape ----

#[test]
fn classic_not_found() {
    let body = json!({
        "error": {
            "message": "Repository not found",
            "detail": "The repository does not exist or you do not have access"
        }
    });
    let err = api_error("API Error", Some(404), Some(OriginalError::Json(body)));
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::NotFound, 404)
    );
}

#[test]
fn classic_access_denied() {
    let body = json!({
        "error": {
            "message": "Access denied to this repository",
            "detail": "You need admin permissions to perform this action"
        }
    });
    let err = api_error("API Error", Some(403), Some(OriginalError::Json(body)));
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::AccessDenied, 403)
    );
}

#[test]
fn classic_validation_error() {
    let body = json!({
        "error": {
            "message": "Invalid parameter: repository name",
            "detail": "Repository name can only contain alphanumeric characters"
        }
    });
    let err = api_error("API Error", Some(400), Some(OriginalError::Json(body)));
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::ValidationError, 400)
    );
}

#[test]
fn classic_rate_limit_error() {
    let body = json!({
        "error": {
            "message": "Too many requests",
            "detail": "Rate limit exceeded. Try again later."
        }
    });
    let err = api_error("API Error", Some(429), Some(OriginalError::Json(body)));
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::RateLimitError, 429)
    );
}

// ---- `{ type: "error", ... }` shape ----

#[test]
fn type_error_not_found() {
    let body = json!({"type": "error", "status": 404, "message": "Resource not found"});
    let err = api_error("API Error", Some(404), Some(OriginalError::Json(body)));
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::NotFound, 404)
    );
}

#[test]
fn type_error_forbidden() {
    let body = json!({"type": "error", "status": 403, "message": "Forbidden"});
    let err = api_error("API Error", Some(403), Some(OriginalError::Json(body)));
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::AccessDenied, 403)
    );
}

// ---- `{ errors: [{...}] }` shape ----

#[test]
fn errors_array_validation() {
    let body = json!({
        "errors": [{
            "status": 400,
            "code": "INVALID_REQUEST_PARAMETER",
            "title": "Invalid parameter value",
            "message": "The parameter is not valid"
        }]
    });
    let err = api_error("API Error", Some(400), Some(OriginalError::Json(body)));
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::ValidationError, 400)
    );
}

// ---- Network errors surfaced via message text ----

#[test]
fn network_error_failed_to_fetch() {
    let err = unexpected("Failed to fetch", None);
    assert_eq!(
        detect_error_type(&err, &ctx()),
        detected(ErrorCode::NetworkError, 500)
    );
}

#[test]
fn network_error_common_messages() {
    for msg in [
        "network error occurred",
        "ECONNREFUSED",
        "ENOTFOUND",
        "Network request failed",
        "Failed to fetch",
    ] {
        let err = unexpected(msg.to_string(), None);
        assert_eq!(
            detect_error_type(&err, &ctx()),
            detected(ErrorCode::NetworkError, 500),
            "message: {msg}"
        );
    }
}

// ---- Body parser unit tests ----

#[test]
fn parse_body_composes_message_and_detail() {
    let body = r#"{"type":"error","error":{"message":"Not found","detail":"no such repo"}}"#;
    let parsed = parse_error_body(body);
    assert_eq!(
        parsed.message.as_deref(),
        Some("Not found Detail: no such repo")
    );
    match parsed.original {
        Some(OriginalError::Json(v)) => assert_eq!(v["message"], "Not found"),
        other => panic!("expected Json original, got {other:?}"),
    }
}

#[test]
fn parse_body_handles_flat_message() {
    let body = r#"{"message":"Some error"}"#;
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("Some error"));
}

#[test]
fn parse_body_handles_non_json() {
    let body = "404 page not found";
    let parsed = parse_error_body(body);
    assert_eq!(parsed.message.as_deref(), Some("404 page not found"));
    match parsed.original {
        Some(OriginalError::String(s)) => assert_eq!(s, body),
        other => panic!("expected String original, got {other:?}"),
    }
}

#[test]
fn classify_401_produces_auth_invalid() {
    let err = classify(StatusCode::UNAUTHORIZED, r#"{"error":{"message":"bad cred"}}"#);
    assert_eq!(err.status_code, Some(401));
    assert!(err.message.contains("Authentication failed"));
    assert!(err.message.contains("bad cred"));
}

#[test]
fn classify_429_produces_api_error_with_429() {
    let err = classify(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"message":"slow down"}}"#,
    );
    assert_eq!(err.status_code, Some(429));
    assert!(err.message.contains("Rate limit"));
}
