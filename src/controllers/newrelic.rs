#![allow(clippy::doc_markdown)]

//! New Relic NerdGraph controller path.
//!
//! Unlike the REST vendors, New Relic exposes a single GraphQL endpoint. The
//! one public tool — `newrelic_query` — wraps the caller's GraphQL document
//! (and optional variables) into the `{query, variables}` envelope NerdGraph
//! expects and POSTs it to `/graphql`. Authentication is a static User API key
//! from `NEW_RELIC_API_KEY`, injected as the custom `API-Key` header via
//! [`Credentials::ApiKeyHeader`]; everything after auth — transport, the
//! `errors`-array reclassification, output rendering, raw-response persistence,
//! and JMESPath filtering — is the same code the other vendors use.

use reqwest::Client;
use serde_json::{Value, json};

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::NewRelicQueryArgs;
use crate::transport::HttpMethod;
use crate::vendor::newrelic::{API_KEY_HEADER, GRAPHQL_PATH, NewRelicVendor};

/// New Relic-specific request context. Carries the concrete [`NewRelicVendor`]
/// (not a `&dyn Vendor`) so the API-key read can be driven, plus the shared
/// client and config.
pub struct NewRelicContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a NewRelicVendor,
}

impl<'a> NewRelicContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a NewRelicVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

/// Run a NerdGraph query. Resolves the API key, builds the `{query, variables}`
/// body, and POSTs it to the single `/graphql` endpoint. Kept as an `async fn`
/// — there is a `?` on the key resolution before the dispatch await, so the
/// single-tail-await `impl Future` optimisation does not apply.
pub async fn query(
    ctx: &NewRelicContext<'_>,
    args: &NewRelicQueryArgs,
) -> Result<ControllerResponse, McpError> {
    let key = ctx.vendor.api_key(ctx.config)?;
    let creds = Credentials::ApiKeyHeader {
        header_name: API_KEY_HEADER.to_owned(),
        key,
    };

    let mut body = json!({ "query": &args.query });
    if let Some(variables) = &args.variables
        && let Value::Object(map) = &mut body
    {
        map.insert("variables".into(), variables.clone());
    }

    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Post,
        GRAPHQL_PATH,
        None,
        Some(body),
        args.jq.as_deref(),
        fmt,
    )
    .await
}
