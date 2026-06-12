// Tool descriptions in `descriptions/*.md` are LLM-facing MCP payloads that we
// surface verbatim from the TS reference implementations. Clippy's doc-markdown
// lint is a poor fit here — it would rewrite the prompts we need to keep stable.
#![allow(clippy::doc_markdown)]

//! MCP tool registration for the Atlassian product surface.
//!
//! [`AtlassianServer`] hosts three `#[tool_router]` impl blocks on the same
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
//! - **`conf_*`** (five generic verbs) — Confluence Cloud, ported from
//!   `@aashari/mcp-server-atlassian-confluence`. Paths are passed through
//!   verbatim (callers supply `/wiki/api/v2/...` or `/wiki/rest/api/...`).
//!   Same `ATLASSIAN_SITE_NAME`-derived base URL as Jira.

pub mod args;

use std::sync::Arc;

use reqwest::Client;
use rmcp::{
    ErrorData as RmcpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    tool, tool_handler, tool_router,
};

use crate::config::Config;
use crate::constants::{PACKAGE_NAME, VERSION};
use crate::controllers::api::{BitbucketContext, HandleContext, handle_read, handle_write};
use crate::controllers::circleci::CircleCiContext;
use crate::controllers::edx::EdxContext;
use crate::controllers::grafana::GrafanaContext;
use crate::controllers::handle_clone;
use crate::controllers::newrelic::NewRelicContext;
use crate::controllers::postman::PostmanContext;
use crate::controllers::slack::SlackContext;
use crate::controllers::zoom::ZoomContext;
use crate::error::format_error_for_mcp_tool;
use crate::format::truncation::truncate_for_ai;
use crate::transport::{HttpMethod, build_client};
use crate::vendor::bitbucket::BitbucketVendor;
use crate::vendor::circleci::CircleCiVendor;
use crate::vendor::confluence::ConfluenceVendor;
use crate::vendor::edx::EdxVendor;
use crate::vendor::grafana::GrafanaVendor;
use crate::vendor::jira::JiraVendor;
use crate::vendor::newrelic::NewRelicVendor;
use crate::vendor::postman::PostmanVendor;
use crate::vendor::slack::SlackVendor;
use crate::vendor::zoom::ZoomVendor;
use crate::workspace::WorkspaceCache;
use args::{
    CloneArgs, EdxDiscussionCommentCreateArgs, EdxDiscussionCommentsArgs, EdxDiscussionCourseArgs,
    EdxDiscussionThreadCreateArgs, EdxDiscussionThreadsArgs, EdxDiscussionTopicsArgs,
    GrafanaListDatasourcesArgs, GrafanaQueryLogsArgs, NewRelicQueryArgs, ReadArgs, WriteArgs,
};

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
    confluence_vendor: ConfluenceVendor,
    zoom_vendor: ZoomVendor,
    circleci_vendor: CircleCiVendor,
    slack_vendor: SlackVendor,
    postman_vendor: PostmanVendor,
    edx_vendor: EdxVendor,
    newrelic_vendor: NewRelicVendor,
    grafana_vendor: GrafanaVendor,
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
            ConfluenceVendor::new(),
            ZoomVendor::new(),
            CircleCiVendor::new(),
            SlackVendor::new(),
            PostmanVendor::new(),
            EdxVendor::new(),
            NewRelicVendor::new(),
            GrafanaVendor::new(),
        ))
    }

    /// Build a server from caller-supplied components. Useful when tests or
    /// embedders want to pre-configure the `Config` or point any vendor
    /// at a mock URL via `with_base_url`.
    // One owned vendor per product — the arg list grows by one with each new
    // vendor. Bundling them into a struct would just move the same fields
    // around without removing any, so the lint is suppressed rather than
    // worked around.
    #[allow(clippy::too_many_arguments)]
    pub fn with_components(
        config: Config,
        client: Client,
        bitbucket_vendor: BitbucketVendor,
        jira_vendor: JiraVendor,
        confluence_vendor: ConfluenceVendor,
        zoom_vendor: ZoomVendor,
        circleci_vendor: CircleCiVendor,
        slack_vendor: SlackVendor,
        postman_vendor: PostmanVendor,
        edx_vendor: EdxVendor,
        newrelic_vendor: NewRelicVendor,
        grafana_vendor: GrafanaVendor,
    ) -> Self {
        Self {
            state: Arc::new(ServerState {
                client,
                config,
                bitbucket_vendor,
                jira_vendor,
                confluence_vendor,
                zoom_vendor,
                circleci_vendor,
                slack_vendor,
                postman_vendor,
                edx_vendor,
                newrelic_vendor,
                grafana_vendor,
                workspace_cache: WorkspaceCache::new(),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Combined router that drives `#[tool_handler]`. Stitches together
    /// the three vendor-scoped routers via the `Add` impl on
    /// [`ToolRouter`](rmcp::handler::server::router::tool::ToolRouter).
    /// Naming this method `tool_router` (the macro's default) lets
    /// `#[tool_handler]` find it without a custom `router = …` attr.
    fn tool_router() -> ToolRouter<Self> {
        Self::bitbucket_router()
            + Self::jira_router()
            + Self::confluence_router()
            + Self::zoom_router()
            + Self::circleci_router()
            + Self::slack_router()
            + Self::postman_router()
            + Self::edx_discussion_router()
            + Self::newrelic_router()
            + Self::grafana_router()
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

    fn confluence_ctx(&self) -> HandleContext<'_> {
        HandleContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.confluence_vendor,
        )
    }

    /// Zoom-specific context. Unlike the Atlassian vendors, Zoom carries its
    /// own credential lifecycle (Server-to-Server OAuth bearer), so it uses a
    /// dedicated [`ZoomContext`] rather than the vendor-neutral
    /// [`HandleContext`].
    fn zoom_ctx(&self) -> ZoomContext<'_> {
        ZoomContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.zoom_vendor,
        )
    }

    /// CircleCI-specific context. Like Zoom, CircleCI carries its own
    /// credential lookup (a static Bearer token from config), so it uses a
    /// dedicated [`CircleCiContext`] rather than the vendor-neutral
    /// [`HandleContext`].
    fn circleci_ctx(&self) -> CircleCiContext<'_> {
        CircleCiContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.circleci_vendor,
        )
    }

    /// Slack-specific context. Like CircleCI, Slack carries its own credential
    /// lookup (a static OAuth token from config), so it uses a dedicated
    /// [`SlackContext`] rather than the vendor-neutral [`HandleContext`].
    fn slack_ctx(&self) -> SlackContext<'_> {
        SlackContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.slack_vendor,
        )
    }

    /// Postman-specific context. Carries its own credential lookup (a static
    /// API key from config) and is the one vendor that authenticates via a
    /// custom `X-API-Key` header, so it uses a dedicated [`PostmanContext`].
    fn postman_ctx(&self) -> PostmanContext<'_> {
        PostmanContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.postman_vendor,
        )
    }

    fn edx_ctx(&self) -> EdxContext<'_> {
        EdxContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.edx_vendor,
        )
    }

    /// New Relic-specific context. Carries its own credential lookup (a static
    /// User API key from config) and authenticates via the custom `API-Key`
    /// header, so it uses a dedicated [`NewRelicContext`].
    fn newrelic_ctx(&self) -> NewRelicContext<'_> {
        NewRelicContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.newrelic_vendor,
        )
    }

    /// Grafana-specific context. Carries its own credential lookup (a static
    /// service-account token from config) and authenticates via
    /// `Authorization: Bearer`, so it uses a dedicated [`GrafanaContext`].
    fn grafana_ctx(&self) -> GrafanaContext<'_> {
        GrafanaContext::new(
            &self.state.client,
            &self.state.config,
            &self.state.grafana_vendor,
        )
    }
}

