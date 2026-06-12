#![allow(clippy::doc_markdown)]

//! Grafana controller path.
//!
//! Two read tools sit on Grafana's HTTP API, both authenticated with a static
//! service-account token (`GRAFANA_TOKEN`) injected as `Authorization: Bearer`
//! via [`Credentials::Bearer`]:
//!
//! - [`query_logs`] — runs a LogQL query against a Loki datasource through
//!   Grafana's datasource proxy (`GET .../api/datasources/proxy/uid/{uid}/loki/api/v1/query_range`).
//! - [`list_datasources`] — `GET /api/datasources`, used to discover the Loki
//!   datasource UID that `query_logs` needs.
//!
//! Everything after auth — base-URL resolution, query encoding, transport,
//! error classification, output rendering, raw-response persistence, and
//! JMESPath filtering — is the same code the other vendors use.

use reqwest::Client;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::{GrafanaListDatasourcesArgs, GrafanaQueryLogsArgs, QueryParams};
use crate::transport::HttpMethod;
use crate::vendor::grafana::{
    DATASOURCE_PROXY_PREFIX, DATASOURCES_PATH, GrafanaVendor, LOKI_QUERY_RANGE_PATH,
};

/// Grafana-specific request context. Carries the concrete [`GrafanaVendor`]
/// (not a `&dyn Vendor`) so the token read can be driven, plus the shared
/// client and config.
pub struct GrafanaContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a GrafanaVendor,
}

impl<'a> GrafanaContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a GrafanaVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Run a LogQL query against a Loki datasource via Grafana's datasource proxy.
/// Resolves the token, builds the proxy path for the caller-supplied datasource
/// UID, and forwards the LogQL plus optional range/limit knobs as query params.
/// Kept as an `async fn` — there is a `?` on the token resolution before the
/// dispatch await, so the single-tail-await `impl Future` optimisation does not
/// apply.
pub async fn query_logs(
    ctx: &GrafanaContext<'_>,
    args: &GrafanaQueryLogsArgs,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config)?;
    let creds = Credentials::Bearer { token };

    // datasource UIDs are `[a-zA-Z0-9_-]`, so direct interpolation is safe.
    let path = format!(
        "{DATASOURCE_PROXY_PREFIX}/{uid}{LOKI_QUERY_RANGE_PATH}",
        uid = args.datasource_uid,
    );

    let mut qp: QueryParams = QueryParams::new();
    qp.insert("query".into(), args.query.clone());
    if let Some(start) = &args.start {
        qp.insert("start".into(), start.clone());
    }
    if let Some(end) = &args.end {
        qp.insert("end".into(), end.clone());
    }
    if let Some(limit) = args.limit {
        qp.insert("limit".into(), limit.to_string());
    }
    if let Some(direction) = &args.direction {
        qp.insert("direction".into(), direction.clone());
    }
    if let Some(step) = &args.step {
        qp.insert("step".into(), step.clone());
    }

    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        &path,
        Some(&qp),
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// List configured datasources so the caller can discover a Loki datasource's
/// UID. Same auth/transport path as [`query_logs`]; filtering to Loki
/// datasources is left to the caller's `jq` (e.g. `[?type=='loki']`).
pub async fn list_datasources(
    ctx: &GrafanaContext<'_>,
    args: &GrafanaListDatasourcesArgs,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config)?;
    let creds = Credentials::Bearer { token };

    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        DATASOURCES_PATH,
        None,
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}
