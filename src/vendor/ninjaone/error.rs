use reqwest::StatusCode;
use serde_json::Value;

use crate::error::{McpError, OriginalError, api_error, auth_invalid};

pub fn classify(status: StatusCode, body: &str) -> McpError {
    let original = serde_json::from_str::<Value>(body).map_or_else(
        |_| OriginalError::String(body.to_owned()),
        OriginalError::Json,
    );
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("errorMessage")
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("NinjaOne API error")
                .to_owned()
        });

    match status.as_u16() {
        401 | 403 => {
            let mut error = auth_invalid(format!("NinjaOne authentication failed: {detail}"));
            error.status_code = Some(status.as_u16());
            error.original = Some(original);
            error
        }
        code => api_error(
            format!("NinjaOne API request failed: {detail}"),
            Some(code),
            Some(original),
        ),
    }
}
