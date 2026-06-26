#![allow(clippy::doc_markdown)]

//! WRDS (Wharton Research Data Services) vendor — the one non-HTTP integration.
//!
//! Every other vendor in this crate is a REST API behind the HTTP
//! [`Vendor`](crate::vendor::Vendor) trait. WRDS is different: there is **no REST
//! API**. Programmatic access is a direct **PostgreSQL** connection to
//! `wrds-pgdata.wharton.upenn.edu:9737` (SSL required) — exactly what the
//! official `wrds` Python package wraps. So this module deliberately does *not*
//! implement [`Vendor`]; it owns a Postgres connection path instead, and maps
//! [`tokio_postgres::Error`] onto the same [`McpError`] envelope (see [`error`]).
//!
//! ## Design notes
//!
//! - **Lazy + connect-per-call.** Nothing dials WRDS until a `wrds_*` tool is
//!   invoked. Each call opens a fresh connection, runs one query, and drops it.
//!   MCP tool calls are low-frequency and interactive, so a connection pool is
//!   premature; a fresh connection avoids any stale-socket/liveness bookkeeping.
//!   The TLS [`ClientConfig`] *is* cached (building it loads the OS trust store).
//! - **`to_jsonb` everywhere.** Every query is wrapped so PostgreSQL itself does
//!   the type → JSON conversion (`jsonb_agg(to_jsonb(row))`). The client reads
//!   each result set as a single [`serde_json::Value`] array — no per-column
//!   type mapping, no precision loss surprises, and the result feeds the shared
//!   TOON/JSON renderer unchanged.
//! - **Read-only by construction.** Each session sets
//!   `default_transaction_read_only = on` and a `statement_timeout`, and the
//!   query wrapper places the caller's SQL inside a subquery — so only a single
//!   `SELECT`/`VALUES` can run (a stray `INSERT`/`UPDATE`/DDL or multi-statement
//!   body fails to parse). WRDS accounts are read-only at the server too; this is
//!   defence in depth.
//! - **Injection-safe discovery.** `wrds_list_tables` / `wrds_describe_table`
//!   pass the library/table names as bound query parameters against
//!   `information_schema`, never string-interpolated.

pub mod error;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::ClientConfig;
use serde_json::Value;
use tokio_postgres::Config as PgConfig;
use tokio_postgres::config::SslMode;
use tokio_postgres::types::ToSql;
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::debug;

use crate::config::{Config, VENDOR_WRDS};
use crate::error::{McpError, OriginalError, api_error, auth_missing};

/// Default WRDS Cloud Postgres host.
pub const DEFAULT_HOST: &str = "wrds-pgdata.wharton.upenn.edu";
/// Default WRDS Postgres port.
pub const DEFAULT_PORT: u16 = 9737;
/// Default WRDS database name.
pub const DEFAULT_DBNAME: &str = "wrds";

/// Default result-set row cap (token-cost guard) when the caller omits one.
pub const DEFAULT_ROW_LIMIT: u32 = 1_000;
/// Hard upper bound on the row cap a caller may request.
pub const MAX_ROW_LIMIT: u32 = 100_000;

/// Per-statement timeout applied to every session.
const STATEMENT_TIMEOUT_MS: u32 = 60_000;
/// Connection establishment timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// `application_name` reported to Postgres (shows up in `pg_stat_activity`).
const APPLICATION_NAME: &str = "mcp-atlassian";

/// Clamp a caller-supplied row limit into `[1, MAX_ROW_LIMIT]`, defaulting when
/// absent.
pub fn clamp_row_limit(requested: Option<u32>) -> u32 {
    requested
        .unwrap_or(DEFAULT_ROW_LIMIT)
        .clamp(1, MAX_ROW_LIMIT)
}

/// WRDS Postgres vendor. Holds only a lazily-built, cached TLS config; the
/// connection itself is opened per query. Not [`Clone`] (the `OnceLock` cache
/// is per-instance) and not a [`Vendor`] (WRDS is not HTTP) — it lives behind
/// the server's `Arc<ServerState>` like every other vendor.
#[derive(Default)]
pub struct WrdsVendor {
    tls: OnceLock<Arc<ClientConfig>>,
}

/// Resolved connection parameters for one WRDS session.
struct ConnParams {
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    ssl_mode: SslMode,
}

