//! Default workspace resolution with per-process memoisation. Mirrors
//! `src/utils/workspace.util.ts`.
//!
//! Lookup order:
//! 1. `BITBUCKET_DEFAULT_WORKSPACE` env var (served from config cascade).
//! 2. API: `GET /2.0/user/permissions/workspaces?pagelen=10`, returning the
//!    first `values[].workspace.slug`.
//! 3. `None` when the user has no accessible workspaces or the call fails.
//!
//! The memo caches across all `BitbucketServer` instances in a process;
//! callers wanting fresh lookups can invoke [`reset_cache`] (tests).

use std::sync::RwLock;
use std::sync::OnceLock;

use serde_json::Value;
use tracing::{debug, warn};

use crate::auth::Credentials;
use crate::error::auth_missing_default;
use crate::transport::{
    RequestOptions, ResponseBody, TransportResponse, fetch_bitbucket_with_base,
};

static DEFAULT_WORKSPACE: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn slot() -> &'static RwLock<Option<String>> {
    DEFAULT_WORKSPACE.get_or_init(|| RwLock::new(None))
}

/// Clear the memo. Exposed for tests.
pub fn reset_cache() {
    if let Ok(mut guard) = slot().write() {
        *guard = None;
    }
}

/// Resolve the default workspace slug. Returns `None` when both the env
/// variable is unset and the API call finds no workspaces.
pub async fn resolve_default_workspace(
    ctx: &crate::controllers::api::HandleContext<'_>,
) -> Option<String> {
    if let Ok(guard) = slot().read()
        && let Some(cached) = guard.as_ref()
    {
        debug!(slug = %cached, "workspace: cache hit");
        return Some(cached.clone());
    }

    if let Some(slug) = ctx.config.get("BITBUCKET_DEFAULT_WORKSPACE")
        && !slug.is_empty()
    {
        debug!(slug = %slug, "workspace: resolved from env");
        store(slug.to_owned());
        return Some(slug.to_owned());
    }

    match fetch_first_workspace(ctx).await {
        Ok(Some(slug)) => {
            debug!(slug = %slug, "workspace: resolved from API");
            store(slug.clone());
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

fn store(slug: String) {
    if let Ok(mut guard) = slot().write() {
        *guard = Some(slug);
    }
}

async fn fetch_first_workspace(
    ctx: &crate::controllers::api::HandleContext<'_>,
) -> Result<Option<String>, crate::error::McpError> {
    let creds = Credentials::resolve(ctx.config).ok_or_else(auth_missing_default)?;
    let response: TransportResponse = fetch_bitbucket_with_base(
        ctx.base_url,
        ctx.client,
        &creds,
        ctx.config,
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
