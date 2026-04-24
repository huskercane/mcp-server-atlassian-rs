use std::time::Duration;

pub const VERSION: &str = "3.1.0";

pub const PACKAGE_NAME: &str = "@huskercane/mcp-server-atlassian-bitbucket-rs";

pub const UNSCOPED_PACKAGE_NAME: &str = "mcp-server-atlassian-bitbucket-rs";

pub const CLI_NAME: &str = "mcp-atlassian-bitbucket";

pub mod network_timeouts {
    use super::Duration;

    pub const DEFAULT_REQUEST: Duration = Duration::from_secs(30);
    pub const LARGE_REQUEST: Duration = Duration::from_secs(60);
    pub const SEARCH_REQUEST: Duration = Duration::from_secs(45);
}

pub mod data_limits {
    pub const MAX_RESPONSE_SIZE: usize = 10 * 1024 * 1024;
    pub const MAX_PAGE_SIZE: u32 = 100;
    pub const DEFAULT_PAGE_SIZE: u32 = 50;
}
