use std::time::Duration;

use mcp_server_atlassian::transport::raw_response::{
    artifact, artifact_for_path, begin_artifact, read_artifact_chunk, remove_artifact, save,
    save_artifact,
};
use pretty_assertions::assert_eq;
use serde_json::json;

/// The test runs side-effects on the real filesystem (matches TS behaviour);
/// we clean up our own files to stay polite.
async fn cleanup(path: &std::path::Path) {
    let _ = remove_artifact(path).await;
}

#[tokio::test]
async fn removing_committed_artifact_unregisters_id_and_path() {
    let path = save_artifact("registry-cleanup", "registered")
        .await
        .unwrap();
    let metadata = artifact_for_path(&path).unwrap();

    remove_artifact(&path).await.unwrap();

    assert!(!path.exists());
    assert!(artifact(&metadata.id).is_none());
    assert!(artifact_for_path(&path).is_none());
}

#[tokio::test]
async fn removing_missing_artifact_unregisters_stale_metadata() {
    let path = save_artifact("registry-missing", "registered")
        .await
        .unwrap();
    let metadata = artifact_for_path(&path).unwrap();
    tokio::fs::remove_file(&path).await.unwrap();

    remove_artifact(&path).await.unwrap();

    assert!(artifact(&metadata.id).is_none());
    assert!(artifact_for_path(&path).is_none());
}

#[tokio::test]
async fn artifact_chunks_resume_by_byte_offset() {
    let path = save_artifact("chunk-test", "abcdefghij").await.unwrap();
    let artifact = artifact_for_path(&path).unwrap();
    let (_, first, next, eof) = read_artifact_chunk(&artifact.id, 0, 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, b"abcd");
    assert_eq!(next, 4);
    assert!(!eof);
    let (_, second, next, eof) = read_artifact_chunk(&artifact.id, next, 20)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second, b"efghij");
    assert_eq!(next, 10);
    assert!(eof);
    cleanup(&path).await;
}

#[tokio::test]
async fn streamed_artifact_enforces_limit_and_hashes_incrementally() {
    let mut writer = begin_artifact("bounded-test", "ndjson", "application/x-ndjson", 6)
        .await
        .unwrap();
    writer.write_chunk(b"abc").await.unwrap();
    writer.write_chunk(b"def").await.unwrap();
    let err = writer.write_chunk(b"g").await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge);
    let artifact = writer.commit().await.unwrap();
    assert_eq!(artifact.artifact.size, 6);
    assert_eq!(
        artifact.sha256,
        "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721"
    );
    assert_eq!(
        tokio::fs::read(&artifact.artifact.path).await.unwrap(),
        b"abcdef"
    );
    cleanup(&artifact.artifact.path).await;
}

#[tokio::test]
async fn writes_file_under_tmp_mcp() {
    let response = json!({"values":[{"id":1},{"id":2}]});
    let path = save(
        "https://api.bitbucket.org/2.0/repositories/foo",
        "GET",
        None,
        &response,
        200,
        Duration::from_millis(123),
    )
    .await
    .expect("raw response should be written");

    assert!(
        path.starts_with(
            std::env::temp_dir()
                .join("mcp")
                .join("mcp-server-atlassian")
        )
    );
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
        stem.chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-' || c == 'T' || c == 'Z'),
        "unexpected filename chars in {stem}"
    );

    let content = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(content.contains("RAW API RESPONSE LOG"));
    assert!(content.contains("URL: https://api.bitbucket.org/2.0/repositories/foo"));
    assert!(content.contains("Method: GET"));
    assert!(content.contains("Status Code: 200"));
    assert!(content.contains("\"id\": 1"));
    // Seven separators: three pairs framing each labelled section, plus one
    // closing separator at the end (matches TS `response.util.ts`).
    assert_eq!(content.matches("=".repeat(80).as_str()).count(), 7);

    cleanup(&path).await;
}

#[tokio::test]
async fn request_body_section_contains_body_or_noop() {
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
    .await
    .unwrap();
    let content = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(content.contains("\"foo\": \"bar\""));
    cleanup(&path).await;

    let path = save(
        "https://api.bitbucket.org/2.0/foo",
        "GET",
        None,
        &resp,
        200,
        Duration::from_millis(25),
    )
    .await
    .unwrap();
    let content = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(content.contains("(no request body)"));
    cleanup(&path).await;
}
