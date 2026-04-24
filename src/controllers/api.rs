#![allow(clippy::doc_markdown)]

//! Generic Bitbucket API controller. Ports
//! `src/controllers/atlassian.api.controller.ts`.
//!
//! Pipeline (shared by all five HTTP verbs):
//! 1. Resolve credentials (fail with [`auth_missing_default`] when missing).
//! 2. Normalise path: prepend `/` and then `/2.0` when not already present.
//! 3. Append the supplied `queryParams` as a URL-encoded query string.
//! 4. Dispatch the request through the transport layer, which handles the
//!    body classification, raw-response persistence, and Bitbucket error
//!    parsing.
//! 5. Apply the JMESPath filter to the response JSON (pass-through for
//!    text/empty bodies — matches TS behaviour which filters "any").
//! 6. Render as TOON or JSON according to `outputFormat`.

use std::path::PathBuf;

use reqwest::Client;
use serde_json::Value;
use tracing::debug;
use url::form_urlencoded;

use crate::auth::Credentials;
use crate::config::Config;
use crate::error::{McpError, auth_missing_default};
use crate::format::{OutputFormat, jmespath::apply_jq_filter, render};
use crate::tools::args::{QueryParams, ReadArgs, WriteArgs};
use crate::transport::{
    HttpMethod, RequestOptions, ResponseBody, TransportResponse, fetch_bitbucket_with_base,
};

/// Shared dependencies threaded into controller calls. Keeps the pipeline
/// deterministic and avoids hidden singletons.
#[derive(Debug, Clone)]
pub struct HandleContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub base_url: &'a str,
}

impl<'a> HandleContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, base_url: &'a str) -> Self {
        Self {
            client,
            config,
            base_url,
        }
    }
}

/// Final response returned to tool/CLI adapters.
#[derive(Debug, Clone)]
pub struct ControllerResponse {
    pub content: String,
    pub raw_response_path: Option<PathBuf>,
}

/// Main entry point for the five verbs. Tool/CLI handlers call this.
pub async fn handle_request(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    path: &str,
    query_params: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let creds = Credentials::resolve(ctx.config).ok_or_else(auth_missing_default)?;
    let normalized = normalize_and_append(path, query_params);
    debug!(%normalized, method = method.as_str(), "controller: dispatching");

    let opts = RequestOptions {
        method: Some(method),
        body,
        ..RequestOptions::default()
    };
    let response =
        fetch_bitbucket_with_base(ctx.base_url, ctx.client, &creds, ctx.config, &normalized, opts)
            .await?;

    Ok(render_response(&response, jq, output_format))
}

/// Convenience wrapper for read-shaped tools. Just dispatches to
/// [`handle_request`] with no body.
pub async fn handle_read(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    args: &ReadArgs,
) -> Result<ControllerResponse, McpError> {
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    handle_request(
        ctx,
        method,
        &args.path,
        args.query_params.as_ref(),
        None,
        args.jq.as_deref(),
        fmt,
    )
    .await
}

/// Convenience wrapper for write-shaped tools (POST / PUT / PATCH).
pub async fn handle_write(
    ctx: &HandleContext<'_>,
    method: HttpMethod,
    args: &WriteArgs,
) -> Result<ControllerResponse, McpError> {
    let fmt = args.output_format.map_or(OutputFormat::Toon, Into::into);
    handle_request(
        ctx,
        method,
        &args.path,
        args.query_params.as_ref(),
        Some(args.body.clone()),
        args.jq.as_deref(),
        fmt,
    )
    .await
}

fn normalize_and_append(path: &str, query_params: Option<&QueryParams>) -> String {
    let normalized = normalize_path(path);
    match query_params {
        Some(qp) if !qp.is_empty() => {
            let query: String = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(qp.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .finish();
            let joiner = if normalized.contains('?') { '&' } else { '?' };
            format!("{normalized}{joiner}{query}")
        }
        _ => normalized,
    }
}

/// Prepend `/2.0` to any path that is not already scoped to the Bitbucket
/// v2 API. Matches TS `normalizePath`.
pub fn normalize_path(path: &str) -> String {
    let mut out = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    if !out.starts_with("/2.0") {
        out = format!("/2.0{out}");
    }
    out
}

fn render_response(
    response: &TransportResponse,
    jq: Option<&str>,
    format: OutputFormat,
) -> ControllerResponse {
    let content = match &response.data {
        ResponseBody::Json(value) => {
            let filtered = apply_jq_filter(value, jq);
            render(&filtered, format)
        }
        ResponseBody::Text(text) => {
            // TS `applyJqFilter` on a string returns the same string when no
            // filter is supplied; otherwise it fails validation and we surface
            // the raw text. We keep things simple: text bodies pass through.
            text.clone()
        }
        ResponseBody::Empty => render(&Value::Object(serde_json::Map::new()), format),
    };

    ControllerResponse {
        content,
        raw_response_path: response.raw_response_path.clone(),
    }
}
