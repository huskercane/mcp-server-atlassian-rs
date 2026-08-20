#![allow(clippy::doc_markdown)]

//! Where the one-time MFA code for a NinjaOne login comes from.
//!
//! The server deliberately knows nothing about any particular password
//! manager. An operator configures a **command that prints the current code to
//! stdout**, and whatever produces it — Bitwarden, 1Password, `pass`, a
//! YubiKey, a shell script — stays entirely on their side of the boundary:
//!
//! ```text
//! NINJAONE_TOTP_COMMAND = "bw get totp ninja-qa5"
//! NINJAONE_TOTP_COMMAND = "op item get ninja-qa5 --otp"
//! NINJAONE_TOTP_COMMAND = "oathtool --totp -b \"$(pass ninja/qa5)\""
//! NINJAONE_TOTP_COMMAND = "ykman oath accounts code -s ninja-qa5"
//! ```
//!
//! Two properties make this the cheap option rather than an abstraction for
//! its own sake: every vault CLI already emits a finished 6-digit code, so no
//! TOTP seed ever reaches this process or its config file, and this crate
//! takes no dependency on any vault SDK.
//!
//! When no vault CLI is reachable, `NINJAONE_TOTP_SECRET` takes the
//! `otpauth://totp/...` URI directly and the code is derived in-process (see
//! [`totp`](super::totp)). That trades a long-lived seed on disk (or in the OS
//! keychain) for having no external moving parts, so the command is tried
//! first when both are set.
//!
//! The full priority order — explicit `mfaCode`, then command, then seed —
//! lives in [`NinjaOneVendor::mfa_code`](super::NinjaOneVendor), which is
//! where the per-alias overrides and the keychain lookup for the seed are
//! resolved. This module owns only the running of the command.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

use crate::error::{McpError, auth_invalid, unexpected};

/// Vault CLIs are not fast — an unlocked `bw` round-trip is comfortably over a
/// second, and a hardware token may wait on a touch — but a login must not
/// hang a tool call indefinitely.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// A one-time code is short. This only rejects obvious junk (a usage message,
/// an HTML error page) before it is sent as a credential; the server remains
/// the authority on whether the code is valid.
const MAX_CODE_LEN: usize = 32;

/// Execute the configured command and return the code it printed.
///
/// The command runs through the platform shell so an operator can use the
/// pipes and substitutions their vault CLI needs (`oathtool -b "$(pass …)"`).
/// That is the same trust model the rest of this config already has — the file
/// holds passwords — but it does mean `NINJAONE_TOTP_COMMAND` is executable
/// configuration, so it must never be settable from a tool argument.
///
/// The child inherits this process's environment on purpose: session handles
/// like `BW_SESSION` are how vault CLIs stay unlocked.
pub async fn run_command(command: &str) -> Result<String, McpError> {
    let command = command.trim();
    let child = shell(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            unexpected(
                format!("Failed to run NINJAONE_TOTP_COMMAND: {error}"),
                None,
            )
        })?;

    let output = match timeout(COMMAND_TIMEOUT, child.wait_with_output()).await {
        Ok(result) => result.map_err(|error| {
            unexpected(
                format!("NINJAONE_TOTP_COMMAND failed while running: {error}"),
                None,
            )
        })?,
        Err(_) => {
            return Err(unexpected(
                format!(
                    "NINJAONE_TOTP_COMMAND did not produce a code within {}s. If it prompts for \
                     a vault unlock or a hardware touch, unlock first (or run it once by hand) \
                     and try again.",
                    COMMAND_TIMEOUT.as_secs()
                ),
                None,
            ));
        }
    };

    if !output.status.success() {
        // stderr is where a locked vault reports itself ("You are not logged
        // in."), so it is the actionable part. stdout is withheld: on success
        // it is the code, and a partial failure may still have printed it.
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        let detail = if detail.is_empty() {
            "no error output".to_owned()
        } else {
            truncate(detail)
        };
        return Err(auth_invalid(format!(
            "NINJAONE_TOTP_COMMAND exited with {}: {detail}",
            output
                .status
                .code()
                .map_or_else(|| "a signal".to_owned(), |code| format!("status {code}")),
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let code = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();

    if code.is_empty() {
        return Err(auth_invalid(
            "NINJAONE_TOTP_COMMAND printed nothing. It must write the current one-time code to \
             stdout (e.g. `bw get totp <item>`, `op item get <item> --otp`).",
        ));
    }
    if code.len() > MAX_CODE_LEN || code.contains(char::is_whitespace) {
        // Deliberately does not echo what was printed: whatever it is, it came
        // from a secret-bearing command.
        return Err(auth_invalid(format!(
            "NINJAONE_TOTP_COMMAND printed {} characters where a one-time code was expected. It \
             must print only the code.",
            code.len()
        )));
    }

    debug!(len = code.len(), "ninjaone: obtained MFA code from command");
    Ok(code.to_owned())
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

/// Keep a failure message readable when a command dumps a usage block.
fn truncate(text: &str) -> String {
    const LIMIT: usize = 300;
    if text.chars().count() <= LIMIT {
        return text.to_owned();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}…")
}
