//! Cross-platform shutdown-signal helper.
//!
//! Both transports (stdio and streamable-HTTP) share the same signal set:
//! Ctrl-C on every platform, plus SIGTERM on Unix. Matches TS
//! `['SIGINT', 'SIGTERM'].forEach(...)` at `src/index.ts:475`.

use tokio::signal;
use tracing::{info, warn};

/// Resolves when the process receives SIGINT (Ctrl-C) or, on Unix, SIGTERM.
///
/// Failures to install signal handlers degrade gracefully: on Unix, a failed
/// SIGTERM install falls back to waiting on Ctrl-C only, with a warning.
pub async fn wait() {
    #[cfg(unix)]
    {
        use signal::unix::{SignalKind, signal as unix_signal};
        let mut sigterm = match unix_signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(err) => {
                warn!(error = %err, "failed to install SIGTERM handler; Ctrl-C only");
                let _ = signal::ctrl_c().await;
                info!("received SIGINT");
                return;
            }
        };
        tokio::select! {
            res = signal::ctrl_c() => {
                if let Err(err) = res {
                    warn!(error = %err, "failed to await Ctrl-C");
                } else {
                    info!("received SIGINT");
                }
            }
            _ = sigterm.recv() => info!("received SIGTERM"),
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(err) = signal::ctrl_c().await {
            warn!(error = %err, "failed to await Ctrl-C");
        } else {
            info!("received Ctrl-C");
        }
    }
}
