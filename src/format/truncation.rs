//! Character-budget enforcement for MCP tool output. Mirrors the TS
//! `truncateForAI` function.

use std::fmt::Write as _;
use std::path::Path;

use super::markdown::{format_heading, format_separator};

/// ~10k tokens at 4 chars/token. Controls when we start truncating.
pub const MAX_RESPONSE_CHARS: usize = 40_000;

/// Truncate over-budget content and append a guidance block with a pointer
/// to the on-disk raw response. Returns the original content unchanged when
/// it fits within the budget.
pub fn truncate_for_ai(content: &str, raw_response_path: Option<&Path>) -> String {
    if content.len() <= MAX_RESPONSE_CHARS {
        return content.to_owned();
    }

    let mut cutoff = MAX_RESPONSE_CHARS;
    let search_start = MAX_RESPONSE_CHARS.saturating_sub(500);

    if let Some(last_newline) = content[..MAX_RESPONSE_CHARS].rfind('\n')
        && last_newline > search_start
    {
        cutoff = last_newline;
    }

    // Snap cutoff to a UTF-8 boundary (for content with multi-byte runs)
    while cutoff > 0 && !content.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let truncated_size = cutoff;
    let original_size = content.len();
    let percent = percent_ratio(truncated_size, original_size);
    let tokens_k = rough_k_tokens(truncated_size);
    let orig_k = rough_k_chars(original_size);

    let mut out = String::with_capacity(truncated_size + 512);
    out.push_str(&content[..cutoff]);
    out.push('\n');
    out.push_str(format_separator());
    out.push('\n');
    out.push_str(&format_heading("Response Truncated", 2));
    out.push('\n');
    out.push('\n');
    let _ = writeln!(
        out,
        "This response was truncated to ~{tokens_k}k tokens ({percent}% of original {orig_k}k chars).",
    );
    out.push('\n');
    out.push_str("**To access the complete data:**");
    out.push('\n');

    if let Some(path) = raw_response_path {
        let _ = writeln!(
            out,
            "- The full raw API response is saved at: `{}`",
            path.display()
        );
    }

    out.push_str("- Consider refining your request with more specific filters or selecting fewer fields\n");
    out.push_str("- For paginated data, use smaller page sizes or specific identifiers\n");
    out.push_str("- When searching, use more targeted queries to reduce result sets");

    out
}

/// The three ratio helpers below all operate on byte counts bounded by
/// `MAX_RESPONSE_CHARS * a_modest_factor` (i.e. well under 2^53), so `as f64`
/// and `as u64` conversions are lossless in practice.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn percent_ratio(numerator: usize, denominator: usize) -> u64 {
    if denominator == 0 {
        return 0;
    }
    ((numerator as f64 / denominator as f64) * 100.0).round() as u64
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn rough_k_tokens(bytes: usize) -> u64 {
    (bytes as f64 / 4000.0).round() as u64
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn rough_k_chars(bytes: usize) -> u64 {
    (bytes as f64 / 1000.0).round() as u64
}