// ============================================================================
// Bitbucket tools
// ============================================================================

#[tool_router(router = bitbucket_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/bb_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn bb_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_bb(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/bb_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn bb_post(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_bb(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/bb_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn bb_put(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_bb(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/bb_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn bb_patch(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_bb(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/bb_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn bb_delete(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_bb(self, HttpMethod::Delete, &args).await)
    }

    #[doc = include_str!("descriptions/bb_clone.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn bb_clone(
        &self,
        Parameters(args): Parameters<CloneArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_clone(self, &args).await)
    }
}

// ============================================================================
// edX discussion tools
// ============================================================================

#[tool_router(router = edx_discussion_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/edx_discussion_course.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn edx_discussion_course(
        &self,
        Parameters(args): Parameters<EdxDiscussionCourseArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_edx_discussion_course(self, &args).await)
    }

    #[doc = include_str!("descriptions/edx_discussion_topics.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn edx_discussion_topics(
        &self,
        Parameters(args): Parameters<EdxDiscussionTopicsArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_edx_discussion_topics(self, &args).await)
    }

    #[doc = include_str!("descriptions/edx_discussion_threads.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn edx_discussion_threads(
        &self,
        Parameters(args): Parameters<EdxDiscussionThreadsArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_edx_discussion_threads(self, &args).await)
    }

    #[doc = include_str!("descriptions/edx_discussion_thread_create.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn edx_discussion_thread_create(
        &self,
        Parameters(args): Parameters<EdxDiscussionThreadCreateArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_edx_discussion_thread_create(self, &args).await)
    }

    #[doc = include_str!("descriptions/edx_discussion_comments.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn edx_discussion_comments(
        &self,
        Parameters(args): Parameters<EdxDiscussionCommentsArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_edx_discussion_comments(self, &args).await)
    }

    #[doc = include_str!("descriptions/edx_discussion_comment_create.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn edx_discussion_comment_create(
        &self,
        Parameters(args): Parameters<EdxDiscussionCommentCreateArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_edx_discussion_comment_create(self, &args).await)
    }
}

