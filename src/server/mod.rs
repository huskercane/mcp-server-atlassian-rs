//! MCP server transports.
//!
//! - [`run_stdio`] — stdio JSON-RPC transport (default).
//! - [`run_http`]  — streamable-HTTP transport, behind `TRANSPORT_MODE=http`.

pub mod http;
pub mod session;
pub mod shutdown;
pub mod stdio;

pub use http::run_http;
pub use stdio::run_stdio;
