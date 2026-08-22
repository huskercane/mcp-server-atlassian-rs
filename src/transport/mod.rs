//! HTTP transport for Atlassian product API calls.
//!
//! Vendor-neutral. The base URL, path normalisation, and non-2xx error
//! envelope parsing are all delegated to the [`Vendor`] trait
//! ([`crate::vendor`]). Everything else (auth header, request building,
//! 10 MB response cap, body classification, raw-response persistence) is
//! shared across vendors.
//!
//! Response classifier (matches the TS reference for both Bitbucket and
//! Jira via [`fetch`]):
//! - `204` → empty object, no raw path
//! - `text/plain` → raw text pass-through (e.g. Bitbucket diffs), no raw
//!   path
//! - empty body → empty object, no raw path
//! - JSON parse success → parsed value + raw response persisted to disk
//! - JSON parse failure → raw text, no raw path
//!
//! ## Back-compat shims
//!
//! [`fetch_bitbucket`] and [`fetch_bitbucket_with_base`] are preserved as
//! thin shims that construct a [`BitbucketVendor`] and call [`fetch`].
//! New code should call [`fetch`] directly with the vendor it needs.

pub mod raw_response;
mod response_cache;

/// Re-export of the Bitbucket error parser at its old path. Kept so
/// downstream tests (`tests/bitbucket_error_tests.rs`) and any external
/// consumers continue to compile after the parser moved into
/// [`crate::vendor::bitbucket::error`].
pub use crate::vendor::bitbucket::error as bitbucket_error;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures::TryStreamExt as _;
use reqwest::header::{
    ACCEPT, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue,
};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, BufReader};
use tokio_util::io::StreamReader;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::auth::Credentials;
use crate::config::Config;
use crate::constants::{data_limits::MAX_RESPONSE_SIZE, network_timeouts::DEFAULT_REQUEST};
use crate::error::{McpError, OriginalError, api_error, auth_invalid, unexpected};
use crate::vendor::Vendor;
use crate::vendor::bitbucket::BitbucketVendor;

/// Stream a successful upstream body directly into an atomic artifact. The
/// byte ceiling is checked for every decoded chunk, so transfer-encoded
/// responses cannot bypass it by omitting Content-Length.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_streamed_artifact(
    _client: &Client,
    vendor: &dyn Vendor,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
    filename_prefix: &str,
    extension: &str,
    content_type: &str,
    max_bytes: u64,
) -> Result<raw_response::StreamedArtifact, McpError> {
    fetch_streamed_artifact_with_policy(
        vendor,
        credentials,
        config,
        path,
        options,
        filename_prefix,
        extension,
        content_type,
        StreamingPolicy::new(max_bytes, max_bytes),
    )
    .await
}

#[derive(Debug, Clone)]
pub struct StreamingPolicy {
    pub max_encoded_bytes: u64,
    pub max_decoded_bytes: u64,
    pub idle_read_timeout: Duration,
    pub total_deadline: Duration,
    pub max_attempts: usize,
    pub cancellation: CancellationToken,
    pub aggregate: Option<Arc<StreamingAggregateQuota>>,
    pub disk: Option<Arc<StreamingDiskQuota>>,
}

#[derive(Debug)]
pub struct StreamingAggregateQuota {
    encoded: AtomicU64,
    decoded: AtomicU64,
    pub max_encoded_bytes: u64,
    pub max_decoded_bytes: u64,
}

/// One request's private view of the process-wide streaming disk coordinator.
/// The owner identifier and its reservation leases never cross a public tool
/// boundary.
#[derive(Debug)]
pub struct StreamingDiskQuota {
    coordinator: Arc<StreamingDiskCoordinator>,
    owner: u64,
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
}

#[derive(Debug)]
struct StreamingDiskCoordinator {
    limit: u64,
    state: Mutex<StreamingDiskState>,
}

// Waiters use strict FIFO ordering. A request at the head of the queue is the
// only request considered until enough bytes are released for it, preventing
// smaller later writes from starving an earlier writer. Every wait is bounded
// by its transaction cancellation token and absolute deadline.

#[derive(Debug, Default)]
struct StreamingDiskState {
    reserved: u64,
    peak: u64,
    next_reservation: u64,
    next_waiter: u64,
    transactions: HashMap<u64, TransactionReservation>,
    reservations: HashMap<u64, OwnedReservation>,
    waiters: VecDeque<ReservationWaiter>,
}

#[derive(Debug)]
struct TransactionReservation {
    reserved: u64,
    limit: u64,
}

#[derive(Debug)]
struct OwnedReservation {
    owner: u64,
    amount: u64,
}

#[derive(Debug)]
struct ReservationWaiter {
    id: u64,
    reservation: u64,
    owner: u64,
    amount: u64,
    status: Arc<AtomicU8>,
    notify: Arc<tokio::sync::Notify>,
}

/// A narrow, exactly-once reservation owned by one writer or committed file.
#[derive(Debug)]
pub(crate) struct StreamingDiskLease {
    transaction: Arc<StreamingDiskQuota>,
    reservation: u64,
}

static STREAMING_DISK_COORDINATOR: OnceLock<Arc<StreamingDiskCoordinator>> = OnceLock::new();
static NEXT_STREAMING_TRANSACTION: AtomicU64 = AtomicU64::new(1);

