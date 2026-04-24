//! HTTP transport for Bitbucket API calls.
//!
//! Ports `src/utils/transport.util.ts` with full response-contract parity:
//!
//! - Basic auth header built from resolved [`Credentials`](crate::auth::Credentials).
//! - Configurable request timeout via `ATLASSIAN_REQUEST_TIMEOUT`, defaulting
//!   to [`NETWORK_TIMEOUTS::DEFAULT_REQUEST`](crate::constants::network_timeouts).
//! - 10 MB response size cap from the advertised `Content-Length` header
//!   (CWE-770 mitigation).
//! - Response classifier matching TS behaviour:
//!   - `204` → empty object, no raw path
//!   - `text/plain` → raw text pass-through (diffs), no raw path
//!   - empty body → empty object, no raw path
//!   - JSON parse success → parsed value + raw response persisted to disk
//!   - JSON parse failure → raw text, no raw path
//! - Non-ok responses: Bitbucket error body parsed via
//!   [`bitbucket_error`](self::bitbucket_error) and mapped to the typed
//!   [`McpError`](crate::error::McpError) factories.

pub mod bitbucket_error;
pub mod raw_response;

use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use tracing::debug;

use crate::auth::Credentials;
use crate::config::Config;
use crate::constants::{data_limits::MAX_RESPONSE_SIZE, network_timeouts::DEFAULT_REQUEST};
use crate::error::{
    McpError, OriginalError, api_error, auth_invalid, auth_missing_default, unexpected,
};

const BASE_URL: &str = "https://api.bitbucket.org";

/// HTTP verb set accepted by the generic Bitbucket client. Mirrors the TS
/// `RequestOptions.method` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    fn as_reqwest_method(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Put => Method::PUT,
            Self::Patch => Method::PATCH,
            Self::Delete => Method::DELETE,
        }
    }
}

/// Request options for a single Bitbucket call. Matches TS `RequestOptions`.
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    pub method: Option<HttpMethod>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    pub timeout: Option<Duration>,
}

/// What the TS code calls `TransportResponse<T>`. `data` is the successfully
/// parsed body (JSON value, raw text for `text/plain`, or `{}` for empties);
/// `raw_response_path` points at the on-disk persisted JSON body when one was
/// written.
#[derive(Debug, Clone)]
pub struct TransportResponse {
    pub data: ResponseBody,
    pub raw_response_path: Option<std::path::PathBuf>,
}

/// Typed response body. `Json` is the canonical successful case; the other
/// variants preserve the TS contract for diffs and DELETEs.
#[derive(Debug, Clone)]
pub enum ResponseBody {
    Json(Value),
    Text(String),
    Empty,
}

