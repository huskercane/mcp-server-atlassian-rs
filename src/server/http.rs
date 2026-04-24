//! Streamable-HTTP MCP transport.
//!
//! Wraps rmcp's [`StreamableHttpService`] in an Axum router that adds the
//! cross-cutting concerns the TS reference layered over Express:
//!
//! - Bind `127.0.0.1` only; port from `PORT`, default 3000.
//! - `Origin` allowlist (DNS-rebinding guard), globally applied.
//! - CORS with mirrored origins and headers, globally applied.
//! - 1 MB body cap on `/mcp` routes.
//! - `GET /` plaintext health endpoint.
//! - 30-minute idle session reap, sweeping every 5 minutes (see
//!   [`crate::server::session`]).
//!
//! Ported from `src/index.ts:113-360`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any_service, get};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tracing::{info, warn};

use crate::constants::VERSION;
use crate::server::session::{DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL, ReapingSessionManager};
use crate::server::shutdown;
use crate::tools::AtlassianServer;

const BODY_LIMIT_BYTES: usize = 1_000_000;
const DEFAULT_PORT: u16 = 3000;

/// Boot the streamable-HTTP server on `127.0.0.1:${PORT:-3000}`.
///
/// Matches TS `startServer('http')`.
pub async fn run_http() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;

    // Shared across rmcp (drops in-flight SSE on cancel) and axum (stops
    // accepting new connections + drains existing ones on cancel).
    let cancel = CancellationToken::new();
    let app = build_app_with_cancel(DEFAULT_IDLE_TTL, DEFAULT_SWEEP_INTERVAL, cancel.clone());

    info!(%bound, "Atlassian MCP server listening on streamable-HTTP transport");
    let shutdown_cancel = cancel;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown::wait().await;
            info!("shutdown signal received; draining HTTP sessions");
            shutdown_cancel.cancel();
        })
        .await?;
    Ok(())
}

/// Build the full Axum app with a caller-owned cancellation token. Tests use
/// the unparameterized [`build_app`].
pub fn build_app_with_cancel(
    idle_ttl: Duration,
    sweep_interval: Duration,
    cancel: CancellationToken,
) -> Router {
    let manager = Arc::new(ReapingSessionManager::new(idle_ttl));
    manager.spawn_reaper(sweep_interval);

    let streamable = StreamableHttpService::new(
        || {
            AtlassianServer::new()
                .map_err(|e| std::io::Error::other(format!("AtlassianServer::new: {e}")))
        },
        Arc::clone(&manager),
        StreamableHttpServerConfig::default().with_cancellation_token(cancel),
    );

    // Express `cors({ origin: true })` reflects the caller's Origin and mirrors
    // the requested headers on preflight. No credentials, no exposed headers —
    // TS parity.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods(AllowMethods::list([
            Method::GET,
            Method::POST,
            Method::DELETE,
            Method::OPTIONS,
        ]))
        .allow_headers(AllowHeaders::mirror_request());

    // 1 MB body cap applies only to /mcp routes — TS used `express.json({ limit
    // '1mb' })` which only activates on JSON bodies; the Rust parallel is to
    // scope the raw body-limit to /mcp, where the only body-bearing handlers
    // live.
    let mcp_routes = Router::new()
        .route("/mcp", any_service(streamable))
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES));

    // Origin guard + CORS apply globally — TS applies them via `app.use` before
    // registering the `/mcp` routes, so both cover the `GET /` health endpoint
    // as well.
    Router::new()
        .route("/", get(health))
        .merge(mcp_routes)
        .layer(middleware::from_fn(origin_allowlist))
        .layer(cors)
}

/// Build the app without an externally-owned cancellation token. Convenience
/// for tests that don't exercise shutdown semantics.
pub fn build_app(idle_ttl: Duration, sweep_interval: Duration) -> Router {
    build_app_with_cancel(idle_ttl, sweep_interval, CancellationToken::new())
}

async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        format!("Atlassian MCP Server v{VERSION} is running"),
    )
}

/// Reject requests whose `Origin` is not a loopback scheme/host pair.
///
/// Absent `Origin` header is allowed (non-browser clients). CORS preflight
/// (OPTIONS) is handled by the outer [`CorsLayer`] before this middleware
/// sees the request, but we also short-circuit OPTIONS here as belt-and-
/// suspenders in case the layer ordering is ever changed.
async fn origin_allowlist(req: Request, next: Next) -> Response {
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    let Some(origin) = req.headers().get(header::ORIGIN) else {
        return next.run(req).await;
    };

    let Ok(origin_str) = origin.to_str() else {
        warn!("rejected request with invalid origin encoding");
        return forbidden("Forbidden: invalid origin");
    };

    if is_allowed_origin(origin_str) {
        next.run(req).await
    } else {
        warn!(origin = origin_str, "rejected request with invalid origin");
        forbidden("Forbidden: invalid origin")
    }
}

fn forbidden(msg: &'static str) -> Response {
    (
        StatusCode::FORBIDDEN,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        msg,
    )
        .into_response()
}

fn is_allowed_origin(origin: &str) -> bool {
    // Same shape as TS: scheme + loopback host, optionally followed by ':port'.
    const ALLOWED: &[&str] = &[
        "http://localhost",
        "http://127.0.0.1",
        "http://[::1]",
        "https://localhost",
        "https://127.0.0.1",
        "https://[::1]",
    ];
    ALLOWED
        .iter()
        .any(|allowed| origin == *allowed || origin.starts_with(&format!("{allowed}:")))
}

#[cfg(test)]
mod tests {
    use super::is_allowed_origin;

    #[test]
    fn loopback_origins_allowed() {
        for origin in [
            "http://localhost",
            "http://localhost:3000",
            "http://127.0.0.1:8080",
            "https://[::1]:9000",
            "https://localhost",
        ] {
            assert!(is_allowed_origin(origin), "should allow {origin}");
        }
    }

    #[test]
    fn non_loopback_origins_rejected() {
        for origin in [
            "http://evil.com",
            "http://localhost.evil.com",
            "https://127.0.0.1.evil.com",
            "ftp://localhost",
            "",
        ] {
            assert!(!is_allowed_origin(origin), "should reject {origin}");
        }
    }
}
