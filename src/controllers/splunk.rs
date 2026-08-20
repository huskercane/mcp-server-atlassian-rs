#![allow(clippy::doc_markdown)]

//! Splunk search and saved-search controller path.

use reqwest::Client;
use reqwest::header::AUTHORIZATION;

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{
    ControllerResponse, HandleContext, dispatch_form_with_creds, dispatch_with_creds,
};
use crate::error::{McpError, api_error};
use crate::format::OutputFormat;
use crate::tools::args::{
    QueryParams, SplunkCreateJobArgs, SplunkJobResultsArgs, SplunkListSavedSearchesArgs,
    SplunkSearchArgs,
};
use crate::transport::HttpMethod;
use crate::vendor::splunk::{
    SAVED_SEARCHES_PATH, SEARCH_EXPORT_PATH, SEARCH_JOBS_PATH, SplunkVendor,
};

pub struct SplunkContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a SplunkVendor,
}

impl<'a> SplunkContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a SplunkVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

pub async fn search(
    ctx: &SplunkContext<'_>,
    args: &SplunkSearchArgs,
) -> Result<ControllerResponse, McpError> {
    let creds = credentials(ctx).await?;
    let mut form = search_form(
        &args.search,
        args.earliest_time.as_deref(),
        args.latest_time.as_deref(),
        args.max_time,
    );
    form.insert("output_mode".into(), "json_rows".into());

    let format = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_form_with_creds(
        &handle,
        &creds,
        HttpMethod::Post,
        SEARCH_EXPORT_PATH,
        None,
        form,
        args.jq.as_deref(),
        format,
    )
    .await
}

pub async fn create_job(
    ctx: &SplunkContext<'_>,
    args: &SplunkCreateJobArgs,
) -> Result<ControllerResponse, McpError> {
    let creds = credentials(ctx).await?;
    let mut form = search_form(
        &args.search,
        args.earliest_time.as_deref(),
        args.latest_time.as_deref(),
        args.max_time,
    );
    if let Some(max_count) = args.max_count {
        form.insert("max_count".into(), max_count.to_string());
    }
    form.insert("output_mode".into(), "json".into());

    let format = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_form_with_creds(
        &handle,
        &creds,
        HttpMethod::Post,
        SEARCH_JOBS_PATH,
        None,
        form,
        args.jq.as_deref(),
        format,
    )
    .await
}

pub async fn job_results(
    ctx: &SplunkContext<'_>,
    args: &SplunkJobResultsArgs,
) -> Result<ControllerResponse, McpError> {
    validate_path_segment(&args.sid, "sid")?;
    let creds = credentials(ctx).await?;
    let mut query = QueryParams::new();
    query.insert("output_mode".into(), "json_rows".into());
    if let Some(count) = args.count {
        query.insert("count".into(), count.to_string());
    }
    if let Some(offset) = args.offset {
        query.insert("offset".into(), offset.to_string());
    }

    let path = format!("/services/search/v2/jobs/{}/results", args.sid);
    let format = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        &path,
        Some(&query),
        None,
        args.jq.as_deref(),
        format,
    )
    .await
}

pub async fn list_saved_searches(
    ctx: &SplunkContext<'_>,
    args: &SplunkListSavedSearchesArgs,
) -> Result<ControllerResponse, McpError> {
    let creds = credentials(ctx).await?;
    let mut query = QueryParams::new();
    query.insert("output_mode".into(), "json".into());
    if let Some(search) = &args.search {
        query.insert("search".into(), search.clone());
    }
    if let Some(count) = args.count {
        query.insert("count".into(), count.to_string());
    }
    if let Some(offset) = args.offset {
        query.insert("offset".into(), offset.to_string());
    }

    let format = args.output_format.map_or(OutputFormat::Toon, Into::into);
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        HttpMethod::Get,
        SAVED_SEARCHES_PATH,
        Some(&query),
        None,
        args.jq.as_deref(),
        format,
    )
    .await
}

async fn credentials(ctx: &SplunkContext<'_>) -> Result<Credentials, McpError> {
    let token = ctx.vendor.token(ctx.config).await?;
    let scheme = ctx.vendor.auth_scheme(ctx.config);
    Ok(Credentials::ApiKeyHeader {
        header_name: AUTHORIZATION.as_str().to_owned(),
        key: format!("{scheme} {token}"),
    })
}

fn search_form(
    search: &str,
    earliest_time: Option<&str>,
    latest_time: Option<&str>,
    max_time: Option<u32>,
) -> QueryParams {
    let mut form = QueryParams::new();
    form.insert("search".into(), search.to_owned());
    if let Some(earliest_time) = earliest_time {
        form.insert("earliest_time".into(), earliest_time.to_owned());
    }
    if let Some(latest_time) = latest_time {
        form.insert("latest_time".into(), latest_time.to_owned());
    }
    if let Some(max_time) = max_time {
        form.insert("max_time".into(), max_time.to_string());
    }
    form
}

fn validate_path_segment(value: &str, name: &str) -> Result<(), McpError> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'?' | b'#'))
    {
        return Err(api_error(
            format!("Invalid Splunk {name}: expected a non-empty path segment"),
            Some(400),
            None,
        ));
    }
    Ok(())
}
