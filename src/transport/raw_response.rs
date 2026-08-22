//! Persist successful JSON API responses to `/tmp/mcp/<unscoped-pkg>/` for
//! offline inspection. Mirrors TS `response.util.ts`.
//!
//! Filename is `<iso-ts-dashed>-<8hex>.txt` (colons/dots in the timestamp are
//! replaced with `-` to match TS). Body uses 80-char `=` separators around
//! metadata / request body / response data sections.
//!
//! All filesystem I/O goes through `tokio::fs` so the request path stays
//! cooperatively async — a heavy traffic burst can't block tokio worker
//! threads on `mkdir`/`write` syscalls.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncWriteExt, BufWriter};
use tracing::debug;

use crate::constants::UNSCOPED_PACKAGE_NAME;

static SESSION_DIR: OnceLock<PathBuf> = OnceLock::new();
static ARTIFACTS: OnceLock<RwLock<HashMap<String, RegisteredArtifact>>> = OnceLock::new();
static ORPHANED_RESERVATIONS: OnceLock<std::sync::Mutex<Vec<OrphanedReservation>>> =
    OnceLock::new();
static NEXT_ARTIFACT_GENERATION: AtomicU64 = AtomicU64::new(1);
static MONOTONIC_ORIGIN: OnceLock<Instant> = OnceLock::new();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static PIN_RELEASED: OnceLock<tokio::sync::Notify> = OnceLock::new();
static RECLAMATION_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static RETENTION_SWEEPER: OnceLock<std::sync::Mutex<Option<RetentionSweeper>>> = OnceLock::new();
#[cfg(test)]
static DELETE_FAILURES: OnceLock<std::sync::Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
#[cfg(test)]
static DELETE_ATTEMPTS: OnceLock<std::sync::Mutex<Vec<PathBuf>>> = OnceLock::new();

#[derive(Debug)]
struct OrphanedReservation {
    path: PathBuf,
    _reservation: super::StreamingDiskLease,
}

#[derive(Debug)]
struct RegisteredArtifact {
    metadata: ArtifactMetadata,
    disk: Option<DiskReservation>,
    generation: u64,
    committed_at: Duration,
    lifecycle: ArtifactLifecycle,
    pins: u64,
    retention_eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactLifecycle {
    Readable,
    PendingDelete,
}

#[derive(Debug)]
struct RetentionSweeper {
    cancellation: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
struct DiskReservation {
    artifact: super::StreamingDiskLease,
    sidecar: Option<super::StreamingDiskLease>,
}

/// Exact registry-generation pin held from lookup through the final read.
/// Dropping it releases one pin and schedules pending reclamation exactly once.
#[derive(Debug)]
pub struct ArtifactReadPin {
    metadata: ArtifactMetadata,
    generation: u64,
    released: bool,
}

impl ArtifactReadPin {
    pub fn metadata(&self) -> &ArtifactMetadata {
        &self.metadata
    }
}

impl Drop for ArtifactReadPin {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let should_reclaim = release_pin(&self.metadata.id, self.generation);
        if should_reclaim && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let id = self.metadata.id.clone();
            let generation = self.generation;
            runtime.spawn(async move {
                let _ = reclaim_pending_artifact(&id, generation).await;
            });
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactMetadata {
    pub id: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
    pub etag: String,
}

/// Metadata produced while an artifact is written incrementally.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamedArtifact {
    #[serde(flatten)]
    pub artifact: ArtifactMetadata,
    pub sha256: String,
    pub head: String,
    pub tail: String,
    /// Bytes received from the HTTP body before content decoding. For locally
    /// generated artifacts this is equal to the artifact size.
    pub encoded_bytes: u64,
    /// Bytes emitted after content decoding and persisted to the artifact.
    pub decoded_bytes: u64,
}

/// Same-filesystem atomic artifact writer with bounded preview state.
pub struct ArtifactWriter {
    id: String,
    filename: String,
    path: PathBuf,
    partial_path: PathBuf,
    content_type: String,
    writer: Option<BufWriter<fs::File>>,
    size: u64,
    max_bytes: u64,
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    hasher: Sha256,
    committed: bool,
    disk_reservation: Option<super::StreamingDiskLease>,
    #[cfg(test)]
    fault: Option<ArtifactWriterFault>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactWriterFault {
    Write,
    Commit,
}

impl ArtifactWriter {
    pub fn set_disk_quota(&mut self, quota: &std::sync::Arc<super::StreamingDiskQuota>) {
        debug_assert_eq!(self.size, 0);
        self.disk_reservation = Some(
            quota
                .lease()
                .expect("new streaming disk transaction accepts a writer lease"),
        );
    }

    #[cfg(test)]
    fn inject_fault(&mut self, fault: ArtifactWriterFault) {
        self.fault = Some(fault);
    }

    pub async fn write_chunk(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let next = self
            .size
            .checked_add(u64::try_from(bytes.len()).map_err(std::io::Error::other)?)
            .ok_or_else(|| std::io::Error::other("streamed artifact byte counter overflow"))?;
        if next > self.max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!("streamed artifact exceeds {} bytes", self.max_bytes),
            ));
        }
        let byte_count = u64::try_from(bytes.len()).map_err(std::io::Error::other)?;
        if let Some(reservation) = &self.disk_reservation {
            reservation.grow(byte_count).await?;
        }
        #[cfg(test)]
        if self.fault == Some(ArtifactWriterFault::Write) {
            return Err(std::io::Error::other("injected artifact write failure"));
        }
        self.writer
            .as_mut()
            .expect("artifact writer is open")
            .write_all(bytes)
            .await?;
        let head_limit = crate::constants::data_limits::STREAM_PREVIEW_HEAD_SIZE;
        if self.head.len() < head_limit {
            let take = (head_limit - self.head.len()).min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
        }
        let tail_limit = crate::constants::data_limits::STREAM_PREVIEW_TAIL_SIZE;
        self.tail.extend(bytes.iter().copied());
        while self.tail.len() > tail_limit {
            self.tail.pop_front();
        }
        self.hasher.update(bytes);
        self.size = next;
        Ok(())
    }

