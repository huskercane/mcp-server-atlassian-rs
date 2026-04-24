//! Session manager that wraps [`LocalSessionManager`] with an idle-reap task.
//!
//! rmcp's built-in [`LocalSessionManager`] tracks completed-cache TTLs on
//! per-request channels but does not expire the top-level session map. The TS
//! reference (`src/index.ts:113-360`) expires sessions after 30 min of
//! inactivity, sweeping every 5 min. This wrapper delegates every
//! `SessionManager` call to `LocalSessionManager` and records a last-seen
//! timestamp so a background task can close idle sessions on the same cadence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::Stream;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::common::server_side_http::{ServerSseMessage, SessionId};
use rmcp::transport::streamable_http_server::SessionManager;
use rmcp::transport::streamable_http_server::session::local::{
    LocalSessionManager, LocalSessionManagerError,
};
use tokio::sync::RwLock;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, warn};

/// TS reference uses a 30-minute idle timeout. See `src/index.ts`.
pub const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(30 * 60);
/// TS reference sweeps every 5 minutes.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// [`SessionManager`] that layers an idle-reap policy over
/// [`LocalSessionManager`].
#[derive(Debug, Clone)]
pub struct ReapingSessionManager {
    inner: Arc<LocalSessionManager>,
    last_seen: Arc<RwLock<HashMap<SessionId, Instant>>>,
    idle_ttl: Duration,
}

impl ReapingSessionManager {
    /// Construct a new manager. `idle_ttl` is the duration after last activity
    /// at which a session becomes eligible for reap.
    pub fn new(idle_ttl: Duration) -> Self {
        Self {
            inner: Arc::new(LocalSessionManager::default()),
            last_seen: Arc::new(RwLock::new(HashMap::new())),
            idle_ttl,
        }
    }

    /// Spawn the background sweep task. It ticks every `sweep_interval` and
    /// closes sessions whose last-seen instant is older than `idle_ttl`.
    ///
    /// Returns the task handle; callers that care about shutdown should abort
    /// it when the server exits. The default call-site ignores the handle and
    /// relies on process exit.
    pub fn spawn_reaper(self: &Arc<Self>, sweep_interval: Duration) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = interval(sweep_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            // Consume the immediate first tick so the first sweep happens one
            // interval in, matching TS `setInterval` semantics.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                this.reap_once().await;
            }
        })
    }

    /// Run a single sweep pass. Exposed for tests; production uses the loop
    /// inside [`Self::spawn_reaper`].
    pub async fn reap_once(&self) {
        let now = Instant::now();
        let expired: Vec<SessionId> = {
            let seen = self.last_seen.read().await;
            seen.iter()
                .filter(|(_, last)| now.duration_since(**last) > self.idle_ttl)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in expired {
            debug!(session_id = %id, "reaping idle session");
            if let Err(err) = self.inner.close_session(&id).await {
                warn!(session_id = %id, error = %err, "failed to close idle session");
            }
            self.last_seen.write().await.remove(&id);
        }
    }

    async fn bump(&self, id: &SessionId) {
        self.last_seen.write().await.insert(id.clone(), Instant::now());
    }

    async fn forget(&self, id: &SessionId) {
        self.last_seen.write().await.remove(id);
    }
}

impl SessionManager for ReapingSessionManager {
    type Error = LocalSessionManagerError;
    type Transport = <LocalSessionManager as SessionManager>::Transport;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let (id, transport) = self.inner.create_session().await?;
        self.bump(&id).await;
        Ok((id, transport))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        let resp = self.inner.initialize_session(id, message).await?;
        self.bump(id).await;
        Ok(resp)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        let exists = self.inner.has_session(id).await?;
        if exists {
            self.bump(id).await;
        }
        Ok(exists)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        let res = self.inner.close_session(id).await;
        self.forget(id).await;
        res
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let stream = self.inner.create_stream(id, message).await?;
        self.bump(id).await;
        Ok(stream)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.inner.accept_message(id, message).await?;
        self.bump(id).await;
        Ok(())
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let stream = self.inner.create_standalone_stream(id).await?;
        self.bump(id).await;
        Ok(stream)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let stream = self.inner.resume(id, last_event_id).await?;
        self.bump(id).await;
        Ok(stream)
    }
}
