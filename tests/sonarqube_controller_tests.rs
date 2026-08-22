#![allow(clippy::doc_markdown)]

//! Controller-pipeline tests for the SonarQube vendor. Exercises the full path a
//! `sonarqube_*` tool takes: read the static `SONARQUBE_TOKEN` from config →
//! dispatch through the shared transport with an `Authorization: Bearer <token>`
//! header → classify Sonar's `errors: [{msg}]` envelope. Also covers the
//! headline feature: resolving a scanner `ceTaskId` to an `analysisId` before
//! reporting the quality gate — the bridge from a CircleCI log to "why did Sonar
//! fail".
//!
//! Sonar's Web API is stood up on a wiremock instance, so these tests need no
//! network and no global state — the base-URL override is what makes that
//! possible.

use std::collections::HashMap;

use mcp_server_devtools::config::Config;
use mcp_server_devtools::controllers::sonarqube::{SonarqubeContext, quality_gate, search_issues};
use mcp_server_devtools::error::ErrorKind;
use mcp_server_devtools::tools::args::{
    OutputFormatArg, SonarqubeQualityGateArgs, SonarqubeSearchIssuesArgs,
};
use mcp_server_devtools::transport::build_client;
use mcp_server_devtools::vendor::sonarqube::SonarqubeVendor;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("SONARQUBE_TOKEN".into(), "squ_tok-123".into());
    m
}

fn vendor(server: &MockServer) -> SonarqubeVendor {
    SonarqubeVendor::with_base_url(server.uri())
}

fn gate_args() -> SonarqubeQualityGateArgs {
    SonarqubeQualityGateArgs {
        output_format: Some(OutputFormatArg::Json),
        ..Default::default()
    }
}

fn issues_args(component_keys: &str) -> SonarqubeSearchIssuesArgs {
    SonarqubeSearchIssuesArgs {
        component_keys: component_keys.to_string(),
        branch: None,
        pull_request: None,
        types: None,
        severities: None,
        statuses: None,
        resolved: None,
        page_size: None,
        organization: None,
        jq: None,
        output_format: Some(OutputFormatArg::Json),
    }
}

#[tokio::test]
async fn quality_gate_by_project_and_pr_sends_bearer_and_params() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/qualitygates/project_status"))
        .and(header("authorization", "Bearer squ_tok-123"))
        .and(query_param("projectKey", "my-org_my-repo"))
        .and(query_param("pullRequest", "42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projectStatus": {
                "status": "ERROR",
                "conditions": [
                    {
                        "status": "ERROR",
                        "metricKey": "new_coverage",
                        "comparator": "LT",
                        "errorThreshold": "80",
                        "actualValue": "63.4"
                    }
                ]
            }
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let mut args = gate_args();
    args.project_key = Some("my-org_my-repo".into());
    args.pull_request = Some("42".into());

    let resp = quality_gate(&ctx, &args).await.unwrap();
    assert!(resp.content.contains("new_coverage"));
    assert!(resp.content.contains("63.4"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn quality_gate_by_ce_task_id_resolves_analysis_id_first() {
    let server = MockServer::start().await;

    // Step 1: ceTaskId → analysisId.
    Mock::given(method("GET"))
        .and(path("/api/ce/task"))
        .and(header("authorization", "Bearer squ_tok-123"))
        .and(query_param("id", "AXm-task-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task": {"id": "AXm-task-1", "status": "SUCCESS", "analysisId": "AYn-analysis-9"}
        })))
        .mount(&server)
        .await;

    // Step 2: quality gate keyed by the resolved analysisId.
    Mock::given(method("GET"))
        .and(path("/api/qualitygates/project_status"))
        .and(query_param("analysisId", "AYn-analysis-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projectStatus": {"status": "ERROR", "conditions": []}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let mut args = gate_args();
    args.ce_task_id = Some("AXm-task-1".into());

    let resp = quality_gate(&ctx, &args).await.unwrap();
    assert!(resp.content.contains("ERROR"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn quality_gate_ce_task_without_analysis_id_surfaces_status() {
    let server = MockServer::start().await;

    // Task still running — no analysisId yet.
    Mock::given(method("GET"))
        .and(path("/api/ce/task"))
        .and(query_param("id", "AXm-pending"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task": {"id": "AXm-pending", "status": "IN_PROGRESS"}
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let mut args = gate_args();
    args.ce_task_id = Some("AXm-pending".into());

    let err = quality_gate(&ctx, &args).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("IN_PROGRESS"));
}

#[tokio::test]
async fn quality_gate_requires_a_selector() {
    // No projectKey / ceTaskId / analysisId → a validation error before any network.
    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = SonarqubeVendor::with_base_url("http://127.0.0.1:0");
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let err = quality_gate(&ctx, &gate_args()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("projectKey"));
}

#[tokio::test]
async fn quality_gate_rejects_branch_and_pr_together() {
    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = SonarqubeVendor::with_base_url("http://127.0.0.1:0");
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let mut args = gate_args();
    args.project_key = Some("p".into());
    args.branch = Some("main".into());
    args.pull_request = Some("42".into());

    let err = quality_gate(&ctx, &args).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert!(err.message.contains("not both"));
}

#[tokio::test]
async fn search_issues_sends_bearer_and_scoped_params() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/issues/search"))
        .and(header("authorization", "Bearer squ_tok-123"))
        .and(query_param("componentKeys", "my-org_my-repo"))
        .and(query_param("pullRequest", "42"))
        .and(query_param("types", "BUG,VULNERABILITY"))
        .and(query_param("resolved", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [
                {
                    "rule": "java:S2095",
                    "severity": "BLOCKER",
                    "type": "BUG",
                    "message": "Use try-with-resources or close this resource",
                    "component": "my-org_my-repo:src/Foo.java",
                    "line": 41
                }
            ]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let mut args = issues_args("my-org_my-repo");
    args.pull_request = Some("42".into());
    args.types = Some("BUG,VULNERABILITY".into());
    args.resolved = Some(false);
    args.jq = Some("issues[*].{rule: rule, line: line}".into());

    let resp = search_issues(&ctx, &args).await.unwrap();
    assert!(resp.content.contains("java:S2095"));
    if let Some(p) = resp.raw_response_path {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn sonar_error_envelope_is_surfaced() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/issues/search"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "errors": [{"msg": "Component key 'nope' not found"}]
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = vendor(&server);
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let err = search_issues(&ctx, &issues_args("nope")).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ApiError);
    assert_eq!(err.status_code, Some(404));
    assert!(err.message.contains("Component key 'nope' not found"));
}

#[tokio::test]
async fn missing_token_surfaces_auth_missing_at_call_time() {
    // A deployment without Sonar configured must not crash; the error appears
    // only when a `sonarqube_*` tool is actually invoked, before any network call.
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = SonarqubeVendor::with_base_url("http://127.0.0.1:0");
    let ctx = SonarqubeContext::new(&client, &config, &vendor);

    let mut args = gate_args();
    args.project_key = Some("p".into());

    let err = quality_gate(&ctx, &args).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthMissing);
    assert!(err.message.contains("SONARQUBE_TOKEN"));
}