// ============================================================================
// Jira tools
// ============================================================================

#[tool_router(router = jira_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/jira_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn jira_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_jira(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/jira_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn jira_post(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_jira(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/jira_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn jira_put(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_jira(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/jira_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn jira_patch(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_jira(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/jira_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn jira_delete(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_jira(self, HttpMethod::Delete, &args).await)
    }
}

// ============================================================================
// Confluence tools
// ============================================================================

#[tool_router(router = confluence_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/conf_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn conf_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_confluence(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/conf_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn conf_post(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_confluence(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/conf_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn conf_put(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_confluence(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/conf_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn conf_patch(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_confluence(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/conf_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn conf_delete(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_confluence(self, HttpMethod::Delete, &args).await)
    }
}

// ============================================================================
// Zoom tools
// ============================================================================

#[tool_router(router = zoom_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/zoom_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn zoom_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_zoom(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/zoom_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn zoom_post(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_zoom(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/zoom_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn zoom_put(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_zoom(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/zoom_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn zoom_patch(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_zoom(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/zoom_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn zoom_delete(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_zoom(self, HttpMethod::Delete, &args).await)
    }
}

// ============================================================================
// CircleCI tools
// ============================================================================

#[tool_router(router = circleci_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/circleci_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn circleci_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_circleci(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/circleci_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn circleci_post(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_circleci(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/circleci_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn circleci_put(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_circleci(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/circleci_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn circleci_patch(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_circleci(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/circleci_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn circleci_delete(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_circleci(self, HttpMethod::Delete, &args).await)
    }
}

// ============================================================================
// Slack tools
// ============================================================================

#[tool_router(router = slack_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/slack_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn slack_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_slack(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/slack_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn slack_post(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_slack(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/slack_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn slack_put(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_slack(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/slack_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn slack_patch(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_slack(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/slack_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn slack_delete(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_slack(self, HttpMethod::Delete, &args).await)
    }
}

// ============================================================================
// Postman tools
// ============================================================================

#[tool_router(router = postman_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/postman_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn postman_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_postman(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/postman_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn postman_post(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_postman(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/postman_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn postman_put(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_postman(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/postman_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn postman_patch(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_postman(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/postman_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn postman_delete(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_postman(self, HttpMethod::Delete, &args).await)
    }
}

