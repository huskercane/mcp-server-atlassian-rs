//! Cross-platform shutdown-signal helper.
//!
//! Both transports (stdio and streamable-HTTP) share the same signal set:
//! Ctrl-C on every platform, plus SIGTERM on Unix. Matches TS
//! `['SIGINT', 'SIGTERM'].forEach(...)` at `src/index.ts:475`.
//!
//! Registration is deliberately split from waiting. A `tokio::signal` handler
//! is only installed when its future is first polled, so a server that binds
//! its listener and *then* awaits the signal spends its first moments
//! reachable but unable to shut down gracefully: a SIGTERM arriving in that
//! window hits the default disposition and kills the process outright, mid
//! request, with no drain. Call [`install`] before the listener starts
//! accepting and [`ShutdownSignal::wait`] afterwards, so the handler is in
//! place before the process is reachable.

use tokio::signal;
use tracing::{info, warn};

/// Shutdown signal handlers, already registered with the OS.
pub struct ShutdownSignal {
    #[cfg(unix)]
    sigterm: Option<signal::unix::Signal>,
    #[cfg(unix)]
    sigint: Option<signal::unix::Signal>,
}

/// Register the shutdown signal handlers now.
///
/// Failures degrade gracefully rather than aborting startup: on Unix a failed
/// SIGTERM or SIGINT install is warned about and that signal falls back to its
/// default disposition, with [`ShutdownSignal::wait`] awaiting whichever
/// handlers did install (or Ctrl-C, if neither did).
pub fn install() -> ShutdownSignal {
    #[cfg(unix)]
    {
        use signal::unix::{SignalKind, signal as unix_signal};

        let sigterm = match unix_signal(SignalKind::terminate()) {
            Ok(handler) => Some(handler),
            Err(err) => {
                warn!(error = %err, "failed to install SIGTERM handler; Ctrl-C only");
                None
            }
        };
        // Registered as a stream rather than via `signal::ctrl_c()` for the
        // same reason as SIGTERM: `ctrl_c` installs on first poll, which is
        // too late.
        let sigint = match unix_signal(SignalKind::interrupt()) {
            Ok(handler) => Some(handler),
            Err(err) => {
                warn!(error = %err, "failed to install SIGINT handler");
                None
            }
        };
        ShutdownSignal { sigterm, sigint }
    }

    #[cfg(not(unix))]
    {
        ShutdownSignal {}
    }
}

impl ShutdownSignal {
    /// Resolve when the process receives SIGINT (Ctrl-C) or, on Unix, SIGTERM.
    pub async fn wait(self) {
        #[cfg(unix)]
        {
            let Self { sigterm, sigint } = self;
            match (sigterm, sigint) {
                (Some(mut sigterm), Some(mut sigint)) => {
                    tokio::select! {
                        _ = sigterm.recv() => info!("received SIGTERM"),
                        _ = sigint.recv() => info!("received SIGINT"),
                    }
                }
                (Some(mut sigterm), None) => {
                    sigterm.recv().await;
                    info!("received SIGTERM");
                }
                (None, Some(mut sigint)) => {
                    sigint.recv().await;
                    info!("received SIGINT");
                }
                // Neither handler installed: fall back to Ctrl-C, which at
                // least reinstates SIGINT if the failure was transient.
                (None, None) => {
                    if let Err(err) = signal::ctrl_c().await {
                        warn!(error = %err, "failed to await Ctrl-C");
                    } else {
                        info!("received SIGINT");
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            let Self {} = self;
            if let Err(err) = signal::ctrl_c().await {
                warn!(error = %err, "failed to await Ctrl-C");
            } else {
                info!("received Ctrl-C");
            }
        }
    }
}
