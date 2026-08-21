//! Tool-schema sanity: round-trip the `AtlassianServer`'s advertised info,
//! and verify the `args` types serialise with camelCase keys (TS parity).

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::tools::AtlassianServer;
use mcp_server_atlassian::tools::args::{
    ArtifactReadArgs, CircleCiLogsArgs, OutputFormatArg, QueryParams, ReadArgs,
    SonarqubeQualityGateArgs, SonarqubeSearchIssuesArgs, WriteArgs,
};

#[test]
fn artifact_read_args_support_resume_offsets() {
    let args: ArtifactReadArgs = serde_json::from_value(json!({
        "artifactId": "artifact-123",
        "offset": 65536,
        "maxBytes": 32768
    }))
    .unwrap();
    assert_eq!(args.artifact_id, "artifact-123");
    assert_eq!(args.offset, 65_536);
    assert_eq!(args.max_bytes, Some(32_768));
}
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::bitbucket::BitbucketVendor;
use mcp_server_atlassian::vendor::circleci::CircleCiVendor;
use mcp_server_atlassian::vendor::confluence::ConfluenceVendor;
use mcp_server_atlassian::vendor::edx::EdxVendor;
use mcp_server_atlassian::vendor::grafana::GrafanaVendor;
use mcp_server_atlassian::vendor::jira::JiraVendor;
use mcp_server_atlassian::vendor::newrelic::NewRelicVendor;
use mcp_server_atlassian::vendor::ninjaone::NinjaOneVendor;
use mcp_server_atlassian::vendor::postman::PostmanVendor;
use mcp_server_atlassian::vendor::slack::SlackVendor;
use mcp_server_atlassian::vendor::sonarqube::SonarqubeVendor;
use mcp_server_atlassian::vendor::splunk::SplunkVendor;
use mcp_server_atlassian::vendor::zoom::ZoomVendor;
use rmcp::ServerHandler;
use serde_json::json;
use std::collections::HashMap;

#[test]
fn server_info_reports_expected_identity() {
    let server = AtlassianServer::with_components(
        Config::from_map(HashMap::new()),
        build_client().unwrap(),
        BitbucketVendor::new(),
        JiraVendor::new(),
        ConfluenceVendor::new(),
        ZoomVendor::new(),
        CircleCiVendor::new(),
        SlackVendor::new(),
        PostmanVendor::new(),
        EdxVendor::new(),
        NewRelicVendor::new(),
        GrafanaVendor::new(),
        SonarqubeVendor::new(),
        SplunkVendor::new(),
        NinjaOneVendor::new(),
    );
    let info = server.get_info();
    assert_eq!(
        info.server_info.name,
        mcp_server_atlassian::constants::PACKAGE_NAME
    );
    assert_eq!(
        info.server_info.version,
        mcp_server_atlassian::constants::VERSION
    );
    assert!(info.capabilities.tools.is_some());
}

#[test]
fn read_args_uses_camel_case_json() {
    let args: ReadArgs = serde_json::from_value(json!({
        "path": "/workspaces",
        "queryParams": {"pagelen": "25"},
        "jq": "values[*].slug",
        "outputFormat": "json"
    }))
    .unwrap();
    assert_eq!(args.path, "/workspaces");
    assert_eq!(
        args.query_params.as_ref().unwrap().get("pagelen").unwrap(),
        "25"
    );
    assert_eq!(args.jq.as_deref(), Some("values[*].slug"));
    assert_eq!(args.output_format, Some(OutputFormatArg::Json));
}

#[test]
fn write_args_uses_camel_case_json() {
    let args: WriteArgs = serde_json::from_value(json!({
        "path": "/repositories/foo/prs",
        "body": {"title": "new"},
        "queryParams": {"pagelen": "5"},
        "outputFormat": "toon"
    }))
    .unwrap();
    assert_eq!(args.body, json!({"title": "new"}));
    assert_eq!(args.output_format, Some(OutputFormatArg::Toon));
}

#[test]
fn circleci_logs_args_use_camel_case_json() {
    let args: CircleCiLogsArgs = serde_json::from_value(json!({
        "projectSlug": "gh/acme/web",
        "jobNumber": 123,
        "stepNumber": 2,
        "failedOnly": true,
        "condensed": true,
        "contextLines": 5,
        "outputFormat": "json"
    }))
    .unwrap();
    assert_eq!(args.project_slug, "gh/acme/web");
    assert_eq!(args.job_number, 123);
    assert_eq!(args.step_number, Some(2));
    assert!(args.failed_only);
    assert!(args.condensed);
    assert_eq!(args.context_lines, Some(5));
    assert_eq!(args.output_format, Some(OutputFormatArg::Json));
}

#[test]
fn sonarqube_quality_gate_args_use_camel_case_json() {
    let args: SonarqubeQualityGateArgs = serde_json::from_value(json!({
        "projectKey": "my-org_my-repo",
        "pullRequest": "42",
        "ceTaskId": "AbCd-1234",
        "outputFormat": "json"
    }))
    .unwrap();
    assert_eq!(args.project_key.as_deref(), Some("my-org_my-repo"));
    assert_eq!(args.pull_request.as_deref(), Some("42"));
    assert_eq!(args.ce_task_id.as_deref(), Some("AbCd-1234"));
    assert_eq!(args.output_format, Some(OutputFormatArg::Json));
}

#[test]
fn sonarqube_search_issues_args_use_camel_case_json() {
    let args: SonarqubeSearchIssuesArgs = serde_json::from_value(json!({
        "componentKeys": "my-org_my-repo",
        "pullRequest": "42",
        "types": "BUG,VULNERABILITY",
        "resolved": false,
        "pageSize": 25,
        "outputFormat": "json"
    }))
    .unwrap();
    assert_eq!(args.component_keys, "my-org_my-repo");
    assert_eq!(args.pull_request.as_deref(), Some("42"));
    assert_eq!(args.types.as_deref(), Some("BUG,VULNERABILITY"));
    assert_eq!(args.resolved, Some(false));
    assert_eq!(args.page_size, Some(25));
}

#[test]
fn query_params_preserve_ordering() {
    let mut qp = QueryParams::new();
    qp.insert("a".into(), "1".into());
    qp.insert("b".into(), "2".into());
    qp.insert("c".into(), "3".into());
    let s = serde_json::to_string(&qp).unwrap();
    // BTreeMap → alphabetical order, reliable for URL encoding and fixtures.
    assert_eq!(s, r#"{"a":"1","b":"2","c":"3"}"#);
}
