#![allow(clippy::doc_markdown)]

//! edX / Open edX discussion controller path.
//!
//! The public MCP tools are discussion-specific and build documented
//! `/api/discussion/v1/...` requests. Authentication is a static bearer token
//! from `EDX_ACCESS_TOKEN`; everything after that uses the shared request
//! dispatcher, output rendering, raw-response persistence, and JMESPath
//! filtering.

use reqwest::Client;
use serde_json::{Value, json};

use crate::auth::Credentials;
use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::{McpError, api_error};
use crate::format::OutputFormat;
use crate::tools::args::{
    EdxDiscussionCommentCreateArgs, EdxDiscussionCommentsArgs, EdxDiscussionCourseArgs,
    EdxDiscussionThreadCreateArgs, EdxDiscussionThreadsArgs, EdxDiscussionTopicsArgs,
    OutputFormatArg, QueryParams,
};
use crate::transport::HttpMethod;
use crate::vendor::edx::EdxVendor;

pub struct EdxContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a EdxVendor,
}

impl<'a> EdxContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a EdxVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

pub async fn course(
    ctx: &EdxContext<'_>,
    args: &EdxDiscussionCourseArgs,
) -> Result<ControllerResponse, McpError> {
    dispatch(
        ctx,
        HttpMethod::Get,
        &format!(
            "/api/discussion/v1/courses/{}/",
            encode_path_segment(&args.course_id)
        ),
        None,
        None,
        args.output.jq.as_deref(),
        output_format(args.output.output_format),
    )
    .await
}

pub async fn topics(
    ctx: &EdxContext<'_>,
    args: &EdxDiscussionTopicsArgs,
) -> Result<ControllerResponse, McpError> {
    dispatch(
        ctx,
        HttpMethod::Get,
        &format!(
            "/api/discussion/v1/course_topics/{}/",
            encode_path_segment(&args.course_id)
        ),
        None,
        None,
        args.output.jq.as_deref(),
        output_format(args.output.output_format),
    )
    .await
}

pub async fn threads(
    ctx: &EdxContext<'_>,
    args: &EdxDiscussionThreadsArgs,
) -> Result<ControllerResponse, McpError> {
    let mut qp = QueryParams::new();
    qp.insert("course_id".into(), args.course_id.clone());
    if let Some(topic_id) = &args.topic_id {
        qp.insert("topic_id".into(), topic_id.clone());
    }
    if let Some(following) = args.following {
        qp.insert("following".into(), bool_param(following).into());
    }
    if let Some(view) = args.view {
        qp.insert("view".into(), view.as_str().into());
    }
    if let Some(text_search) = &args.text_search {
        qp.insert("text_search".into(), text_search.clone());
    }
    if let Some(order_by) = args.order_by {
        qp.insert("order_by".into(), order_by.as_str().into());
    }
    if let Some(order_direction) = args.order_direction {
        qp.insert("order_direction".into(), order_direction.as_str().into());
    }
    if let Some(page_size) = args.page_size {
        qp.insert("page_size".into(), page_size.to_string());
    }
    if let Some(page) = args.page {
        qp.insert("page".into(), page.to_string());
    }

    dispatch(
        ctx,
        HttpMethod::Get,
        "/api/discussion/v1/threads/",
        Some(&qp),
        None,
        args.output.jq.as_deref(),
        output_format(args.output.output_format),
    )
    .await
}

pub async fn create_thread(
    ctx: &EdxContext<'_>,
    args: &EdxDiscussionThreadCreateArgs,
) -> Result<ControllerResponse, McpError> {
    let mut body = json!({
        "course_id": &args.course_id,
        "topic_id": &args.topic_id,
        "type": args.thread_type.as_str(),
        "title": &args.title,
        "raw_body": &args.raw_body,
    });

    if let Some(group_id) = args.group_id
        && let Value::Object(map) = &mut body
    {
        map.insert("group_id".into(), json!(group_id));
    }

    dispatch(
        ctx,
        HttpMethod::Post,
        "/api/discussion/v1/threads/",
        None,
        Some(body),
        args.output.jq.as_deref(),
        output_format(args.output.output_format),
    )
    .await
}

pub async fn comments(
    ctx: &EdxContext<'_>,
    args: &EdxDiscussionCommentsArgs,
) -> Result<ControllerResponse, McpError> {
    let mut qp = QueryParams::new();
    qp.insert("thread_id".into(), args.thread_id.clone());
    if let Some(endorsed) = args.endorsed {
        qp.insert("endorsed".into(), bool_param(endorsed).into());
    }
    if let Some(page_size) = args.page_size {
        qp.insert("page_size".into(), page_size.to_string());
    }
    if let Some(page) = args.page {
        qp.insert("page".into(), page.to_string());
    }

    dispatch(
        ctx,
        HttpMethod::Get,
        "/api/discussion/v1/comments/",
        Some(&qp),
        None,
        args.output.jq.as_deref(),
        output_format(args.output.output_format),
    )
    .await
}

pub async fn create_comment(
    ctx: &EdxContext<'_>,
    args: &EdxDiscussionCommentCreateArgs,
) -> Result<ControllerResponse, McpError> {
    if args.thread_id.is_none() && args.parent_id.is_none() {
        return Err(api_error(
            "edx_discussion_comment_create requires either `threadId` or `parentId`.",
            None,
            None,
        ));
    }

    let mut body = json!({
        "raw_body": &args.raw_body,
    });
    if let Value::Object(map) = &mut body {
        if let Some(thread_id) = &args.thread_id {
            map.insert("thread_id".into(), json!(thread_id));
        }
        if let Some(parent_id) = &args.parent_id {
            map.insert("parent_id".into(), json!(parent_id));
        }
    }

    dispatch(
        ctx,
        HttpMethod::Post,
        "/api/discussion/v1/comments/",
        None,
        Some(body),
        args.output.jq.as_deref(),
        output_format(args.output.output_format),
    )
    .await
}

async fn dispatch(
    ctx: &EdxContext<'_>,
    method: HttpMethod,
    path: &str,
    query_params: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    let token = ctx.vendor.token(ctx.config)?;
    let creds = Credentials::Bearer { token };
    let handle = HandleContext::new(ctx.client, ctx.config, ctx.vendor);
    dispatch_with_creds(
        &handle,
        &creds,
        method,
        path,
        query_params,
        body,
        jq,
        output_format,
    )
    .await
}

fn output_format(value: Option<OutputFormatArg>) -> OutputFormat {
    value.map_or(OutputFormat::Toon, Into::into)
}

fn bool_param(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn encode_path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
