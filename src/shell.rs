//! Shell subprocess helper. Mirrors `src/utils/shell.util.ts`.
//!
//! Important: uses `tokio::process::Command` directly (not `std::process`)
//! and does **not** invoke a shell. Arguments are passed to the kernel
//! verbatim, which closes the command-injection surface (CWE-78) the TS
//! reference also guarded against via `execFile`.

use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::error::{McpError, unexpected};

/// How long a single command is allowed to run. Matches TS's 5-minute cap.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Outcome of a successful subprocess invocation.
#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
}

impl ShellOutput {
    /// The text we surface to callers. Matches TS `executeShellCommand`:
    /// returns stdout when non-empty, otherwise a success banner.
    pub fn display(&self, operation: &str) -> String {
        if self.stdout.trim().is_empty() {
            format!("Successfully {operation}.")
        } else {
            self.stdout.clone()
        }
    }
}

/// Run a program with the supplied arguments and return stdout on success.
///
/// - `file`: path or command name to execute (looked up on `PATH`).
/// - `args`: program arguments passed verbatim; no shell expansion.
/// - `operation`: short human-readable description used in error messages.
pub async fn execute(
    file: &str,
    args: &[&str],
    operation: &str,
) -> Result<ShellOutput, McpError> {
    execute_with_timeout(file, args, operation, DEFAULT_TIMEOUT).await
}

/// Variant with an explicit timeout; mostly useful for tests.
pub async fn execute_with_timeout(
    file: &str,
    args: &[&str],
    operation: &str,
    deadline: Duration,
) -> Result<ShellOutput, McpError> {
    let mut cmd = Command::new(file);
    cmd.args(args);
    cmd.kill_on_drop(true);

    let fut = cmd.output();
    let io_result: std::io::Result<std::process::Output> = match timeout(deadline, fut).await {
        Ok(r) => r,
        Err(_) => {
            return Err(unexpected(
                format!(
                    "Failed to {operation}: command `{file}` timed out after {}s",
                    deadline.as_secs()
                ),
                None,
            ));
        }
    };

    let output = io_result.map_err(|err| {
        unexpected(
            format!("Failed to {operation}: {err}"),
            None,
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if output.status.success() {
        return Ok(ShellOutput { stdout, stderr });
    }

    // Preserve TS behavior: the error message prefers stderr > stdout > status.
    let detail = if !stderr.trim().is_empty() {
        stderr.trim().to_owned()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_owned()
    } else {
        output
            .status
            .code()
            .map_or_else(|| "terminated by signal".to_owned(), |c| format!("exit code {c}"))
    };

    Err(unexpected(
        format!("Failed to {operation}: {detail}"),
        None,
    ))
}
