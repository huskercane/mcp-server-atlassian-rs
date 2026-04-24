//! Tool-schema sanity: round-trip the `AtlassianServer`'s advertised info,
//! and verify the `args` types serialise with camelCase keys (TS parity).

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::tools::AtlassianServer;
use mcp_server_atlassian::tools::args::{
    OutputFormatArg, QueryParams, ReadArgs, WriteArgs,
};
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::bitbucket::BitbucketVendor;
use mcp_server_atlassian::vendor::jira::JiraVendor;
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
    assert_eq!(args.query_params.as_ref().unwrap().get("pagelen").unwrap(), "25");
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
fn query_params_preserve_ordering() {
    let mut qp = QueryParams::new();
    qp.insert("a".into(), "1".into());
    qp.insert("b".into(), "2".into());
    qp.insert("c".into(), "3".into());
    let s = serde_json::to_string(&qp).unwrap();
    // BTreeMap → alphabetical order, reliable for URL encoding and fixtures.
    assert_eq!(s, r#"{"a":"1","b":"2","c":"3"}"#);
}
