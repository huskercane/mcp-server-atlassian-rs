use std::time::Duration;

use mcp_server_atlassian_bitbucket::transport::raw_response::save;
use pretty_assertions::assert_eq;
use serde_json::json;

/// The test runs side-effects on the real filesystem (matches TS behaviour);
/// we clean up our own files to stay polite.
fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

#[test]
fn writes_file_under_tmp_mcp() {
    let response = json!({"values":[{"id":1},{"id":2}]});
    let path = save(
        "https://api.bitbucket.org/2.0/repositories/foo",
        "GET",
        None,
        &response,
        200,
        Duration::from_millis(123),
    )
    .expect("raw response should be written");

    assert!(path.starts_with("/tmp/mcp/mcp-server-atlassian-bitbucket-rs/"));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    assert_eq!(
        std::path::Path::new(file_name)
            .extension()
            .and_then(|e| e.to_str()),
        Some("txt")
    );
    // Filename shape: <iso-ts-dashed>-<8hex>.txt. iso-ts-dashed replaces ':' and '.' in
    // `YYYY-MM-DDTHH:MM:SS.mmmZ`, giving a digits/dashes/T/Z alphabet.
    let stem = file_name.trim_end_matches(".txt");
    assert!(
        stem.chars().all(|c| c.is_ascii_hexdigit() || c == '-' || c == 'T' || c == 'Z'),
        "unexpected filename chars in {stem}"
    );

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("RAW API RESPONSE LOG"));
    assert!(content.contains("URL: https://api.bitbucket.org/2.0/repositories/foo"));
    assert!(content.contains("Method: GET"));
    assert!(content.contains("Status Code: 200"));
    assert!(content.contains("\"id\": 1"));
    // Seven separators: three pairs framing each labelled section, plus one
    // closing separator at the end (matches TS `response.util.ts`).
    assert_eq!(content.matches("=".repeat(80).as_str()).count(), 7);

    cleanup(&path);
}

#[test]
fn request_body_section_contains_body_or_noop() {
    let req_body = json!({"foo": "bar"});
    let resp = json!({"ok": true});
    let path = save(
        "https://api.bitbucket.org/2.0/foo",
        "POST",
        Some(&req_body),
        &resp,
        201,
        Duration::from_millis(50),
    )
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("\"foo\": \"bar\""));
    cleanup(&path);

    let path = save(
        "https://api.bitbucket.org/2.0/foo",
        "GET",
        None,
        &resp,
        200,
        Duration::from_millis(25),
    )
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("(no request body)"));
    cleanup(&path);
}