// ============================================================================
// New Relic tools
// ============================================================================

#[tool_router(router = newrelic_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/newrelic_query.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn newrelic_query(
        &self,
        Parameters(args): Parameters<NewRelicQueryArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_newrelic_query(self, &args).await)
    }
}

// ============================================================================
// Grafana tools
// ============================================================================

#[tool_router(router = grafana_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/grafana_query_logs.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn grafana_query_logs(
        &self,
        Parameters(args): Parameters<GrafanaQueryLogsArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_grafana_query_logs(self, &args).await)
    }

    #[doc = include_str!("descriptions/grafana_list_datasources.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn grafana_list_datasources(
        &self,
        Parameters(args): Parameters<GrafanaListDatasourcesArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_grafana_list_datasources(self, &args).await)
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

async fn run_read_confluence(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &ReadArgs,
) -> CallToolResult {
    match handle_read(&server.confluence_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_confluence(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &WriteArgs,
) -> CallToolResult {
    match handle_write(&server.confluence_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_read_zoom(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &ReadArgs,
) -> CallToolResult {
    match crate::controllers::zoom::handle_read(&server.zoom_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_zoom(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &WriteArgs,
) -> CallToolResult {
    match crate::controllers::zoom::handle_write(&server.zoom_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_read_circleci(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &ReadArgs,
) -> CallToolResult {
    match crate::controllers::circleci::handle_read(&server.circleci_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_circleci(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &WriteArgs,
) -> CallToolResult {
    match crate::controllers::circleci::handle_write(&server.circleci_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_read_slack(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &ReadArgs,
) -> CallToolResult {
    match crate::controllers::slack::handle_read(&server.slack_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_slack(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &WriteArgs,
) -> CallToolResult {
    match crate::controllers::slack::handle_write(&server.slack_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_read_postman(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &ReadArgs,
) -> CallToolResult {
    match crate::controllers::postman::handle_read(&server.postman_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_postman(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &WriteArgs,
) -> CallToolResult {
    match crate::controllers::postman::handle_write(&server.postman_ctx(), method, args).await {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_newrelic_query(server: &AtlassianServer, args: &NewRelicQueryArgs) -> CallToolResult {
    match crate::controllers::newrelic::query(&server.newrelic_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_grafana_query_logs(
    server: &AtlassianServer,
    args: &GrafanaQueryLogsArgs,
) -> CallToolResult {
    match crate::controllers::grafana::query_logs(&server.grafana_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_grafana_list_datasources(
    server: &AtlassianServer,
    args: &GrafanaListDatasourcesArgs,
) -> CallToolResult {
    match crate::controllers::grafana::list_datasources(&server.grafana_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_course(
    server: &AtlassianServer,
    args: &EdxDiscussionCourseArgs,
) -> CallToolResult {
    match crate::controllers::edx::course(&server.edx_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_topics(
    server: &AtlassianServer,
    args: &EdxDiscussionTopicsArgs,
) -> CallToolResult {
    match crate::controllers::edx::topics(&server.edx_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_threads(
    server: &AtlassianServer,
    args: &EdxDiscussionThreadsArgs,
) -> CallToolResult {
    match crate::controllers::edx::threads(&server.edx_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_thread_create(
    server: &AtlassianServer,
    args: &EdxDiscussionThreadCreateArgs,
) -> CallToolResult {
    match crate::controllers::edx::create_thread(&server.edx_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_comments(
    server: &AtlassianServer,
    args: &EdxDiscussionCommentsArgs,
) -> CallToolResult {
    match crate::controllers::edx::comments(&server.edx_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_comment_create(
    server: &AtlassianServer,
    args: &EdxDiscussionCommentCreateArgs,
) -> CallToolResult {
    match crate::controllers::edx::create_comment(&server.edx_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_clone(server: &AtlassianServer, args: &CloneArgs) -> CallToolResult {
    match handle_clone(&server.bitbucket_typed_ctx(), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

fn success_response(resp: &crate::controllers::ControllerResponse) -> CallToolResult {
    let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
    CallToolResult::success(vec![Content::text(text)])
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
