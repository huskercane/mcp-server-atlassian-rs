#![allow(clippy::doc_markdown)]

//! Generic API controller. Vendor-neutral: every product-specific concern
//! (base URL, path normalisation, error envelope shape) lives behind the
//! [`Vendor`](crate::vendor::Vendor) trait carried by [`HandleContext`].
//!
//! Pipeline (shared by all five HTTP verbs):
//! 1. Resolve credentials via [`Credentials::require_for_async`]; missing or
//!    keychain-specific failures propagate verbatim.
//! 2. Apply the vendor's path normalisation (Bitbucket prepends `/2.0`;
//!    Jira passes through verbatim).
//! 3. Append the supplied `queryParams` as a URL-encoded query string.
//! 4. Dispatch the request through the vendor-neutral transport, which
//!    handles auth, body classification, raw-response persistence, and
//!    delegates non-2xx parsing to the vendor.
//! 5. Apply the JMESPath filter to the response JSON (pass-through for
//!    text/empty bodies — matches TS behaviour which filters "any").
//! 6. Render as TOON or JSON according to `outputFormat`.

use std::path::PathBuf;

use reqwest::Client;
use serde_json::Value;
use tracing::debug;
use url::form_urlencoded;

use crate::auth::Credentials;
use crate::config::{Config, VENDOR_BITBUCKET};
use crate::error::McpError;
use crate::format::{OutputFormat, jmespath::apply_jq_filter, render};
use crate::tools::args::{QueryParams, ReadArgs, WriteArgs};
use crate::transport::{HttpMethod, RequestOptions, ResponseBody, TransportResponse, fetch};
use crate::vendor::Vendor;
use crate::vendor::bitbucket::BitbucketVendor;
use crate::workspace::WorkspaceCache;

/// Shared dependencies threaded into controller calls. Keeps the pipeline
/// deterministic and avoids hidden singletons.
///
/// `vendor` is borrowed (`&'a dyn Vendor`) so a single owned vendor inside
/// the server state can back many concurrent requests without allocation.
#[derive(Clone, Copy)]
pub struct HandleContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a dyn Vendor,
}

impl<'a> HandleContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a dyn Vendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Bitbucket-only request context. Wraps a [`HandleContext`] whose vendor
/// is statically known to be a [`BitbucketVendor`], plus the per-instance
/// [`WorkspaceCache`] used by `resolve_default_workspace`.
///
/// Operations that are Bitbucket-only by definition (`handle_clone`,
/// workspace resolution) take `&BitbucketContext` rather than the
/// vendor-neutral [`HandleContext`] so the type system rejects calls made
/// with a Jira (or any future) vendor at compile time. Generic API tools
/// like `bb_get`/`bb_post` continue to use [`HandleContext`] via
/// [`Self::handle`].
#[derive(Clone, Copy)]
pub struct BitbucketContext<'a> {
    inner: HandleContext<'a>,
    cache: &'a WorkspaceCache,
}

impl<'a> BitbucketContext<'a> {
    /// Construct a Bitbucket-scoped context. Takes a concrete
    /// `&BitbucketVendor` (not `&dyn Vendor`) so misuse with another
    /// vendor is impossible. A debug-build assertion confirms the vendor
    /// canonical name as belt-and-braces in case the trait impl ever
    /// drifts.
    pub fn new(
        client: &'a Client,
        config: &'a Config,
        vendor: &'a BitbucketVendor,
        cache: &'a WorkspaceCache,
    ) -> Self {
        debug_assert_eq!(
            <BitbucketVendor as Vendor>::name(vendor),
            VENDOR_BITBUCKET,
            "BitbucketVendor::name() must return VENDOR_BITBUCKET"
        );
        Self {
            inner: HandleContext::new(client, config, vendor),
            cache,
        }
    }

    /// Borrow the underlying vendor-neutral context. Used by helpers
    /// that delegate to `transport::fetch` or `handle_request`.
    pub fn handle(&self) -> &HandleContext<'a> {
        &self.inner
    }

    /// Borrow the per-instance workspace cache.
    pub fn cache(&self) -> &'a WorkspaceCache {
        self.cache
    }
}

/// Final response returned to tool/CLI adapters.
#[derive(Debug, Clone)]
pub struct ControllerResponse {
    pub content: String,
    pub raw_response_path: Option<PathBuf>,
}

/// Main entry point for the five verbs. Tool/CLI handlers call this.
pub async fn handle_request(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    path: &str,
    query_params: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let creds = Credentials::require_for_async(ctx.config, ctx.vendor.name()).await?;
    let normalized = normalize_and_append(ctx.vendor, path, query_params);
    debug!(
        %normalized,
        method = method.as_str(),
        vendor = ctx.vendor.name(),
        "controller: dispatching"
    );

    let opts = RequestOptions {
        method: Some(method),
        body,
        ..RequestOptions::default()
    };
    let response: TransportResponse =
        fetch(ctx.client, ctx.vendor, &creds, ctx.config, &normalized, opts).await?;

    Ok(render_response(&response, jq, output_format))
}

/// Convenience wrapper for read-shaped tools. Just dispatches to
/// [`handle_request`] with no body.
pub async fn handle_read(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    args: &ReadArgs,
) -> Result<ControllerResponse, McpError> {
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    handle_request(
        ctx,
        method,
        &args.path,
        args.query_params.as_ref(),
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// Convenience wrapper for write-shaped tools (POST / PUT / PATCH).
pub async fn handle_write(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    args: &WriteArgs,
) -> Result<ControllerResponse, McpError> {
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    handle_request(
        ctx,
        method,
        &args.path,
        args.query_params.as_ref(),
        Some(args.body.clone()),
        args.jq.as_deref(),
        fmt,
    )
    .await
}

fn normalize_and_append(
    vendor: &dyn Vendor,
    path: &str,
    query_params: Option<&QueryParams>,
) -> String {
    let normalized = vendor.normalize_path(path);
    match query_params {
        Some(qp) if !qp.is_empty() => {
            let query: String = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(qp.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .finish();
            let joiner = if normalized.contains('?') { '&' } else { '?' };
            format!("{normalized}{joiner}{query}")
        }
        _ => normalized,
    }
}

/// Bitbucket-specific path normalisation as a free function. Preserved so
/// existing tests (and any external consumers) that assert the `/2.0`
/// prefix behaviour without first constructing a vendor continue to work.
/// Production code should prefer [`Vendor::normalize_path`] via the
/// [`HandleContext`].
pub fn normalize_path(path: &str) -> String {
    BitbucketVendor::new().normalize_path(path)
}

fn render_response(
    response: &TransportResponse,
    jq: Option<&str>,
    format: OutputFormat,
) -> ControllerResponse {
    let content = match &response.data {
        ResponseBody::Json(value) => {
            let filtered = apply_jq_filter(value, jq);
            render(&filtered, format)
        }
        ResponseBody::Text(text) => {
            // TS `applyJqFilter` on a string returns the same string when no
            // filter is supplied; otherwise it fails validation and we surface
            // the raw text. We keep things simple: text bodies pass through.
            text.clone()
        }
        ResponseBody::Empty => render(&Value::Object(serde_json::Map::new()), format),
    };

    ControllerResponse {
        content,
        raw_response_path: response.raw_response_path.clone(),
    }
}
