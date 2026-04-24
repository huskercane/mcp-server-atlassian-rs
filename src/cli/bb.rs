#![allow(clippy::doc_markdown)]

//! Bitbucket Cloud CLI subcommand group (`bb get`, `bb post`, `bb clone`, …).
//!
//! Mirrors the MCP `bb_*` tool surface — same inputs, same controller
//! pipeline, same vendor. The only difference is presentation: stdout
//! print vs the wrapped MCP tool envelope.

use clap::Subcommand;

use crate::cli::api::{ReadOpts, WriteOpts, parse_object, parse_query_params};
use crate::controllers::api::{BitbucketContext, ControllerResponse, HandleContext, handle_request};
use crate::controllers::handle_clone;
use crate::error::McpError;
use crate::tools::args::CloneArgs;
use crate::transport::{HttpMethod, build_client};
use crate::vendor::bitbucket::BitbucketVendor;
use crate::workspace::WorkspaceCache;

/// Verbs exposed under `mcp-atlassian-bitbucket bb …`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// GET any Bitbucket endpoint. Returns the response body to stdout.
    Get(ReadOpts),
    /// POST to any Bitbucket endpoint.
    Post(WriteOpts),
    /// PUT to any Bitbucket endpoint.
    Put(WriteOpts),
    /// PATCH any Bitbucket endpoint.
    Patch(WriteOpts),
    /// DELETE any Bitbucket endpoint. Returns response body if any.
    Delete(ReadOpts),
    /// Clone a Bitbucket repository to your local filesystem using SSH
    /// (preferred) or HTTPS.
    Clone(CloneOpts),
}

#[derive(Debug, Clone, clap::Args)]
pub struct CloneOpts {
    /// Repository slug to clone.
    #[arg(short = 'r', long = "repo-slug")]
    pub repo_slug: String,

    /// Directory path where the repository will be cloned (absolute path
    /// recommended).
    #[arg(short = 't', long = "target-path")]
    pub target_path: String,

    /// Workspace slug containing the repository. Uses the default workspace
    /// when omitted.
    #[arg(short = 'w', long = "workspace-slug")]
    pub workspace_slug: Option<String>,
}

/// Dispatch a `bb` subcommand. Constructs a Bitbucket vendor and prints
/// the rendered response to stdout.
///
/// `clone` requires the Bitbucket-typed context (carries the workspace
/// cache); the generic verbs use the vendor-neutral [`HandleContext`]
/// since they go through `handle_request`.
pub async fn dispatch(command: Command) -> Result<(), McpError> {
    let config = crate::config::load();
    let client = build_client()?;
    let vendor = BitbucketVendor::new();
    let cache = WorkspaceCache::new();

    let response = match command {
        Command::Clone(opts) => {
            let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);
            call_clone(&ctx, opts).await?
        }
        other => {
            let ctx = HandleContext::new(&client, &config, &vendor);
            match other {
                Command::Get(opts) => call_read(&ctx, HttpMethod::Get, opts).await?,
                Command::Delete(opts) => call_read(&ctx, HttpMethod::Delete, opts).await?,
                Command::Post(opts) => call_write(&ctx, HttpMethod::Post, opts).await?,
                Command::Put(opts) => call_write(&ctx, HttpMethod::Put, opts).await?,
                Command::Patch(opts) => call_write(&ctx, HttpMethod::Patch, opts).await?,
                Command::Clone(_) => unreachable!("Clone handled in outer match"),
            }
        }
    };
    println!("{}", response.content);
    Ok(())
}

async fn call_clone(
    ctx: &BitbucketContext<'_>,
    opts: CloneOpts,
) -> Result<ControllerResponse, McpError> {
    let args = CloneArgs {
        workspace_slug: opts.workspace_slug,
        repo_slug: opts.repo_slug,
        target_path: opts.target_path,
    };
    handle_clone(ctx, &args).await
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
