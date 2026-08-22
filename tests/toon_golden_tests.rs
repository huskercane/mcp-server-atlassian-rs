//! Golden-file tests for TOON output.
//!
//! These lock the exact bytes the MCP client (an LLM) receives, so a future
//! encoder upgrade cannot silently change them. Expected values were generated
//! from the TypeScript reference encoder `@toon-format/toon` 4.1.1 — the same
//! encoder the ported TS servers use — and every case below is annotated with
//! whether our encoder reproduces it.
//!
//! Three cases deliberately diverge from the reference. All three are *extra
//! quoting* or an alternate-but-valid spelling: they carry identical meaning to
//! the model, they cost a couple of characters, and none of them loses
//! information. They are recorded rather than fixed so the gap stays visible,
//! and so a future encoder change that closes it shows up as a test to update.
//!
//! History: the previous encoder (`toon-format` 0.5.0) treated `-` as a
//! structural character and quoted every string containing a hyphen — Jira
//! keys, UUIDs, repo slugs, branch names. Over a 489-case corpus it matched the
//! reference 401 times against the current encoder's 449, with zero cases the
//! old one got right and the current one gets wrong.

// These fire on the *payload fixtures*, not on real constants: `3.14` is a
// float we deliberately encode, and `1e10` is a magnitude the encoder must
// format. Both are test data, so the lints are noise here.
#![allow(clippy::approx_constant, clippy::unreadable_literal)]

use mcp_server_atlassian::format::{OutputFormat, render};
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn toon_golden_array_root() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!([{"k": 1}, {"k": 2}]);
    assert_eq!(render(&input, OutputFormat::Toon), "[2]{k}:\n  1\n  2");
}

#[test]
fn toon_golden_empty_containers() {
    // KNOWN DIVERGENCE from the reference (predates this encoder; not introduced here).
    // reference emits: "o:\na: []\nn: null"
    // Extra quoting / alternate empty-array form: same meaning to the model.
    let input = json!({"o": {}, "a": [], "n": null});
    assert_eq!(render(&input, OutputFormat::Toon), "o:\na[0]:\nn: null");
}

#[test]
fn toon_golden_escapes() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"s": ["tab\there", "nl\nhere", "back\\slash"]});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "s[3]: \"tab\\there\",\"nl\\nhere\",\"back\\\\slash\""
    );
}

#[test]
fn toon_golden_jira_issue_key() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"key": "PROJ-7", "id": "10042"});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "key: PROJ-7\nid: \"10042\""
    );
}

#[test]
fn toon_golden_key_order_preserved() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"zebra": 1, "apple": 2, "middle": 3});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "zebra: 1\napple: 2\nmiddle: 3"
    );
}

#[test]
fn toon_golden_nested_jira_row() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"issues": [{"id": 1, "key": "PROJ-1", "self": "https://x.atlassian.net/rest/api/3/issue/1", "fields": {"summary": "A summary", "status": {"name": "In Progress"}, "labels": ["back-end", "perf"]}}]});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "issues[1]:\n  - id: 1\n    key: PROJ-1\n    self: \"https://x.atlassian.net/rest/api/3/issue/1\"\n    fields:\n      summary: A summary\n      status:\n        name: In Progress\n      labels[2]: back-end,perf"
    );
}

#[test]
fn toon_golden_numbers() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"n": [0, 42, -42, 3.14, 10000000000.0, -0.0015]});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "n[6]: 0,42,-42,3.14,10000000000,-0.0015"
    );
}

#[test]
fn toon_golden_primitive_root() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!("just a string");
    assert_eq!(render(&input, OutputFormat::Toon), "just a string");
}

#[test]
fn toon_golden_quoting_edges() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"s": ["a,b", "has \"q\"", " lead", "trail ", "07", "true", "3.14", "", "a:b", "x[1]", "-x"]});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "s[11]: \"a,b\",\"has \\\"q\\\"\",\" lead\",\"trail \",\"07\",\"true\",\"3.14\",\"\",\"a:b\",\"x[1]\",\"-x\""
    );
}

#[test]
fn toon_golden_ragged_rows() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"v": [{"a": 1}, {"a": 1, "b": 2}, {"b": 2}]});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "v[3]:\n  - a: 1\n  - a: 1\n    b: 2\n  - b: 2"
    );
}

#[test]
fn toon_golden_repo_slug_and_branch() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"slug": "my-repo-name", "ref": "feature/PROJ-7-add-thing"});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "slug: my-repo-name\nref: feature/PROJ-7-add-thing"
    );
}

#[test]
fn toon_golden_tabular_rows() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"issues": [{"key": "PROJ-1", "status": "Open"}, {"key": "PROJ-2", "status": "Done"}]});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "issues[2]{key,status}:\n  PROJ-1,Open\n  PROJ-2,Done"
    );
}

#[test]
fn toon_golden_unicode() {
    // Byte-identical to the `@toon-format/toon` reference.
    let input = json!({"s": ["héllo", "日本語", "emoji 🎯"]});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "s[3]: héllo,日本語,emoji 🎯"
    );
}

#[test]
fn toon_golden_uuid_and_dates() {
    // KNOWN DIVERGENCE from the reference (predates this encoder; not introduced here).
    // reference emits: "id: 550e8400-e29b-41d4-a716-446655440000\ncreated: \"2026-08-22T14:30:00.000-0500\"\nduedate: 2026-08-22"
    // Extra quoting / alternate empty-array form: same meaning to the model.
    let input = json!({"id": "550e8400-e29b-41d4-a716-446655440000", "created": "2026-08-22T14:30:00.000-0500", "duedate": "2026-08-22"});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "id: 550e8400-e29b-41d4-a716-446655440000\ncreated: \"2026-08-22T14:30:00.000-0500\"\nduedate: \"2026-08-22\""
    );
}

#[test]
fn toon_golden_version_and_ip() {
    // KNOWN DIVERGENCE from the reference (predates this encoder; not introduced here).
    // reference emits: "version: 1.2.3\nhost: 10.0.0.1"
    // Extra quoting / alternate empty-array form: same meaning to the model.
    let input = json!({"version": "1.2.3", "host": "10.0.0.1"});
    assert_eq!(
        render(&input, OutputFormat::Toon),
        "version: \"1.2.3\"\nhost: \"10.0.0.1\""
    );
}

// --- structural guarantees -------------------------------------------------

#[test]
fn object_key_order_is_preserved_not_sorted() {
    // `serde_json/preserve_order` must stay enabled: the encoder crate pulls it
    // in, and JS objects preserve insertion order, so sorted keys would diverge
    // from the TS servers on every response. If this fails, a dependency change
    // dropped the feature.
    let input = json!({"zebra": 1, "apple": 2, "middle": 3});
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        r#"{"zebra":1,"apple":2,"middle":3}"#,
        "serde_json must preserve insertion order, not sort keys"
    );
}

#[test]
fn hyphenated_identifiers_are_not_quoted() {
    // Regression lock for the encoder swap: the previous crate quoted every
    // string containing a hyphen, which is most Atlassian identifiers.
    let input = json!({
        "key": "PROJ-7",
        "uuid": "550e8400-e29b-41d4-a716-446655440000",
        "slug": "my-repo-name",
        "branch": "feature/PROJ-7-add-thing"
    });
    let out = render(&input, OutputFormat::Toon);
    assert!(
        !out.contains('"'),
        "no identifier should be quoted, got: {out}"
    );
}

#[test]
fn toon_and_json_formats_stay_distinct() {
    let input = json!({"a": 1});
    assert_ne!(
        render(&input, OutputFormat::Toon),
        render(&input, OutputFormat::Json)
    );
}
