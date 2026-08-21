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
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncWriteExt, BufWriter};
use tracing::debug;

use crate::constants::UNSCOPED_PACKAGE_NAME;

static SESSION_DIR: OnceLock<PathBuf> = OnceLock::new();
static ARTIFACTS: OnceLock<RwLock<HashMap<String, RegisteredArtifact>>> = OnceLock::new();

#[derive(Debug)]
struct RegisteredArtifact {
    metadata: ArtifactMetadata,
    disk: Option<DiskReservation>,
}

#[derive(Debug)]
struct DiskReservation {
    quota: std::sync::Arc<super::StreamingDiskQuota>,
    artifact_bytes: u64,
    sidecar_bytes: u64,
}

impl Drop for DiskReservation {
    fn drop(&mut self) {
        let total = self
            .artifact_bytes
            .checked_add(self.sidecar_bytes)
            .expect("registered disk reservation total is checked at acquisition");
        let released = self.quota.release(total);
        debug_assert!(released.is_ok());
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
    disk_quota: Option<std::sync::Arc<super::StreamingDiskQuota>>,
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
    pub fn set_disk_quota(&mut self, quota: std::sync::Arc<super::StreamingDiskQuota>) {
        debug_assert_eq!(self.size, 0);
        self.disk_quota = Some(quota);
    }

    #[cfg(test)]
    fn inject_fault(&mut self, fault: ArtifactWriterFault) {
        self.fault = Some(fault);
    }

    pub async fn write_chunk(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fault == Some(ArtifactWriterFault::Write) {
            return Err(std::io::Error::other("injected artifact write failure"));
        }
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
        if let Some(quota) = &self.disk_quota {
            quota.reserve(u64::try_from(bytes.len()).map_err(std::io::Error::other)?)?;
        }
        if let Err(error) = self
            .writer
            .as_mut()
            .expect("artifact writer is open")
            .write_all(bytes)
            .await
        {
            if let Some(quota) = &self.disk_quota {
                let _ = quota.release(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            }
            return Err(error);
        }
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
                    disk: self.disk_quota.take().map(|quota| DiskReservation {
                        quota,
                        artifact_bytes: self.size,
                        sidecar_bytes: 0,
                    }),
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
            let _ = std::fs::remove_file(&self.partial_path);
            if let Some(quota) = &self.disk_quota {
                let _ = quota.release(self.size);
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
        disk_quota: None,
        #[cfg(test)]
        fault: None,
    })
}

fn artifacts() -> &'static RwLock<HashMap<String, RegisteredArtifact>> {
    ARTIFACTS.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn artifact(id: &str) -> Option<ArtifactMetadata> {
    artifacts()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .map(|registered| registered.metadata.clone())
}

pub fn artifact_for_path(path: &Path) -> Option<ArtifactMetadata> {
    artifacts()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .find(|artifact| artifact.metadata.path == path)
        .map(|registered| registered.metadata.clone())
}

/// Remove a committed artifact and forget its download metadata.
///
/// A missing file is treated as an already-completed removal so stale
/// registry entries cannot outlive controller cleanup or an external unlink.
pub async fn remove_artifact(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for sidecar in [
        path.with_extension("manifest.json"),
        path.with_extension("manifest.json.part"),
    ] {
        match fs::remove_file(sidecar).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|_, artifact| artifact.metadata.path != path);
    Ok(())
}

/// Transfer a successfully committed sidecar reservation to its artifact.
pub(crate) fn attach_sidecar_reservation(
    artifact_path: &Path,
    quota: &std::sync::Arc<super::StreamingDiskQuota>,
    bytes: u64,
) -> std::io::Result<()> {
    let mut entries = artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let registered = entries
        .values_mut()
        .find(|entry| entry.metadata.path == artifact_path)
        .ok_or_else(|| std::io::Error::other("artifact is not registered"))?;
    let disk = registered
        .disk
        .as_mut()
        .ok_or_else(|| std::io::Error::other("artifact has no disk reservation"))?;
    if !std::sync::Arc::ptr_eq(&disk.quota, quota) {
        return Err(std::io::Error::other("sidecar uses a different disk quota"));
    }
    let old = disk.sidecar_bytes;
    if old != 0 {
        quota.release(old)?;
    }
    disk.sidecar_bytes = bytes;
    Ok(())
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
    if let Some(dir) = SESSION_DIR.get() {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => debug!(dir = %dir.display(), "removed temporary response artifacts"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                debug!(%err, dir = %dir.display(), "failed to remove temporary response artifacts");
            }
        }
    }
    artifacts()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn cleanup_abandoned(base: &Path) {
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
        if !process_is_running(pid) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
}

#[cfg(not(windows))]
fn process_is_running(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|output| !output.stdout.is_empty())
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

    let Some(metadata) = artifact(id) else {
        return Ok(None);
    };
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
    use super::*;

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
        first.set_disk_quota(quota.clone());
        let mut second = begin_artifact("reserved-second", "txt", "text/plain", 16)
            .await
            .unwrap();
        second.set_disk_quota(quota.clone());

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
        writer.set_disk_quota(quota.clone());
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
        writer.set_disk_quota(quota.clone());
        writer.write_chunk(b"data").await.unwrap();
        writer.inject_fault(ArtifactWriterFault::Commit);
        assert!(writer.commit().await.is_err());
        assert_eq!(quota.reserved_bytes(), 0);
    }
}
