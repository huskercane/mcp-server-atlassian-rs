use std::time::Duration;

/// This server's own version, reported to MCP clients in `get_info()`, printed
/// by `--version`, and shown in the HTTP health banner.
///
/// Derived from `Cargo.toml` rather than written out, so the crate version and
/// the version users see cannot drift. They had: releases reached `v0.10.0`
/// while this constant still said `3.1.0` (inherited from the TS reference
/// server at port time) and `Cargo.toml` said `0.8.0` — three numbers for one
/// artifact. `Cargo.toml` is now the single place to bump, and the release tag
/// must match it (see the release checklist in CLAUDE.md).
///
/// Note this is unrelated to the MCP *protocol* version, which is negotiated
/// separately via `ProtocolVersion::LATEST`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const PACKAGE_NAME: &str = "@huskercane/mcp-server-atlassian";

pub const UNSCOPED_PACKAGE_NAME: &str = "mcp-server-atlassian";

pub const CLI_NAME: &str = "mcp-atlassian";

pub mod network_timeouts {
    use super::Duration;

    pub const DEFAULT_REQUEST: Duration = Duration::from_secs(30);
    pub const LARGE_REQUEST: Duration = Duration::from_mins(1);
    pub const SEARCH_REQUEST: Duration = Duration::from_secs(45);
}

pub mod data_limits {
    pub const MAX_RESPONSE_SIZE: usize = 10 * 1024 * 1024;
    pub const MAX_PAGE_SIZE: u32 = 100;
    pub const DEFAULT_PAGE_SIZE: u32 = 50;
}
