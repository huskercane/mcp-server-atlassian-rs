use std::path::Path;

use mcp_server_atlassian::format::truncation::{MAX_RESPONSE_CHARS, truncate_for_ai};
use pretty_assertions::assert_eq;

#[test]
fn short_content_passes_through_unchanged() {
    let content = "small\nresponse";
    let out = truncate_for_ai(content, None);
    assert_eq!(out, content);
}

#[test]
fn exact_boundary_content_passes_through() {
    let content = "a".repeat(MAX_RESPONSE_CHARS);
    let out = truncate_for_ai(&content, None);
    assert_eq!(out.len(), MAX_RESPONSE_CHARS);
}

#[test]
fn oversized_content_is_truncated_and_annotated() {
    let mut content = String::new();
    for _ in 0..500 {
        content.push_str(&"x".repeat(90));
        content.push('\n');
    }
    content.push_str(&"y".repeat(10_000));
    assert!(content.len() > MAX_RESPONSE_CHARS);

    let out = truncate_for_ai(&content, None);
    assert!(out.contains("## Response Truncated"));
    assert!(out.contains("---"));
    assert!(out.contains("This response was truncated"));
    assert!(out.contains("To access the complete data"));
    assert!(!out.contains("The full raw API response is saved at"));
}

#[test]
fn includes_raw_path_when_provided() {
    let content = "a".repeat(MAX_RESPONSE_CHARS + 500);
    let path = Path::new("/tmp/mcp/mcp-server-atlassian/abc.txt");
    let out = truncate_for_ai(&content, Some(path));
    assert!(out.contains("The full raw API response is saved at:"));
    assert!(out.contains(path.to_string_lossy().as_ref()));
}

#[test]
fn truncates_at_newline_when_one_is_nearby() {
    let mut content = String::new();
    // first block: 39_600 'a's
    content.push_str(&"a".repeat(39_600));
    content.push('\n');
    // filler past the threshold
    content.push_str(&"b".repeat(1_000));
    assert!(content.len() > MAX_RESPONSE_CHARS);

    let out = truncate_for_ai(&content, None);
    let body_end = out
        .find("\n---")
        .expect("guidance block present after truncation");
    let body = &out[..body_end];
    // The final line of the kept body must be a complete run of 'a's (i.e. the
    // cut happened at the newline, not mid-word).
    assert!(body.ends_with(&"a".repeat(100)), "cut should align to newline");
}
