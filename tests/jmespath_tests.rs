use mcp_server_atlassian::format::jmespath::apply_jq_filter;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn missing_filter_returns_original() {
    let data = json!({"a": 1, "b": 2});
    let out = apply_jq_filter(&data, None);
    assert_eq!(out, data);
}

#[test]
fn empty_filter_returns_original() {
    let data = json!([1, 2, 3]);
    let out = apply_jq_filter(&data, Some("   "));
    assert_eq!(out, data);
}

#[test]
fn simple_field_projection() {
    let data = json!({"name": "Alice", "age": 30});
    let out = apply_jq_filter(&data, Some("name"));
    assert_eq!(out, json!("Alice"));
}

#[test]
fn nested_field_projection() {
    let data = json!({"links": {"html": {"href": "https://example.com"}}});
    let out = apply_jq_filter(&data, Some("links.html.href"));
    assert_eq!(out, json!("https://example.com"));
}

#[test]
fn array_wildcard_projection() {
    let data = json!({
        "values": [
            {"name": "a", "slug": "a-slug"},
            {"name": "b", "slug": "b-slug"}
        ]
    });
    let out = apply_jq_filter(&data, Some("values[*].name"));
    assert_eq!(out, json!(["a", "b"]));
}

#[test]
fn invalid_filter_returns_error_envelope() {
    let data = json!({"name": "Alice"});
    let out = apply_jq_filter(&data, Some("!!! not a jmespath !!!"));
    let obj = out.as_object().expect("error envelope");
    assert!(
        obj.get("_jqError")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("Invalid JMESPath expression:")),
        "error envelope shape"
    );
    assert_eq!(obj.get("_originalData"), Some(&data));
}
