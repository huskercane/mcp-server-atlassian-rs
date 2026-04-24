// Tool descriptions in `descriptions/*.md` are LLM-facing MCP payloads that we
// surface verbatim from the TS reference implementations. Clippy's doc-markdown
// lint is a poor fit here — it would rewrite the prompts we need to keep stable.
#![allow(clippy::doc_markdown)]

//! MCP tool registration for the Atlassian product surface.
//!
//! [`AtlassianServer`] hosts two `#[tool_router]` impl blocks on the same
//! handler type, then combines them in an inherent
//! [`AtlassianServer::tool_router`] so [`#[tool_handler]`](rmcp::tool_handler)
//! sees a single `ToolRouter` containing every tool:
//!
//! - **`bb_*`** (six tools — five generic verbs + `bb_clone`) — Bitbucket
//!   Cloud, ported from `@aashari/mcp-server-atlassian-bitbucket`. Path
//!   normalisation auto-prepends `/2.0`.
//! - **`jira_*`** (five generic verbs) — Jira Cloud, ported from
//!   `@aashari/mcp-server-atlassian-jira`. Paths are passed through
//!   verbatim (callers supply `/rest/api/3/...`). The base URL is derived
//!   per-request from `ATLASSIAN_SITE_NAME`; Bitbucket-only deployments
//!   are unaffected — Jira tools surface a clear configuration error at
//!   tool-call time only when the env var is missing.

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
use crate::controllers::api::{BitbucketContext, HandleContext, handle_read, handle_write};
use crate::controllers::handle_clone;
use crate::error::format_error_for_mcp_tool;
use crate::format::truncation::truncate_for_ai;
use crate::transport::{HttpMethod, build_client};
use crate::vendor::bitbucket::BitbucketVendor;
use crate::vendor::jira::JiraVendor;
use crate::workspace::WorkspaceCache;
use args::{CloneArgs, ReadArgs, WriteArgs};

#[derive(Clone)]
pub struct AtlassianServer {
    state: Arc<ServerState>,
    // The `#[tool_handler]` macro references this field by name at expansion
    // time; the rustc reference tracker doesn't see that, so we silence the
    // dead-code lint explicitly.
    #[allow(dead_code)]
    tool_router: ToolRouter<AtlassianServer>,
}

struct ServerState {
    client: Client,
    config: Config,
    bitbucket_vendor: BitbucketVendor,
    jira_vendor: JiraVendor,
    /// Per-instance workspace cache. Lives here (not as a process-global
    /// singleton) so multi-server embedders never leak one account's
    /// default workspace into another's lookups.
    workspace_cache: WorkspaceCache,
}

impl AtlassianServer {
    /// Standard constructor. Loads config from the environment cascade and
    /// builds a fresh HTTP client. Both vendors are constructed eagerly,
    /// but neither one resolves its base URL at this point — the
    /// `JiraVendor` defers `ATLASSIAN_SITE_NAME` lookup to per-request
    /// time, so a Bitbucket-only deployment boots without Jira config.
    pub fn new() -> Result<Self, crate::error::McpError> {
        let config = crate::config::load();
        let client = build_client()?;
        Ok(Self::with_components(
            config,
            client,
            BitbucketVendor::new(),
            JiraVendor::new(),
        ))
    }

    /// Build a server from caller-supplied components. Useful when tests or
    /// embedders want to pre-configure the `Config` or point either vendor
    /// at a mock URL via `with_base_url`.
    pub fn with_components(
        config: Config,
        client: Client,
        bitbucket_vendor: BitbucketVendor,
        jira_vendor: JiraVendor,
    ) -> Self {
        Self {
            state: Arc::new(ServerState {
                client,
                config,
                bitbucket_vendor,
                jira_vendor,
                workspace_cache: WorkspaceCache::new(),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Combined router that drives `#[tool_handler]`. Stitches together
    /// the two vendor-scoped routers via the `Add` impl on
    /// [`ToolRouter`](rmcp::handler::server::router::tool::ToolRouter).
    /// Naming this method `tool_router` (the macro's default) lets
    /// `#[tool_handler]` find it without a custom `router = …` attr.
    fn tool_router() -> ToolRouter<Self> {
        Self::bitbucket_router() + Self::jira_router()
    }

    fn bitbucket_ctx(&self) -> HandleContext<'_> {
        HandleContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.bitbucket_vendor,
        )
    }

    /// Bitbucket-only typed context for `bb_clone` and any future
    /// Bitbucket-specific operation. Carries the workspace cache so
    /// `resolve_default_workspace` lookups stay scoped to this server
    /// instance.
    fn bitbucket_typed_ctx(&self) -> BitbucketContext<'_> {
        BitbucketContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.bitbucket_vendor,
            &self.state.workspace_cache,
        )
    }

    fn jira_ctx(&self) -> HandleContext<'_> {
        HandleContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.jira_vendor,
        )
    }
}

// ============================================================================
// Bitbucket tools
// ============================================================================

#[tool_router(router = bitbucket_router)]
impl AtlassianServer {
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
        Ok(run_read_bb(self, HttpMethod::Get, &args).await)
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
        Ok(run_write_bb(self, HttpMethod::Post, &args).await)
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
        Ok(run_write_bb(self, HttpMethod::Put, &args).await)
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
        Ok(run_write_bb(self, HttpMethod::Patch, &args).await)
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
        Ok(run_read_bb(self, HttpMethod::Delete, &args).await)
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

// ============================================================================
// Jira tools
// ============================================================================

#[tool_router(router = jira_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/jira_get.md")]
    #[tool(
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true,
        ),
    )]
    async fn jira_get(&self, Parameters(args): Parameters<ReadArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_jira(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/jira_post.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        ),
    )]
    async fn jira_post(&self, Parameters(args): Parameters<WriteArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_jira(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/jira_put.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true,
        ),
    )]
    async fn jira_put(&self, Parameters(args): Parameters<WriteArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_jira(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/jira_patch.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true,
        ),
    )]
    async fn jira_patch(&self, Parameters(args): Parameters<WriteArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_jira(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/jira_delete.md")]
    #[tool(
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true,
        ),
    )]
    async fn jira_delete(&self, Parameters(args): Parameters<ReadArgs>) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_jira(self, HttpMethod::Delete, &args).await)
    }
}

#[tool_handler]
impl ServerHandler for AtlassianServer {
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

async fn run_read_bb(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &ReadArgs,
) -> CallToolResult {
    match handle_read(&server.bitbucket_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_bb(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &WriteArgs,
) -> CallToolResult {
    match handle_write(&server.bitbucket_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_read_jira(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &ReadArgs,
) -> CallToolResult {
    match handle_read(&server.jira_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_jira(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &WriteArgs,
) -> CallToolResult {
    match handle_write(&server.jira_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_clone(server: &AtlassianServer, args: &CloneArgs) -> CallToolResult {
    match handle_clone(&server.bitbucket_typed_ctx(), args).await {
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
