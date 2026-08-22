//! End-to-end tests that drive the `mcp-devtools` binary through
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
//! - `LOG_STDERR` gates console logging without touching the log file.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command as StdCommand, Stdio};
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use tokio::process::Command as TokioCommand;

const BIN: &str = "mcp-devtools";

/// The line `run_http` writes once its listener is bound, carrying the address
/// the OS actually gave it.
const STARTUP_MARKER: &str = "listening on streamable-HTTP transport";

/// Spawn the HTTP transport on an OS-assigned port, with stderr piped.
///
/// `PORT=0` on purpose: the kernel picks a port at bind time, so there is no
/// interval in which the port is reserved for this child but not yet held by
/// it, and therefore no way for a sibling test to be handed the same number.
/// The child reports what it got on its startup line.
///
/// `RUST_LOG` is cleared so an ambient value in the developer's or CI's
/// environment cannot decide the outcome.
fn spawn_http(log_stderr: Option<&str>) -> tokio::process::Child {
    let mut command = TokioCommand::new(cargo_bin(BIN));
    command
        .env("TRANSPORT_MODE", "http")
        .env("PORT", "0")
        .env_remove("RUST_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match log_stderr {
        Some(value) => {
            command.env("LOG_STDERR", value);
        }
        None => {
            command.env_remove("LOG_STDERR");
        }
    }
    command.spawn().expect("spawn binary")
}

/// Read the child's stderr until `marker` appears, returning everything read.
///
/// This is the readiness signal a port probe cannot be: it comes from the
/// child itself, so no other process can satisfy it.
async fn read_stderr_until(
    stderr: &mut tokio::process::ChildStderr,
    marker: &str,
    timeout: Duration,
) -> String {
    use tokio::io::AsyncReadExt as _;

    let deadline = tokio::time::Instant::now() + timeout;
    let mut collected = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if String::from_utf8_lossy(&collected).contains(marker) {
            return String::from_utf8_lossy(&collected).into_owned();
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for {marker:?} on stderr; got:\n{}",
            String::from_utf8_lossy(&collected)
        );
        match tokio::time::timeout(remaining, stderr.read(&mut buf)).await {
            Ok(Ok(0)) => panic!(
                "binary exited before writing {marker:?}; stderr:\n{}",
                String::from_utf8_lossy(&collected)
            ),
            Ok(Ok(read)) => collected.extend_from_slice(&buf[..read]),
            Ok(Err(error)) => panic!("reading stderr failed: {error}"),
            // Deadline hit mid-read; the assertion above reports it next lap.
            Err(_) => {}
        }
    }
}

/// Pull the bound port out of the startup line's `bound=127.0.0.1:<port>`.
fn bound_port(startup_output: &str) -> u16 {
    let (_, rest) = startup_output
        .split_once("127.0.0.1:")
        .expect("startup line carries the bound address");
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("bound port is numeric")
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
    // Top-level groups including the new `creds` group.
    for grp in ["bb", "jira", "conf", "creds"] {
        assert!(
            stdout.contains(grp),
            "help missing top-level group {grp}:\n{stdout}"
        );
    }
}

#[test]
fn creds_help_lists_every_subcommand() {
    let output = StdCommand::new(cargo_bin(BIN))
        .args(["creds", "--help"])
        .env_remove("TRANSPORT_MODE")
        .output()
        .expect("spawn binary");
    assert!(output.status.success(), "status: {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in ["set", "get", "rm", "migrate"] {
        assert!(
            stdout.contains(sub),
            "creds --help missing subcommand {sub}:\n{stdout}"
        );
    }
}

#[test]
fn creds_set_rejects_unknown_kind() {
    let output = StdCommand::new(cargo_bin(BIN))
        .args([
            "creds",
            "set",
            "--kind",
            "nonsense",
            "--vendor",
            "bitbucket",
            "--principal",
            "x@y",
        ])
        .env_remove("TRANSPORT_MODE")
        .output()
        .expect("spawn binary");
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown kind") || stderr.contains("invalid value"),
        "expected kind-validation error, got:\n{stderr}"
    );
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

#[test]
fn stdio_transport_answers_modern_discover_without_initialize() {
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
        r#"{"jsonrpc":"2.0","id":2,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"rust-binary-test","version":"0"}}}}"#,
        "\n",
    );
    stdin.write_all(request.as_bytes()).expect("write discover");
    stdin.flush().expect("flush");

    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read response line");
    let response: serde_json::Value = serde_json::from_str(&line).expect("JSON-RPC response");
    assert_eq!(response["result"]["resultType"], "complete");
    assert!(
        response["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.iter().any(|v| v == "2026-07-28")),
        "server did not advertise MCP 2026-07-28: {response}"
    );

    drop(stdin);
    drop(reader);
    let status = child.wait().expect("wait for exit");
    assert!(status.success(), "unexpected exit: {status:?}");
}

