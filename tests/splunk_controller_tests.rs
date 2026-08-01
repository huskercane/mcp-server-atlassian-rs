use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::splunk::{
    SplunkContext, create_job, job_results, list_saved_searches, search,
};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::tools::args::{
    OutputFormatArg, SplunkCreateJobArgs, SplunkJobResultsArgs, SplunkListSavedSearchesArgs,
    SplunkSearchArgs,
};
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::splunk::SplunkVendor;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config() -> Config {
    Config::from_map(HashMap::from([(
        "SPLUNK_TOKEN".to_owned(),
        "jwt-token".to_owned(),
    )]))
}

fn json_output() -> OutputFormatArg {
    OutputFormatArg::Json
}

#[tokio::test]
async fn export_search_posts_urlencoded_form_with_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/search/v2/jobs/export"))
        .and(header("authorization", "Bearer jwt-token"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("search=search+index%3Dmain"))
        .and(body_string_contains("earliest_time=-15m"))
        .and(body_string_contains("latest_time=now"))
        .and(body_string_contains("output_mode=json_rows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "fields": ["host", "count"],
            "rows": [["api-1", 4]]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config();
    let vendor = SplunkVendor::with_base_url(server.uri());
    let ctx = SplunkContext::new(&client, &config, &vendor);
    let args = SplunkSearchArgs {
        search: "search index=main".into(),
        earliest_time: Some("-15m".into()),
        latest_time: Some("now".into()),
        max_time: None,
        jq: Some("rows".into()),
        output_format: Some(json_output()),
    };

    let response = search(&ctx, &args).await.unwrap();
    assert!(response.content.contains("api-1"));
    cleanup(response.raw_response_path);
}

#[tokio::test]
async fn create_job_returns_sid_and_form_options() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/search/jobs"))
        .and(body_string_contains("max_count=500"))
        .and(body_string_contains("max_time=30"))
        .and(body_string_contains("output_mode=json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sid":"171234.42"})))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config();
    let vendor = SplunkVendor::with_base_url(server.uri());
    let ctx = SplunkContext::new(&client, &config, &vendor);
    let args = SplunkCreateJobArgs {
        search: "search index=main | stats count".into(),
        earliest_time: Some("-1h".into()),
        latest_time: Some("now".into()),
        max_count: Some(500),
        max_time: Some(30),
        jq: Some("sid".into()),
        output_format: Some(json_output()),
    };

    let response = create_job(&ctx, &args).await.unwrap();
    assert!(response.content.contains("171234.42"));
    cleanup(response.raw_response_path);
}

#[tokio::test]
async fn job_results_uses_v2_endpoint_and_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/search/v2/jobs/171234.42/results"))
        .and(query_param("output_mode", "json_rows"))
        .and(query_param("count", "25"))
        .and(query_param("offset", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "fields": ["host"],
            "rows": [["api-2"]]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config();
    let vendor = SplunkVendor::with_base_url(server.uri());
    let ctx = SplunkContext::new(&client, &config, &vendor);
    let args = SplunkJobResultsArgs {
        sid: "171234.42".into(),
        count: Some(25),
        offset: Some(50),
        jq: None,
        output_format: Some(json_output()),
    };

    let response = job_results(&ctx, &args).await.unwrap();
    assert!(response.content.contains("api-2"));
    cleanup(response.raw_response_path);
}

#[tokio::test]
async fn saved_searches_support_filter_and_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/services/saved/searches"))
        .and(query_param("output_mode", "json"))
        .and(query_param("search", "name=\"Errors\""))
        .and(query_param("count", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "entry": [{"name":"Errors","content":{"search":"index=main error"}}]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config();
    let vendor = SplunkVendor::with_base_url(server.uri());
    let ctx = SplunkContext::new(&client, &config, &vendor);
    let args = SplunkListSavedSearchesArgs {
        search: Some("name=\"Errors\"".into()),
        count: Some(10),
        offset: None,
        jq: Some("entry[*].name".into()),
        output_format: Some(json_output()),
    };

    let response = list_saved_searches(&ctx, &args).await.unwrap();
    assert!(response.content.contains("Errors"));
    cleanup(response.raw_response_path);
}

#[tokio::test]
async fn invalid_sid_is_rejected_before_dispatch() {
    let client = build_client().unwrap();
    let config = config();
    let vendor = SplunkVendor::with_base_url("http://127.0.0.1:0");
    let ctx = SplunkContext::new(&client, &config, &vendor);
    let args = SplunkJobResultsArgs {
        sid: "../server/info".into(),
        count: None,
        offset: None,
        jq: None,
        output_format: None,
    };

    let error = job_results(&ctx, &args).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::ApiError);
    assert_eq!(error.status_code, Some(400));
}

fn cleanup(path: Option<std::path::PathBuf>) {
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}
