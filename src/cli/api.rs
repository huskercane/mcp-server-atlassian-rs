#![allow(clippy::doc_markdown)]

//! `get` / `post` / `put` / `patch` / `delete` CLI subcommands. These shell
//! out to the same controller pipeline the MCP tools use, so behaviour is
//! identical modulo presentation (stdout print vs wrapped tool response).

use clap::Subcommand;
use serde_json::Value;

use crate::controllers::api::{
    ControllerResponse, HandleContext, handle_request,
};
use crate::controllers::handle_clone;
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::{CloneArgs, QueryParams};
use crate::transport::{HttpMethod, build_client};

/// The subcommands exposed by the binary when argv is non-empty.
#[derive(Debug, Subcommand)]
pub enum ApiCommand {
    /// GET any Bitbucket endpoint. Returns filtered output to stdout.
    Get(ReadOpts),
    /// POST to any Bitbucket endpoint.
    Post(WriteOpts),
    /// PUT to any Bitbucket endpoint.
    Put(WriteOpts),
    /// PATCH any Bitbucket endpoint.
    Patch(WriteOpts),
    /// DELETE any Bitbucket endpoint. Returns response body (if any).
    Delete(ReadOpts),
    /// Clone a Bitbucket repository to your local filesystem using SSH
    /// (preferred) or HTTPS.
    Clone(CloneOpts),
}

#[derive(Debug, Clone, clap::Args)]
pub struct CloneOpts {
    /// Repository slug to clone.
    #[arg(short = 'r', long = "repo-slug")]
    repo_slug: String,

    /// Directory path where the repository will be cloned (absolute path
    /// recommended).
    #[arg(short = 't', long = "target-path")]
    target_path: String,

    /// Workspace slug containing the repository. Uses default workspace if
    /// not provided.
    #[arg(short = 'w', long = "workspace-slug")]
    workspace_slug: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct ReadOpts {
    /// API endpoint path (e.g., "/workspaces", "/repositories/{workspace}/{repo}").
    #[arg(short = 'p', long = "path")]
    path: String,

    /// Query parameters as JSON string (e.g., '{"pagelen": "25"}').
    #[arg(short = 'q', long = "query-params")]
    query_params: Option<String>,

    /// JMESPath expression to filter/transform the response.
    #[arg(long = "jq")]
    jq: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct WriteOpts {
    /// API endpoint path (e.g., "/repositories/{workspace}/{repo}/pullrequests").
    #[arg(short = 'p', long = "path")]
    path: String,

    /// Request body as JSON string.
    #[arg(short = 'b', long = "body")]
    body: String,

    /// Query parameters as JSON string.
    #[arg(short = 'q', long = "query-params")]
    query_params: Option<String>,

    /// JMESPath expression to filter/transform the response.
    #[arg(long = "jq")]
    jq: Option<String>,
}

pub async fn dispatch(command: ApiCommand) -> Result<(), McpError> {
    let config = crate::config::load();
    let client = build_client()?;
    let base = "https://api.bitbucket.org";
    let ctx = HandleContext::new(&client, &config, base);

    let response = match command {
        ApiCommand::Get(opts) => call_read(&ctx, HttpMethod::Get, opts).await?,
        ApiCommand::Delete(opts) => call_read(&ctx, HttpMethod::Delete, opts).await?,
        ApiCommand::Post(opts) => call_write(&ctx, HttpMethod::Post, opts).await?,
        ApiCommand::Put(opts) => call_write(&ctx, HttpMethod::Put, opts).await?,
        ApiCommand::Patch(opts) => call_write(&ctx, HttpMethod::Patch, opts).await?,
        ApiCommand::Clone(opts) => call_clone(&ctx, opts).await?,
    };
    println!("{}", response.content);
    Ok(())
}

async fn call_clone(
    ctx: &HandleContext<'_>,
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
        OutputFormat::Toon,
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
        OutputFormat::Toon,
    )
    .await
}

/// Parse a JSON string that must decode to an object. Matches TS `parseJson`:
/// rejects arrays, null, or primitives.
pub fn parse_object(json: &str, field: &str) -> Result<Value, McpError> {
    let value: Value = serde_json::from_str(json).map_err(|_| {
        crate::error::unexpected(
            format!("Invalid JSON in --{field}. Please provide valid JSON."),
            None,
        )
    })?;
    if !value.is_object() {
        let kind = match &value {
            Value::Null => "null",
            Value::Array(_) => "array",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Object(_) => unreachable!(),
        };
        return Err(crate::error::unexpected(
            format!("Invalid --{field}: expected a JSON object, got {kind}."),
            None,
        ));
    }
    Ok(value)
}

/// Parse `--query-params` JSON into a string-to-string map. Anything else
/// surfaces as a JSON-validation error to the user.
pub fn parse_query_params(input: Option<&str>) -> Result<Option<QueryParams>, McpError> {
    let Some(raw) = input else { return Ok(None) };
    let value = parse_object(raw, "query-params")?;
    let obj = value.as_object().expect("parse_object guarantees object");
    let mut out = QueryParams::new();
    for (k, v) in obj {
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            _ => {
                return Err(crate::error::unexpected(
                    format!(
                        "Invalid --query-params: value for \"{k}\" must be a string, boolean, or number."
                    ),
                    None,
                ));
            }
        };
        out.insert(k.clone(), s);
    }
    Ok(Some(out))
}