    pub async fn commit(mut self) -> std::io::Result<StreamedArtifact> {
        let mut writer = self.writer.take().expect("artifact writer is open");
        writer.flush().await?;
        let file = writer.into_inner();
        file.sync_all().await?;
        drop(file);
        #[cfg(test)]
        if self.fault == Some(ArtifactWriterFault::Commit) {
            return Err(std::io::Error::other("injected artifact commit failure"));
        }
        let generation = next_artifact_generation()?;
        fs::rename(&self.partial_path, &self.path).await?;
        let metadata = ArtifactMetadata {
            id: self.id.clone(),
            path: self.path.clone(),
            filename: self.filename.clone(),
            content_type: self.content_type.clone(),
            size: self.size,
            etag: format!("\"{}-{}\"", self.id, self.size),
        };
        artifacts()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                self.id.clone(),
                RegisteredArtifact {
                    metadata: metadata.clone(),
                    disk: self
                        .disk_reservation
                        .take()
                        .map(|artifact| DiskReservation {
                            artifact,
                            sidecar: None,
                        }),
                    generation,
                    committed_at: monotonic_now(),
                    lifecycle: ArtifactLifecycle::Readable,
                    pins: 0,
                    retention_eligible: false,
                },
            );
        self.committed = true;
        Ok(StreamedArtifact {
            artifact: metadata,
            sha256: format!("{:x}", self.hasher.clone().finalize()),
            head: String::from_utf8_lossy(&self.head).into_owned(),
            tail: String::from_utf8_lossy(self.tail.make_contiguous()).into_owned(),
            encoded_bytes: self.size,
            decoded_bytes: self.size,
        })
    }
}

impl Drop for ArtifactWriter {
    fn drop(&mut self) {
        if !self.committed {
            let cleaned = match std::fs::remove_file(&self.partial_path) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
            };
            if cleaned {
                self.disk_reservation.take();
            } else if let Some(reservation) = self.disk_reservation.take() {
                retain_orphaned_reservation(self.partial_path.clone(), reservation);
            }
        }
    }
}

pub async fn begin_artifact(
    filename_prefix: &str,
    extension: &str,
    content_type: &str,
    max_bytes: u64,
) -> std::io::Result<ArtifactWriter> {
    let dir = init();
    fs::create_dir_all(&dir).await?;
    let safe_prefix: String = filename_prefix
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = uuid::Uuid::new_v4().to_string();
    let extension = extension.trim_start_matches('.');
    let filename = format!("{safe_prefix}-{id}.{extension}");
    let path = dir.join(&filename);
    let partial_path = dir.join(format!(".{filename}.part"));
    let file = fs::File::create(&partial_path).await?;
    Ok(ArtifactWriter {
        id,
        filename,
        path,
        partial_path,
        content_type: content_type.to_owned(),
        writer: Some(BufWriter::with_capacity(
            crate::constants::data_limits::STREAM_WRITE_BUFFER_SIZE,
            file,
        )),
        size: 0,
        max_bytes,
        head: Vec::with_capacity(crate::constants::data_limits::STREAM_PREVIEW_HEAD_SIZE),
        tail: std::collections::VecDeque::with_capacity(
            crate::constants::data_limits::STREAM_PREVIEW_TAIL_SIZE,
        ),
        hasher: Sha256::new(),
        committed: false,
        disk_reservation: None,
        #[cfg(test)]
        fault: None,
    })
}

fn artifacts() -> &'static RwLock<HashMap<String, RegisteredArtifact>> {
    ARTIFACTS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn next_artifact_generation() -> std::io::Result<u64> {
    NEXT_ARTIFACT_GENERATION
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| std::io::Error::other("artifact generation identifier overflow"))
}

fn monotonic_now() -> Duration {
    MONOTONIC_ORIGIN.get_or_init(Instant::now).elapsed()
}

fn pin_released() -> &'static tokio::sync::Notify {
    PIN_RELEASED.get_or_init(tokio::sync::Notify::new)
}

fn reclamation_lock() -> &'static tokio::sync::Mutex<()> {
    RECLAMATION_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn retention_sweeper() -> &'static std::sync::Mutex<Option<RetentionSweeper>> {
    RETENTION_SWEEPER.get_or_init(|| std::sync::Mutex::new(None))
}

pub fn artifact(id: &str) -> Option<ArtifactMetadata> {
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return None;
    }
    artifacts()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .filter(|registered| registered.lifecycle == ArtifactLifecycle::Readable)
        .map(|registered| registered.metadata.clone())
}

pub fn artifact_for_path(path: &Path) -> Option<ArtifactMetadata> {
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return None;
    }
    artifacts()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .find(|artifact| {
            artifact.lifecycle == ArtifactLifecycle::Readable && artifact.metadata.path == path
        })
        .map(|registered| registered.metadata.clone())
}

/// Pin the current readable generation before a caller opens its file.
pub fn pin_artifact(id: &str) -> Option<ArtifactReadPin> {
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return None;
    }
    let mut entries = artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return None;
    }
    let registered = entries.get_mut(id)?;
    if registered.lifecycle != ArtifactLifecycle::Readable {
        return None;
    }
    registered.pins = registered.pins.checked_add(1)?;
    Some(ArtifactReadPin {
        metadata: registered.metadata.clone(),
        generation: registered.generation,
        released: false,
    })
}

fn release_pin(id: &str, generation: u64) -> bool {
    let mut entries = artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(registered) = entries.get_mut(id) else {
        return false;
    };
    if registered.generation != generation || registered.pins == 0 {
        return false;
    }
    registered.pins -= 1;
    pin_released().notify_waiters();
    registered.pins == 0 && registered.lifecycle == ArtifactLifecycle::PendingDelete
}

/// Reconcile externally removed artifacts with the in-memory registry. This
/// keeps reservations conservative until every physical sidecar is gone.
pub(crate) fn reconcile_missing_artifacts() {
    let mut entries = artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    entries.retain(|_, registered| {
        let path = &registered.metadata.path;
        let manifest = path.with_extension("manifest.json");
        let manifest_part = path.with_extension("manifest.json.part");
        if path.exists() {
            if !manifest.exists()
                && !manifest_part.exists()
                && !manifest_auxiliary_exists(path)
                && let Some(disk) = &mut registered.disk
            {
                disk.sidecar.take();
            }
            return true;
        }
        let Ok(associated) = associated_paths(path) else {
            return true;
        };
        for sidecar in associated.into_iter().filter(|candidate| candidate != path) {
            match std::fs::remove_file(sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return true,
            }
        }
        false
    });
    drop(entries);
    reconcile_orphaned_reservations();
}

