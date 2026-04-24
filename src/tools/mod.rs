// Tool descriptions in `descriptions/*.md` are LLM-facing MCP payloads that we
// surface verbatim from the TS reference implementation. Clippy's doc-markdown
// lint is a poor fit here — it would rewrite the prompts we need to keep stable.
#![allow(clippy::doc_markdown)]

//! MCP tool registration for the five generic Bitbucket REST verbs.
//!
//! Ports `src/tools/atlassian.api.tool.ts`. Tool descriptions live in
//! sibling markdown files under `descriptions/` and are pulled in via
//! `include_str!` so the rmcp `#[tool]` macro picks them up through its
//! doc-comment fallback. They are the exact LLM-facing strings from TS —
//! part of the public contract because prompts and evals depend on them.

pub mod args;

use std::sync::Arc;

use reqwest::Client;
use rmcp::{
    ErrorData as RmcpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};

use crate::config::Config;
use crate::constants::{PACKAGE_NAME, VERSION};
use crate::controllers::api::{HandleContext, handle_read, handle_write};
use crate::controllers::handle_clone;
use crate::error::format_error_for_mcp_tool;
use crate::format::truncation::truncate_for_ai;
use crate::transport::{HttpMethod, build_client};
use args::{CloneArgs, ReadArgs, WriteArgs};

const DEFAULT_BASE_URL: &str = "https://api.bitbucket.org";

#[derive(Clone)]
pub struct BitbucketServer {
    state: Arc<ServerState>,
    // The `#[tool_handler]` macro references this field by name at expansion
    // time; the rustc reference tracker doesn't see that, so we silence the
    // dead-code lint explicitly.
    #[allow(dead_code)]
    tool_router: ToolRouter<BitbucketServer>,
}

struct ServerState {
    client: Client,
    config: Config,
    base_url: String,
}

#[tool_router]
impl BitbucketServer {
    /// Standard constructor. Loads config from the environment cascade and
    /// builds a fresh HTTP client.
    pub fn new() -> Result<Self, crate::error::McpError> {
        let config = crate::config::load();
        let client = build_client()?;
        Ok(Self::with_components(config, client, DEFAULT_BASE_URL))
    }

    /// Build a server from caller-supplied components. Useful when tests or
    /// embedders want to pre-configure the `Config` or point at a mock URL.
    pub fn with_components(
        config: Config,
        client: Client,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            state: Arc::new(ServerState {
                client,
                config,
                base_url: base_url.into(),
            }),
            tool_router: Self::tool_router(),
        }
    }

    fn ctx(&self) -> HandleContext<'_> {
        HandleContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.base_url,
        )
    }

    #[doc = include_str!("descriptions/bb_get.md")]
    #[tool(
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true,
        ),
    )]
    async fn bb_get(&self, Parameters(args): Parameters<ReadArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_read(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/bb_post.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        ),
    )]
    async fn bb_post(&self, Parameters(args): Parameters<WriteArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_write(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/bb_put.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true,
        ),
    )]
    async fn bb_put(&self, Parameters(args): Parameters<WriteArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_write(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/bb_patch.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        ),
    )]
    async fn bb_patch(&self, Parameters(args): Parameters<WriteArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_write(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/bb_delete.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true,
        ),
    )]
    async fn bb_delete(&self, Parameters(args): Parameters<ReadArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_read(self, HttpMethod::Delete, &args).await)
    }

    #[doc = include_str!("descriptions/bb_clone.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        ),
    )]
    async fn bb_clone(&self, Parameters(args): Parameters<CloneArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_clone(self, &args).await)
    }
}

#[tool_handler]
impl ServerHandler for BitbucketServer {
    fn get_info(&self) -> ServerInfo {
        let mut implementation = Implementation::default();
        PACKAGE_NAME.clone_into(&mut implementation.name);
        VERSION.clone_into(&mut implementation.version);

        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = implementation;
        info
    }
}

// ---- helpers ----

async fn run_read(server: &BitbucketServer, method: HttpMethod, args: &ReadArgs) -> CallToolResult {
    match handle_read(&server.ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write(server: &BitbucketServer, method: HttpMethod, args: &WriteArgs) -> CallToolResult {
    match handle_write(&server.ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_clone(server: &BitbucketServer, args: &CloneArgs) -> CallToolResult {
    match handle_clone(&server.ctx(), args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

fn error_to_result(err: &crate::error::McpError) -> CallToolResult {
    let formatted = format_error_for_mcp_tool(err);
    let text = formatted
        .content
        .into_iter()
        .next()
        .map_or_else(String::new, |c| c.text);
    CallToolResult::error(vec![Content::text(text)])
}
