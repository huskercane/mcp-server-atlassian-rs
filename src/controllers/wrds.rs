#![allow(clippy::doc_markdown)]

//! WRDS controller path.
//!
//! WRDS is PostgreSQL, not HTTP, so this controller does not go through the
//! shared [`dispatch_with_creds`](crate::controllers::api::dispatch_with_creds)
//! transport. Instead it asks the [`WrdsVendor`] to run a query, gets back a
//! [`serde_json::Value`] array (Postgres did the type → JSON conversion), and
//! feeds it through the *same* JMESPath filter + TOON/JSON renderer every other
//! vendor uses — so `jq` and `outputFormat` behave identically. There is no
//! raw-response file (that is an HTTP-transport concept), so
//! [`ControllerResponse::raw_response_path`] is always `None`.

use serde_json::Value;

use crate::config::Config;
use crate::controllers::api::ControllerResponse;
use crate::error::McpError;
use crate::format::{OutputFormat, jmespath::apply_jq_filter, render};
use crate::tools::args::{
    WrdsDescribeTableArgs, WrdsListLibrariesArgs, WrdsListTablesArgs, WrdsQueryArgs,
};
use crate::vendor::wrds::{WrdsVendor, clamp_row_limit};

/// WRDS-specific request context. Carries the concrete [`WrdsVendor`] (which
/// owns the Postgres connection path) plus config. Unlike the HTTP vendors there
/// is no shared `reqwest::Client` to thread through.
pub struct WrdsContext<'a> {
    pub config: &'a Config,
    pub vendor: &'a WrdsVendor,
}

impl<'a> WrdsContext<'a> {
    pub fn new(config: &'a Config, vendor: &'a WrdsVendor) -> Self {
        Self { config, vendor }
    }
}

/// Render a result-set [`Value`] through the shared filter + formatter.
fn respond(data: &Value, jq: Option<&str>, fmt: OutputFormat) -> ControllerResponse {
    let filtered = apply_jq_filter(data, jq);
    ControllerResponse {
        content: render(&filtered, fmt),
        raw_response_path: None,
    }
}

/// Run an arbitrary read-only SQL query against WRDS.
pub async fn query(
    ctx: &WrdsContext<'_>,
    args: &WrdsQueryArgs,
) -> Result<ControllerResponse, McpError> {
    let limit = clamp_row_limit(args.row_limit);
    let data = ctx.vendor.run_sql(ctx.config, &args.sql, limit).await?;
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    Ok(respond(&data, args.jq.as_deref(), fmt))
}

/// List the WRDS libraries (schemas) the configured user can access.
pub async fn list_libraries(
    ctx: &WrdsContext<'_>,
    args: &WrdsListLibrariesArgs,
) -> Result<ControllerResponse, McpError> {
    let data = ctx
        .vendor
        .list_libraries(ctx.config, clamp_row_limit(None))
        .await?;
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    Ok(respond(&data, args.jq.as_deref(), fmt))
}

/// List the tables/views inside one WRDS library.
pub async fn list_tables(
    ctx: &WrdsContext<'_>,
    args: &WrdsListTablesArgs,
) -> Result<ControllerResponse, McpError> {
    let limit = clamp_row_limit(args.row_limit);
    let data = ctx
        .vendor
        .list_tables(ctx.config, &args.library, limit)
        .await?;
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    Ok(respond(&data, args.jq.as_deref(), fmt))
}

/// Describe one WRDS table's columns.
pub async fn describe_table(
    ctx: &WrdsContext<'_>,
    args: &WrdsDescribeTableArgs,
) -> Result<ControllerResponse, McpError> {
    let data = ctx
        .vendor
        .describe_table(
            ctx.config,
            &args.library,
            &args.table,
            clamp_row_limit(None),
        )
        .await?;
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    Ok(respond(&data, args.jq.as_deref(), fmt))
}
