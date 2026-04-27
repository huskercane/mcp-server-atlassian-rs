//! Default workspace resolution with per-instance memoisation. Mirrors
//! `src/utils/workspace.util.ts`.
//!
//! Lookup order:
//! 1. `BITBUCKET_DEFAULT_WORKSPACE` env var (served from config cascade,
//!    scoped to the bitbucket vendor section).
//! 2. API: `GET /2.0/user/permissions/workspaces?pagelen=10`, returning the
//!    first `values[].workspace.slug`.
//! 3. `None` when the user has no accessible workspaces or the call fails.
//!
//! ## Cache scoping
//!
//! The cache is owned by [`WorkspaceCache`], which lives on each
//! [`AtlassianServer`](crate::tools::AtlassianServer)'s `ServerState`. It
//! is **per-instance**, not process-global, so a library embedder hosting
//! two servers with different credentials will not get one account's
//! workspace leaking into the other's lookups.

use std::sync::RwLock;

use serde_json::Value;
use tracing::{debug, warn};

use crate::auth::Credentials;
use crate::config::VENDOR_BITBUCKET;
use crate::controllers::api::BitbucketContext;
use crate::transport::{RequestOptions, ResponseBody, TransportResponse, fetch};

/// Memoises the resolved default workspace slug for one server instance.
///
/// Construct one per [`AtlassianServer`](crate::tools::AtlassianServer)
/// and pass it into [`BitbucketContext`] alongside the vendor. Tests get
/// isolation by constructing a fresh cache per test.
#[derive(Debug, Default)]
pub struct WorkspaceCache {
    slot: RwLock<Option<String>>,
}

impl WorkspaceCache {
    pub fn new() -> Self {
        Self {
            slot: RwLock::new(None),
        }
    }

    /// Returns the cached slug, if any.
    pub fn get(&self) -> Option<String> {
        self.slot.read().ok().and_then(|g| g.clone())
    }

    /// Stores the slug. Lock-poison failures are ignored (best-effort
    /// caching is fine; the next call will repopulate).
    pub fn set(&self, slug: String) {
        if let Ok(mut guard) = self.slot.write() {
            *guard = Some(slug);
        }
    }

    /// Clears the cache. Exposed for tests that want to force a fresh
    /// API lookup mid-test.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.slot.write() {
            *guard = None;
        }
    }
}

/// Resolve the default workspace slug. Returns `None` when both the env
/// variable is unset and the API call finds no workspaces.
///
/// Takes a [`BitbucketContext`] (not the vendor-neutral
/// [`HandleContext`](crate::controllers::api::HandleContext)) so the
/// type system enforces that this Bitbucket-only operation can never
/// be invoked with a Jira vendor.
pub async fn resolve_default_workspace(ctx: &BitbucketContext<'_>) -> Option<String> {
    if let Some(cached) = ctx.cache().get() {
        debug!(slug = %cached, "workspace: cache hit");
        return Some(cached);
    }

    if let Some(slug) = ctx
        .handle()
        .config
        .get_for(VENDOR_BITBUCKET, "BITBUCKET_DEFAULT_WORKSPACE")
        && !slug.is_empty()
    {
        debug!(slug = %slug, "workspace: resolved from env");
        ctx.cache().set(slug.to_owned());
        return Some(slug.to_owned());
    }

    match fetch_first_workspace(ctx).await {
        Ok(Some(slug)) => {
            debug!(slug = %slug, "workspace: resolved from API");
            ctx.cache().set(slug.clone());
            Some(slug)
        }
        Ok(None) => {
            warn!("workspace: API returned no accessible workspaces");
            None
        }
        Err(err) => {
            warn!(%err, "workspace: API lookup failed");
            None
        }
    }
}

async fn fetch_first_workspace(
    ctx: &BitbucketContext<'_>,
) -> Result<Option<String>, crate::error::McpError> {
    let handle = ctx.handle();
    let creds = Credentials::require_async(handle.config).await?;
    let response: TransportResponse = fetch(
        handle.client,
        handle.vendor,
        &creds,
        handle.config,
        "/2.0/user/permissions/workspaces?pagelen=10",
        RequestOptions::default(),
    )
    .await?;

    let Some(value) = as_json(&response.data) else {
        return Ok(None);
    };
    Ok(extract_first_slug(value))
}

fn as_json(body: &ResponseBody) -> Option<&Value> {
    match body {
        ResponseBody::Json(v) => Some(v),
        _ => None,
    }
}

fn extract_first_slug(value: &Value) -> Option<String> {
    value
        .get("values")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("workspace"))
        .and_then(|ws| ws.get("slug"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}
