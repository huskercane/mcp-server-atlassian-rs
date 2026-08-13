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

use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use reqwest::Client;
use rmcp::{
    ErrorData as RmcpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock as Content, Implementation, ProtocolVersion,
        ServerCapabilities, ServerInfo,
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
use crate::controllers::ninjaone::NinjaOneContext;
use crate::controllers::postman::PostmanContext;
use crate::controllers::slack::SlackContext;
use crate::controllers::sonarqube::SonarqubeContext;
use crate::controllers::splunk::SplunkContext;
#[cfg(feature = "wrds")]
use crate::controllers::wrds::WrdsContext;
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
use crate::vendor::ninjaone::NinjaOneVendor;
use crate::vendor::postman::PostmanVendor;
use crate::vendor::slack::SlackVendor;
use crate::vendor::sonarqube::SonarqubeVendor;
use crate::vendor::splunk::SplunkVendor;
#[cfg(feature = "wrds")]
use crate::vendor::wrds::WrdsVendor;
use crate::vendor::zoom::ZoomVendor;
use crate::workspace::WorkspaceCache;
use args::{
    CircleCiLogsArgs, CloneArgs, EdxDiscussionCommentCreateArgs, EdxDiscussionCommentsArgs,
    EdxDiscussionCourseArgs, EdxDiscussionThreadCreateArgs, EdxDiscussionThreadsArgs,
    EdxDiscussionTopicsArgs, GrafanaListDatasourcesArgs, GrafanaQueryLogsArgs, NewRelicQueryArgs,
    NinjaOneReadArgs, NinjaOneWriteArgs, ReadArgs, SonarqubeQualityGateArgs,
    SonarqubeSearchIssuesArgs, SplunkCreateJobArgs, SplunkJobResultsArgs,
    SplunkListSavedSearchesArgs, SplunkSearchArgs, WriteArgs,
};
#[cfg(feature = "wrds")]
use args::{WrdsDescribeTableArgs, WrdsListLibrariesArgs, WrdsListTablesArgs, WrdsQueryArgs};

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
    config: RwLock<Config>,
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
    sonarqube_vendor: SonarqubeVendor,
    splunk_vendor: SplunkVendor,
    ninjaone_vendor: NinjaOneVendor,
    /// WRDS (PostgreSQL) vendor. Feature-gated: a `--no-default-features` build
    /// drops the Postgres dependency tree entirely, so this field and the
    /// `wrds_*` tools simply don't exist. Constructed internally (it is cheap
    /// and stateless apart from a lazily-built TLS config) so the public
    /// `with_components` signature doesn't change with the feature.
    #[cfg(feature = "wrds")]
    wrds_vendor: WrdsVendor,
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
        // Snapshot the file before loading it so a change racing startup is
        // still observed by the watcher after the server is constructed.
        let watched_config = crate::config::global::default_path()
            .filter(|path| path.exists())
            .map(|path| {
                let contents = std::fs::read(&path).ok();
                (path, contents)
            });
        let config = crate::config::load();
        let client = build_client()?;
        let server = Self::with_components(
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
            SonarqubeVendor::new(),
            SplunkVendor::new(),
            NinjaOneVendor::new(),
        );
        if let Some((path, contents)) = watched_config {
            spawn_config_watcher(&server.state, path, contents);
        }
        Ok(server)
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
        sonarqube_vendor: SonarqubeVendor,
        splunk_vendor: SplunkVendor,
        ninjaone_vendor: NinjaOneVendor,
    ) -> Self {
        Self {
            state: Arc::new(ServerState {
                client,
                config: RwLock::new(config),
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
                sonarqube_vendor,
                splunk_vendor,
                ninjaone_vendor,
                #[cfg(feature = "wrds")]
                wrds_vendor: WrdsVendor::new(),
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
        let router = Self::bitbucket_router()
            + Self::jira_router()
            + Self::confluence_router()
            + Self::zoom_router()
            + Self::circleci_router()
            + Self::slack_router()
            + Self::postman_router()
            + Self::edx_discussion_router()
            + Self::newrelic_router()
            + Self::grafana_router()
            + Self::sonarqube_router()
            + Self::splunk_router();
        let router = router + Self::ninjaone_router();
        // WRDS tools only exist when the `wrds` feature is on (default).
        #[cfg(feature = "wrds")]
        let router = router + Self::wrds_router();
        router
    }

    fn config(&self) -> Config {
        self.state
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn bitbucket_ctx<'a>(&'a self, config: &'a Config) -> HandleContext<'a> {
        HandleContext::new(&self.state.client, config, &self.state.bitbucket_vendor)
    }

    /// Bitbucket-only typed context for `bb_clone` and any future
    /// Bitbucket-specific operation. Carries the workspace cache so
    /// `resolve_default_workspace` lookups stay scoped to this server
    /// instance.
    fn bitbucket_typed_ctx<'a>(&'a self, config: &'a Config) -> BitbucketContext<'a> {
        BitbucketContext::new(
            &self.state.client,
            config,
            &self.state.bitbucket_vendor,
            &self.state.workspace_cache,
        )
    }

    fn jira_ctx<'a>(&'a self, config: &'a Config) -> HandleContext<'a> {
        HandleContext::new(&self.state.client, config, &self.state.jira_vendor)
    }

    fn confluence_ctx<'a>(&'a self, config: &'a Config) -> HandleContext<'a> {
        HandleContext::new(&self.state.client, config, &self.state.confluence_vendor)
    }

    /// Zoom-specific context. Unlike the Atlassian vendors, Zoom carries its
    /// own credential lifecycle (Server-to-Server OAuth bearer), so it uses a
    /// dedicated [`ZoomContext`] rather than the vendor-neutral
    /// [`HandleContext`].
    fn zoom_ctx<'a>(&'a self, config: &'a Config) -> ZoomContext<'a> {
        ZoomContext::new(&self.state.client, config, &self.state.zoom_vendor)
    }

    /// CircleCI-specific context. Like Zoom, CircleCI carries its own
    /// credential lookup (a static Bearer token from config), so it uses a
    /// dedicated [`CircleCiContext`] rather than the vendor-neutral
    /// [`HandleContext`].
    fn circleci_ctx<'a>(&'a self, config: &'a Config) -> CircleCiContext<'a> {
        CircleCiContext::new(&self.state.client, config, &self.state.circleci_vendor)
    }

    /// Slack-specific context. Like CircleCI, Slack carries its own credential
    /// lookup (a static OAuth token from config), so it uses a dedicated
    /// [`SlackContext`] rather than the vendor-neutral [`HandleContext`].
    fn slack_ctx<'a>(&'a self, config: &'a Config) -> SlackContext<'a> {
        SlackContext::new(&self.state.client, config, &self.state.slack_vendor)
    }

    /// Postman-specific context. Carries its own credential lookup (a static
    /// API key from config) and is the one vendor that authenticates via a
    /// custom `X-API-Key` header, so it uses a dedicated [`PostmanContext`].
    fn postman_ctx<'a>(&'a self, config: &'a Config) -> PostmanContext<'a> {
        PostmanContext::new(&self.state.client, config, &self.state.postman_vendor)
    }

    fn edx_ctx<'a>(&'a self, config: &'a Config) -> EdxContext<'a> {
        EdxContext::new(&self.state.client, config, &self.state.edx_vendor)
    }

    /// New Relic-specific context. Carries its own credential lookup (a static
    /// User API key from config) and authenticates via the custom `API-Key`
    /// header, so it uses a dedicated [`NewRelicContext`].
    fn newrelic_ctx<'a>(&'a self, config: &'a Config) -> NewRelicContext<'a> {
        NewRelicContext::new(&self.state.client, config, &self.state.newrelic_vendor)
    }

    /// Grafana-specific context. Carries its own credential lookup (a static
    /// service-account token from config) and authenticates via
    /// `Authorization: Bearer`, so it uses a dedicated [`GrafanaContext`].
    fn grafana_ctx<'a>(&'a self, config: &'a Config) -> GrafanaContext<'a> {
        GrafanaContext::new(&self.state.client, config, &self.state.grafana_vendor)
    }

    /// SonarQube-specific context. Like CircleCI/Grafana, Sonar carries its own
    /// credential lookup (a static user token from config) and authenticates via
    /// `Authorization: Bearer`, so it uses a dedicated [`SonarqubeContext`].
    fn sonarqube_ctx<'a>(&'a self, config: &'a Config) -> SonarqubeContext<'a> {
        SonarqubeContext::new(&self.state.client, config, &self.state.sonarqube_vendor)
    }

    fn splunk_ctx<'a>(&'a self, config: &'a Config) -> SplunkContext<'a> {
        SplunkContext::new(&self.state.client, config, &self.state.splunk_vendor)
    }

    fn ninjaone_ctx<'a>(&'a self, config: &'a Config) -> NinjaOneContext<'a> {
        NinjaOneContext::new(&self.state.client, config, &self.state.ninjaone_vendor)
    }

    /// WRDS-specific context. WRDS is PostgreSQL, not HTTP, so this context
    /// carries no `reqwest::Client` — just config and the Postgres vendor.
    #[cfg(feature = "wrds")]
    fn wrds_ctx<'a>(&'a self, config: &'a Config) -> WrdsContext<'a> {
        WrdsContext::new(config, &self.state.wrds_vendor)
    }
}

const CONFIG_WATCH_INTERVAL: Duration = Duration::from_millis(500);

fn spawn_config_watcher(
    state: &Arc<ServerState>,
    path: PathBuf,
    mut last_contents: Option<Vec<u8>>,
) {
    let state: Weak<ServerState> = Arc::downgrade(state);
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(path = %path.display(), "global config watcher requires a Tokio runtime");
        return;
    };
    runtime.spawn(async move {
        let mut interval = tokio::time::interval(CONFIG_WATCH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval` ticks immediately; the first comparison also closes the
        // startup race between the initial snapshot and Config::load().
        loop {
            interval.tick().await;
            let Some(state) = state.upgrade() else {
                return;
            };
            let contents = match tokio::fs::read(&path).await {
                Ok(contents) => Some(contents),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "failed to watch global config");
                    continue;
                }
            };
            if contents == last_contents {
                continue;
            }
            last_contents = contents;

            // An editor may expose a partially-written file briefly. Keep the
            // last known-good snapshot and retry when the bytes change again.
            if path.exists()
                && let Err(err) =
                    crate::config::global::read_all_vendors(&path, crate::constants::PACKAGE_NAME)
            {
                tracing::warn!(path = %path.display(), error = %err, "global config changed but is not valid JSON; keeping previous config");
                continue;
            }

            let config = crate::config::load_from_global_path(Some(&path));
            *state
                .config
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
            state.workspace_cache.clear();
            tracing::info!(path = %path.display(), "reloaded global config");
        }
    });
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

    #[doc = include_str!("descriptions/circleci_logs.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn circleci_logs(
        &self,
        Parameters(args): Parameters<CircleCiLogsArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_circleci_logs(self, &args).await)
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

// ============================================================================
// SonarQube tools
// ============================================================================

#[tool_router(router = sonarqube_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/sonarqube_quality_gate.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn sonarqube_quality_gate(
        &self,
        Parameters(args): Parameters<SonarqubeQualityGateArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_sonarqube_quality_gate(self, &args).await)
    }

    #[doc = include_str!("descriptions/sonarqube_search_issues.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn sonarqube_search_issues(
        &self,
        Parameters(args): Parameters<SonarqubeSearchIssuesArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_sonarqube_search_issues(self, &args).await)
    }

    #[doc = include_str!("descriptions/sonarqube_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn sonarqube_get(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_sonarqube_get(self, &args).await)
    }
}

// ============================================================================
// Splunk tools
// ============================================================================

#[tool_router(router = splunk_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/splunk_search.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn splunk_search(
        &self,
        Parameters(args): Parameters<SplunkSearchArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_splunk_search(self, &args).await)
    }

    #[doc = include_str!("descriptions/splunk_create_job.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn splunk_create_job(
        &self,
        Parameters(args): Parameters<SplunkCreateJobArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_splunk_create_job(self, &args).await)
    }

    #[doc = include_str!("descriptions/splunk_job_results.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn splunk_job_results(
        &self,
        Parameters(args): Parameters<SplunkJobResultsArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_splunk_job_results(self, &args).await)
    }

    #[doc = include_str!("descriptions/splunk_list_saved_searches.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn splunk_list_saved_searches(
        &self,
        Parameters(args): Parameters<SplunkListSavedSearchesArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_splunk_list_saved_searches(self, &args).await)
    }
}

// ============================================================================
// NinjaOne tools
// ============================================================================

#[tool_router(router = ninjaone_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/ninjaone_get.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn ninjaone_get(
        &self,
        Parameters(args): Parameters<NinjaOneReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_ninjaone(self, HttpMethod::Get, &args).await)
    }

    #[doc = include_str!("descriptions/ninjaone_post.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn ninjaone_post(
        &self,
        Parameters(args): Parameters<NinjaOneWriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_ninjaone(self, HttpMethod::Post, &args).await)
    }

    #[doc = include_str!("descriptions/ninjaone_put.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn ninjaone_put(
        &self,
        Parameters(args): Parameters<NinjaOneWriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_ninjaone(self, HttpMethod::Put, &args).await)
    }

    #[doc = include_str!("descriptions/ninjaone_patch.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = true,
    ))]
    async fn ninjaone_patch(
        &self,
        Parameters(args): Parameters<NinjaOneWriteArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_write_ninjaone(self, HttpMethod::Patch, &args).await)
    }

    #[doc = include_str!("descriptions/ninjaone_delete.md")]
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = true,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn ninjaone_delete(
        &self,
        Parameters(args): Parameters<NinjaOneReadArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_read_ninjaone(self, HttpMethod::Delete, &args).await)
    }
}

// ============================================================================
// WRDS tools (feature = "wrds")
// ============================================================================

#[cfg(feature = "wrds")]
#[tool_router(router = wrds_router)]
impl AtlassianServer {
    #[doc = include_str!("descriptions/wrds_query.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn wrds_query(
        &self,
        Parameters(args): Parameters<WrdsQueryArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_wrds_query(self, &args).await)
    }

    #[doc = include_str!("descriptions/wrds_list_libraries.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn wrds_list_libraries(
        &self,
        Parameters(args): Parameters<WrdsListLibrariesArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_wrds_list_libraries(self, &args).await)
    }

    #[doc = include_str!("descriptions/wrds_list_tables.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn wrds_list_tables(
        &self,
        Parameters(args): Parameters<WrdsListTablesArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_wrds_list_tables(self, &args).await)
    }

    #[doc = include_str!("descriptions/wrds_describe_table.md")]
    #[tool(annotations(
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = true,
    ))]
    async fn wrds_describe_table(
        &self,
        Parameters(args): Parameters<WrdsDescribeTableArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(run_wrds_describe_table(self, &args).await)
    }
}

#[tool_handler]
impl ServerHandler for AtlassianServer {
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, RmcpError> {
        let mut tools = self.tool_router.list_all();
        // MCP 2026-07-28 recommends deterministic ordering so clients can
        // reuse prompt caches. The list is build-static and principal-agnostic,
        // making a short public cache safe.
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(rmcp::model::ListToolsResult::with_all_items(tools)
            .with_ttl_ms(300_000)
            .with_cache_scope(rmcp::model::CacheScope::Public))
    }

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
    let config = server.config();
    match handle_read(&server.bitbucket_ctx(&config), method, args).await {
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
    let config = server.config();
    match handle_write(&server.bitbucket_ctx(&config), method, args).await {
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
    let config = server.config();
    match handle_read(&server.jira_ctx(&config), method, args).await {
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
    let config = server.config();
    match handle_write(&server.jira_ctx(&config), method, args).await {
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
    let config = server.config();
    match handle_read(&server.confluence_ctx(&config), method, args).await {
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
    let config = server.config();
    match handle_write(&server.confluence_ctx(&config), method, args).await {
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
    let config = server.config();
    match crate::controllers::zoom::handle_read(&server.zoom_ctx(&config), method, args).await {
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
    let config = server.config();
    match crate::controllers::zoom::handle_write(&server.zoom_ctx(&config), method, args).await {
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
    let config = server.config();
    match crate::controllers::circleci::handle_read(&server.circleci_ctx(&config), method, args)
        .await
    {
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
    let config = server.config();
    match crate::controllers::circleci::handle_write(&server.circleci_ctx(&config), method, args)
        .await
    {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_circleci_logs(server: &AtlassianServer, args: &CircleCiLogsArgs) -> CallToolResult {
    let config = server.config();
    match crate::controllers::circleci::handle_logs(&server.circleci_ctx(&config), args).await {
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
    let config = server.config();
    match crate::controllers::slack::handle_read(&server.slack_ctx(&config), method, args).await {
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
    let config = server.config();
    match crate::controllers::slack::handle_write(&server.slack_ctx(&config), method, args).await {
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
    let config = server.config();
    match crate::controllers::postman::handle_read(&server.postman_ctx(&config), method, args).await
    {
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
    let config = server.config();
    match crate::controllers::postman::handle_write(&server.postman_ctx(&config), method, args)
        .await
    {
        Ok(resp) => {
            let text = truncate_for_ai(&resp.content, resp.raw_response_path.as_deref());
            CallToolResult::success(vec![Content::text(text)])
        }
        Err(err) => error_to_result(&err),
    }
}

async fn run_read_ninjaone(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &NinjaOneReadArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::ninjaone::handle_read(&server.ninjaone_ctx(&config), method, args)
        .await
    {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_write_ninjaone(
    server: &AtlassianServer,
    method: HttpMethod,
    args: &NinjaOneWriteArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::ninjaone::handle_write(&server.ninjaone_ctx(&config), method, args)
        .await
    {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_newrelic_query(server: &AtlassianServer, args: &NewRelicQueryArgs) -> CallToolResult {
    let config = server.config();
    match crate::controllers::newrelic::query(&server.newrelic_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_grafana_query_logs(
    server: &AtlassianServer,
    args: &GrafanaQueryLogsArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::grafana::query_logs(&server.grafana_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_grafana_list_datasources(
    server: &AtlassianServer,
    args: &GrafanaListDatasourcesArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::grafana::list_datasources(&server.grafana_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_sonarqube_quality_gate(
    server: &AtlassianServer,
    args: &SonarqubeQualityGateArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::sonarqube::quality_gate(&server.sonarqube_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_sonarqube_search_issues(
    server: &AtlassianServer,
    args: &SonarqubeSearchIssuesArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::sonarqube::search_issues(&server.sonarqube_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_sonarqube_get(server: &AtlassianServer, args: &ReadArgs) -> CallToolResult {
    let config = server.config();
    match crate::controllers::sonarqube::get(&server.sonarqube_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_splunk_search(server: &AtlassianServer, args: &SplunkSearchArgs) -> CallToolResult {
    let config = server.config();
    match crate::controllers::splunk::search(&server.splunk_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_splunk_create_job(
    server: &AtlassianServer,
    args: &SplunkCreateJobArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::splunk::create_job(&server.splunk_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_splunk_job_results(
    server: &AtlassianServer,
    args: &SplunkJobResultsArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::splunk::job_results(&server.splunk_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_splunk_list_saved_searches(
    server: &AtlassianServer,
    args: &SplunkListSavedSearchesArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::splunk::list_saved_searches(&server.splunk_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

#[cfg(feature = "wrds")]
async fn run_wrds_query(server: &AtlassianServer, args: &WrdsQueryArgs) -> CallToolResult {
    let config = server.config();
    match crate::controllers::wrds::query(&server.wrds_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

#[cfg(feature = "wrds")]
async fn run_wrds_list_libraries(
    server: &AtlassianServer,
    args: &WrdsListLibrariesArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::wrds::list_libraries(&server.wrds_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

#[cfg(feature = "wrds")]
async fn run_wrds_list_tables(
    server: &AtlassianServer,
    args: &WrdsListTablesArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::wrds::list_tables(&server.wrds_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

#[cfg(feature = "wrds")]
async fn run_wrds_describe_table(
    server: &AtlassianServer,
    args: &WrdsDescribeTableArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::wrds::describe_table(&server.wrds_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_course(
    server: &AtlassianServer,
    args: &EdxDiscussionCourseArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::edx::course(&server.edx_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_topics(
    server: &AtlassianServer,
    args: &EdxDiscussionTopicsArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::edx::topics(&server.edx_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_threads(
    server: &AtlassianServer,
    args: &EdxDiscussionThreadsArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::edx::threads(&server.edx_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_thread_create(
    server: &AtlassianServer,
    args: &EdxDiscussionThreadCreateArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::edx::create_thread(&server.edx_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_comments(
    server: &AtlassianServer,
    args: &EdxDiscussionCommentsArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::edx::comments(&server.edx_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_edx_discussion_comment_create(
    server: &AtlassianServer,
    args: &EdxDiscussionCommentCreateArgs,
) -> CallToolResult {
    let config = server.config();
    match crate::controllers::edx::create_comment(&server.edx_ctx(&config), args).await {
        Ok(resp) => success_response(&resp),
        Err(err) => error_to_result(&err),
    }
}

async fn run_clone(server: &AtlassianServer, args: &CloneArgs) -> CallToolResult {
    let config = server.config();
    match handle_clone(&server.bitbucket_typed_ctx(&config), args).await {
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

#[cfg(test)]
mod live_config_tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::VENDOR_BITBUCKET;

    fn server_with_config(config: Config) -> AtlassianServer {
        AtlassianServer::with_components(
            config,
            build_client().unwrap(),
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
            SonarqubeVendor::new(),
            SplunkVendor::new(),
            NinjaOneVendor::new(),
        )
    }

    fn config_json(workspace: &str) -> String {
        format!(r#"{{"bitbucket":{{"environments":{{"LIVE_RELOAD_TEST_VALUE":"{workspace}"}}}}}}"#)
    }

    #[tokio::test]
    async fn reloads_changed_global_config_and_keeps_last_good_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("configs.json");
        std::fs::write(&path, config_json("before")).unwrap();

        let initial = Config::load_from_sources(Some(&path), None, &HashMap::new());
        let server = server_with_config(initial);
        let initial_contents = std::fs::read(&path).ok();
        spawn_config_watcher(&server.state, path.clone(), initial_contents);

        std::fs::write(&path, "{").unwrap();
        tokio::time::sleep(CONFIG_WATCH_INTERVAL * 2).await;
        assert_eq!(
            server
                .config()
                .get_for(VENDOR_BITBUCKET, "LIVE_RELOAD_TEST_VALUE"),
            Some("before")
        );

        std::fs::write(&path, config_json("after")).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if server
                    .config()
                    .get_for(VENDOR_BITBUCKET, "LIVE_RELOAD_TEST_VALUE")
                    == Some("after")
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("watcher did not load the changed config");
    }
}
