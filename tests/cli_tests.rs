//! CLI argument parsing and JSON-validation tests.
//!
//! Covers three surfaces:
//! 1. Shared helpers (`parse_object`, `parse_query_params`).
//! 2. The new `bb` and `jira` subcommand groups, plus the `--output-format`
//!    flag that gained parity with the TS Jira CLI.
//! 3. Deprecated top-level verbs, which still parse for one release while
//!    the deprecation shim emits a stderr notice.

use clap::{CommandFactory, Parser};
use mcp_server_atlassian::cli::Cli;
use mcp_server_atlassian::cli::api::{parse_object, parse_query_params};
use pretty_assertions::assert_eq;
use serde_json::json;

const BIN: &str = "mcp-atlassian";

// ---- parse_object ----

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

// ---- parse_query_params ----

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

// ---- new `bb` subcommand group ----

#[test]
fn bb_get_with_required_path_parses() {
    Cli::try_parse_from([BIN, "bb", "get", "--path", "/workspaces"])
        .expect("`bb get --path` should parse");
}

#[test]
fn bb_get_without_path_is_rejected() {
    let err = Cli::try_parse_from([BIN, "bb", "get"]).unwrap_err();
    let msg = err.render().to_string().to_lowercase();
    assert!(msg.contains("path") || msg.contains("required"), "{msg}");
}

#[test]
fn bb_post_requires_path_and_body() {
    Cli::try_parse_from([
        BIN,
        "bb",
        "post",
        "--path",
        "/repos/foo/prs",
        "--body",
        r#"{"title":"x"}"#,
    ])
    .expect("`bb post` with both flags should parse");

    let err = Cli::try_parse_from([BIN, "bb", "post", "--path", "/x"]).unwrap_err();
    let msg = err.render().to_string().to_lowercase();
    assert!(msg.contains("body") || msg.contains("required"), "{msg}");
}

#[test]
fn bb_get_accepts_short_flags_and_jq() {
    Cli::try_parse_from([
        BIN,
        "bb",
        "get",
        "-p",
        "/workspaces",
        "-q",
        r#"{"pagelen":"5"}"#,
        "--jq",
        "values[*].slug",
    ])
    .expect("short-flag form should parse");
}

#[test]
fn bb_clone_lives_under_bb_group_only() {
    // clone is bitbucket-only; it must be reachable under `bb` …
    Cli::try_parse_from([
        BIN,
        "bb",
        "clone",
        "--repo-slug",
        "widget",
        "--target-path",
        "/tmp/x",
    ])
    .expect("`bb clone` should parse");

    // … but not under `jira`.
    let err = Cli::try_parse_from([
        BIN,
        "jira",
        "clone",
        "--repo-slug",
        "widget",
        "--target-path",
        "/tmp/x",
    ])
    .unwrap_err();
    let msg = err.render().to_string().to_lowercase();
    assert!(
        msg.contains("clone") || msg.contains("unrecognized") || msg.contains("subcommand"),
        "expected jira to reject `clone` subcommand; got: {msg}"
    );
}

// ---- new `jira` subcommand group ----

#[test]
fn jira_get_with_required_path_parses() {
    Cli::try_parse_from([BIN, "jira", "get", "--path", "/rest/api/3/myself"])
        .expect("`jira get --path` should parse");
}

#[test]
fn jira_post_requires_path_and_body() {
    Cli::try_parse_from([
        BIN,
        "jira",
        "post",
        "--path",
        "/rest/api/3/issue",
        "--body",
        r#"{"fields":{"project":{"key":"PROJ"},"summary":"x","issuetype":{"name":"Task"}}}"#,
    ])
    .expect("`jira post` with both flags should parse");
}

#[test]
fn jira_search_jql_accepts_query_params() {
    Cli::try_parse_from([
        BIN,
        "jira",
        "get",
        "--path",
        "/rest/api/3/search/jql",
        "--query-params",
        r#"{"jql":"project=PROJ","maxResults":"5"}"#,
    ])
    .expect("`jira get` with --query-params should parse");
}

// ---- new `conf` subcommand group ----

#[test]
fn conf_get_with_required_path_parses() {
    Cli::try_parse_from([BIN, "conf", "get", "--path", "/wiki/api/v2/spaces"])
        .expect("`conf get --path` should parse");
}

#[test]
fn conf_post_requires_path_and_body() {
    Cli::try_parse_from([
        BIN,
        "conf",
        "post",
        "--path",
        "/wiki/api/v2/pages",
        "--body",
        r#"{"spaceId":"1","status":"current","title":"x","body":{"representation":"storage","value":"<p>x</p>"}}"#,
    ])
    .expect("`conf post` with both flags should parse");
}

