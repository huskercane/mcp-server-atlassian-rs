//! `mcp-atlassian` binary entry point.
//!
//! Runtime mode is chosen by argv + `TRANSPORT_MODE`, matching the TS
//! behaviour at `src/index.ts:380-400`:
//! - Arguments present: route to the CLI (`cli::run`).
//! - Otherwise: read `TRANSPORT_MODE` (default `stdio`) and start either the
//!   stdio or streamable-HTTP transport.

use std::process::ExitCode;

use mcp_server_atlassian::{cli, logger, server};

#[tokio::main]
async fn main() -> ExitCode {
    logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        cli::run(args).await
    } else {
        let mode = std::env::var("TRANSPORT_MODE")
            .unwrap_or_else(|_| "stdio".into())
            .to_ascii_lowercase();
        let result = match mode.as_str() {
            "http" => server::run_http().await,
            "stdio" => server::run_stdio().await,
            other => {
                eprintln!("unknown TRANSPORT_MODE \"{other}\", defaulting to stdio");
                server::run_stdio().await
            }
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("fatal: {err}");
                ExitCode::FAILURE
            }
        }
    }
}
