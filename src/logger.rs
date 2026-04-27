//! Process-wide logging setup.
//!
//! Behaviour:
//! - One log file per process at `$HOME/.mcp/data/<unscoped-pkg>.<sessionId>.log`.
//! - Header with session ID, pid, cwd, argv is written before any tracing
//!   subscriber attaches.
//! - `tracing-subscriber` is then initialised with two fmt layers: stderr
//!   (ANSI, no target) and the same log file (plain text, target included).
//!   Both writers run output through a redactor that masks Atlassian API
//!   tokens, JWTs, `Authorization` headers, and URL-embedded basic auth.
//! - Retention: at startup, files in the log dir matching the package
//!   prefix are pruned by age (`LOG_RETENTION_DAYS`) and then by total size
//!   (`LOG_RETENTION_MAX_BYTES`, oldest-first). The sweep runs on a detached
//!   thread; the current session's file is never deleted.
//! - Filter precedence:
//!     1. `RUST_LOG` if set — passed straight to `EnvFilter`.
//!     2. `DEBUG=true` or `DEBUG=1` — bumps the whole subscriber to `debug`.
//!     3. Default `info,mcp_server_atlassian=debug`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::constants::UNSCOPED_PACKAGE_NAME;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

const LOG_RETENTION_DAYS: u64 = 7;
const LOG_RETENTION_MAX_BYTES: u64 = 50 * 1024 * 1024;
const REDACTION_PLACEHOLDER: &str = "XXX-REDACTED-XXX";

/// Convenience: build a `Duration` from a whole-day count without
/// triggering clippy's `duration_suboptimal_units` lint at every
/// call site (`from_hours`/`from_days` aren't quite the right shape
/// for our retention math here).
#[allow(clippy::duration_suboptimal_units)]
const fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

/// Initialise process-wide logging. Idempotent — subsequent calls are no-ops
/// and return the same path.
pub fn init() -> PathBuf {
    if let Some(path) = LOG_PATH.get() {
        return path.clone();
    }

    let session_id = uuid::Uuid::new_v4().to_string();

    let log_dir = dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".mcp")
        .join("data");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{UNSCOPED_PACKAGE_NAME}.{session_id}.log"));

    let header = format!(
        "# {pkg} Log Session\nSession ID: {sid}\nStarted: {ts}\nProcess ID: {pid}\nWorking Directory: {cwd}\nCommand: {cmd}\n\n## Log Entries\n\n",
        pkg = UNSCOPED_PACKAGE_NAME,
        sid = session_id,
        ts = iso_timestamp(),
        pid = std::process::id(),
        cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        cmd = std::env::args().collect::<Vec<_>>().join(" "),
    );

    let file_writer = match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
    {
        Ok(mut f) => {
            let _ = f.write_all(header.as_bytes());
            let _ = f.flush();
            OpenOptions::new().append(true).open(&log_path).ok()
        }
        Err(_) => None,
    };

    let redactor = Arc::new(Redactor::new());

    let filter = build_filter(
        std::env::var("RUST_LOG").ok().as_deref(),
        std::env::var("DEBUG").ok().as_deref(),
    );

    let stderr_layer = fmt::layer()
        .with_writer(RedactingMakeWriter {
            inner: std::io::stderr,
            redactor: redactor.clone(),
        })
        .with_target(false);

    let file_layer = file_writer.map(|file| {
        fmt::layer()
            .with_writer(RedactingMakeWriter {
                inner: Mutex::new(file),
                redactor: redactor.clone(),
            })
            .with_ansi(false)
            .with_target(true)
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    let _ = LOG_PATH.set(log_path.clone());

    let dir_for_sweep = log_dir.clone();
    let keep = log_path.clone();
    std::thread::Builder::new()
        .name("log-retention".into())
        .spawn(move || {
            let _ = sweep_retention(
                &dir_for_sweep,
                &keep,
                days(LOG_RETENTION_DAYS),
                LOG_RETENTION_MAX_BYTES,
                SystemTime::now(),
            );
        })
        .ok();

    log_path
}

fn build_filter(rust_log: Option<&str>, debug: Option<&str>) -> EnvFilter {
    if let Some(spec) = rust_log
        && !spec.is_empty()
        && let Ok(filter) = EnvFilter::try_new(spec)
    {
        return filter;
    }
    if matches!(debug, Some("true" | "1")) {
        return EnvFilter::new("debug");
    }
    EnvFilter::new("info,mcp_server_atlassian=debug")
}

// --- Redaction --------------------------------------------------------------

struct Redactor {
    patterns: Vec<regex::Regex>,
}

impl Redactor {
    fn new() -> Self {
        // Patterns ordered most-specific first so credential-shaped strings
        // are redacted before more generic catch-alls.
        let raw = [
            // Atlassian Cloud API token format: ATATT3xFfGF0... = <hex tail>
            r"ATATT3xFfGF0[A-Za-z0-9_\-]+=[A-Fa-f0-9]+",
            // Authorization: Basic <b64> / Bearer <token>
            r"(?i)authorization:\s*(?:basic|bearer)\s+[A-Za-z0-9+/=._\-]+",
            // JWTs (three base64url segments)
            r"eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+",
            // URL-embedded basic auth: https://user:pass@host
            r"(?P<scheme>https?://)[^:/\s@]+:[^@\s]+@",
        ];
        let patterns = raw
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();
        Self { patterns }
    }

    fn redact<'a>(&self, input: &'a str) -> std::borrow::Cow<'a, str> {
        let mut out = std::borrow::Cow::Borrowed(input);
        for re in &self.patterns {
            // URL-basic-auth keeps the scheme so the URL is still recognisable.
            let replacement = if re.as_str().starts_with("(?P<scheme>") {
                format!("${{scheme}}{REDACTION_PLACEHOLDER}@")
            } else {
                REDACTION_PLACEHOLDER.to_string()
            };
            let new = re.replace_all(&out, replacement.as_str());
            if let std::borrow::Cow::Owned(s) = new {
                out = std::borrow::Cow::Owned(s);
            }
        }
        out
    }
}