fn manifest_auxiliary_exists(path: &Path) -> bool {
    let manifest = path.with_extension("manifest.json");
    let (Some(parent), Some(manifest_name)) = (manifest.parent(), manifest.file_name()) else {
        return false;
    };
    let prefix = format!("{}.", manifest_name.to_string_lossy());
    std::fs::read_dir(parent).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
    })
}

fn associated_paths(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = vec![
        path.to_path_buf(),
        path.with_extension("manifest.json"),
        path.with_extension("manifest.json.part"),
    ];
    if let (Some(parent), Some(filename)) = (path.parent(), path.file_name()) {
        paths.push(parent.join(format!(".{}.part", filename.to_string_lossy())));
    }
    let manifest = path.with_extension("manifest.json");
    if let (Some(parent), Some(manifest_name)) = (manifest.parent(), manifest.file_name()) {
        let prefix = format!("{}.", manifest_name.to_string_lossy());
        match std::fs::read_dir(parent) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_name().to_string_lossy().starts_with(&prefix) {
                        paths.push(entry.path());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn orphaned_reservations() -> &'static std::sync::Mutex<Vec<OrphanedReservation>> {
    ORPHANED_RESERVATIONS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

pub(crate) fn retain_orphaned_reservation(path: PathBuf, reservation: super::StreamingDiskLease) {
    if path.exists() {
        orphaned_reservations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(OrphanedReservation {
                path,
                _reservation: reservation,
            });
    }
}

fn reconcile_orphaned_reservations() {
    orphaned_reservations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|orphan| orphan.path.exists());
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReclamationReport {
    pub attempted: usize,
    pub removed: usize,
    pub failed: usize,
}

/// Remove a committed artifact without racing an existing full or range read.
/// New reads stop immediately; physical deletion waits for the last exact
/// generation pin and registry reservations remain live on any deletion error.
pub async fn remove_artifact(path: &Path) -> std::io::Result<()> {
    let pending = {
        let mut entries = artifacts()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries
            .iter_mut()
            .find(|(_, artifact)| artifact.metadata.path == path)
            .map(|(id, artifact)| {
                artifact.lifecycle = ArtifactLifecycle::PendingDelete;
                (id.clone(), artifact.generation, artifact.pins)
            })
    };
    if let Some((id, generation, pins)) = pending {
        if pins == 0 {
            reclaim_pending_artifact(&id, generation).await?;
        }
        return Ok(());
    }

    remove_associated_files(path).await?;
    reconcile_orphaned_reservations();
    Ok(())
}

async fn reclaim_pending_artifact(id: &str, generation: u64) -> std::io::Result<bool> {
    let _reclamation = reclamation_lock().lock().await;
    let path = {
        let entries = artifacts()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(registered) = entries.get(id) else {
            return Ok(false);
        };
        if registered.generation != generation
            || registered.lifecycle != ArtifactLifecycle::PendingDelete
            || registered.pins != 0
        {
            return Ok(false);
        }
        registered.metadata.path.clone()
    };

    remove_associated_files(&path).await?;
    let mut entries = artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let removed = match entries.get(id) {
        None => true,
        Some(registered) if registered.generation == generation => {
            entries.remove(id);
            true
        }
        Some(_) => false,
    };
    drop(entries);
    if !removed {
        return Err(std::io::Error::other(
            "artifact registry generation changed during reclamation",
        ));
    }
    reconcile_orphaned_reservations();
    Ok(true)
}

async fn remove_associated_files(path: &Path) -> std::io::Result<()> {
    for candidate in associated_paths(path)? {
        remove_reclamation_file(&candidate).await?;
    }
    Ok(())
}

async fn remove_reclamation_file(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        DELETE_ATTEMPTS
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path.to_path_buf());
        let mut failures = DELETE_FAILURES
            .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(remaining) = failures.get_mut(path)
            && *remaining != 0
        {
            *remaining -= 1;
            return Err(std::io::Error::other("injected artifact deletion failure"));
        }
    }
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(crate) fn set_committed_at(id: &str, committed_at: Duration) {
    artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(id)
        .expect("test artifact is registered")
        .committed_at = committed_at;
}

#[cfg(test)]
fn inject_delete_failures(path: PathBuf, count: u64) {
    DELETE_FAILURES
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path, count);
}

#[cfg(test)]
fn take_delete_attempts() -> Vec<PathBuf> {
    std::mem::take(
        &mut *DELETE_ATTEMPTS
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

pub(crate) async fn sweep_expired_at(now: Duration, retention: Duration) -> ReclamationReport {
    sweep_expired_at_filtered(now, retention, None).await
}

async fn sweep_expired_at_filtered(
    now: Duration,
    retention: Duration,
    only_ids: Option<&[String]>,
) -> ReclamationReport {
    let candidates = {
        let mut entries = artifacts()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ordered: Vec<_> = entries
            .iter()
            .filter(|(_, artifact)| {
                only_ids.is_none_or(|ids| ids.contains(&artifact.metadata.id))
                    && ((artifact.lifecycle == ArtifactLifecycle::PendingDelete
                        && artifact.pins == 0)
                        || (artifact.retention_eligible
                            && artifact.lifecycle == ArtifactLifecycle::Readable
                            && artifact
                                .committed_at
                                .checked_add(retention)
                                .is_some_and(|expires| expires <= now)))
            })
            .map(|(id, artifact)| (artifact.committed_at, id.clone(), artifact.generation))
            .collect();
        ordered.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        ordered.truncate(crate::constants::data_limits::MAX_STREAMING_ARTIFACT_RECLAIMS_PER_SWEEP);
        for (_, id, _) in &ordered {
            if let Some(artifact) = entries.get_mut(id) {
                artifact.lifecycle = ArtifactLifecycle::PendingDelete;
            }
        }
        ordered
    };

    let mut report = ReclamationReport::default();
    for (_, id, generation) in candidates {
        let pinned = artifacts()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .is_some_and(|artifact| artifact.pins != 0);
        if pinned {
            continue;
        }
        report.attempted += 1;
        match reclaim_pending_artifact(&id, generation).await {
            Ok(true) => report.removed += 1,
            Ok(false) => {}
            Err(error) => {
                report.failed += 1;
                debug!(%error, artifact_id = %id, "artifact reclamation deferred after deletion failure");
            }
        }
    }
    report
}

#[cfg(test)]
pub(crate) async fn sweep_artifacts_at(
    ids: &[String],
    now: Duration,
    retention: Duration,
) -> ReclamationReport {
    sweep_expired_at_filtered(now, retention, Some(ids)).await
}

/// Transfer a successfully committed sidecar reservation to its artifact.
pub(crate) fn attach_sidecar_reservation(
    artifact_path: &Path,
    quota: &std::sync::Arc<super::StreamingDiskQuota>,
    reservation: &mut Option<super::StreamingDiskLease>,
) -> std::io::Result<()> {
    let mut entries = artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let registered = entries
        .values_mut()
        .find(|entry| entry.metadata.path == artifact_path)
        .ok_or_else(|| std::io::Error::other("artifact is not registered"))?;
    if registered.lifecycle != ArtifactLifecycle::Readable {
        return Err(std::io::Error::other("artifact deletion is pending"));
    }
    let disk = registered
        .disk
        .as_mut()
        .ok_or_else(|| std::io::Error::other("artifact has no disk reservation"))?;
    let sidecar = reservation
        .as_ref()
        .ok_or_else(|| std::io::Error::other("sidecar reservation was already transferred"))?;
    if !disk.artifact.same_transaction(quota) || !sidecar.same_transaction(quota) {
        return Err(std::io::Error::other("sidecar uses a different disk quota"));
    }
    disk.sidecar = reservation.take();
    registered.committed_at = monotonic_now();
    registered.retention_eligible = true;
    Ok(())
}

/// Start the bounded periodic retention pass for the process session.
pub fn start_retention_sweeper() {
    let mut active = retention_sweeper()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if active.is_some() {
        return;
    }
    SHUTTING_DOWN.store(false, Ordering::Release);
    let config = crate::config::load();
    let retention = config.streaming_artifact_retention();
    let interval = config.streaming_artifact_sweep_interval();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            tokio::select! {
                () = task_cancellation.cancelled() => break,
                _ = ticker.tick() => {
                    let report = sweep_expired_at(monotonic_now(), retention).await;
                    if report.removed != 0 || report.failed != 0 {
                        debug!(removed = report.removed, failed = report.failed, "completed artifact retention sweep");
                    }
                }
            }
        }
    });
    *active = Some(RetentionSweeper { cancellation, task });
}

