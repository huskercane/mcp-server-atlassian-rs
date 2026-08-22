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

pub const PACKAGE_NAME: &str = "@huskercane/mcp-server-devtools";

pub const UNSCOPED_PACKAGE_NAME: &str = "mcp-server-devtools";

/// Previous package identifiers accepted when reading existing MCP config.
pub const LEGACY_PACKAGE_NAME: &str = "@huskercane/mcp-server-atlassian";
pub const LEGACY_UNSCOPED_PACKAGE_NAME: &str = "mcp-server-atlassian";

pub const CLI_NAME: &str = "mcp-devtools";

pub mod network_timeouts {
    use super::Duration;

    pub const DEFAULT_REQUEST: Duration = Duration::from_secs(30);
    pub const LARGE_REQUEST: Duration = Duration::from_mins(1);
    pub const SEARCH_REQUEST: Duration = Duration::from_secs(45);
}

pub mod data_limits {
    pub const MAX_RESPONSE_SIZE: usize = 10 * 1024 * 1024;
    /// Hard decoded-byte ceiling for streamed log artifacts. This is enforced
    /// while consuming the body, including chunked responses.
    pub const MAX_STREAMED_ARTIFACT_SIZE: u64 = 512 * 1024 * 1024;
    pub const STREAM_WRITE_BUFFER_SIZE: usize = 64 * 1024;
    pub const STREAM_PREVIEW_HEAD_SIZE: usize = 5 * 1024;
    pub const STREAM_PREVIEW_TAIL_SIZE: usize = 30 * 1024;
    pub const MAX_STREAM_RECORD_SIZE: usize = 1024 * 1024;
    pub const STREAM_IDLE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    pub const STREAM_TOTAL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);
    pub const STREAM_MAX_ATTEMPTS: usize = 3;
    pub const MAX_TIME_PARTITIONS: usize = 16;
    /// Default number of time-partition requests acquired concurrently.
    pub const DEFAULT_PARALLEL_TIME_PARTITIONS: usize = 4;
    /// Successful downloadable artifacts remain readable for one hour unless
    /// an internal server setting selects another bounded value.
    pub const DEFAULT_STREAMING_ARTIFACT_RETENTION: std::time::Duration =
        std::time::Duration::from_hours(1);
    pub const MIN_STREAMING_ARTIFACT_RETENTION: std::time::Duration =
        std::time::Duration::from_mins(5);
    pub const MAX_STREAMING_ARTIFACT_RETENTION: std::time::Duration =
        std::time::Duration::from_hours(24 * 7);
    pub const DEFAULT_STREAMING_ARTIFACT_SWEEP_INTERVAL: std::time::Duration =
        std::time::Duration::from_mins(1);
    pub const MIN_STREAMING_ARTIFACT_SWEEP_INTERVAL: std::time::Duration =
        std::time::Duration::from_secs(5);
    pub const MAX_STREAMING_ARTIFACT_SWEEP_INTERVAL: std::time::Duration =
        std::time::Duration::from_hours(1);
    pub const STREAMING_ARTIFACT_SHUTDOWN_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(5);
    pub const MAX_STREAMING_ARTIFACT_RECLAIMS_PER_SWEEP: usize = 64;
    pub const MAX_PAGE_SIZE: u32 = 100;
    pub const DEFAULT_PAGE_SIZE: u32 = 50;
}
