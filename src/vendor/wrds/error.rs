#![allow(clippy::doc_markdown)]

//! WRDS error classification.
//!
//! WRDS is PostgreSQL, so failures arrive as [`tokio_postgres::Error`] rather
//! than an HTTP response. This module maps them onto the same [`McpError`]
//! envelope the HTTP vendors produce, so tool output is consistent:
//!
//! - **Server-side (db) errors** carry a SQLSTATE code. Authentication failures
//!   (bad password / role) become [`auth_invalid`]; missing schema/table/column
//!   and syntax/privilege/timeout errors become [`api_error`] with the HTTP
//!   status the equivalent REST failure would have used (404 / 400 / 403 / 504)
//!   so `McpError::mcp_code` classifies them the same way.
//! - **Client-side errors** (TLS handshake, connection refused, I/O) have no
//!   SQLSTATE and become a generic [`api_error`] connection failure.

use tokio_postgres::error::SqlState;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

/// Map a [`tokio_postgres::Error`] (from connect or query) onto an [`McpError`].
pub fn classify(err: &tokio_postgres::Error) -> McpError {
    let Some(db) = err.as_db_error() else {
        // No SQLSTATE: TLS / connection / I/O failure raised client-side.
        return api_error(
            format!("WRDS connection failed: {err}"),
            None,
            Some(OriginalError::String(err.to_string())),
        );
    };

    let code = db.code();
    let mut message = db.message().to_owned();
    if let Some(detail) = db.detail() {
        message.push_str(" — ");
        message.push_str(detail);
    }
    if let Some(hint) = db.hint() {
        message.push_str(" (hint: ");
        message.push_str(hint);
        message.push(')');
    }

    if code == &SqlState::INVALID_PASSWORD || code == &SqlState::INVALID_AUTHORIZATION_SPECIFICATION
    {
        return auth_invalid(format!("WRDS authentication failed: {message}"));
    }
    if code == &SqlState::INSUFFICIENT_PRIVILEGE {
        return api_error(format!("WRDS access denied: {message}"), Some(403), None);
    }
    if code == &SqlState::UNDEFINED_TABLE
        || code == &SqlState::UNDEFINED_COLUMN
        || code == &SqlState::UNDEFINED_OBJECT
        || code == &SqlState::UNDEFINED_FUNCTION
        || code == &SqlState::INVALID_SCHEMA_NAME
    {
        return api_error(format!("WRDS: {message}"), Some(404), None);
    }
    if code == &SqlState::QUERY_CANCELED {
        return api_error(
            "WRDS query canceled (statement timeout exceeded)".to_owned(),
            Some(504),
            None,
        );
    }
    // Syntax errors and other access-rule violations are caller-fixable input
    // problems → 400, mirroring how the REST vendors surface a bad request.
    if code.code().starts_with("42") {
        return api_error(format!("WRDS query rejected: {message}"), Some(400), None);
    }

    api_error(
        format!("WRDS query failed: {message}"),
        None,
        Some(OriginalError::String(format!("SQLSTATE {}", code.code()))),
    )
}
