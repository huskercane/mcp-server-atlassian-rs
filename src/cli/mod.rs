//! Command-line interface. Ports the Commander-based CLI from
//! `src/cli/index.ts` and `src/cli/atlassian.api.cli.ts`.
//!
//! The binary has two modes (matching TS):
//! - no arguments → stdio MCP server (see `main.rs`)
//! - any arguments → CLI dispatch through this module

pub mod api;

use std::process::ExitCode;

use clap::Parser;

use crate::constants::{CLI_NAME, VERSION};

#[derive(Debug, Parser)]
#[command(
    name = CLI_NAME,
    version = VERSION,
    about = "A Model Context Protocol (MCP) server for Atlassian Bitbucket integration",
    disable_help_subcommand = true,
    propagate_version = true,
)]
pub struct Cli {
    #[command(subcommand)]
    command: api::ApiCommand,
}

/// Entry point used by `main.rs`. Parses arguments from the supplied iterator
/// and dispatches to the matching subcommand.
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

    match api::dispatch(cli.command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}", crate::error::format_cli_error(&err));
            ExitCode::FAILURE
        }
    }
}
