//! Generic NinjaOne request controller.

use reqwest::Client;
use serde_json::Value;

use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::{NinjaOneReadArgs, NinjaOneWriteArgs, QueryParams};
use crate::transport::HttpMethod;
use crate::vendor::ninjaone::NinjaOneVendor;

pub struct NinjaOneContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a NinjaOneVendor,
}

impl<'a> NinjaOneContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a NinjaOneVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn request(
    ctx: &NinjaOneContext<'_>,
    server: Option<&str>,
    method: HttpMethod,
    path: &str,
    query: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let credentials = ctx.vendor.credentials(ctx.config)?;
    let vendor = ctx.vendor.for_server(server);
    let handle = HandleContext::new(ctx.client, ctx.config, &vendor);
    dispatch_with_creds(
        &handle,
        &credentials,
        method,
        path,
        query,
        body,
        jq,
        output_format,
    )
    .await
}

pub async fn handle_read(
    ctx: &NinjaOneContext<'_>,
    method: HttpMethod,
    args: &NinjaOneReadArgs,
) -> Result<ControllerResponse, McpError> {
    request(
        ctx,
        args.server.as_deref(),
        method,
        &args.path,
        args.query_params.as_ref(),
        None,
        args.jq.as_deref(),
        args.output_format.map_or(OutputFormat::Toon, Into::into),
    )
    .await
}

pub async fn handle_write(
    ctx: &NinjaOneContext<'_>,
    method: HttpMethod,
    args: &NinjaOneWriteArgs,
) -> Result<ControllerResponse, McpError> {
    request(
        ctx,
        args.server.as_deref(),
        method,
        &args.path,
        args.query_params.as_ref(),
        Some(args.body.clone()),
        args.jq.as_deref(),
        args.output_format.map_or(OutputFormat::Toon, Into::into),
    )
    .await
}
