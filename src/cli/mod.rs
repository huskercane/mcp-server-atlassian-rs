//! Command-line interface for the Atlassian MCP binary.
//!
//! Two modes (matching the TS reference):
//! - **No arguments** → start the stdio MCP server (see `main.rs`).
//! - **Any arguments** → CLI dispatch through this module.
//!
//! The CLI exposes two subcommand groups, one per Atlassian product, so a
//! command's vendor is unambiguous:
//!
//! ```text
//! mcp-atlassian bb   <get|post|put|patch|delete|clone> ...
//! mcp-atlassian jira <get|post|put|patch|delete>       ...
//! mcp-atlassian conf <get|post|put|patch|delete>       ...
//! ```
//!
//! ## Deprecated top-level verbs
//!
//! The original CLI exposed Bitbucket verbs at the top level (`get`,
//! `post`, …). Those are preserved as hidden aliases that emit a one-line
//! stderr deprecation notice and route into the `bb` group. They will be
//! removed in the next major release; migrate to the explicit `bb`
//! prefix.

pub mod api;
pub mod bb;
pub mod conf;
pub mod creds;
pub mod jira;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::constants::{CLI_NAME, VERSION};

#[derive(Debug, Parser)]
#[command(
    name = CLI_NAME,
    version = VERSION,
    about = "A Model Context Protocol (MCP) server for Atlassian Bitbucket, Jira, and Confluence",
    disable_help_subcommand = true,
    propagate_version = true,
)]
pub struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

/// Top-level subcommand surface.
#[derive(Debug, Subcommand)]
pub enum TopCommand {
    /// Bitbucket Cloud REST API (`bb get|post|put|patch|delete|clone`).
    Bb {
        #[command(subcommand)]
        action: bb::Command,
    },
    /// Jira Cloud REST API (`jira get|post|put|patch|delete`).
    Jira {
        #[command(subcommand)]
        action: jira::Command,
    },
    /// Confluence Cloud REST API (`conf get|post|put|patch|delete`).
    Conf {
        #[command(subcommand)]
        action: conf::Command,
    },
    /// Manage credentials in the OS keychain (`creds set|get|rm|migrate`).
    Creds {
        #[command(subcommand)]
        action: creds::Command,
    },

    // ----------------------------------------------------------------------
    // Deprecated top-level Bitbucket verbs.
    //
    // Hidden from `--help` (so new users see only `bb`/`jira`) but still
    // parseable so existing scripts keep working for one release cycle.
    // Each variant emits a one-line stderr notice in `dispatch_legacy` and
    // routes into `bb::dispatch`.
    // ----------------------------------------------------------------------
    #[command(hide = true)]
    Get(api::ReadOpts),
    #[command(hide = true)]
    Post(api::WriteOpts),
    #[command(hide = true)]
    Put(api::WriteOpts),
    #[command(hide = true)]
    Patch(api::WriteOpts),
    #[command(hide = true)]
    Delete(api::ReadOpts),
    #[command(hide = true)]
    Clone(bb::CloneOpts),
}

/// Entry point used by `main.rs`. Parses arguments from the supplied
/// iterator and dispatches to the matching subcommand.
pub async fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = String>,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(err) => {
            // clap already formatted the message; use its built-in exit code
            err.exit();
        }
    };

    let result = match cli.command {
        TopCommand::Bb { action } => bb::dispatch(action).await,
        TopCommand::Jira { action } => jira::dispatch(action).await,
        TopCommand::Conf { action } => conf::dispatch(action).await,
        TopCommand::Creds { action } => creds::dispatch(action).await,
        legacy => dispatch_legacy(legacy).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}", crate::error::format_cli_error(&err));
            ExitCode::FAILURE
        }
    }
}

/// Translate a deprecated top-level verb into the equivalent `bb`
/// subcommand. Emits a single stderr line so the user sees the migration
/// hint without polluting the actual command output on stdout.
async fn dispatch_legacy(legacy: TopCommand) -> Result<(), crate::error::McpError> {
    let action = match legacy {
        TopCommand::Get(opts) => {
            warn_deprecated("get");
            bb::Command::Get(opts)
        }
        TopCommand::Post(opts) => {
            warn_deprecated("post");
            bb::Command::Post(opts)
        }
        TopCommand::Put(opts) => {
            warn_deprecated("put");
            bb::Command::Put(opts)
        }
        TopCommand::Patch(opts) => {
            warn_deprecated("patch");
            bb::Command::Patch(opts)
        }
        TopCommand::Delete(opts) => {
            warn_deprecated("delete");
            bb::Command::Delete(opts)
        }
        TopCommand::Clone(opts) => {
            warn_deprecated("clone");
            bb::Command::Clone(opts)
        }
        // Bb / Jira / Conf / Creds are handled by the caller; reaching here is a logic bug.
        TopCommand::Bb { .. }
        | TopCommand::Jira { .. }
        | TopCommand::Conf { .. }
        | TopCommand::Creds { .. } => unreachable!(
            "vendor groups are dispatched directly; legacy path receives only flat verbs"
        ),
    };
    bb::dispatch(action).await
}

fn warn_deprecated(verb: &str) {
    eprintln!(
        "warning: top-level `{verb}` is deprecated; use `bb {verb}` instead. \
         This shim will be removed in the next major release."
    );
}