/// Stop new pins and sweeps, wait a bounded time for active reads, drain
/// eligible deletion work, then reconcile the process-owned session.
pub async fn shutdown_and_cleanup() {
    SHUTTING_DOWN.store(true, Ordering::Release);
    let sweeper = retention_sweeper()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(sweeper) = sweeper {
        sweeper.cancellation.cancel();
        let mut task = sweeper.task;
        if tokio::time::timeout(
            crate::constants::data_limits::STREAMING_ARTIFACT_SHUTDOWN_TIMEOUT,
            &mut task,
        )
        .await
        .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }

    {
        let mut entries = artifacts()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for artifact in entries.values_mut() {
            artifact.lifecycle = ArtifactLifecycle::PendingDelete;
        }
    }
    let deadline = tokio::time::Instant::now()
        + crate::constants::data_limits::STREAMING_ARTIFACT_SHUTDOWN_TIMEOUT;
    loop {
        let pins = active_pin_count();
        if pins == 0 {
            break;
        }
        if tokio::time::timeout_at(deadline, pin_released().notified())
            .await
            .is_err()
        {
            debug!(pins, "artifact shutdown deadline elapsed with active reads");
            break;
        }
    }
    if active_pin_count() == 0 {
        let _ = sweep_expired_at(Duration::MAX, Duration::ZERO).await;
    }
    cleanup_current_session();
}

fn active_pin_count() -> u64 {
    artifacts()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .try_fold(0_u64, |total, artifact| total.checked_add(artifact.pins))
        .unwrap_or(u64::MAX)
}

fn base_dir() -> PathBuf {
    std::env::temp_dir().join("mcp").join(UNSCOPED_PACKAGE_NAME)
}