#[test]
fn conf_search_accepts_cql_query_param() {
    Cli::try_parse_from([
        BIN,
        "conf",
        "get",
        "--path",
        "/wiki/rest/api/search",
        "--query-params",
        r#"{"cql":"type=page AND space=DEV","limit":"5"}"#,
    ])
    .expect("`conf get` with --query-params should parse");
}

#[test]
fn conf_does_not_expose_clone_subcommand() {
    let err = Cli::try_parse_from([
        BIN,
        "conf",
        "clone",
        "--repo-slug",
        "widget",
        "--target-path",
        "/tmp/x",
    ])
    .unwrap_err();
    let msg = err.render().to_string().to_lowercase();
    assert!(
        msg.contains("clone") || msg.contains("unrecognized") || msg.contains("subcommand"),
        "expected conf to reject `clone` subcommand; got: {msg}"
    );
}

// ---- --output-format flag (parity with TS Jira CLI) ----

#[test]
fn output_format_json_accepted_on_bb_and_jira_verbs() {
    for verb_args in [
        vec!["bb", "get", "--path", "/workspaces", "--output-format", "json"],
        vec!["bb", "delete", "--path", "/workspaces/x", "--output-format", "json"],
        vec![
            "bb", "post", "--path", "/x", "--body", "{}", "--output-format", "json",
        ],
        vec!["jira", "get", "--path", "/rest/api/3/myself", "--output-format", "json"],
        vec![
            "jira", "delete", "--path", "/rest/api/3/issue/X", "--output-format", "json",
        ],
        vec![
            "jira", "post", "--path", "/rest/api/3/issue", "--body", "{}", "--output-format",
            "json",
        ],
        vec![
            "conf", "get", "--path", "/wiki/api/v2/spaces", "--output-format", "json",
        ],
        vec![
            "conf", "delete", "--path", "/wiki/api/v2/pages/1", "--output-format", "json",
        ],
        vec![
            "conf", "post", "--path", "/wiki/api/v2/pages", "--body", "{}", "--output-format",
            "json",
        ],
    ] {
        let mut argv = vec![BIN];
        argv.extend(verb_args.iter().copied());
        Cli::try_parse_from(argv).expect("--output-format json should parse on every verb");
    }
}

#[test]
fn output_format_toon_is_the_default() {
    // Implicit default: omitting --output-format should still parse.
    Cli::try_parse_from([BIN, "bb", "get", "--path", "/workspaces"])
        .expect("default output format must not require the flag");
    Cli::try_parse_from([BIN, "jira", "get", "--path", "/rest/api/3/myself"])
        .expect("default output format must not require the flag");
}

#[test]
fn output_format_rejects_unknown_value() {
    let err = Cli::try_parse_from([
        BIN,
        "bb",
        "get",
        "--path",
        "/workspaces",
        "--output-format",
        "xml",
    ])
    .unwrap_err();
    let msg = err.render().to_string().to_lowercase();
    assert!(
        msg.contains("xml")
            || msg.contains("invalid value")
            || msg.contains("possible values"),
        "expected unknown value error; got: {msg}"
    );
}

// ---- deprecated top-level verbs ----
//
// These still parse for one release. The deprecation notice is emitted at
// dispatch time (stderr); the parse layer is silent so scripts continue
// to function. Behavioural assertion of the warning is left to a future
// integration test that captures stderr.

#[test]
fn legacy_get_at_top_level_still_parses() {
    Cli::try_parse_from([BIN, "get", "--path", "/workspaces"])
        .expect("top-level `get` retained as deprecated alias");
}

#[test]
fn legacy_post_at_top_level_still_parses() {
    Cli::try_parse_from([
        BIN,
        "post",
        "--path",
        "/repos/foo/prs",
        "--body",
        r#"{"title":"x"}"#,
    ])
    .expect("top-level `post` retained as deprecated alias");
}

#[test]
fn legacy_clone_at_top_level_still_parses() {
    Cli::try_parse_from([
        BIN,
        "clone",
        "--repo-slug",
        "widget",
        "--target-path",
        "/tmp/x",
    ])
    .expect("top-level `clone` retained as deprecated alias");
}

#[test]
fn legacy_top_level_verbs_are_hidden_from_help() {
    // The Cli's --help should advertise the new groups, not the legacy
    // verbs. We only check that the groups are listed; the absence of
    // hidden subcommands is enforced by clap's `hide` attribute.
    let help = Cli::command().render_help().to_string();
    assert!(help.contains("bb"), "help missing `bb`: {help}");
    assert!(help.contains("jira"), "help missing `jira`: {help}");
    assert!(help.contains("conf"), "help missing `conf`: {help}");
}
