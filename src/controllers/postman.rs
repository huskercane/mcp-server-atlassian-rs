#![allow(clippy::doc_markdown)]

//! Postman controller path.
//!
//! Postman reads its static API key from config (via
//! [`PostmanVendor::key`](crate::vendor::postman::PostmanVendor::key)) and
//! injects a [`Credentials::ApiKeyHeader`] — carrying Postman's `X-API-Key`
//! header name — into the shared dispatch path ([`dispatch_with_creds`]). It is
//! the one vendor that authenticates outside the `Authorization` header;
//! everything after auth — path normalisation, query encoding, transport,
//! error classification, output rendering — is the same code the other vendors
//! use.

use reqwest::Client;
use serde_json::Value;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::{QueryParams, ReadArgs, WriteArgs};
use crate::transport::HttpMethod;
use crate::vendor::postman::{API_KEY_HEADER, PostmanVendor};

/// Postman-specific request context. Carries the concrete [`PostmanVendor`]
/// (not a `&dyn Vendor`) so the key read can be driven, plus the shared client
/// and config.
pub struct PostmanContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a PostmanVendor,
}

impl<'a> PostmanContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a PostmanVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Resolve the API key, then dispatch the request. Kept as an `async fn` —
/// there is a `?` on the key resolution before the dispatch await, so the
/// single-tail-await `impl Future` optimisation does not apply.
pub async fn handle_request(
    ctx: &PostmanContext<'_>,
    method: HttpMethod,
    path: &str,
    query_params: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let key = ctx.vendor.key(ctx.config)?;
    let creds = Credentials::ApiKeyHeader {
        header_name: API_KEY_HEADER.to_owned(),
        key,
    };
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
    ctx: &PostmanContext<'_>,
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
    ctx: &PostmanContext<'_>,
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