/// Prepare a process-owned artifact directory and remove artifacts left by
/// processes that are no longer running. Safe to call more than once.
pub fn init() -> PathBuf {
    SESSION_DIR
        .get_or_init(|| {
            let base = base_dir();
            let _ = std::fs::create_dir_all(&base);
            cleanup_abandoned(&base);
            let dir = base.join(format!(
                "session-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let _ = std::fs::create_dir_all(&dir);
            dir
        })
        .clone()
}

/// Remove artifacts owned by this process. Forced termination cannot run this
/// hook; the next process startup removes the abandoned session directory.
pub fn cleanup_current_session() {
    let pins = active_pin_count();
    if pins != 0 {
        debug!(pins, "retaining artifact session with active reads");
        reconcile_missing_artifacts();
        return;
    }
    let mut cleaned = false;
    if let Some(dir) = SESSION_DIR.get() {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => {
                cleaned = true;
                debug!(dir = %dir.display(), "removed temporary response artifacts");
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => cleaned = true,
            Err(err) => {
                debug!(%err, dir = %dir.display(), "failed to remove temporary response artifacts");
            }
        }
    }
    if cleaned {
        artifacts()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        orphaned_reservations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    } else {
        reconcile_missing_artifacts();
    }
}

fn cleanup_abandoned(base: &Path) {
    cleanup_abandoned_with(base, process_is_running);
}

fn cleanup_abandoned_with(base: &Path, is_running: impl Fn(u32) -> bool) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            // Clean up files created by the legacy flat-directory layout.
            let _ = std::fs::remove_file(path);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(pid) = name
            .strip_prefix("session-")
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if !is_running(pid) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    let Ok(output) = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
    else {
        return true;
    };
    if !output.status.success() {
        return true;
    }
    String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
}

#[cfg(not(windows))]
fn process_is_running(pid: u32) -> bool {
    let Ok(output) = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
    else {
        return true;
    };
    if !output.status.success() {
        return output.status.code() != Some(1);
    }
    !output.stdout.is_empty()
}

/// Write the raw API response to disk and return the path written. Returns
/// `None` on any failure — parity with TS behaviour, which logs but does not
/// propagate errors from this subsystem.
pub async fn save(
    url: &str,
    method: &str,
    request_body: Option<&Value>,
    response_data: &Value,
    status_code: u16,
    duration: Duration,
) -> Option<PathBuf> {
    let dir = init();
    if let Err(err) = fs::create_dir_all(&dir).await {
        debug!(%err, dir = %dir.display(), "failed to create raw response dir");
        return None;
    }

    let filename = generate_filename();
    let path = dir.join(filename);

    let content = build_content(
        url,
        method,
        request_body,
        response_data,
        status_code,
        duration,
    );

    match fs::write(&path, content.as_bytes()).await {
        Ok(()) => {
            debug!(path = %path.display(), "saved raw response");
            Some(path)
        }
        Err(err) => {
            debug!(%err, path = %path.display(), "failed to persist raw response");
            None
        }
    }
}

/// Save a large, already-rendered tool artifact without wrapping it in the
/// generic JSON API-response envelope.
pub async fn save_artifact(filename_prefix: &str, content: &str) -> Option<PathBuf> {
    let dir = init();
    if fs::create_dir_all(&dir).await.is_err() {
        return None;
    }
    let safe_prefix: String = filename_prefix
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = uuid::Uuid::new_v4().to_string();
    let filename = format!("{safe_prefix}-{id}.log");
    let path = dir.join(&filename);
    let partial_path = dir.join(format!(".{filename}.part"));
    let generation = match next_artifact_generation() {
        Ok(generation) => generation,
        Err(error) => {
            debug!(%error, "failed to allocate artifact generation");
            return None;
        }
    };
    let write_result = async {
        let mut file = fs::File::create(&partial_path).await?;
        file.write_all(content.as_bytes()).await?;
        file.flush().await?;
        drop(file);
        fs::rename(&partial_path, &path).await
    }
    .await;
    match write_result {
        Ok(()) => {
            let size = u64::try_from(content.len()).unwrap_or(u64::MAX);
            let metadata = ArtifactMetadata {
                id: id.clone(),
                path: path.clone(),
                filename,
                content_type: "text/plain; charset=utf-8".to_owned(),
                size,
                etag: format!("\"{id}-{size}\""),
            };
            artifacts()
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    id,
                    RegisteredArtifact {
                        metadata,
                        disk: None,
                        generation,
                        committed_at: monotonic_now(),
                        lifecycle: ArtifactLifecycle::Readable,
                        pins: 0,
                        retention_eligible: true,
                    },
                );
            debug!(path = %path.display(), bytes = content.len(), "saved tool artifact");
            Some(path)
        }
        Err(err) => {
            let _ = fs::remove_file(&partial_path).await;
            debug!(%err, path = %path.display(), "failed to save tool artifact");
            None
        }
    }
}

pub async fn read_artifact_chunk(
    id: &str,
    offset: u64,
    max_bytes: usize,
) -> std::io::Result<Option<(ArtifactMetadata, Vec<u8>, u64, bool)>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let Some(pin) = pin_artifact(id) else {
        return Ok(None);
    };
    let metadata = pin.metadata().clone();
    let offset = offset.min(metadata.size);
    let mut file = fs::File::open(&metadata.path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let remaining = metadata.size.saturating_sub(offset);
    let read_len = usize::try_from(remaining.min(max_bytes as u64)).unwrap_or(max_bytes);
    let mut data = vec![0; read_len];
    file.read_exact(&mut data).await?;
    let next_offset = offset + u64::try_from(data.len()).unwrap_or(0);
    Ok(Some((
        metadata,
        data,
        next_offset,
        next_offset >= remaining + offset,
    )))
}

fn generate_filename() -> String {
    let ts = iso_dashed();
    let mut bytes = [0u8; 4];
    rand::fill(&mut bytes);
    let mut hex = String::with_capacity(8);
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    format!("{ts}-{hex}.txt")
}

