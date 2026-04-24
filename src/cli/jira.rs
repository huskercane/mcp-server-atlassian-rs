#![allow(clippy::doc_markdown)]

//! Jira Cloud CLI subcommand group (`jira get`, `jira post`, …).
//!
//! Mirrors the MCP `jira_*` tool surface. There is no `clone` verb —
//! Jira has no repos. The `ATLASSIAN_SITE_NAME` env var is required at
//! tool-call time; an unconfigured site surfaces as a clear
//! authentication error rather than a CLI argument error.

use clap::Subcommand;

use crate::cli::api::{ReadOpts, WriteOpts, parse_object, parse_query_params};
use crate::controllers::api::{ControllerResponse, HandleContext, handle_request};
use crate::error::McpError;
use crate::transport::{HttpMethod, build_client};
use crate::vendor::jira::JiraVendor;

/// Verbs exposed under `mcp-atlassian-bitbucket jira …`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// GET any Jira endpoint. Returns the response body to stdout.
    Get(ReadOpts),
    /// POST to any Jira endpoint.
    Post(WriteOpts),
    /// PUT to any Jira endpoint.
    Put(WriteOpts),
    /// PATCH any Jira endpoint.
    Patch(WriteOpts),
    /// DELETE any Jira endpoint. Returns response body if any.
    Delete(ReadOpts),
}

/// Dispatch a `jira` subcommand. Constructs a Jira vendor and prints
/// the rendered response to stdout.
pub async fn dispatch(command: Command) -> Result<(), McpError> {
    let config = crate::config::load();
    let client = build_client()?;
    let vendor = JiraVendor::new();
    let ctx = HandleContext::new(&client, &config, &vendor);

    let response = match command {
        Command::Get(opts) => call_read(&ctx, HttpMethod::Get, opts).await?,
        Command::Delete(opts) => call_read(&ctx, HttpMethod::Delete, opts).await?,
        Command::Post(opts) => call_write(&ctx, HttpMethod::Post, opts).await?,
        Command::Put(opts) => call_write(&ctx, HttpMethod::Put, opts).await?,
        Command::Patch(opts) => call_write(&ctx, HttpMethod::Patch, opts).await?,
    };
    println!("{}", response.content);
    Ok(())
}

async fn call_read(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    opts: ReadOpts,
) -> Result<ControllerResponse, McpError> {
    let query_params = parse_query_params(opts.query_params.as_deref())?;
    handle_request(
        ctx,
        method,
        &opts.path,
        query_params.as_ref(),
        None,
        opts.jq.as_deref(),
        opts.output_format.into(),
    )
    .await
}

async fn call_write(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    opts: WriteOpts,
) -> Result<ControllerResponse, McpError> {
    let body = parse_object(&opts.body, "body")?;
    let query_params = parse_query_params(opts.query_params.as_deref())?;
    handle_request(
        ctx,
        method,
        &opts.path,
        query_params.as_ref(),
        Some(body),
        opts.jq.as_deref(),
        opts.output_format.into(),
    )
    .await
}