struct RedactingMakeWriter<M> {
    inner: M,
    redactor: Arc<Redactor>,
}

impl<'a, M> MakeWriter<'a> for RedactingMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
            redactor: self.redactor.clone(),
        }
    }
}

struct RedactingWriter<W: Write> {
    inner: W,
    redactor: Arc<Redactor>,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // tracing-subscriber's fmt layer writes a complete formatted line per
        // call, so per-write redaction is safe — there's no risk of a token
        // straddling two write boundaries in practice.
        if let Ok(s) = std::str::from_utf8(buf) {
            let redacted = self.redactor.redact(s);
            self.inner.write_all(redacted.as_bytes())?;
            // Contract: report bytes consumed from `buf`, not bytes written.
            Ok(buf.len())
        } else {
            self.inner.write(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// --- Retention --------------------------------------------------------------

fn sweep_retention(
    dir: &Path,
    keep: &Path,
    max_age: Duration,
    max_total_bytes: u64,
    now: SystemTime,
) -> std::io::Result<()> {
    let prefix = format!("{UNSCOPED_PACKAGE_NAME}.");
    let mut candidates: Vec<(PathBuf, SystemTime, u64)> = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path == keep {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix)
            || path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("log"))
        {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        candidates.push((path, modified, meta.len()));
    }

    // Age pass.
    candidates.retain(|(path, modified, _size)| {
        match now.duration_since(*modified) {
            Ok(age) if age > max_age => {
                let _ = std::fs::remove_file(path);
                false
            }
            _ => true,
        }
    });

    // Size pass: oldest first. The kept file counts toward the cap so that
    // `max_total_bytes` reflects total dir size, not just deletable bytes.
    candidates.sort_by_key(|(_, modified, _)| *modified);
    let kept_size = std::fs::metadata(keep).map_or(0, |m| m.len());
    let mut total: u64 = kept_size + candidates.iter().map(|(_, _, size)| *size).sum::<u64>();
    for (path, _, size) in &candidates {
        if total <= max_total_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(*size);
        }
    }

    Ok(())
}

// --- Time helpers -----------------------------------------------------------

#[allow(clippy::many_single_char_names)]
fn iso_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let h = sod / 3600;
    let m = (sod % 3600) / 60;
    let s = sod % 60;
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
pub(crate) fn days_to_ymd(mut days: i64) -> (i64, u32, u32) {
    days += 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const DEFAULT_DISPLAY: &str = "mcp_server_atlassian=debug,info";

    // ---- build_filter ----

    #[test]
    fn rust_log_takes_precedence_over_debug() {
        let f = build_filter(Some("warn"), Some("true"));
        assert_eq!(format!("{f}"), "warn");
    }

    #[test]
    fn debug_true_enables_debug_everywhere() {
        let f = build_filter(None, Some("true"));
        assert_eq!(format!("{f}"), "debug");
    }

    #[test]
    fn debug_one_enables_debug_everywhere() {
        let f = build_filter(None, Some("1"));
        assert_eq!(format!("{f}"), "debug");
    }

    #[test]
    fn unset_uses_default() {
        let f = build_filter(None, None);
        assert_eq!(format!("{f}"), DEFAULT_DISPLAY);
    }

    #[test]
    fn empty_rust_log_falls_through_to_debug_when_debug_true() {
        let f = build_filter(Some(""), Some("true"));
        assert_eq!(format!("{f}"), "debug");
    }

    #[test]
    fn debug_other_values_do_not_enable_debug() {
        let f = build_filter(None, Some("yes"));
        assert_eq!(format!("{f}"), DEFAULT_DISPLAY);
    }

    #[test]
    fn rust_log_per_target_directive_is_honoured() {
        let f = build_filter(Some("mcp_server_atlassian::controllers=trace"), None);
        assert_eq!(format!("{f}"), "mcp_server_atlassian::controllers=trace");
    }

    // ---- days_to_ymd ----

    #[test]
    fn days_to_ymd_unix_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        assert_eq!(days_to_ymd(19782), (2024, 2, 29));
    }

    // ---- redactor ----

