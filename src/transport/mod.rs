//! HTTP transport for Atlassian product API calls.
//!
//! Vendor-neutral. The base URL, path normalisation, and non-2xx error
//! envelope parsing are all delegated to the [`Vendor`] trait
//! ([`crate::vendor`]). Everything else (auth header, request building,
//! 10 MB response cap, body classification, raw-response persistence) is
//! shared across vendors.
//!
//! Response classifier (matches the TS reference for both Bitbucket and
//! Jira via [`fetch`]):
//! - `204` → empty object, no raw path
//! - `text/plain` → raw text pass-through (e.g. Bitbucket diffs), no raw
//!   path
//! - empty body → empty object, no raw path
//! - JSON parse success → parsed value + raw response persisted to disk
//! - JSON parse failure → raw text, no raw path
//!
//! ## Back-compat shims
//!
//! [`fetch_bitbucket`] and [`fetch_bitbucket_with_base`] are preserved as
//! thin shims that construct a [`BitbucketVendor`] and call [`fetch`].
//! New code should call [`fetch`] directly with the vendor it needs.

pub mod raw_response;

/// Re-export of the Bitbucket error parser at its old path. Kept so
/// downstream tests (`tests/bitbucket_error_tests.rs`) and any external
/// consumers continue to compile after the parser moved into
/// [`crate::vendor::bitbucket::error`].
pub use crate::vendor::bitbucket::error as bitbucket_error;

use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use tracing::debug;

use crate::auth::Credentials;
use crate::config::Config;
use crate::constants::{data_limits::MAX_RESPONSE_SIZE, network_timeouts::DEFAULT_REQUEST};
use crate::error::{
    McpError, OriginalError, api_error, auth_invalid, unexpected,
};
use crate::vendor::Vendor;
use crate::vendor::bitbucket::BitbucketVendor;

/// HTTP verb set accepted by the generic API client. Mirrors the TS
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

/// Request options for a single API call. Matches TS `RequestOptions`.
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

/// Vendor-neutral entry point. Resolves the vendor's base URL, builds the
/// auth header, sends the request, and classifies the response. Non-2xx
/// responses go through [`Vendor::classify_error`] for vendor-specific
/// envelope parsing.
///
/// The `path` parameter is forwarded as-is; callers (typically the
/// controller layer) are expected to have already applied
/// [`Vendor::normalize_path`] so that path normalisation lives in one
/// place. The transport itself only joins base + path.
pub async fn fetch(
    client: &Client,
    vendor: &dyn Vendor,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    let base = vendor.base_url(config)?;
    let url = normalize_url_with_base(&base, path);
    let method = options.method.unwrap_or(HttpMethod::Get);

    let auth_header = validate_auth(credentials)?;
    let timeout = resolve_timeout(config, options.timeout);

    let request_body_for_log = options.body.clone();
    let req = build_request(client, method, &url, &auth_header, &options, timeout);

    debug!(
        %url,
        method = method.as_str(),
        vendor = vendor.name(),
        "dispatching API request"
    );

    let start = std::time::Instant::now();
    let response = req.send().await.map_err(|e| map_reqwest_error(&e, &url))?;
    let duration = start.elapsed();

    enforce_content_length_cap(&response)?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(vendor.classify_error(status, &body_text));
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
        .await
    } else {
        None
    };

    Ok(TransportResponse {
        data: body,
        raw_response_path: raw_path,
    })
}

/// Bitbucket-specialised shim. Equivalent to calling [`fetch`] with a
/// fresh [`BitbucketVendor`]. Preserved for back-compat; new code should
/// call [`fetch`] with the vendor explicitly.
pub async fn fetch_bitbucket(
    client: &Client,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    let vendor = BitbucketVendor::new();
    fetch(client, &vendor, credentials, config, path, options).await
}

/// Bitbucket-specialised shim that overrides the base URL (e.g. to point at
/// a wiremock in tests). Equivalent to calling [`fetch`] with
/// [`BitbucketVendor::with_base_url`].
pub async fn fetch_bitbucket_with_base(
    base_url: &str,
    client: &Client,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    let vendor = BitbucketVendor::with_base_url(base_url);
    fetch(client, &vendor, credentials, config, path, options).await
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
            format!("Request timeout: API did not respond in time at {url}"),
            Some(408),
            Some(OriginalError::String(err.to_string())),
        );
    }
    if err.is_connect() {
        return api_error(
            format!("Network error connecting to API: {err}"),
            Some(503),
            Some(OriginalError::String(err.to_string())),
        );
    }
    unexpected(err.to_string(), Some(OriginalError::String(err.to_string())))
}

/// Exposed for callers that just want a well-formed auth header (e.g. tests
/// and diagnostics). Prefer [`fetch`] for real traffic.
///
/// Synchronous; safe in tests and diagnostics. Async server paths must
/// use [`Credentials::require_async`] so the keychain syscall doesn't
/// block a Tokio worker.
pub fn require_credentials(config: &Config) -> Result<Credentials, McpError> {
    Credentials::require(config)
}
