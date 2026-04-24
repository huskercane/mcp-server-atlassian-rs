//! Contextualised logger matching the TypeScript reference.
//!
//! Behavior it preserves:
//! - One log file per process at `$HOME/.mcp/data/<unscoped-pkg>.<sessionId>.log`.
//! - Log header with session ID, pid, cwd, argv.
//! - Line format `[HH:MM:SS] [LEVEL] [<source>] <message>`, where `<source>` is
//!   `module/path.ts@function` (without the `src/` prefix, kept for wire
//!   compatibility with prompts and fixtures).
//! - Log lines also go to stderr (stdio MCP transport uses stdout, so stderr
//!   is safe for human-visible logs).
//! - `DEBUG` env var controls debug-level module filtering with wildcard
//!   patterns: `DEBUG=true`, `DEBUG=controllers/*,services/*`, etc.
//!
//! The implementation is a thin wrapper that fans out to the `tracing` crate
//! for the stderr leg and to plain `std::fs::OpenOptions` for the file leg.
//! This keeps the format identical to TS without fighting
//! `tracing-subscriber`'s formatter.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tracing::{Level, debug, error, info, warn};

use crate::constants::UNSCOPED_PACKAGE_NAME;

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
static SESSION_ID: OnceLock<String> = OnceLock::new();
static DEBUG_RULES: OnceLock<Vec<regex::Regex>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Initialise the logger. Idempotent — subsequent calls are no-ops.
/// Returns the path of the current session's log file.
pub fn init() -> PathBuf {
    if let Some(path) = LOG_PATH.get() {
        return path.clone();
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let _ = SESSION_ID.set(session_id.clone());

    let debug_rules = parse_debug_env(std::env::var("DEBUG").ok().as_deref());
    let _ = DEBUG_RULES.set(debug_rules);

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

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
    {
        let _ = file.write_all(header.as_bytes());
        let _ = LOG_FILE.set(Mutex::new(file));
    }

    let _ = LOG_PATH.set(log_path.clone());
    log_path
}

/// Session UUID for the current process (after [`init`]).
pub fn session_id() -> Option<&'static str> {
    SESSION_ID.get().map(String::as_str)
}

/// Log file path (after [`init`]).
pub fn log_path() -> Option<&'static PathBuf> {
    LOG_PATH.get()
}

/// Per-file / per-method logger handle. Call [`Logger::for_context`] at the
/// top of a module and `for_method` inside specific functions.
#[derive(Debug, Clone)]
pub struct Logger {
    source: String,
    module_path: String,
}

impl Logger {
    pub fn for_context(file: impl Into<String>) -> Self {
        let file = file.into();
        let module_path = strip_src_prefix(&file).to_owned();
        let source = format!("[{module_path}]");
        Self {
            source,
            module_path,
        }
    }

    pub fn for_context_fn(file: impl Into<String>, function: impl AsRef<str>) -> Self {
        let file = file.into();
        let module_path = strip_src_prefix(&file).to_owned();
        let source = format!("[{}@{}]", module_path, function.as_ref());
        Self {
            source,
            module_path,
        }
    }

    #[must_use]
    pub fn for_method(&self, method: impl AsRef<str>) -> Self {
        Self::for_context_fn(&self.module_path, method)
    }

    pub fn info(&self, message: impl AsRef<str>) {
        self.emit(Level::INFO, message.as_ref());
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.emit(Level::WARN, message.as_ref());
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.emit(Level::ERROR, message.as_ref());
    }

    pub fn debug(&self, message: impl AsRef<str>) {
        if !debug_enabled_for(&self.module_path) {
            return;
        }
        self.emit(Level::DEBUG, message.as_ref());
    }

    fn emit(&self, level: Level, message: &str) {
        let line = format!(
            "{ts} [{lvl}] {src} {msg}",
            ts = time_only(),
            lvl = level_str(level),
            src = self.source,
            msg = message,
        );

        // File leg
        if let Some(file) = LOG_FILE.get()
            && let Ok(mut guard) = file.lock()
        {
            let _ = writeln!(guard, "{line}");
        }

        // tracing leg (stderr via tracing-subscriber when installed)
        match level {
            Level::INFO => info!(target: "bitbucket", "{line}"),
            Level::WARN => warn!(target: "bitbucket", "{line}"),
            Level::ERROR => error!(target: "bitbucket", "{line}"),
            Level::DEBUG | Level::TRACE => debug!(target: "bitbucket", "{line}"),
        }
    }
}

fn strip_src_prefix(path: &str) -> &str {
    path.strip_prefix("src/").unwrap_or(path)
}

fn level_str(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// Parse the `DEBUG` env var into module-path regexes. Behavior matches TS
/// `isDebugEnabledForModule`:
/// - unset / empty -> no rules (debug suppressed)
/// - `true` or `1` -> a single `.*` rule (everything enabled)
/// - comma-separated list of globs -> per-glob regex (`*` -> `.*`, `?` -> `.`)
pub fn parse_debug_env(value: Option<&str>) -> Vec<regex::Regex> {
    let Some(raw) = value else { return vec![] };
    if raw.is_empty() {
        return vec![];
    }
    if raw == "true" || raw == "1" {
        return vec![regex::Regex::new("^.*$").expect("static regex")];
    }
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|pattern| {
            let mut out = String::from("^");
            for ch in pattern.chars() {
                match ch {
                    '*' => out.push_str(".*"),
                    '?' => out.push('.'),
                    c if regex_syntax::is_meta_character(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push('$');
            regex::Regex::new(&out).ok()
        })
        .collect()
}

fn debug_enabled_for(module_path: &str) -> bool {
    let Some(rules) = DEBUG_RULES.get() else {
        return false;
    };
    if rules.is_empty() {
        return false;
    }
    let without_src = strip_src_prefix(module_path);
    rules
        .iter()
        .any(|re| re.is_match(module_path) || re.is_match(without_src))
}

fn time_only() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs_total = now.as_secs();
    let h = (secs_total / 3600) % 24;
    let m = (secs_total / 60) % 60;
    let s = secs_total % 60;
    format!("[{h:02}:{m:02}:{s:02}]")
}

#[allow(clippy::many_single_char_names)]
fn iso_timestamp() -> String {
    // Minimal ISO-8601 UTC timestamp without external deps. Format:
    // YYYY-MM-DDTHH:MM:SSZ. Accurate to whole seconds (matches TS precision
    // at the header level).
    let now = std::time::SystemTime::now()
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

// Howard Hinnant's civil_from_days algorithm (public domain). All intermediate
// values are well below 2^31 for any realistic date; casts are safe.
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

// We only need to know whether a character needs escaping in a regex pattern
// when converting the glob. `regex_syntax` is already pulled in transitively
// via `regex`; we re-export the single helper we need to avoid adding a
// direct dep.
mod regex_syntax {
    pub fn is_meta_character(c: char) -> bool {
        matches!(
            c,
            '\\' | '.'
                | '+'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        )
    }
}
