//! Subprocess helper tests. Exercises the common Unix utilities
//! (`echo`, `false`, `sleep`) that ship with every supported target.

use std::time::Duration;

use mcp_server_atlassian::shell::{DEFAULT_TIMEOUT, execute, execute_with_timeout};
use pretty_assertions::assert_eq;

#[tokio::test]
async fn execute_returns_stdout_on_success() {
    let out = execute("echo", &["hello world"], "echo test")
        .await
        .expect("echo should succeed");
    assert_eq!(out.stdout.trim(), "hello world");
}

#[tokio::test]
async fn execute_surfaces_stderr_on_failure() {
    let err = execute("false", &[], "fail test").await.unwrap_err();
    assert!(err.message.to_lowercase().contains("fail test"));
}

#[tokio::test]
async fn missing_binary_yields_error() {
    let err = execute("definitely-not-a-command-xyz123", &[], "run missing bin")
        .await
        .unwrap_err();
    assert!(err.message.to_lowercase().contains("run missing bin"));
}

#[tokio::test]
async fn timeout_kills_long_running_command() {
    let err = execute_with_timeout(
        "sleep",
        &["10"],
        "sleep test",
        Duration::from_millis(50),
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("timed out"));
}

#[test]
fn default_timeout_is_five_minutes() {
    assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(5 * 60));
}
