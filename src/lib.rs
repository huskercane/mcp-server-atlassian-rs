//! Rust port of the TypeScript `@aashari/mcp-server-atlassian-bitbucket-rs`.
//!
//! Phase 1 scope: foundation modules (config, auth, errors, logger, constants)
//! and a minimal `rmcp` stdio server skeleton. Tools, CLI, and the streamable
//! HTTP transport are added in later phases.

#![deny(rust_2018_idioms)]

pub mod auth;
pub mod cli;
pub mod config;
pub mod constants;
pub mod controllers;
pub mod error;
pub mod format;
pub mod logger;
pub mod pagination;
pub mod server;
pub mod shell;
pub mod tools;
pub mod transport;
pub mod workspace;