#[tokio::test]
async fn http_transport_binds_and_serves_health() {
    let mut child = spawn_http(None);
    let mut stderr = child.stderr.take().expect("stderr piped");
    let startup = read_stderr_until(&mut stderr, STARTUP_MARKER, Duration::from_secs(10)).await;
    let port = bound_port(&startup);

    let body = reqwest::get(format!("http://127.0.0.1:{port}/"))
        .await
        .expect("GET /")
        .text()
        .await
        .expect("body");
    assert!(
        body.contains("mcp-server-devtools"),
        "unexpected banner: {body}"
    );

    // Force-kill; a cleaner exit is exercised by `sigterm_triggers_graceful_exit`.
    child.start_kill().ok();
    let _ = child.wait().await;
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_triggers_graceful_exit() {
    let mut child = spawn_http(None);
    let mut stderr = child.stderr.take().expect("stderr piped");
    // Readiness has to come from this child, not from the port. `run_http`
    // installs the signal handler before it binds, so a child that has written
    // its startup line is ready to shut down gracefully — whereas an open port
    // could belong to a sibling test's server, leaving this child to take the
    // signal while still starting up and die on SIGTERM's default disposition.
    read_stderr_until(&mut stderr, STARTUP_MARKER, Duration::from_secs(10)).await;

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

/// Spawn the HTTP transport with `LOG_STDERR` set as given (or removed for
/// `None`), wait for its startup line, then kill it and return everything it
/// wrote to stderr.
///
/// Only usable when logging is expected to be *on*: the startup line is both
/// the readiness signal and the thing under test. A run that never writes it
/// fails inside [`read_stderr_until`], naming the marker it waited for.
async fn stderr_from_logging_http_run(log_stderr: Option<&str>) -> String {
    let mut child = spawn_http(log_stderr);
    let mut stderr = child.stderr.take().expect("stderr piped");
    let collected = read_stderr_until(&mut stderr, STARTUP_MARKER, Duration::from_secs(10)).await;
    child.start_kill().ok();
    let _ = child.wait().await;
    collected
}

#[tokio::test]
async fn stderr_logging_is_enabled_by_default() {
    let stderr = stderr_from_logging_http_run(None).await;
    assert!(
        stderr.contains(STARTUP_MARKER),
        "expected startup log on stderr by default, got:\n{stderr}"
    );
}

#[tokio::test]
async fn log_stderr_off_silences_console() {
    // Switching logging off removes the very line the other runs use as a
    // readiness signal, and `PORT=0` means there is no known port to probe
    // either. What is left is a settle window: let the child run far longer
    // than a logging-enabled start takes, confirm it is still up (with
    // `PORT=0` it cannot have died of a port clash), then assert it stayed
    // silent. `stderr_logging_is_enabled_by_default` is the control — it waits
    // for that same line and shows it lands promptly under identical
    // conditions — so a quiet second here means silenced, not merely early.
    //
    // The asymmetry is deliberate: an overloaded machine can only make this
    // window *less* conclusive, never make the assertion fail spuriously.
    let mut child = spawn_http(Some("off"));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        child.try_wait().expect("poll child").is_none(),
        "binary exited during the settle window instead of serving"
    );

    child.start_kill().ok();
    let output = child.wait_with_output().await.expect("collect output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.trim().is_empty(),
        "LOG_STDERR=off should leave stderr empty, got:\n{stderr}"
    );
}

#[tokio::test]
async fn log_stderr_unrecognised_value_keeps_console_logging() {
    // Only the documented off-switches disable output; anything else must be
    // treated as "leave logging on" rather than silently swallowing logs.
    let stderr = stderr_from_logging_http_run(Some("yes")).await;
    assert!(
        stderr.contains(STARTUP_MARKER),
        "unrecognised LOG_STDERR value should keep stderr logging, got:\n{stderr}"
    );
}

/// The version users see must be the crate version.
///
/// Regression guard: this constant was once a hardcoded `"3.1.0"` inherited
/// from the TS reference server, so a `v0.11.0` release shipped a binary that
/// introduced itself as `3.1.0` to every MCP client. Deriving it from
/// `CARGO_PKG_VERSION` makes that drift impossible; this test fails if anyone
/// writes a literal back.
#[test]
fn version_reported_to_users_is_the_crate_version() {
    assert_eq!(
        mcp_server_devtools::constants::VERSION,
        env!("CARGO_PKG_VERSION")
    );
}