    #[test]
    fn redacts_atlassian_api_token() {
        let r = Redactor::new();
        let s = "token=ATATT3xFfGF0RH9I_DgF2g4zYZ_wbQkXe-Wk0N2c0vg0XKHOESubWbHmhPE6Fifsy=08F068BE";
        let out = r.redact(s);
        assert!(!out.contains("ATATT3xFfGF0"), "token leaked: {out}");
        assert!(out.contains(REDACTION_PLACEHOLDER), "placeholder missing: {out}");
    }

    #[test]
    fn redacts_authorization_basic_header() {
        let r = Redactor::new();
        let s = "Authorization: Basic dXNlcjpwYXNz";
        let out = r.redact(s);
        assert_eq!(out, REDACTION_PLACEHOLDER);
    }

    #[test]
    fn redacts_authorization_bearer_header() {
        let r = Redactor::new();
        let s = "authorization: Bearer abc123.def456";
        let out = r.redact(s);
        assert_eq!(out, REDACTION_PLACEHOLDER);
    }

    #[test]
    fn redacts_jwt() {
        let r = Redactor::new();
        let s = "tok eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.abc-_def trailing";
        let out = r.redact(s);
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"), "jwt leaked: {out}");
        assert!(out.contains("trailing"));
    }

    #[test]
    fn redacts_url_embedded_basic_auth_keeping_scheme() {
        let r = Redactor::new();
        let s = "fetch https://alice:s3cret@example.com/path";
        let out = r.redact(s);
        assert!(!out.contains("alice"), "user leaked: {out}");
        assert!(!out.contains("s3cret"), "password leaked: {out}");
        assert!(out.contains("https://"), "scheme stripped: {out}");
        assert!(out.contains("@example.com/path"), "host stripped: {out}");
    }

    #[test]
    fn leaves_clean_text_unchanged() {
        let r = Redactor::new();
        let s = "GET /repos/foo/bar status=200 ms=45";
        let out = r.redact(s);
        assert_eq!(out, s);
    }

    // ---- redacting_writer ----

    #[test]
    fn redacting_writer_masks_inline_token() {
        let mut buf: Vec<u8> = Vec::new();
        let r = Arc::new(Redactor::new());
        {
            let mut w = RedactingWriter {
                inner: &mut buf,
                redactor: r,
            };
            let _ = w.write_all(
                b"line with ATATT3xFfGF0abc-DEF=AAAA1111 trailing\n",
            );
        }
        let out = String::from_utf8(buf).unwrap();
        assert!(!out.contains("ATATT3xFfGF0"), "leaked: {out}");
        assert!(out.contains("trailing"));
        assert!(out.ends_with('\n'));
    }

    // ---- sweep_retention ----

    fn touch(path: &Path, mtime: SystemTime, size: usize) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(&vec![b'.'; size]).unwrap();
        f.flush().unwrap();
        f.set_modified(mtime).unwrap();
    }

    #[test]
    fn retention_deletes_files_older_than_max_age() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let old = now - days(8);
        let young = now - days(2);

        let kept = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.session.log"));
        let stale = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.old.log"));
        let recent = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.recent.log"));

        touch(&kept, now, 10);
        touch(&stale, old, 10);
        touch(&recent, young, 10);

        sweep_retention(
            dir.path(),
            &kept,
            days(7),
            10 * 1024 * 1024,
            now,
        )
        .unwrap();

        assert!(kept.exists(), "current session file deleted");
        assert!(!stale.exists(), "stale file not deleted");
        assert!(recent.exists(), "recent file deleted");
    }

    #[test]
    fn retention_enforces_size_cap_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let one_day = days(1);

        let kept = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.kept.log"));
        let oldest = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.a.log"));
        let middle = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.b.log"));
        let newest = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.c.log"));

        touch(&kept, now, 100);
        touch(&oldest, now - one_day * 3, 100);
        touch(&middle, now - one_day * 2, 100);
        touch(&newest, now - one_day, 100);

        // 4 files * 100B = 400B; cap at 250B → must drop the two oldest.
        sweep_retention(
            dir.path(),
            &kept,
            days(7),
            250,
            now,
        )
        .unwrap();

        assert!(kept.exists(), "current session file deleted");
        assert!(!oldest.exists(), "oldest not pruned for size");
        assert!(!middle.exists(), "middle not pruned for size");
        assert!(newest.exists(), "newest pruned unexpectedly");
    }

    #[test]
    fn retention_skips_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let old = now - days(30);

        let kept = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.session.log"));
        let unrelated = dir.path().join("some-other-tool.log");
        let no_ext = dir.path().join(format!("{UNSCOPED_PACKAGE_NAME}.session.bak"));

        touch(&kept, now, 10);
        touch(&unrelated, old, 10);
        touch(&no_ext, old, 10);

        sweep_retention(
            dir.path(),
            &kept,
            days(7),
            10 * 1024 * 1024,
            now,
        )
        .unwrap();

        assert!(unrelated.exists(), "unrelated file deleted");
        assert!(no_ext.exists(), "non-.log file deleted");
    }
}
