//! Stdio MCP transport. Ported from `startServer('stdio')` in `src/index.ts`.

use rmcp::service::ServiceExt;
use rmcp::transport::io::stdio;
use tracing::info;

use crate::error::McpError;
use crate::server::shutdown;
use crate::tools::AtlassianServer;

/// Boot the server on the stdio transport, consuming the calling task until
/// the peer disconnects or a shutdown signal is received. Matches TS
/// `startServer('stdio')` + the SIGINT/SIGTERM handlers in
/// `setupGracefulShutdown` (`src/index.ts:411-478`).
pub async fn run_stdio() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let handler = AtlassianServer::new().map_err(boxed_err)?;
    let transport = stdio();
    let service = handler.serve(transport).await?;

    // Cancelling this token tells the rmcp service task to shut down, which in
    // turn makes `service.waiting()` resolve.
    let cancel = service.cancellation_token();
    let shutdown_task = tokio::spawn(async move {
        shutdown::wait().await;
        info!("shutdown signal received; closing stdio transport");
        cancel.cancel();
    });

    let waited = service.waiting().await;
    // Natural exit (peer closed stdio): abort the signal task so it doesn't
    // linger for a signal that will never come.
    shutdown_task.abort();
    waited?;
    Ok(())
}

fn boxed_err(err: McpError) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(err)
}
