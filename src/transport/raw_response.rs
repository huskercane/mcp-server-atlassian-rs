//! Persist successful JSON API responses to `/tmp/mcp/<unscoped-pkg>/` for
//! offline inspection. Mirrors TS `response.util.ts`.
//!
//! Filename is `<iso-ts-dashed>-<8hex>.txt` (colons/dots in the timestamp are
//! replaced with `-` to match TS). Body uses 80-char `=` separators around
//! metadata / request body / response data sections.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde_json::Value;
use tracing::debug;

use crate::constants::UNSCOPED_PACKAGE_NAME;

/// Write the raw API response to disk and return the path written. Returns
/// `None` on any failure — parity with TS behaviour, which logs but does not
/// propagate errors from this subsystem.
pub fn save(
    url: &str,
    method: &str,
    request_body: Option<&Value>,
    response_data: &Value,
    status_code: u16,
    duration: Duration,
) -> Option<PathBuf> {
    let dir = PathBuf::from("/tmp")
        .join("mcp")
        .join(UNSCOPED_PACKAGE_NAME);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        debug!(%err, dir = %dir.display(), "failed to create raw response dir");
        return None;
    }

    let filename = generate_filename();
    let path = dir.join(filename);

    let content = build_content(url, method, request_body, response_data, status_code, duration);

    match std::fs::File::create(&path).and_then(|mut f| f.write_all(content.as_bytes())) {
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

fn generate_filename() -> String {
    let ts = iso_dashed();
    let mut bytes = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut bytes);
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
