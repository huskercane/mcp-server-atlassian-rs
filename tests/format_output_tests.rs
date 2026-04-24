use mcp_server_atlassian::format::{OutputFormat, render, to_pretty_json};
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn parse_defaults_to_toon() {
    assert_eq!(OutputFormat::parse(None), OutputFormat::Toon);
    assert_eq!(OutputFormat::parse(Some("")), OutputFormat::Toon);
    assert_eq!(OutputFormat::parse(Some("toon")), OutputFormat::Toon);
    assert_eq!(OutputFormat::parse(Some("TOON")), OutputFormat::Toon);
    assert_eq!(OutputFormat::parse(Some("unknown")), OutputFormat::Toon);
}

#[test]
fn parse_recognizes_json() {
    assert_eq!(OutputFormat::parse(Some("json")), OutputFormat::Json);
    assert_eq!(OutputFormat::parse(Some("JSON")), OutputFormat::Json);
    assert_eq!(OutputFormat::parse(Some(" json ")), OutputFormat::Json);
}

#[test]
fn json_output_is_pretty_printed() {
    let data = json!({"name":"Alice","age":30});
    let out = render(&data, OutputFormat::Json);
    assert_eq!(out, to_pretty_json(&data));
    assert!(out.contains("  \"name\": \"Alice\""));
}

#[test]
fn toon_output_contains_data_values() {
    // TS test contract: output must contain the values regardless of the
    // chosen format. We don't assert exact TOON bytes because the encoder is
    // owned by the toon-format crate; a future upgrade could tweak syntax.
    let data = json!({
        "users": [
            {"id": 1, "name": "Alice", "role": "admin"},
            {"id": 2, "name": "Bob", "role": "user"}
        ]
    });
    let out = render(&data, OutputFormat::Toon);
    assert!(out.contains("Alice"));
    assert!(out.contains("Bob"));
    assert!(out.contains("admin"));
}

#[test]
fn toon_handles_primitive_and_empty() {
    // TOON is tolerant of primitives and empty containers (empty objects/arrays
    // legitimately render to an empty string). We just assert the encoder
    // doesn't panic and that populated values round-trip.
    assert!(render(&json!("hello"), OutputFormat::Toon).contains("hello"));
    assert!(render(&json!(42), OutputFormat::Toon).contains("42"));
    assert!(render(&json!(true), OutputFormat::Toon).contains("true"));
    let _ = render(&json!({}), OutputFormat::Toon);
    let _ = render(&json!([]), OutputFormat::Toon);
}