impl WrdsVendor {
    /// Production constructor. Resolves all connection params from config at
    /// query time; nothing here touches the network.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve connection parameters from the `wrds` config section. Username
    /// and password are required; everything else defaults to the WRDS Cloud
    /// values. Errors with an actionable message at tool-call time so a
    /// non-WRDS deployment still boots.
    fn conn_params(config: &Config) -> Result<ConnParams, McpError> {
        let user = require(config, "WRDS_USERNAME")?;
        let password = require(config, "WRDS_PASSWORD")?;
        let host = config
            .get_for(VENDOR_WRDS, "WRDS_HOST")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_HOST)
            .to_owned();
        let port = config
            .get_for(VENDOR_WRDS, "WRDS_PORT")
            .and_then(|v| v.trim().parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        let dbname = config
            .get_for(VENDOR_WRDS, "WRDS_DBNAME")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_DBNAME)
            .to_owned();
        let ssl_mode = match config
            .get_for(VENDOR_WRDS, "WRDS_SSLMODE")
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("disable") => SslMode::Disable,
            Some("prefer") => SslMode::Prefer,
            // WRDS requires SSL; that is also the default for any other value.
            _ => SslMode::Require,
        };

        Ok(ConnParams {
            host,
            port,
            dbname,
            user,
            password,
            ssl_mode,
        })
    }

    /// Build (once) and cache the rustls client config. Uses the OS trust store
    /// for roots and the aws-lc-rs provider already linked via reqwest, so it
    /// adds no crypto code of its own. On a build failure nothing is cached and
    /// the (rare) error propagates.
    fn tls_config(&self) -> Result<Arc<ClientConfig>, McpError> {
        if let Some(cfg) = self.tls.get() {
            return Ok(cfg.clone());
        }
        let cfg = Arc::new(build_tls_config()?);
        // A concurrent builder may have won the race; either value is valid.
        let _ = self.tls.set(cfg.clone());
        Ok(cfg)
    }

    /// Open a fresh authenticated, TLS-secured connection and spawn its driver
    /// task. The returned client owns the connection; dropping it ends the
    /// driver.
    async fn connect(&self, config: &Config) -> Result<tokio_postgres::Client, McpError> {
        let params = Self::conn_params(config)?;
        let tls_cfg = self.tls_config()?;
        let tls = MakeRustlsConnect::new((*tls_cfg).clone());

        let mut pg = PgConfig::new();
        pg.host(&params.host)
            .port(params.port)
            .dbname(&params.dbname)
            .user(&params.user)
            .password(&params.password)
            .ssl_mode(params.ssl_mode)
            .application_name(APPLICATION_NAME)
            .connect_timeout(CONNECT_TIMEOUT);

        debug!(host = %params.host, port = params.port, db = %params.dbname, "wrds: connecting");
        let (client, connection) = pg.connect(tls).await.map_err(|e| error::classify(&e))?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                debug!(error = %e, "wrds: connection closed");
            }
        });
        Ok(client)
    }

    /// Run `base_sql` (with optional bound `params`) and return the result set
    /// as a JSON array. The query is wrapped so Postgres aggregates the rows to
    /// JSONB server-side; the caller's SQL must be a single read-only
    /// `SELECT`/`VALUES`. A `limit` caps the rows materialised.
    pub async fn query_json(
        &self,
        config: &Config,
        base_sql: &str,
        params: &[&(dyn ToSql + Sync)],
        limit: u32,
    ) -> Result<Value, McpError> {
        let client = self.connect(config).await?;

        // Enforce read-only + a statement timeout for the whole session. Both
        // values are server-controlled integers/keywords — no user input.
        client
            .batch_execute(&format!(
                "SET statement_timeout = {STATEMENT_TIMEOUT_MS}; \
                 SET default_transaction_read_only = on;"
            ))
            .await
            .map_err(|e| error::classify(&e))?;

        let wrapped = wrap_query(base_sql, limit);
        let row = client
            .query_one(&wrapped, params)
            .await
            .map_err(|e| error::classify(&e))?;
        let data: Value = row
            .try_get(0)
            .map_err(|e| api_error(format!("WRDS: failed to decode result: {e}"), None, None))?;
        Ok(data)
    }

    /// Run an arbitrary read-only SQL `SELECT` supplied by the caller.
    pub fn run_sql<'a>(
        &'a self,
        config: &'a Config,
        sql: &'a str,
        limit: u32,
    ) -> impl std::future::Future<Output = Result<Value, McpError>> + Send + 'a {
        self.query_json(config, sql, &[], limit)
    }

    /// List the WRDS libraries (Postgres schemas) the current user may access.
    pub fn list_libraries<'a>(
        &'a self,
        config: &'a Config,
        limit: u32,
    ) -> impl std::future::Future<Output = Result<Value, McpError>> + Send + 'a {
        const SQL: &str = "SELECT nspname AS library \
             FROM pg_catalog.pg_namespace \
             WHERE nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
               AND nspname NOT LIKE 'pg_temp_%' \
               AND nspname NOT LIKE 'pg_toast_temp_%' \
               AND has_schema_privilege(current_user, nspname, 'USAGE') \
             ORDER BY nspname";
        self.query_json(config, SQL, &[], limit)
    }

    /// List the tables and views inside one WRDS library (schema).
    pub async fn list_tables(
        &self,
        config: &Config,
        library: &str,
        limit: u32,
    ) -> Result<Value, McpError> {
        const SQL: &str = "SELECT table_name, table_type \
             FROM information_schema.tables \
             WHERE table_schema = $1 \
             ORDER BY table_name";
        self.query_json(config, SQL, &[&library], limit).await
    }

    /// Describe a WRDS table's columns (name, type, nullability), in column
    /// order.
    pub async fn describe_table(
        &self,
        config: &Config,
        library: &str,
        table: &str,
        limit: u32,
    ) -> Result<Value, McpError> {
        const SQL: &str = "SELECT column_name, data_type, is_nullable \
             FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 \
             ORDER BY ordinal_position";
        self.query_json(config, SQL, &[&library, &table], limit)
            .await
    }
}