fn build_content(
    url: &str,
    method: &str,
    request_body: Option<&Value>,
    response_data: &Value,
    status_code: u16,
    duration: Duration,
) -> String {
    let sep = "=".repeat(80);
    let timestamp = iso_full();
    let duration_ms = duration.as_secs_f64() * 1000.0;

    let mut out = String::new();
    out.push_str(&sep);
    out.push('\n');
    out.push_str("RAW API RESPONSE LOG\n");
    out.push_str(&sep);
    out.push_str("\n\n");
    let _ = writeln!(out, "Timestamp: {timestamp}");
    let _ = writeln!(out, "URL: {url}");
    let _ = writeln!(out, "Method: {method}");
    let _ = writeln!(out, "Status Code: {status_code}");
    let _ = writeln!(out, "Duration: {duration_ms:.2}ms");
    out.push('\n');

    out.push_str(&sep);
    out.push_str("\nREQUEST BODY\n");
    out.push_str(&sep);
    out.push('\n');
    match request_body {
        Some(body) => {
            let body_text = match body {
                Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
            out.push_str(&body_text);
        }
        None => out.push_str("(no request body)"),
    }
    out.push_str("\n\n");

    out.push_str(&sep);
    out.push_str("\nRESPONSE DATA\n");
    out.push_str(&sep);
    out.push('\n');
    let data_text = match response_data {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    out.push_str(&data_text);
    out.push('\n');
    out.push_str(&sep);
    out.push('\n');

    out
}

/// ISO-8601 timestamp with colons and dots replaced by `-` (parity with TS
/// `new Date().toISOString().replace(/[:.]/g, '-')`).
fn iso_dashed() -> String {
    let raw = iso_full();
    raw.replace([':', '.'], "-")
}

/// ISO-8601 UTC timestamp, millisecond precision. `YYYY-MM-DDTHH:MM:SS.mmmZ`.
#[allow(clippy::many_single_char_names)]
fn iso_full() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.subsec_millis();
    let secs = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let h = sod / 3600;
    let m = (sod % 3600) / 60;
    let s = sod % 60;
    let (y, mo, d) = crate::logger::days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

#[cfg(test)]
mod artifact_writer_fault_tests {
    //! Tests marked `#[serial_test::serial(artifact_registry)]` share the
    //! process-wide artifact registry. `reconcile_missing_artifacts` sweeps it
    //! wholesale, evicting every entry whose file is gone, and several of these
    //! tests delete an artifact file and then assert on the registry. Run
    //! concurrently, one test's sweep drops another's entry before it can
    //! observe it, and `read_artifact_chunk` returns `Ok(None)` instead of the
    //! expected `Err`. Anything added here that deletes an artifact file, or
    //! reaches `reconcile_missing_artifacts` (`cleanup_current_session` does),
    //! needs the same marker. See issue #8.

    use super::*;

    async fn reserved_success(
        prefix: &str,
        quota: &std::sync::Arc<super::super::StreamingDiskQuota>,
    ) -> StreamedArtifact {
        let mut writer = begin_artifact(prefix, "ndjson", "application/x-ndjson", 16)
            .await
            .unwrap();
        writer.set_disk_quota(quota);
        writer.write_chunk(b"abc").await.unwrap();
        let artifact = writer.commit().await.unwrap();
        let manifest = artifact.artifact.path.with_extension("manifest.json");
        let sidecar = quota.lease().unwrap();
        sidecar.grow(1).await.unwrap();
        fs::write(&manifest, b"m").await.unwrap();
        let mut sidecar = Some(sidecar);
        attach_sidecar_reservation(&artifact.artifact.path, quota, &mut sidecar).unwrap();
        artifact
    }

    #[tokio::test]
    async fn injected_write_failure_leaves_no_partial_or_registry_entry() {
        let mut writer = begin_artifact("fault-write", "txt", "text/plain", 16)
            .await
            .unwrap();
        let path = writer.path.clone();
        let partial = writer.partial_path.clone();
        let id = writer.id.clone();
        writer.inject_fault(ArtifactWriterFault::Write);

        assert!(writer.write_chunk(b"data").await.is_err());
        drop(writer);

        assert!(!path.exists());
        assert!(!partial.exists());
        assert!(artifact(&id).is_none());
    }

    #[tokio::test]
    async fn injected_commit_failure_leaves_no_partial_or_registry_entry() {
        let mut writer = begin_artifact("fault-commit", "txt", "text/plain", 16)
            .await
            .unwrap();
        let path = writer.path.clone();
        let partial = writer.partial_path.clone();
        let id = writer.id.clone();
        writer.write_chunk(b"data").await.unwrap();
        writer.inject_fault(ArtifactWriterFault::Commit);

        assert!(writer.commit().await.is_err());

        assert!(!path.exists());
        assert!(!partial.exists());
        assert!(artifact(&id).is_none());
    }

    #[tokio::test]
    async fn concurrent_writers_never_exceed_quota_and_roll_back_failures() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(6));
        let mut first = begin_artifact("reserved-first", "txt", "text/plain", 16)
            .await
            .unwrap();
        first.set_disk_quota(&quota);
        let mut second = begin_artifact("reserved-second", "txt", "text/plain", 16)
            .await
            .unwrap();
        second.set_disk_quota(&quota);

        first.write_chunk(b"1234").await.unwrap();
        assert!(second.write_chunk(b"5678").await.is_err());
        assert_eq!(quota.reserved_bytes(), 4);
        assert!(quota.peak_reserved_bytes() <= 6);
        drop(second);
        drop(first);
        assert_eq!(quota.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn committed_reservation_is_released_once_after_cleanup() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let mut writer = begin_artifact("reserved-commit", "txt", "text/plain", 16)
            .await
            .unwrap();
        writer.set_disk_quota(&quota);
        writer.write_chunk(b"data").await.unwrap();
        let artifact = writer.commit().await.unwrap();
        assert_eq!(quota.reserved_bytes(), 4);

        remove_artifact(&artifact.artifact.path).await.unwrap();
        assert_eq!(quota.reserved_bytes(), 0);
        remove_artifact(&artifact.artifact.path).await.unwrap();
        assert_eq!(quota.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn commit_failure_releases_full_reservation() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let mut writer = begin_artifact("reserved-fault", "txt", "text/plain", 16)
            .await
            .unwrap();
        writer.set_disk_quota(&quota);
        writer.write_chunk(b"data").await.unwrap();
        writer.inject_fault(ArtifactWriterFault::Commit);
        assert!(writer.commit().await.is_err());
        assert_eq!(quota.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn acquired_write_reservation_rolls_back_only_after_partial_cleanup() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let mut writer = begin_artifact("reserved-write-fault", "txt", "text/plain", 16)
            .await
            .unwrap();
        let partial = writer.partial_path.clone();
        writer.set_disk_quota(&quota);
        writer.inject_fault(ArtifactWriterFault::Write);
        assert!(writer.write_chunk(b"data").await.is_err());
        assert_eq!(quota.reserved_bytes(), 4);
        drop(writer);
        assert!(!partial.exists());
        assert_eq!(quota.reserved_bytes(), 0);
    }

    // Serialised on the artifact registry; see the note at the top of the module.
    #[tokio::test]
    #[serial_test::serial(artifact_registry)]
    async fn missing_file_reconciliation_removes_sidecars_registry_and_reservations() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(32));
        let mut writer = begin_artifact("reserved-reconcile", "txt", "text/plain", 16)
            .await
            .unwrap();
        writer.set_disk_quota(&quota);
        writer.write_chunk(b"data").await.unwrap();
        let committed = writer.commit().await.unwrap();
        let sidecar = committed.artifact.path.with_extension("manifest.json");
        let sidecar_reservation = quota.lease().unwrap();
        sidecar_reservation.grow(3).await.unwrap();
        let mut sidecar_reservation = Some(sidecar_reservation);
        fs::write(&sidecar, b"{}\n").await.unwrap();
        attach_sidecar_reservation(&committed.artifact.path, &quota, &mut sidecar_reservation)
            .unwrap();
        assert_eq!(quota.reserved_bytes(), 7);

        fs::remove_file(&committed.artifact.path).await.unwrap();
        reconcile_missing_artifacts();
        assert!(!sidecar.exists());
        assert!(artifact(&committed.artifact.id).is_none());
        assert_eq!(quota.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn successful_splunk_expires_while_unexpired_loki_remains_reserved() {
        let expired_quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let live_quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let expired = reserved_success("splunk-retention-expired", &expired_quota).await;
        let live = reserved_success("loki-retention-live", &live_quota).await;
        set_committed_at(&expired.artifact.id, Duration::from_secs(10));
        set_committed_at(&live.artifact.id, Duration::from_secs(19));

        let ids = vec![expired.artifact.id.clone(), live.artifact.id.clone()];
        let report =
            sweep_artifacts_at(&ids, Duration::from_secs(20), Duration::from_secs(5)).await;
        assert!(report.removed >= 1);
        assert!(artifact(&expired.artifact.id).is_none());
        assert!(!expired.artifact.path.exists());
        assert_eq!(expired_quota.reserved_bytes(), 0);
        assert!(artifact(&live.artifact.id).is_some());
        assert_eq!(live_quota.reserved_bytes(), 4);

        remove_artifact(&live.artifact.path).await.unwrap();
        assert_eq!(live_quota.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn live_transaction_artifacts_are_not_retention_eligible_before_manifest_transition() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let mut writer = begin_artifact(
            "retention-live-transaction",
            "ndjson",
            "application/x-ndjson",
            16,
        )
        .await
        .unwrap();
        writer.set_disk_quota(&quota);
        writer.write_chunk(b"abc").await.unwrap();
        let provisional = writer.commit().await.unwrap();
        set_committed_at(&provisional.artifact.id, Duration::ZERO);
        let ids = vec![provisional.artifact.id.clone()];

        let report =
            sweep_artifacts_at(&ids, Duration::from_secs(100), Duration::from_secs(1)).await;
        assert_eq!(report, ReclamationReport::default());
        assert!(artifact(&provisional.artifact.id).is_some());
        assert_eq!(quota.reserved_bytes(), 3);

        remove_artifact(&provisional.artifact.path).await.unwrap();
        assert_eq!(quota.reserved_bytes(), 0);
    }

    // Serialised on the artifact registry; see the note at the top of the module.
    #[tokio::test]
    #[serial_test::serial(artifact_registry)]
    async fn failed_file_open_releases_its_exact_read_pin() {
        let path = save_artifact("retention-open-failure", "data")
            .await
            .unwrap();
        let registered = artifact_for_path(&path).unwrap();
        fs::remove_file(&path).await.unwrap();

        assert!(read_artifact_chunk(&registered.id, 0, 4).await.is_err());
        assert_eq!(
            artifacts()
                .read()
                .unwrap()
                .get(&registered.id)
                .unwrap()
                .pins,
            0
        );
        reconcile_missing_artifacts();
        assert!(!artifacts().read().unwrap().contains_key(&registered.id));
    }

    // Serialised on the artifact registry; see the note at the top of the module.
    #[tokio::test]
    #[serial_test::serial(artifact_registry)]
    async fn expired_pin_blocks_new_reads_and_defers_deletion_until_release() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let committed = reserved_success("circleci-retention-pinned", &quota).await;
        set_committed_at(&committed.artifact.id, Duration::ZERO);
        let pin = pin_artifact(&committed.artifact.id).unwrap();

        let ids = vec![committed.artifact.id.clone()];
        let report = sweep_artifacts_at(&ids, Duration::from_secs(2), Duration::from_secs(1)).await;
        assert_eq!(report.attempted, 0);
        assert!(artifact(&committed.artifact.id).is_none());
        assert!(pin_artifact(&committed.artifact.id).is_none());
        assert!(committed.artifact.path.exists());
        assert_eq!(quota.reserved_bytes(), 4);
        cleanup_current_session();
        assert!(
            committed.artifact.path.exists(),
            "session cleanup must retain a pending artifact with an active pin"
        );

        drop(pin);
        tokio::time::timeout(Duration::from_secs(2), async {
            while committed.artifact.path.exists() || quota.reserved_bytes() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(quota.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn deletion_failure_retains_registry_files_and_reservations_for_retry() {
        let quota = std::sync::Arc::new(super::super::StreamingDiskQuota::new(16));
        let committed = reserved_success("retention-retry", &quota).await;
        let manifest_part = committed.artifact.path.with_extension("manifest.json.part");
        let replacement = committed
            .artifact
            .path
            .with_extension("manifest.json.replaced-test");
        fs::write(&manifest_part, b"partial").await.unwrap();
        fs::write(&replacement, b"replacement").await.unwrap();
        set_committed_at(&committed.artifact.id, Duration::ZERO);
        inject_delete_failures(committed.artifact.path.clone(), 1);

        let ids = vec![committed.artifact.id.clone()];
        let failed = sweep_artifacts_at(&ids, Duration::from_secs(2), Duration::from_secs(1)).await;
        assert_eq!(failed.failed, 1);
        assert!(committed.artifact.path.exists());
        assert_eq!(quota.reserved_bytes(), 4);
        assert!(pin_artifact(&committed.artifact.id).is_none());
        assert!(
            artifacts()
                .read()
                .unwrap()
                .contains_key(&committed.artifact.id),
            "failed deletion retains the pending registry generation"
        );

        let retried =
            sweep_artifacts_at(&ids, Duration::from_secs(3), Duration::from_secs(1)).await;
        assert_eq!(retried.removed, 1);
        assert!(!committed.artifact.path.exists());
        assert!(!manifest_part.exists());
        assert!(!replacement.exists());
        assert_eq!(quota.reserved_bytes(), 0);
        assert!(
            !artifacts()
                .read()
                .unwrap()
                .contains_key(&committed.artifact.id)
        );
    }

    #[tokio::test]
    async fn expiration_is_oldest_first_with_artifact_id_as_tie_breaker() {
        let first_path = save_artifact("retention-order-a", "a").await.unwrap();
        let second_path = save_artifact("retention-order-b", "b").await.unwrap();
        let newest_path = save_artifact("retention-order-c", "c").await.unwrap();
        let first = artifact_for_path(&first_path).unwrap();
        let second = artifact_for_path(&second_path).unwrap();
        let newest = artifact_for_path(&newest_path).unwrap();
        set_committed_at(&first.id, Duration::ZERO);
        set_committed_at(&second.id, Duration::ZERO);
        set_committed_at(&newest.id, Duration::from_secs(1));
        inject_delete_failures(first_path.clone(), 1);
        inject_delete_failures(second_path.clone(), 1);
        inject_delete_failures(newest_path.clone(), 1);
        take_delete_attempts();

        let ids = vec![first.id.clone(), second.id.clone(), newest.id.clone()];
        let report = sweep_artifacts_at(&ids, Duration::from_secs(2), Duration::from_secs(1)).await;
        assert_eq!(report.failed, 3);
        let attempts = take_delete_attempts();
        let artifact_attempts: Vec<_> = attempts
            .into_iter()
            .filter(|path| path == &first_path || path == &second_path || path == &newest_path)
            .collect();
        let mut expected = if first.id < second.id {
            vec![first_path.clone(), second_path.clone()]
        } else {
            vec![second_path.clone(), first_path.clone()]
        };
        expected.push(newest_path.clone());
        assert_eq!(artifact_attempts, expected);

        sweep_artifacts_at(&ids, Duration::from_secs(3), Duration::from_secs(1)).await;
        assert!(!first_path.exists());
        assert!(!second_path.exists());
        assert!(!newest_path.exists());
    }

    #[tokio::test]
    async fn fifo_waiter_wakes_only_after_pinned_reclamation_physically_succeeds() {
        let coordinator = std::sync::Arc::new(super::super::StreamingDiskCoordinator::new(4));
        // Keep the production deadline comfortably beyond scheduler delays from
        // the rest of the parallel unit suite; the explicit timeout below still
        // bounds the wake-up assertion itself.
        let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
        let holder = super::super::StreamingDiskQuota::with_coordinator(
            coordinator.clone(),
            4,
            tokio_util::sync::CancellationToken::new(),
            deadline,
        );
        let waiter = super::super::StreamingDiskQuota::with_coordinator(
            coordinator,
            4,
            tokio_util::sync::CancellationToken::new(),
            deadline,
        );
        let committed = reserved_success("retention-wake", &holder).await;
        set_committed_at(&committed.artifact.id, Duration::ZERO);
        let pin = pin_artifact(&committed.artifact.id).unwrap();
        let waiting_lease = waiter.lease().unwrap();
        let waiting = tokio::spawn(async move {
            waiting_lease.grow(4).await.unwrap();
            waiting_lease
        });
        tokio::task::yield_now().await;

        let ids = vec![committed.artifact.id.clone()];
        sweep_artifacts_at(&ids, Duration::from_secs(2), Duration::from_secs(1)).await;
        assert!(!waiting.is_finished());
        assert_eq!(holder.reserved_bytes(), 4);
        drop(pin);
        let waiting_lease = tokio::time::timeout(Duration::from_secs(10), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(holder.reserved_bytes(), 0);
        assert_eq!(waiter.reserved_bytes(), 4);
        drop(waiting_lease);
        assert_eq!(waiter.reserved_bytes(), 0);
    }

    #[tokio::test]
    async fn each_retention_pass_has_a_strict_reclamation_bound() {
        let mut paths = Vec::new();
        let mut ids = Vec::new();
        for index in 0..=crate::constants::data_limits::MAX_STREAMING_ARTIFACT_RECLAIMS_PER_SWEEP {
            let path = save_artifact(&format!("retention-bound-{index}"), "x")
                .await
                .unwrap();
            let artifact = artifact_for_path(&path).unwrap();
            set_committed_at(&artifact.id, Duration::ZERO);
            paths.push(path);
            ids.push(artifact.id);
        }

        let first = sweep_artifacts_at(&ids, Duration::from_secs(2), Duration::from_secs(1)).await;
        assert_eq!(
            first.attempted,
            crate::constants::data_limits::MAX_STREAMING_ARTIFACT_RECLAIMS_PER_SWEEP
        );
        assert_eq!(first.removed, first.attempted);
        assert_eq!(paths.iter().filter(|path| path.exists()).count(), 1);
        let second = sweep_artifacts_at(&ids, Duration::from_secs(3), Duration::from_secs(1)).await;
        assert_eq!(second.removed, 1);
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[tokio::test]
    async fn pinned_pending_entries_do_not_starve_later_expired_artifacts() {
        let mut registered = Vec::new();
        for index in 0..=crate::constants::data_limits::MAX_STREAMING_ARTIFACT_RECLAIMS_PER_SWEEP {
            let path = save_artifact(&format!("retention-pinned-bound-{index}"), "x")
                .await
                .unwrap();
            let artifact = artifact_for_path(&path).unwrap();
            set_committed_at(&artifact.id, Duration::ZERO);
            registered.push((artifact.id, path));
        }
        registered.sort_by(|left, right| left.0.cmp(&right.0));
        let pins: Vec<_> = registered
            .iter()
            .take(crate::constants::data_limits::MAX_STREAMING_ARTIFACT_RECLAIMS_PER_SWEEP)
            .map(|(id, _)| pin_artifact(id).unwrap())
            .collect();
        let ids: Vec<_> = registered.iter().map(|(id, _)| id.clone()).collect();

        let first = sweep_artifacts_at(&ids, Duration::from_secs(2), Duration::from_secs(1)).await;
        assert_eq!(first.attempted, 0);
        let second = sweep_artifacts_at(&ids, Duration::from_secs(3), Duration::from_secs(1)).await;
        assert_eq!(second.removed, 1);
        assert!(!registered.last().unwrap().1.exists());

        drop(pins);
        tokio::time::timeout(Duration::from_secs(5), async {
            while registered.iter().any(|(_, path)| path.exists()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn abandoned_scavenging_keeps_live_process_sessions() {
        let base = tempfile::tempdir().unwrap();
        let live = base
            .path()
            .join(format!("session-{}-live", std::process::id()));
        let abandoned = base.path().join("session-4294967295-abandoned");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&abandoned).unwrap();
        std::fs::write(live.join("artifact"), b"live").unwrap();
        std::fs::write(abandoned.join("artifact"), b"stale").unwrap();
        let legacy = base.path().join("legacy-artifact");
        std::fs::write(&legacy, b"stale").unwrap();

        cleanup_abandoned_with(base.path(), |pid| pid == std::process::id());

        assert!(live.exists());
        assert!(!abandoned.exists());
        assert!(!legacy.exists());
    }
}
