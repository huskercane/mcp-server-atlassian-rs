#![allow(clippy::doc_markdown)]

//! CircleCI controller path.
//!
//! CircleCI does not use the shared [`Credentials::require_for_async`]
//! resolver: it reads its static personal API token from config (via
//! [`CircleCiVendor::token`](crate::vendor::circleci::CircleCiVendor::token))
//! and injects a [`Credentials::Bearer`] into the shared dispatch path
//! ([`dispatch_with_creds`]) — the scheme CircleCI's v2 API recommends.
//! Everything after auth — path normalisation, query encoding, transport,
//! error classification, output rendering — is the same code the Atlassian
//! vendors use.

use reqwest::Client;
use serde_json::Value;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::{QueryParams, ReadArgs, WriteArgs};
use crate::transport::HttpMethod;
use crate::vendor::circleci::CircleCiVendor;

/// CircleCI-specific request context. Carries the concrete [`CircleCiVendor`]
/// (not a `&dyn Vendor`) so the token read can be driven, plus the shared
/// client and config.
pub struct CircleCiContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a CircleCiVendor,
}

impl<'a> CircleCiContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a CircleCiVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Resolve the API token, then dispatch the request. Kept as an `async fn` —
/// there is a `?` on the token resolution before the dispatch await, so the
/// single-tail-await `impl Future` optimisation does not apply.
pub async fn handle_request(
    ctx: &CircleCiContext<'_>,
    method: HttpMethod,
    path: &str,
    query_params: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config)?;
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
    ctx: &CircleCiContext<'_>,
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
    ctx: &CircleCiContext<'_>,
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
