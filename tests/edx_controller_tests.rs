#![allow(clippy::doc_markdown)]

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::edx::{
    EdxContext, comments, course, create_comment, create_thread, threads, topics,
};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::tools::args::{
    EdxDiscussionCommentCreateArgs, EdxDiscussionCommentsArgs, EdxDiscussionCourseArgs,
    EdxDiscussionOrderDirection, EdxDiscussionOutputArgs, EdxDiscussionThreadCreateArgs,
    EdxDiscussionThreadOrderBy, EdxDiscussionThreadType, EdxDiscussionThreadsArgs,
    EdxDiscussionTopicsArgs, OutputFormatArg,
};
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::edx::EdxVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("EDX_ACCESS_TOKEN".into(), "edx-token".into());
    m
}

fn output() -> EdxDiscussionOutputArgs {
    EdxDiscussionOutputArgs {
        jq: None,
        output_format: Some(OutputFormatArg::Json),
    }
}

fn ctx<'a>(
    client: &'a reqwest::Client,
    config: &'a Config,
    vendor: &'a EdxVendor,
) -> EdxContext<'a> {
    EdxContext::new(client, config, vendor)
}

#[tokio::test]
async fn course_encodes_course_id_path_and_sends_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/discussion/v1/courses/course-v1%3AedX%2BDemoX%2BDemo_Course/",
        ))
        .and(header("authorization", "Bearer edx-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "course-v1:edX+DemoX+Demo_Course",
            "discussions_enabled": true
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = EdxVendor::with_base_url(server.uri());
    let resp = course(
        &ctx(&client, &config, &vendor),
        &EdxDiscussionCourseArgs {
            course_id: "course-v1:edX+DemoX+Demo_Course".into(),
            output: output(),
        },
    )
    .await
    .unwrap();

    assert!(resp.content.contains("discussions_enabled"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn topics_uses_course_topics_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/discussion/v1/course_topics/course-v1%3AedX%2BDemoX%2BDemo_Course/",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "non_courseware_topics": [{"id": "general", "name": "General"}]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = EdxVendor::with_base_url(server.uri());
    let resp = topics(
        &ctx(&client, &config, &vendor),
        &EdxDiscussionTopicsArgs {
            course_id: "course-v1:edX+DemoX+Demo_Course".into(),
            output: output(),
        },
    )
    .await
    .unwrap();

    assert!(resp.content.contains("General"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn threads_builds_filters_and_supports_jq() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/discussion/v1/threads/"))
        .and(query_param("course_id", "course-v1:edX+DemoX+Demo_Course"))
        .and(query_param("text_search", "exam"))
        .and(query_param("view", "unanswered"))
        .and(query_param("order_by", "last_activity_at"))
        .and(query_param("order_direction", "desc"))
        .and(query_param("page_size", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{"id": "thread-1", "title": "Exam question"}]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = EdxVendor::with_base_url(server.uri());
    let resp = threads(
        &ctx(&client, &config, &vendor),
        &EdxDiscussionThreadsArgs {
            course_id: "course-v1:edX+DemoX+Demo_Course".into(),
            topic_id: None,
            following: None,
            view: Some(mcp_server_atlassian::tools::args::EdxDiscussionThreadView::Unanswered),
            text_search: Some("exam".into()),
            order_by: Some(EdxDiscussionThreadOrderBy::LastActivityAt),
            order_direction: Some(EdxDiscussionOrderDirection::Desc),
            page_size: Some(10),
            page: None,
            output: EdxDiscussionOutputArgs {
                jq: Some("results[*].title".into()),
                output_format: Some(OutputFormatArg::Json),
            },
        },
    )
    .await
    .unwrap();

    assert!(resp.content.contains("Exam question"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn create_thread_posts_expected_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/discussion/v1/threads/"))
        .and(header("authorization", "Bearer edx-token"))
        .and(body_json(json!({
            "course_id": "course-v1:edX+DemoX+Demo_Course",
            "topic_id": "general",
            "type": "question",
            "title": "Need help",
            "raw_body": "How does this work?"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "thread-2",
            "title": "Need help"
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = EdxVendor::with_base_url(server.uri());
    let resp = create_thread(
        &ctx(&client, &config, &vendor),
        &EdxDiscussionThreadCreateArgs {
            course_id: "course-v1:edX+DemoX+Demo_Course".into(),
            topic_id: "general".into(),
            thread_type: EdxDiscussionThreadType::Question,
            title: "Need help".into(),
            raw_body: "How does this work?".into(),
            group_id: None,
            output: output(),
        },
    )
    .await
    .unwrap();

    assert!(resp.content.contains("thread-2"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn comments_lists_thread_comments() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/discussion/v1/comments/"))
        .and(query_param("thread_id", "thread-1"))
        .and(query_param("endorsed", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{"id": "comment-1", "raw_body": "Answer"}]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = EdxVendor::with_base_url(server.uri());
    let resp = comments(
        &ctx(&client, &config, &vendor),
        &EdxDiscussionCommentsArgs {
            thread_id: "thread-1".into(),
            endorsed: Some(true),
            page_size: None,
            page: None,
            output: output(),
        },
    )
    .await
    .unwrap();

    assert!(resp.content.contains("comment-1"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn create_comment_requires_thread_or_parent_id() {
    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = EdxVendor::with_base_url("http://127.0.0.1:0");
    let err = create_comment(
        &ctx(&client, &config, &vendor),
        &EdxDiscussionCommentCreateArgs {
            thread_id: None,
            parent_id: None,
            raw_body: "Missing target".into(),
            output: output(),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("threadId"));
}

#[tokio::test]
async fn missing_token_surfaces_auth_missing_at_call_time() {
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = EdxVendor::with_base_url("http://127.0.0.1:0");
    let err = comments(
        &ctx(&client, &config, &vendor),
        &EdxDiscussionCommentsArgs {
            thread_id: "thread-1".into(),
            endorsed: None,
            page_size: None,
            page: None,
            output: output(),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("EDX_ACCESS_TOKEN"));
}
