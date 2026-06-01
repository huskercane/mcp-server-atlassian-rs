//! Zoom controller path.
//!
//! Zoom does not use the shared [`Credentials::require_for_async`] resolver:
//! it resolves its Server-to-Server OAuth credentials and exchanges them for a
//! short-lived bearer (cached and auto-renewed by
//! [`ZoomVendor`](crate::vendor::zoom::ZoomVendor)), then injects a
//! [`Credentials::Bearer`] into the shared dispatch path
//! ([`dispatch_with_creds`]). Everything after auth — path normalisation,
//! query encoding, transport, error classification, output rendering — is the
//! same code the Atlassian vendors use.

use reqwest::Client;
use serde_json::Value;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::{QueryParams, ReadArgs, WriteArgs};
use crate::transport::HttpMethod;
use crate::vendor::zoom::ZoomVendor;

/// Zoom-specific request context. Carries the concrete [`ZoomVendor`] (not a
/// `&dyn Vendor`) so the token lifecycle can be driven, plus the shared client
/// and config.
pub struct ZoomContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a ZoomVendor,
}

impl<'a> ZoomContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a ZoomVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Resolve a bearer (exchanging/caching as needed), then dispatch the request.
/// Kept as an `async fn` — there is a `?` on the bearer resolution before the
/// dispatch await, so the single-tail-await `impl Future` optimisation does
/// not apply.
pub async fn handle_request(
    ctx: &ZoomContext<'_>,
    method: HttpMethod,
    path: &str,
    query_params: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.bearer(ctx.client, ctx.config).await?;
    let creds = Credentials::Bearer { token };
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        method,
        path,
        query_params,
        body,
        jq,
        output_format,
    )
    .await
}

/// Read-shaped convenience wrapper (no body).
pub async fn handle_read(
    ctx: &ZoomContext<'_>,
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

/// Write-shaped convenience wrapper (POST / PUT / PATCH).
pub async fn handle_write(
    ctx: &ZoomContext<'_>,
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