impl StreamingDiskCoordinator {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            state: Mutex::new(StreamingDiskState::default()),
        }
    }

    fn register_transaction(&self, owner: u64, limit: u64) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transactions
            .insert(owner, TransactionReservation { reserved: 0, limit });
    }

    fn register_reservation(&self, owner: u64) -> std::io::Result<u64> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.transactions.contains_key(&owner) {
            return Err(std::io::Error::other(
                "disk reservation transaction is closed",
            ));
        }
        state.next_reservation = state
            .next_reservation
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("disk reservation identifier overflow"))?;
        let reservation = state.next_reservation;
        state
            .reservations
            .insert(reservation, OwnedReservation { owner, amount: 0 });
        Ok(reservation)
    }

    fn can_grant(state: &StreamingDiskState, limit: u64, amount: u64) -> bool {
        state
            .reserved
            .checked_add(amount)
            .is_some_and(|next| next <= limit)
    }

    fn grant_locked(
        state: &mut StreamingDiskState,
        limit: u64,
        reservation: u64,
        owner: u64,
        amount: u64,
    ) -> std::io::Result<u64> {
        let transaction = state
            .transactions
            .get(&owner)
            .ok_or_else(|| std::io::Error::other("disk reservation transaction is closed"))?;
        let transaction_next = transaction
            .reserved
            .checked_add(amount)
            .filter(|next| *next <= transaction.limit)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "transaction disk reservation quota exceeded or counter overflow",
                )
            })?;
        let global_next = state
            .reserved
            .checked_add(amount)
            .filter(|next| *next <= limit)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "server-wide disk reservation quota exceeded or counter overflow",
                )
            })?;
        let reservation_entry = state.reservations.get(&reservation).ok_or_else(|| {
            std::io::Error::other("disk reservation lease is no longer registered")
        })?;
        if reservation_entry.owner != owner {
            return Err(std::io::Error::other("disk reservation owner mismatch"));
        }
        let reservation_next = reservation_entry
            .amount
            .checked_add(amount)
            .ok_or_else(|| std::io::Error::other("disk reservation counter overflow"))?;
        state.reserved = global_next;
        state.peak = state.peak.max(global_next);
        state
            .transactions
            .get_mut(&owner)
            .expect("transaction validated above")
            .reserved = transaction_next;
        state
            .reservations
            .get_mut(&reservation)
            .expect("reservation validated above")
            .amount = reservation_next;
        Ok(reservation_next)
    }

    fn wake_waiters_locked(state: &mut StreamingDiskState, limit: u64) {
        while !state.waiters.is_empty() {
            let amount = state.waiters.front().expect("waiter exists").amount;
            if !Self::can_grant(state, limit, amount) {
                break;
            }
            let waiter = state.waiters.pop_front().expect("front waiter exists");
            if Self::grant_locked(
                state,
                limit,
                waiter.reservation,
                waiter.owner,
                waiter.amount,
            )
            .is_ok()
            {
                waiter.status.store(1, Ordering::Release);
            } else {
                waiter.status.store(2, Ordering::Release);
            }
            waiter.notify.notify_one();
        }
    }

    async fn acquire(
        &self,
        reservation: u64,
        owner: u64,
        amount: u64,
        cancellation: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> std::io::Result<u64> {
        if amount == 0 {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            return state
                .reservations
                .get(&reservation)
                .filter(|entry| entry.owner == owner)
                .map(|entry| entry.amount)
                .ok_or_else(|| std::io::Error::other("disk reservation owner mismatch"));
        }
        let status = Arc::new(AtomicU8::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());
        let waiter_id;
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let transaction = state
                .transactions
                .get(&owner)
                .ok_or_else(|| std::io::Error::other("disk reservation transaction is closed"))?;
            transaction
                .reserved
                .checked_add(amount)
                .filter(|next| *next <= transaction.limit)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        "transaction disk reservation quota exceeded or counter overflow",
                    )
                })?;
            if state.waiters.is_empty() && Self::can_grant(&state, self.limit, amount) {
                return Self::grant_locked(&mut state, self.limit, reservation, owner, amount);
            }
            state.next_waiter = state
                .next_waiter
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("disk waiter identifier overflow"))?;
            waiter_id = state.next_waiter;
            state.waiters.push_back(ReservationWaiter {
                id: waiter_id,
                reservation,
                owner,
                amount,
                status: status.clone(),
                notify: notify.clone(),
            });
        }
        let outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "disk reservation wait cancelled")),
            () = tokio::time::sleep_until(deadline) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "disk reservation wait deadline exceeded")),
            () = notify.notified() => Ok(()),
        };
        if outcome.is_err() || status.load(Ordering::Acquire) != 1 {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(index) = state
                .waiters
                .iter()
                .position(|waiter| waiter.id == waiter_id)
            {
                state.waiters.remove(index);
                status.store(2, Ordering::Release);
                Self::wake_waiters_locked(&mut state, self.limit);
            } else if status.swap(2, Ordering::AcqRel) == 1 {
                Self::release_locked(&mut state, reservation, owner, amount)?;
                Self::wake_waiters_locked(&mut state, self.limit);
            }
            return outcome.and(Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "disk reservation waiter could not be granted",
            )));
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state
            .reservations
            .get(&reservation)
            .expect("granted reservation remains registered")
            .amount)
    }

    fn release_locked(
        state: &mut StreamingDiskState,
        reservation: u64,
        owner: u64,
        amount: u64,
    ) -> std::io::Result<u64> {
        let entry = state
            .reservations
            .get(&reservation)
            .ok_or_else(|| std::io::Error::other("disk reservation already released"))?;
        if entry.owner != owner {
            return Err(std::io::Error::other("disk reservation owner mismatch"));
        }
        let reservation_next = entry
            .amount
            .checked_sub(amount)
            .ok_or_else(|| std::io::Error::other("disk reservation release underflow"))?;
        let transaction = state
            .transactions
            .get(&owner)
            .ok_or_else(|| std::io::Error::other("disk reservation transaction is closed"))?;
        let transaction_next = transaction
            .reserved
            .checked_sub(amount)
            .ok_or_else(|| std::io::Error::other("transaction reservation release underflow"))?;
        let global_next = state
            .reserved
            .checked_sub(amount)
            .ok_or_else(|| std::io::Error::other("server reservation release underflow"))?;
        state
            .reservations
            .get_mut(&reservation)
            .expect("reservation validated above")
            .amount = reservation_next;
        state
            .transactions
            .get_mut(&owner)
            .expect("transaction validated above")
            .reserved = transaction_next;
        state.reserved = global_next;
        Ok(reservation_next)
    }

    fn release(&self, reservation: u64, owner: u64, amount: u64) -> std::io::Result<u64> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remaining = Self::release_locked(&mut state, reservation, owner, amount)?;
        Self::wake_waiters_locked(&mut state, self.limit);
        Ok(remaining)
    }

    fn unregister_reservation(&self, reservation: u64, owner: u64) -> std::io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state
            .reservations
            .get(&reservation)
            .ok_or_else(|| std::io::Error::other("disk reservation already released"))?;
        if entry.owner != owner {
            return Err(std::io::Error::other("disk reservation owner mismatch"));
        }
        if entry.amount != 0 {
            return Err(std::io::Error::other(
                "cannot unregister a live disk reservation",
            ));
        }
        state.reservations.remove(&reservation);
        Ok(())
    }
}

