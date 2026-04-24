//! Minimal markdown helpers used across the response pipeline. Mirrors the
//! subset of `src/utils/formatter.util.ts` that phase 2 actually exercises.
//! The remaining helpers (formatPagination, formatBulletList, formatDiff,
//! etc.) come with the controller/tool ports in a later phase.

/// The standard horizontal rule used in tool output. Matches TS
/// `formatSeparator()`.
pub fn format_separator() -> &'static str {
    "---"
}

/// Format a markdown heading. The level is clamped to the legal 1..=6 range.
/// Matches TS `formatHeading(text, level)`.
pub fn format_heading(text: &str, level: u8) -> String {
    let clamped = level.clamp(1, 6);
    let hashes = "#".repeat(usize::from(clamped));
    format!("{hashes} {text}")
}