/// Read a required WRDS credential, trimming and rejecting blanks.
fn require(config: &Config, key: &str) -> Result<String, McpError> {
    config
        .get_for(VENDOR_WRDS, key)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            auth_missing(format!(
                "{key} is required for wrds_* tools. Set your WRDS username and password \
                 (WRDS_USERNAME / WRDS_PASSWORD) under the `wrds` section of \
                 ~/.mcp/configs.json or in the environment."
            ))
        })
}

/// Wrap a caller `SELECT` so Postgres aggregates it to a JSONB array
/// server-side and the row count is capped. A trailing `;` is stripped so the
/// wrapped subquery parses; any *embedded* statement separator simply fails to
/// parse, which is the desired rejection of multi-statement input.
fn wrap_query(base_sql: &str, limit: u32) -> String {
    let trimmed = base_sql.trim().trim_end_matches(';').trim_end();
    format!(
        "SELECT coalesce(jsonb_agg(__r), '[]'::jsonb) AS data \
         FROM (SELECT to_jsonb(__t) AS __r FROM ({trimmed}) __t LIMIT {limit}) __s"
    )
}

/// Build a rustls client config: OS trust store for roots, aws-lc-rs provider
/// (already linked via reqwest) for crypto. Explicit provider selection avoids
/// depending on a process-global default being installed.
fn build_tls_config() -> Result<ClientConfig, McpError> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for cert in loaded.certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(api_error(
            "WRDS TLS: no system root certificates available to validate the WRDS server",
            None,
            loaded
                .errors
                .first()
                .map(|e| OriginalError::String(e.to_string())),
        ));
    }

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| api_error(format!("WRDS TLS setup failed: {e}"), None, None))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_query_strips_trailing_semicolon_and_caps_rows() {
        let sql = wrap_query("SELECT 1 AS n;  ", 50);
        assert!(sql.contains("FROM (SELECT 1 AS n) __t"));
        assert!(sql.ends_with("LIMIT 50) __s"));
        assert!(sql.starts_with("SELECT coalesce(jsonb_agg"));
    }

    #[test]
    fn clamp_row_limit_applies_default_and_bounds() {
        assert_eq!(clamp_row_limit(None), DEFAULT_ROW_LIMIT);
        assert_eq!(clamp_row_limit(Some(0)), 1);
        assert_eq!(clamp_row_limit(Some(10)), 10);
        assert_eq!(clamp_row_limit(Some(u32::MAX)), MAX_ROW_LIMIT);
    }
}