impl StreamingDiskQuota {
    fn next_owner() -> u64 {
        NEXT_STREAMING_TRANSACTION
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .expect("streaming disk transaction identifier space exhausted")
    }

    #[cfg(test)]
    pub fn new(limit: u64) -> Self {
        let coordinator = Arc::new(StreamingDiskCoordinator::new(limit));
        let owner = Self::next_owner();
        coordinator.register_transaction(owner, limit);
        Self {
            coordinator,
            owner,
            cancellation: CancellationToken::new(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(30),
        }
    }

    fn with_coordinator(
        coordinator: Arc<StreamingDiskCoordinator>,
        limit: u64,
        cancellation: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Arc<Self> {
        let owner = Self::next_owner();
        coordinator.register_transaction(owner, limit);
        Arc::new(Self {
            coordinator,
            owner,
            cancellation,
            deadline,
        })
    }

    pub fn server_transaction(cancellation: CancellationToken) -> Arc<Self> {
        raw_response::reconcile_missing_artifacts();
        let limit = crate::constants::data_limits::MAX_STREAMED_ARTIFACT_SIZE;
        let coordinator = STREAMING_DISK_COORDINATOR
            .get_or_init(|| Arc::new(StreamingDiskCoordinator::new(limit)))
            .clone();
        Self::with_coordinator(
            coordinator,
            limit,
            cancellation,
            tokio::time::Instant::now()
                + crate::constants::data_limits::STREAM_TOTAL_REQUEST_TIMEOUT,
        )
    }

    pub(crate) fn lease(self: &Arc<Self>) -> std::io::Result<StreamingDiskLease> {
        let reservation = self.coordinator.register_reservation(self.owner)?;
        Ok(StreamingDiskLease {
            transaction: self.clone(),
            reservation,
        })
    }

    pub fn reserved_bytes(&self) -> u64 {
        self.coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transactions
            .get(&self.owner)
            .map_or(0, |transaction| transaction.reserved)
    }

    pub fn peak_reserved_bytes(&self) -> u64 {
        self.coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .peak
    }
}

impl Drop for StreamingDiskQuota {
    fn drop(&mut self) {
        let mut state = self
            .coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removable = state
            .transactions
            .get(&self.owner)
            .is_some_and(|transaction| transaction.reserved == 0)
            && !state
                .reservations
                .values()
                .any(|reservation| reservation.owner == self.owner)
            && !state
                .waiters
                .iter()
                .any(|waiter| waiter.owner == self.owner);
        if removable {
            state.transactions.remove(&self.owner);
        }
    }
}

impl StreamingDiskLease {
    pub(crate) async fn grow(&self, amount: u64) -> std::io::Result<u64> {
        self.transaction
            .coordinator
            .acquire(
                self.reservation,
                self.transaction.owner,
                amount,
                &self.transaction.cancellation,
                self.transaction.deadline,
            )
            .await
    }

    pub(crate) fn shrink(&self, amount: u64) -> std::io::Result<u64> {
        self.transaction
            .coordinator
            .release(self.reservation, self.transaction.owner, amount)
    }

    pub(crate) fn amount(&self) -> u64 {
        self.transaction
            .coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reservations
            .get(&self.reservation)
            .filter(|reservation| reservation.owner == self.transaction.owner)
            .map_or(0, |reservation| reservation.amount)
    }

    pub(crate) fn same_transaction(&self, transaction: &Arc<StreamingDiskQuota>) -> bool {
        Arc::ptr_eq(&self.transaction, transaction)
    }
}

impl Drop for StreamingDiskLease {
    fn drop(&mut self) {
        let amount = self.amount();
        if amount != 0 {
            let released = self.shrink(amount);
            debug_assert!(released.is_ok());
        }
        let unregistered = self
            .transaction
            .coordinator
            .unregister_reservation(self.reservation, self.transaction.owner);
        debug_assert!(unregistered.is_ok());
    }
}

impl StreamingAggregateQuota {
    pub const fn new(max_encoded_bytes: u64, max_decoded_bytes: u64) -> Self {
        Self {
            encoded: AtomicU64::new(0),
            decoded: AtomicU64::new(0),
            max_encoded_bytes,
            max_decoded_bytes,
        }
    }

    fn add(counter: &AtomicU64, amount: u64, limit: u64, label: &str) -> std::io::Result<()> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(amount).filter(|next| *next <= limit)
            })
            .map(|_| ())
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    format!("aggregate {label} quota exceeded or counter overflow"),
                )
            })
    }

    pub fn add_encoded(&self, amount: u64) -> std::io::Result<()> {
        Self::add(
            &self.encoded,
            amount,
            self.max_encoded_bytes,
            "encoded-byte",
        )
    }
    pub fn add_decoded(&self, amount: u64) -> std::io::Result<()> {
        Self::add(
            &self.decoded,
            amount,
            self.max_decoded_bytes,
            "decoded-byte",
        )
    }
    pub fn encoded_bytes(&self) -> u64 {
        self.encoded.load(Ordering::Acquire)
    }
    pub fn decoded_bytes(&self) -> u64 {
        self.decoded.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod aggregate_quota_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::{StreamingAggregateQuota, StreamingDiskCoordinator, StreamingDiskQuota};

    fn shared_transaction(
        coordinator: &Arc<StreamingDiskCoordinator>,
        limit: u64,
        cancellation: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Arc<StreamingDiskQuota> {
        StreamingDiskQuota::with_coordinator(coordinator.clone(), limit, cancellation, deadline)
    }

    #[test]
    fn checked_aggregate_counters_reject_limits_and_overflow() {
        let quota = StreamingAggregateQuota::new(3, u64::MAX);
        quota.add_encoded(2).unwrap();
        assert!(quota.add_encoded(2).is_err());
        let overflow = StreamingAggregateQuota::new(u64::MAX, u64::MAX);
        overflow.add_decoded(u64::MAX).unwrap();
        assert!(overflow.add_decoded(1).is_err());
    }

    #[tokio::test]
    async fn checked_disk_reservation_rejects_quota_underflow_and_overflow() {
        let quota = std::sync::Arc::new(StreamingDiskQuota::new(10));
        let lease = quota.lease().unwrap();
        assert_eq!(lease.grow(4).await.unwrap(), 4);
        assert_eq!(lease.grow(6).await.unwrap(), 10);
        assert!(lease.grow(1).await.is_err());
        assert_eq!(lease.shrink(6).unwrap(), 4);
        assert_eq!(lease.shrink(4).unwrap(), 0);
        assert!(lease.shrink(1).is_err());
        let overflow = std::sync::Arc::new(StreamingDiskQuota::new(u64::MAX));
        let overflow_lease = overflow.lease().unwrap();
        overflow_lease.grow(u64::MAX).await.unwrap();
        assert!(overflow_lease.grow(1).await.is_err());
    }

    #[tokio::test]
    async fn independent_splunk_and_loki_transactions_share_one_ceiling() {
        let coordinator = Arc::new(StreamingDiskCoordinator::new(10));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let splunk = shared_transaction(&coordinator, 10, CancellationToken::new(), deadline);
        let loki = shared_transaction(&coordinator, 10, CancellationToken::new(), deadline);
        let splunk_file = splunk.lease().unwrap();
        let loki_file = loki.lease().unwrap();
        splunk_file.grow(6).await.unwrap();
        let waiting = tokio::spawn(async move {
            let result = loki_file.grow(5).await;
            (loki_file, result)
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        assert_eq!(coordinator.state.lock().unwrap().reserved, 6);
        splunk_file.shrink(1).unwrap();
        let (loki_file, result) = waiting.await.unwrap();
        assert_eq!(result.unwrap(), 5);
        assert_eq!(splunk.reserved_bytes(), 5);
        assert_eq!(loki.reserved_bytes(), 5);
        assert!(coordinator.state.lock().unwrap().peak <= 10);
        drop(splunk_file);
        drop(loki_file);
        assert_eq!(splunk.reserved_bytes(), 0);
        assert_eq!(loki.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn circleci_waits_behind_ingestion_in_fifo_order() {
        let coordinator = Arc::new(StreamingDiskCoordinator::new(4));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let holder = shared_transaction(&coordinator, 4, CancellationToken::new(), deadline);
        let splunk = shared_transaction(&coordinator, 4, CancellationToken::new(), deadline);
        let circleci = shared_transaction(&coordinator, 4, CancellationToken::new(), deadline);
        let held = holder.lease().unwrap();
        held.grow(4).await.unwrap();
        let first_lease = splunk.lease().unwrap();
        let first = tokio::spawn(async move {
            first_lease.grow(3).await.unwrap();
            first_lease
        });
        while coordinator.state.lock().unwrap().waiters.len() != 1 {
            tokio::task::yield_now().await;
        }
        let second_lease = circleci.lease().unwrap();
        let second = tokio::spawn(async move {
            second_lease.grow(3).await.unwrap();
            second_lease
        });
        while coordinator.state.lock().unwrap().waiters.len() != 2 {
            tokio::task::yield_now().await;
        }
        held.shrink(3).unwrap();
        let first_lease = first.await.unwrap();
        assert!(!second.is_finished());
        drop(first_lease);
        let second_lease = second.await.unwrap();
        assert_eq!(circleci.reserved_bytes(), 3);
        drop(second_lease);
        drop(held);
        assert_eq!(coordinator.state.lock().unwrap().reserved, 0);
    }

    #[tokio::test]
    async fn cancellation_rolls_back_only_the_waiting_transaction() {
        let coordinator = Arc::new(StreamingDiskCoordinator::new(4));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let holder = shared_transaction(&coordinator, 4, CancellationToken::new(), deadline);
        let cancellation = CancellationToken::new();
        let waiter = shared_transaction(&coordinator, 4, cancellation.clone(), deadline);
        let held = holder.lease().unwrap();
        held.grow(4).await.unwrap();
        let waiting_lease = waiter.lease().unwrap();
        let waiting = tokio::spawn(async move { waiting_lease.grow(1).await });
        while coordinator.state.lock().unwrap().waiters.is_empty() {
            tokio::task::yield_now().await;
        }
        cancellation.cancel();
        let error = waiting.await.unwrap().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(waiter.reserved_bytes(), 0);
        assert_eq!(holder.reserved_bytes(), 4);
        assert!(coordinator.state.lock().unwrap().waiters.is_empty());
        drop(held);
        assert_eq!(coordinator.state.lock().unwrap().reserved, 0);
    }

    #[tokio::test]
    async fn deadline_quota_exhaustion_and_owner_isolation_are_explicit() {
        let coordinator = Arc::new(StreamingDiskCoordinator::new(2));
        let holder = shared_transaction(
            &coordinator,
            2,
            CancellationToken::new(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        );
        let waiter = shared_transaction(
            &coordinator,
            2,
            CancellationToken::new(),
            tokio::time::Instant::now() + Duration::from_millis(10),
        );
        let held = holder.lease().unwrap();
        held.grow(2).await.unwrap();
        let waiting = waiter.lease().unwrap();
        let error = waiting.grow(1).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(waiter.reserved_bytes(), 0);
        assert!(
            coordinator
                .release(held.reservation, waiter.owner, 1)
                .is_err()
        );
        assert_eq!(holder.reserved_bytes(), 2);
        assert!(waiting.shrink(1).is_err());
        drop(waiting);
        drop(held);
        assert_eq!(coordinator.state.lock().unwrap().reserved, 0);
    }
}

impl StreamingPolicy {
    pub fn new(max_encoded_bytes: u64, max_decoded_bytes: u64) -> Self {
        Self {
            max_encoded_bytes,
            max_decoded_bytes,
            idle_read_timeout: crate::constants::data_limits::STREAM_IDLE_READ_TIMEOUT,
            total_deadline: crate::constants::data_limits::STREAM_TOTAL_REQUEST_TIMEOUT,
            max_attempts: crate::constants::data_limits::STREAM_MAX_ATTEMPTS,
            cancellation: CancellationToken::new(),
            aggregate: None,
            disk: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn fetch_streamed_artifact_with_policy(
    vendor: &dyn Vendor,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
    filename_prefix: &str,
    extension: &str,
    content_type: &str,
    policy: StreamingPolicy,
) -> Result<raw_response::StreamedArtifact, McpError> {
    let base = vendor.base_url(config)?;
    let url = normalize_url_with_base(&base, path);
    let method = options.method.unwrap_or(HttpMethod::Get);
    let (auth_name, auth_header) = validate_auth(credentials)?;
    let client = streaming_client()?;
    let attempts = policy.max_attempts.max(1);
    let deadline = tokio::time::Instant::now() + policy.total_deadline;
    for attempt in 1..=attempts {
        if policy.cancellation.is_cancelled() {
            return Err(api_error("streaming request cancelled", Some(499), None));
        }
        let remaining = remaining_until(deadline)?;
        let request = build_request(
            client,
            method,
            &url,
            &auth_name,
            &auth_header,
            &options,
            remaining,
        );
        let response = tokio::select! {
            () = policy.cancellation.cancelled() => return Err(api_error("streaming request cancelled", Some(499), None)),
            result = tokio::time::timeout(remaining, request.send()) => match result {
                Ok(Ok(response)) => response,
                Ok(Err(error)) if attempt < attempts && (error.is_connect() || error.is_timeout()) => { retry_stream_attempt(attempt, &policy, deadline).await?; continue; }
                Ok(Err(error)) => return Err(map_reqwest_error(&error, &url)),
                Err(_) if attempt < attempts => { retry_stream_attempt(attempt, &policy, deadline).await?; continue; }
                Err(_) => return Err(api_error("streaming request exceeded total deadline", Some(408), None)),
            }
        };
        let status = response.status();
        if !status.is_success() {
            if attempt < attempts && matches!(status.as_u16(), 429 | 502 | 503 | 504) {
                retry_stream_attempt(attempt, &policy, deadline).await?;
                continue;
            }
            let body = response.text().await.unwrap_or_default();
            return Err(vendor.classify_error(status, &body));
        }
        if response
            .content_length()
            .is_some_and(|length| length > policy.max_encoded_bytes)
        {
            return Err(api_error(
                "encoded response exceeds streamed artifact limit",
                Some(413),
                None,
            ));
        }
        return tokio::time::timeout(
            remaining_until(deadline)?,
            persist_decoded_response(response, filename_prefix, extension, content_type, &policy),
        )
        .await
        .map_err(|_| api_error("streaming request exceeded total deadline", Some(408), None))?;
    }
    unreachable!("bounded streaming retry loop returns")
}

type BoxRead = Pin<Box<dyn AsyncRead + Send>>;
type BoxBufRead = Pin<Box<dyn AsyncBufRead + Send>>;

fn decoded_reader(
    response: reqwest::Response,
    policy: &StreamingPolicy,
    encoded: Arc<AtomicU64>,
) -> Result<BoxRead, McpError> {
    let encodings = response
        .headers()
        .get_all(CONTENT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let encoded_limit = policy.max_encoded_bytes;
    let aggregate = policy.aggregate.clone();
    let stream = response
        .bytes_stream()
        .map_err(std::io::Error::other)
        .and_then(move |chunk| {
            let counter = encoded.clone();
            let aggregate = aggregate.clone();
            async move {
                let amount = u64::try_from(chunk.len()).map_err(std::io::Error::other)?;
                let next = counter
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                        current
                            .checked_add(amount)
                            .filter(|value| *value <= encoded_limit)
                    })
                    .map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::FileTooLarge,
                            "encoded response exceeds streamed artifact limit or counter overflow",
                        )
                    })?;
                let _ = next;
                if let Some(aggregate) = &aggregate {
                    aggregate.add_encoded(amount)?;
                }
                Ok(chunk)
            }
        });
    let mut reader: BoxRead = Box::pin(StreamReader::new(stream));
    for encoding in encodings.iter().rev() {
        let buffered: BoxBufRead = Box::pin(BufReader::with_capacity(
            crate::constants::data_limits::STREAM_WRITE_BUFFER_SIZE,
            reader,
        ));
        reader = match encoding.as_str() {
            "identity" => Box::pin(buffered),
            "gzip" | "x-gzip" => Box::pin(async_compression::tokio::bufread::GzipDecoder::new(
                buffered,
            )),
            "br" => Box::pin(async_compression::tokio::bufread::BrotliDecoder::new(
                buffered,
            )),
            "deflate" => Box::pin(async_compression::tokio::bufread::DeflateDecoder::new(
                buffered,
            )),
            "zstd" => Box::pin(async_compression::tokio::bufread::ZstdDecoder::new(
                buffered,
            )),
            other => {
                return Err(api_error(
                    format!("unsupported Content-Encoding: {other}"),
                    Some(415),
                    None,
                ));
            }
        };
    }
    Ok(reader)
}

async fn persist_decoded_response(
    response: reqwest::Response,
    filename_prefix: &str,
    extension: &str,
    content_type: &str,
    policy: &StreamingPolicy,
) -> Result<raw_response::StreamedArtifact, McpError> {
    let encoded = std::sync::Arc::new(AtomicU64::new(0));
    let mut reader = decoded_reader(response, policy, encoded.clone())?;
    let mut writer = raw_response::begin_artifact(
        filename_prefix,
        extension,
        content_type,
        policy.max_decoded_bytes,
    )
    .await
    .map_err(|error| unexpected(format!("failed to create streamed artifact: {error}"), None))?;
    if let Some(disk) = &policy.disk {
        writer.set_disk_quota(disk);
    }
    let mut buffer = vec![0_u8; crate::constants::data_limits::STREAM_WRITE_BUFFER_SIZE];
    loop {
        let read = tokio::select! {
            () = policy.cancellation.cancelled() => return Err(api_error("streaming request cancelled", Some(499), None)),
            result = tokio::time::timeout(policy.idle_read_timeout, reader.read(&mut buffer)) => result
                .map_err(|_| api_error("streaming response idle-read timeout", Some(408), None))?
                .map_err(|error| {
                    let status = (error.kind() == std::io::ErrorKind::FileTooLarge).then_some(413);
                    api_error(format!("failed to decode streaming response: {error}"), status, None)
                })?,
        };
        if read == 0 {
            break;
        }
        if let Some(aggregate) = &policy.aggregate {
            let read = u64::try_from(read).map_err(|error| {
                api_error(
                    format!("decoded byte count overflow: {error}"),
                    Some(413),
                    None,
                )
            })?;
            aggregate
                .add_decoded(read)
                .map_err(|error| api_error(error.to_string(), Some(413), None))?;
        }
        writer.write_chunk(&buffer[..read]).await.map_err(|error| {
            let status = (error.kind() == std::io::ErrorKind::FileTooLarge).then_some(413);
            api_error(
                format!("failed to persist decoded response: {error}"),
                status,
                None,
            )
        })?;
    }
    let mut artifact = writer.commit().await.map_err(|error| {
        unexpected(format!("failed to commit streamed artifact: {error}"), None)
    })?;
    artifact.encoded_bytes = encoded.load(Ordering::Relaxed);
    artifact.decoded_bytes = artifact.artifact.size;
    Ok(artifact)
}

/// Stream an absolute, unauthenticated URL (for example a signed log-output
/// URL) through the same explicit wire accounting and decoder path.
pub async fn fetch_streamed_url(
    url: &str,
    filename_prefix: &str,
    extension: &str,
    content_type: &str,
    policy: StreamingPolicy,
) -> Result<raw_response::StreamedArtifact, McpError> {
    let client = streaming_client()?;
    let deadline = tokio::time::Instant::now() + policy.total_deadline;
    for attempt in 1..=policy.max_attempts.max(1) {
        let remaining = remaining_until(deadline)?;
        let response = tokio::select! {
            () = policy.cancellation.cancelled() => return Err(api_error("streaming request cancelled", Some(499), None)),
            result = tokio::time::timeout(remaining, client.get(url).timeout(remaining).send()) => match result {
                Ok(Ok(response)) => response,
                Ok(Err(error)) if attempt < policy.max_attempts && (error.is_connect() || error.is_timeout()) => { retry_stream_attempt(attempt, &policy, deadline).await?; continue; }
                Ok(Err(error)) => return Err(map_reqwest_error(&error, url)),
                Err(_) if attempt < policy.max_attempts => { retry_stream_attempt(attempt, &policy, deadline).await?; continue; }
                Err(_) => return Err(api_error("streaming request exceeded total deadline", Some(408), None)),
            }
        };
        let status = response.status();
        if !status.is_success() {
            if attempt < policy.max_attempts && matches!(status.as_u16(), 429 | 502 | 503 | 504) {
                retry_stream_attempt(attempt, &policy, deadline).await?;
                continue;
            }
            let body = response.text().await.unwrap_or_default();
            return Err(api_error(
                format!("streaming request failed with status {}", status.as_u16()),
                Some(status.as_u16()),
                Some(OriginalError::String(body)),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > policy.max_encoded_bytes)
        {
            return Err(api_error(
                "encoded response exceeds streamed artifact limit",
                Some(413),
                None,
            ));
        }
        return tokio::time::timeout(
            remaining_until(deadline)?,
            persist_decoded_response(response, filename_prefix, extension, content_type, &policy),
        )
        .await
        .map_err(|_| api_error("streaming request exceeded total deadline", Some(408), None))?;
    }
    unreachable!("bounded streaming retry loop returns")
}

fn remaining_until(deadline: tokio::time::Instant) -> Result<Duration, McpError> {
    deadline
        .checked_duration_since(tokio::time::Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| api_error("streaming request exceeded total deadline", Some(408), None))
}

async fn retry_stream_attempt(
    attempt: usize,
    policy: &StreamingPolicy,
    deadline: tokio::time::Instant,
) -> Result<(), McpError> {
    let delay = Duration::from_millis(100_u64.saturating_mul(1_u64 << attempt.min(5)));
    tokio::select! {
        () = policy.cancellation.cancelled() => Err(api_error("streaming request cancelled", Some(499), None)),
        () = tokio::time::sleep_until(deadline) => Err(api_error("streaming request exceeded total deadline", Some(408), None)),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// HTTP verb set accepted by the generic API client. Mirrors the TS
/// `RequestOptions.method` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    fn as_reqwest_method(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Put => Method::PUT,
            Self::Patch => Method::PATCH,
            Self::Delete => Method::DELETE,
        }
    }
}

/// Request options for a single API call. Matches TS `RequestOptions`.
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    pub method: Option<HttpMethod>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    /// URL-encoded form body. Used by APIs such as Splunk whose POST
    /// endpoints do not accept JSON. Mutually exclusive with `body`;
    /// `form` takes precedence if both are supplied.
    pub form: Option<BTreeMap<String, String>>,
    pub timeout: Option<Duration>,
}

/// What the TS code calls `TransportResponse<T>`. `data` is the successfully
/// parsed body (JSON value, raw text for `text/plain`, or `{}` for empties);
/// `raw_response_path` points at the on-disk persisted JSON body when one was
/// written.
#[derive(Debug, Clone)]
pub struct TransportResponse {
    pub data: ResponseBody,
    pub raw_response_path: Option<std::path::PathBuf>,
}

/// Typed response body. `Json` is the canonical successful case; the other
/// variants preserve the TS contract for diffs and DELETEs.
#[derive(Debug, Clone)]
pub enum ResponseBody {
    Json(Value),
    Text(String),
    Empty,
}

impl ResponseBody {
    pub fn as_json(&self) -> Option<&Value> {
        if let Self::Json(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

fn log_ninjaone_request(vendor_name: &str, url: &str, method: HttpMethod, body: Option<&Value>) {
    if vendor_name != "ninjaone" {
        return;
    }

    let body = body.map_or(Value::Null, crate::vendor::ninjaone::sanitized_http_json);
    debug!(
        target: crate::vendor::ninjaone::HTTP_LOG_TARGET,
        %url,
        method = method.as_str(),
        body = %body,
        "ninjaone HTTP request"
    );
}

fn log_ninjaone_error_response(
    vendor_name: &str,
    url: &str,
    method: HttpMethod,
    status: StatusCode,
    body_text: &str,
) {
    if vendor_name != "ninjaone" {
        return;
    }

    debug!(
        target: crate::vendor::ninjaone::HTTP_LOG_TARGET,
        %url,
        method = method.as_str(),
        status = status.as_u16(),
        body = %crate::vendor::ninjaone::sanitized_http_text(body_text),
        "ninjaone HTTP response"
    );
}

fn log_ninjaone_response(
    vendor_name: &str,
    url: &str,
    method: HttpMethod,
    status: StatusCode,
    body: &ResponseBody,
) {
    if vendor_name != "ninjaone" {
        return;
    }

    let body = match body {
        ResponseBody::Json(value) => crate::vendor::ninjaone::sanitized_http_json(value),
        _ => serde_json::json!({ "nonJsonBody": "<omitted>" }),
    };
    debug!(
        target: crate::vendor::ninjaone::HTTP_LOG_TARGET,
        %url,
        method = method.as_str(),
        status = status.as_u16(),
        body = %body,
        "ninjaone HTTP response"
    );
}

/// Vendor-neutral entry point. Resolves the vendor's base URL, builds the
/// auth header, sends the request, and classifies the response. Non-2xx
/// responses go through [`Vendor::classify_error`] for vendor-specific
/// envelope parsing.
///
/// The `path` parameter is forwarded as-is; callers (typically the
/// controller layer) are expected to have already applied
/// [`Vendor::normalize_path`] so that path normalisation lives in one
/// place. The transport itself only joins base + path.
pub async fn fetch(
    client: &Client,
    vendor: &dyn Vendor,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    let base = vendor.base_url(config)?;
    let url = normalize_url_with_base(&base, path);
    let method = options.method.unwrap_or(HttpMethod::Get);

    let (auth_name, auth_header) = validate_auth(credentials)?;
    let timeout = resolve_timeout(config, options.timeout);

    let cache_config = response_cache::CacheConfig::from_config(config);
    let cache_key = response_cache::CacheKey::new(
        vendor.name(),
        &url,
        &auth_name,
        &auth_header,
        &options.headers,
    );
    if method != HttpMethod::Get {
        response_cache::invalidate_namespace(vendor.name(), &base);
    } else if cache_config.enabled
        && response_cache::request_is_cacheable(&url, &auth_name, &options)
        && let Some(body) = response_cache::get(&cache_key)
    {
        debug!(vendor = vendor.name(), %url, "HTTP response cache hit");
        return Ok(TransportResponse {
            data: body,
            raw_response_path: None,
        });
    }

    let request_body_for_log = options.body.clone().or_else(|| {
        options
            .form
            .as_ref()
            .and_then(|form| serde_json::to_value(form).ok())
    });
    let req = build_request(
        client,
        method,
        &url,
        &auth_name,
        &auth_header,
        &options,
        timeout,
    );

    debug!(
        %url,
        method = method.as_str(),
        vendor = vendor.name(),
        "dispatching API request"
    );
    log_ninjaone_request(vendor.name(), &url, method, request_body_for_log.as_ref());

    let start = std::time::Instant::now();
    let response = req.send().await.map_err(|e| map_reqwest_error(&e, &url))?;
    let duration = start.elapsed();

    enforce_content_length_cap(&response)?;

    let status = response.status();
    let response_headers = response.headers().clone();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        log_ninjaone_error_response(vendor.name(), &url, method, status, &body_text);
        return Err(vendor.classify_error(status, &body_text));
    }

    let body = classify_body(response).await?;
    log_ninjaone_response(vendor.name(), &url, method, status, &body);

    // Some APIs (notably Slack's Web API) return `200 OK` with an
    // application-level error envelope in the body (`{"ok": false, ...}`). Give
    // the vendor a chance to reclassify such a "success" as a typed error
    // before we treat the body as data or persist it. No-op for vendors whose
    // HTTP status already reflects failure.
    if let ResponseBody::Json(value) = &body
        && let Some(err) = vendor.classify_success_json(value)
    {
        return Err(err);
    }

    if method == HttpMethod::Get
        && cache_config.enabled
        && response_cache::request_is_cacheable(&url, &auth_name, &options)
    {
        response_cache::store(cache_key, &body, &response_headers, &cache_config);
    }

    let raw_path = if let ResponseBody::Json(value) = &body {
        raw_response::save(
            &url,
            method.as_str(),
            request_body_for_log.as_ref(),
            value,
            status.as_u16(),
            duration,
        )
        .await
    } else {
        None
    };

    Ok(TransportResponse {
        data: body,
        raw_response_path: raw_path,
    })
}

/// Bitbucket-specialised shim. Equivalent to calling [`fetch`] with a
/// fresh [`BitbucketVendor`]. Preserved for back-compat; new code should
/// call [`fetch`] with the vendor explicitly.
pub async fn fetch_bitbucket(
    client: &Client,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    let vendor = BitbucketVendor::new();
    fetch(client, &vendor, credentials, config, path, options).await
}

/// Bitbucket-specialised shim that overrides the base URL (e.g. to point at
/// a wiremock in tests). Equivalent to calling [`fetch`] with
/// [`BitbucketVendor::with_base_url`].
pub async fn fetch_bitbucket_with_base(
    base_url: &str,
    client: &Client,
    credentials: &Credentials,
    config: &Config,
    path: &str,
    options: RequestOptions,
) -> Result<TransportResponse, McpError> {
    let vendor = BitbucketVendor::with_base_url(base_url);
    fetch(client, &vendor, credentials, config, path, options).await
}

/// Construct a shared reqwest client with sensible defaults. Callers should
/// cache this for the lifetime of the process.
pub fn build_client() -> Result<Client, McpError> {
    Client::builder()
        .user_agent(format!(
            "{}/{}",
            crate::constants::UNSCOPED_PACKAGE_NAME,
            crate::constants::VERSION
        ))
        .build()
        .map_err(|e| unexpected(format!("failed to build HTTP client: {e}"), None))
}

/// Dedicated client for ingestion bodies. Automatic decompression is disabled
/// so the transport can account for wire bytes before bounded decoding.
fn streaming_client() -> Result<&'static Client, McpError> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .user_agent(format!(
                    "{}/{}",
                    crate::constants::UNSCOPED_PACKAGE_NAME,
                    crate::constants::VERSION
                ))
                .gzip(false)
                .brotli(false)
                .deflate(false)
                .zstd(false)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| {
            unexpected(
                format!("failed to build streaming HTTP client: {error}"),
                None,
            )
        })
}

// ---- helpers ----

fn normalize_url_with_base(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let suffix = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("{base}{suffix}")
}

fn validate_auth(credentials: &Credentials) -> Result<(HeaderName, HeaderValue), McpError> {
    // Scheme-agnostic: `auth_header()` emits Basic for the Atlassian variants,
    // Bearer for Zoom's resolved token, and a bare key for a custom-header API
    // key. `auth_header_name()` picks the header that value rides in —
    // `Authorization` for everything except `ApiKeyHeader`.
    let name = credentials.auth_header_name()?;
    let raw = credentials.auth_header();
    let value =
        HeaderValue::from_str(&raw).map_err(|_| auth_invalid("Invalid authentication header"))?;
    Ok((name, value))
}

fn resolve_timeout(config: &Config, override_timeout: Option<Duration>) -> Duration {
    if let Some(t) = override_timeout {
        return t;
    }
    let env_ms = config.get_int(
        "ATLASSIAN_REQUEST_TIMEOUT",
        i64::try_from(DEFAULT_REQUEST.as_millis()).unwrap_or(30_000),
    );
    if env_ms <= 0 {
        DEFAULT_REQUEST
    } else {
        Duration::from_millis(u64::try_from(env_ms).unwrap_or(30_000))
    }
}

fn build_request(
    client: &Client,
    method: HttpMethod,
    url: &str,
    auth_name: &HeaderName,
    auth: &HeaderValue,
    options: &RequestOptions,
    timeout: Duration,
) -> reqwest::RequestBuilder {
    let mut req = client
        .request(method.as_reqwest_method(), url)
        .timeout(timeout)
        .header(auth_name.clone(), auth.clone())
        .header(ACCEPT, HeaderValue::from_static("application/json"));

    for (k, v) in &options.headers {
        req = req.header(k, v);
    }

    if let Some(form) = options.form.as_ref() {
        req = req.form(form);
    } else if let Some(body) = options.body.as_ref() {
        req = req.json(body);
    } else {
        req = req.header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    req
}

fn enforce_content_length_cap(response: &reqwest::Response) -> Result<(), McpError> {
    let Some(value) = response.headers().get(CONTENT_LENGTH) else {
        return Ok(());
    };
    let Ok(text) = value.to_str() else {
        return Ok(());
    };
    let Ok(size) = text.parse::<u64>() else {
        return Ok(());
    };
    let cap = MAX_RESPONSE_SIZE as u64;
    if size > cap {
        let mb = size / (1024 * 1024);
        let cap_mb = cap / (1024 * 1024);
        let info = serde_json::json!({ "responseSize": size, "limit": MAX_RESPONSE_SIZE });
        return Err(api_error(
            format!("Response size ({mb}MB) exceeds maximum limit of {cap_mb}MB"),
            Some(413),
            Some(OriginalError::Json(info)),
        ));
    }
    Ok(())
}

async fn classify_body(response: reqwest::Response) -> Result<ResponseBody, McpError> {
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(ResponseBody::Empty);
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    if content_type.contains("text/plain") {
        let text = response
            .text()
            .await
            .map_err(|e| unexpected(format!("failed to read text body: {e}"), None))?;
        return Ok(ResponseBody::Text(text));
    }

    let text = response
        .text()
        .await
        .map_err(|e| unexpected(format!("failed to read body: {e}"), None))?;

    if text.trim().is_empty() {
        return Ok(ResponseBody::Empty);
    }

    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Ok(ResponseBody::Json(value)),
        Err(_) => Ok(ResponseBody::Text(text)),
    }
}

fn map_reqwest_error(err: &reqwest::Error, url: &str) -> McpError {
    if err.is_timeout() {
        return api_error(
            format!("Request timeout: API did not respond in time at {url}"),
            Some(408),
            Some(OriginalError::String(err.to_string())),
        );
    }
    if err.is_connect() {
        return api_error(
            format!("Network error connecting to API: {err}"),
            Some(503),
            Some(OriginalError::String(err.to_string())),
        );
    }
    unexpected(
        err.to_string(),
        Some(OriginalError::String(err.to_string())),
    )
}

/// Exposed for callers that just want a well-formed auth header (e.g. tests
/// and diagnostics). Prefer [`fetch`] for real traffic.
///
/// Vendor-scoped; the same email may have a different token per vendor.
/// Synchronous — safe in tests and diagnostics. Async server paths must
/// use [`Credentials::require_for_async`] so the keychain syscall doesn't
/// block a Tokio worker.
pub fn require_credentials(config: &Config, vendor: &str) -> Result<Credentials, McpError> {
    Credentials::require_for(config, vendor)
}
