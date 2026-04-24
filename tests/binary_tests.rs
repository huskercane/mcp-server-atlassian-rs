//! End-to-end tests that drive the `mcp-atlassian` binary through
//! its real `main` function. These tests deliberately stay narrow — per-
//! transport behavior is covered by the library-level tests in
//! `tests/http_transport_tests.rs` and the various tool/controller tests.
//! The value here is validating the argv + env-var wiring in `src/main.rs`.
//!
//! Scope:
//! - `TRANSPORT_MODE` unset, argv empty → stdio transport is started.
//! - `TRANSPORT_MODE=http`, argv empty → HTTP transport is started.
//! - argv non-empty → CLI dispatch.
//! - SIGTERM on the HTTP transport results in a clean exit (Unix only).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Command as StdCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use tokio::process::Command as TokioCommand;

const BIN: &str = "mcp-atlassian";

/// Probe `127.0.0.1:0` to get a port the OS considers free, then drop the
/// listener so the binary-under-test can bind it. There is a small race with
/// any other process that grabs the port in between, but in practice this is
/// the standard approach for ad-hoc port selection in tests.
fn random_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 0");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Poll-connect the given port until something accepts or the deadline expires.
fn wait_for_listen(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

#[test]
fn cli_help_lists_every_subcommand() {
    // argv present → `main` routes to `cli::run`, which prints clap's help.
    let output = StdCommand::new(cargo_bin(BIN))
        .arg("--help")
        .env_remove("TRANSPORT_MODE")
        .output()
        .expect("spawn binary");
    assert!(output.status.success(), "status: {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in ["get", "post", "put", "patch", "delete", "clone"] {
        assert!(
            stdout.contains(sub),
            "help missing subcommand {sub}:\n{stdout}"
        );
    }
}

#[test]
fn stdio_transport_answers_initialize() {
    // argv empty + TRANSPORT_MODE unset → stdio transport. Send a line of
    // newline-delimited JSON-RPC, read one line back, confirm it's an
    // initialize result.
    let mut child = StdCommand::new(cargo_bin(BIN))
        .env_remove("TRANSPORT_MODE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn binary");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    let request = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":"#,
        r#"{"protocolVersion":"2025-06-18","capabilities":{},"#,
        r#""clientInfo":{"name":"rust-binary-test","version":"0"}}}"#,
        "\n",
    );
    stdin.write_all(request.as_bytes()).expect("write init");
    stdin.flush().expect("flush");

    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read response line");

    assert!(
        line.contains("\"jsonrpc\":\"2.0\""),
        "stdout missing jsonrpc envelope: {line}"
    );
    assert!(
        line.contains("\"result\""),
        "stdout missing initialize result: {line}"
    );

    // Closing stdin signals EOF → rmcp's service exits, process terminates.
    drop(stdin);
    drop(reader);
    let status = child.wait().expect("wait for exit");
    assert!(status.success(), "unexpected exit: {status:?}");
}

#[tokio::test]
async fn http_transport_binds_and_serves_health() {
    let port = random_port();
    let mut child = TokioCommand::new(cargo_bin(BIN))
        .env("TRANSPORT_MODE", "http")
        .env("PORT", port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn binary");

    assert!(
        wait_for_listen(port, Duration::from_secs(5)),
        "binary did not bind to 127.0.0.1:{port}",
    );

    let body = reqwest::get(format!("http://127.0.0.1:{port}/"))
        .await
        .expect("GET /")
        .text()
        .await
        .expect("body");
    assert!(
        body.contains("Atlassian MCP Server"),
        "unexpected banner: {body}"
    );

    // Force-kill; a cleaner exit is exercised by `sigterm_triggers_graceful_exit`.
    child.start_kill().ok();
    let _ = child.wait().await;
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_triggers_graceful_exit() {
    let port = random_port();
    let mut child = TokioCommand::new(cargo_bin(BIN))
        .env("TRANSPORT_MODE", "http")
        .env("PORT", port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn binary");

    assert!(
        wait_for_listen(port, Duration::from_secs(5)),
        "binary did not bind to 127.0.0.1:{port}",
    );

    let pid = child.id().expect("child pid");
    // Using /bin/kill avoids pulling `nix` in as a dev-dep for one test.
    let kill_status = StdCommand::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("kill -TERM");
    assert!(kill_status.success(), "kill -TERM failed: {kill_status:?}");

    let exit_status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("process did not exit within 5s of SIGTERM")
        .expect("wait");
    assert!(
        exit_status.success(),
        "binary did not exit cleanly after SIGTERM: {exit_status:?}"
    );
}
