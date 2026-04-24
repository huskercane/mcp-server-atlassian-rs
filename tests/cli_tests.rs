//! CLI argument parsing and JSON-validation tests.

use mcp_server_atlassian_bitbucket::cli::api::{parse_object, parse_query_params};
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn parse_object_accepts_json_objects() {
    let v = parse_object(r#"{"a":1,"b":"two"}"#, "body").unwrap();
    assert_eq!(v, json!({"a":1,"b":"two"}));
}

#[test]
fn parse_object_rejects_arrays_and_primitives() {
    for (bad, kind) in [
        ("[]", "array"),
        ("null", "null"),
        ("42", "number"),
        ("true", "boolean"),
        ("\"str\"", "string"),
    ] {
        let err = parse_object(bad, "body").unwrap_err();
        assert!(
            err.message.contains(kind),
            "expected '{kind}' in error message but got: {}",
            err.message
        );
    }
}

#[test]
fn parse_object_rejects_malformed_json() {
    let err = parse_object("not json", "body").unwrap_err();
    assert!(err.message.contains("Invalid JSON"));
}

#[test]
fn parse_query_params_none_stays_none() {
    assert!(parse_query_params(None).unwrap().is_none());
}

#[test]
fn parse_query_params_extracts_string_values() {
    let qp = parse_query_params(Some(r#"{"pagelen":"25","page":"2"}"#))
        .unwrap()
        .unwrap();
    assert_eq!(qp.get("pagelen").map(String::as_str), Some("25"));
    assert_eq!(qp.get("page").map(String::as_str), Some("2"));
}

#[test]
fn parse_query_params_coerces_numbers_and_bools() {
    let qp = parse_query_params(Some(r#"{"pagelen":25,"flag":true}"#))
        .unwrap()
        .unwrap();
    assert_eq!(qp.get("pagelen").map(String::as_str), Some("25"));
    assert_eq!(qp.get("flag").map(String::as_str), Some("true"));
}

#[test]
fn parse_query_params_rejects_nested_objects() {
    let err = parse_query_params(Some(r#"{"inner":{"x":1}}"#)).unwrap_err();
    assert!(
        err.message.contains("must be a string"),
        "got: {}",
        err.message
    );
}

#[test]
fn cli_parse_accepts_get_with_required_path() {
    use clap::Parser;
    let _ = mcp_server_atlassian_bitbucket::cli::Cli::try_parse_from([
        "mcp-atlassian-bitbucket",
        "get",
        "--path",
        "/workspaces",
    ])
    .expect("get with --path parses");
}

#[test]
fn cli_parse_rejects_get_without_path() {
    use clap::Parser;
    let err = mcp_server_atlassian_bitbucket::cli::Cli::try_parse_from([
        "mcp-atlassian-bitbucket",
        "get",
    ])
    .unwrap_err();
    let msg = err.render().to_string().to_lowercase();
    assert!(msg.contains("path") || msg.contains("required"), "{msg}");
}

#[test]
fn cli_parse_accepts_post_with_path_and_body() {
    use clap::Parser;
    let _ = mcp_server_atlassian_bitbucket::cli::Cli::try_parse_from([
        "mcp-atlassian-bitbucket",
        "post",
        "--path",
        "/repos/foo/prs",
        "--body",
        r#"{"title":"x"}"#,
    ])
    .expect("post with --path+body parses");
}

#[test]
fn cli_parse_rejects_post_without_body() {
    use clap::Parser;
    let err = mcp_server_atlassian_bitbucket::cli::Cli::try_parse_from([
        "mcp-atlassian-bitbucket",
        "post",
        "--path",
        "/x",
    ])
    .unwrap_err();
    let msg = err.render().to_string().to_lowercase();
    assert!(msg.contains("body") || msg.contains("required"), "{msg}");
}

#[test]
fn cli_parse_accepts_short_flags() {
    use clap::Parser;
    let _ = mcp_server_atlassian_bitbucket::cli::Cli::try_parse_from([
        "mcp-atlassian-bitbucket",
        "post",
        "-p",
        "/x",
        "-b",
        r#"{"a":1}"#,
        "-q",
        r#"{"pagelen":"5"}"#,
        "--jq",
        "values[*].name",
    ])
    .expect("short-flag form parses");
}
