use std::io::Cursor;

use mcp_server_devtools::transport::{StreamingPolicy, fetch_streamed_url};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn explicitly_decodes_and_accounts_for_zstd_without_content_length_assumptions() {
    let server = MockServer::start().await;
    let decoded = b"streamed log payload\n".repeat(100);
    let encoded = zstd::stream::encode_all(Cursor::new(&decoded), 1).unwrap();
    Mock::given(method("GET"))
        .and(path("/logs"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-encoding", "zstd")
                .set_body_bytes(encoded.clone()),
        )
        .mount(&server)
        .await;

    let artifact = fetch_streamed_url(
        &format!("{}/logs", server.uri()),
        "compressed-accounting-test",
        "log",
        "text/plain",
        StreamingPolicy::new(encoded.len() as u64, decoded.len() as u64),
    )
    .await
    .unwrap();

    assert_eq!(artifact.encoded_bytes, encoded.len() as u64);
    assert_eq!(artifact.decoded_bytes, decoded.len() as u64);
    assert_eq!(
        tokio::fs::read(&artifact.artifact.path).await.unwrap(),
        decoded
    );
    let _ = tokio::fs::remove_file(artifact.artifact.path).await;
}

#[tokio::test]
async fn decoded_quota_is_enforced_during_decompression() {
    let server = MockServer::start().await;
    let decoded = vec![b'x'; 32 * 1024];
    let encoded = zstd::stream::encode_all(Cursor::new(&decoded), 1).unwrap();
    Mock::given(method("GET"))
        .and(path("/bomb"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-encoding", "zstd")
                .set_body_bytes(encoded.clone()),
        )
        .mount(&server)
        .await;

    let error = fetch_streamed_url(
        &format!("{}/bomb", server.uri()),
        "decoded-limit-test",
        "log",
        "text/plain",
        StreamingPolicy::new(encoded.len() as u64, 1024),
    )
    .await
    .unwrap_err();

    assert_eq!(error.status_code, Some(413));
}

#[tokio::test]
async fn encoded_quota_is_enforced_and_partial_artifact_is_cleaned() {
    let server = MockServer::start().await;
    let encoded = vec![b'x'; 4096];
    Mock::given(method("GET"))
        .and(path("/encoded-limit"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(encoded))
        .mount(&server)
        .await;
    let directory = mcp_server_devtools::transport::raw_response::init();
    let before = std::fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("encoded-limit-test"))
        })
        .collect::<std::collections::HashSet<_>>();

    let error = fetch_streamed_url(
        &format!("{}/encoded-limit", server.uri()),
        "encoded-limit-test",
        "log",
        "text/plain",
        StreamingPolicy::new(1024, 8192),
    )
    .await
    .unwrap_err();

    assert_eq!(error.status_code, Some(413));
    let after = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("encoded-limit-test"))
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(after, before, "quota failure must not leave a part file");
}