impl ResponseBody {
    pub fn as_json(&self) -> Option<&Value> {
        if let Self::Json(v) = self { Some(v) } else { None }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Main entry point. Matches TS `fetchAtlassian`.
pub async fn fetch_bitbucket(
    client: &Client,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    fetch_bitbucket_with_base(BASE_URL, client, credentials, config, path, options).await
}

/// Variant that lets callers point the transport at a non-default base URL
/// (e.g. Bitbucket Server on-prem, or a local wiremock in tests).
pub async fn fetch_bitbucket_with_base(
    base_url: &str,
    client: &Client,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    let url = normalize_url_with_base(base_url, path);
    let method = options.method.unwrap_or(HttpMethod::Get);

    let auth_header = validate_auth(credentials)?;
    let timeout = resolve_timeout(config, options.timeout);

    let request_body_for_log = options.body.clone();
    let req = build_request(client, method, &url, &auth_header, &options, timeout);

    debug!(%url, method = method.as_str(), "dispatching Bitbucket request");

    let start = std::time::Instant::now();
    let response = req.send().await.map_err(|e| map_reqwest_error(&e, &url))?;
    let duration = start.elapsed();

    enforce_content_length_cap(&response)?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(bitbucket_error::classify(status, &body_text));
    }

    let body = classify_body(response).await?;

    let raw_path = if let ResponseBody::Json(value) = &body {
        raw_response::save(
            &url,
            method.as_str(),
            request_body_for_log.as_ref(),
            value,
            status.as_u16(),
            duration,
        )
    } else {
        None
    };

    Ok(TransportResponse {
        data: body,
        raw_response_path: raw_path,
    })
}

/// Construct a shared reqwest client with sensible defaults. Callers should
/// cache this for the lifetime of the process.
pub fn build_client() -> Result<Client, McpError> {
    Client::builder()
        .user_agent(format!(
            "{}/{}",
            crate::constants::UNSCOPED_PACKAGE_NAME,
            crate::constants::VERSION
        ))
        .build()
        .map_err(|e| unexpected(format!("failed to build HTTP client: {e}"), None))
}

// ---- helpers ----

fn normalize_url_with_base(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let suffix = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("{base}{suffix}")
}

fn validate_auth(credentials: &Credentials) -> Result<HeaderValue, McpError> {
    let raw = credentials.basic_auth_header();
    HeaderValue::from_str(&raw).map_err(|_| auth_invalid("Invalid authentication header"))
}

fn resolve_timeout(config: &Config, override_timeout: Option<Duration>) -> Duration {
    if let Some(t) = override_timeout {
        return t;
    }
    let env_ms = config.get_int(
        "ATLASSIAN_REQUEST_TIMEOUT",
        i64::try_from(DEFAULT_REQUEST.as_millis()).unwrap_or(30_000),
    );
    if env_ms <= 0 {
        DEFAULT_REQUEST
    } else {
        Duration::from_millis(u64::try_from(env_ms).unwrap_or(30_000))
    }
}

fn build_request(
    client: &Client,
    method: HttpMethod,
    url: &str,
    auth: &HeaderValue,
    options: &RequestOptions,
    timeout: Duration,
) -> reqwest::RequestBuilder {
    let mut req = client
        .request(method.as_reqwest_method(), url)
        .timeout(timeout)
        .header(AUTHORIZATION, auth.clone())
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .header(ACCEPT, HeaderValue::from_static("application/json"));

    for (k, v) in &options.headers {
        req = req.header(k, v);
    }

    if let Some(body) = options.body.as_ref() {
        req = req.json(body);
    }
    req
}

fn enforce_content_length_cap(response: &reqwest::Response) -> Result<(), McpError> {
    let Some(value) = response.headers().get(CONTENT_LENGTH) else {
        return Ok(());
    };
    let Ok(text) = value.to_str() else {
        return Ok(());
    };
    let Ok(size) = text.parse::<u64>() else {
        return Ok(());
    };
    let cap = MAX_RESPONSE_SIZE as u64;
    if size > cap {
        let mb = size / (1024 * 1024);
        let cap_mb = cap / (1024 * 1024);
        let info = serde_json::json!({ "responseSize": size, "limit": MAX_RESPONSE_SIZE });
        return Err(api_error(
            format!("Response size ({mb}MB) exceeds maximum limit of {cap_mb}MB"),
            Some(413),
            Some(OriginalError::Json(info)),
        ));
    }
    Ok(())
}

async fn classify_body(response: reqwest::Response) -> Result<ResponseBody, McpError> {
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(ResponseBody::Empty);
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    if content_type.contains("text/plain") {
        let text = response
            .text()
            .await
            .map_err(|e| unexpected(format!("failed to read text body: {e}"), None))?;
        return Ok(ResponseBody::Text(text));
    }

    let text = response
        .text()
        .await
        .map_err(|e| unexpected(format!("failed to read body: {e}"), None))?;

    if text.trim().is_empty() {
        return Ok(ResponseBody::Empty);
    }

    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Ok(ResponseBody::Json(value)),
        Err(_) => Ok(ResponseBody::Text(text)),
    }
}

fn map_reqwest_error(err: &reqwest::Error, url: &str) -> McpError {
    if err.is_timeout() {
        return api_error(
            format!("Request timeout: Bitbucket API did not respond in time at {url}"),
            Some(408),
            Some(OriginalError::String(err.to_string())),
        );
    }
    if err.is_connect() {
        return api_error(
            format!("Network error connecting to Bitbucket API: {err}"),
            Some(503),
            Some(OriginalError::String(err.to_string())),
        );
    }
    unexpected(err.to_string(), Some(OriginalError::String(err.to_string())))
}

/// Exposed for callers that just want a well-formed auth header (e.g. tests
/// and diagnostics). Prefer [`fetch_bitbucket`] for real traffic.
pub fn require_credentials(config: &Config) -> Result<Credentials, McpError> {
    Credentials::resolve(config).ok_or_else(auth_missing_default)
}
