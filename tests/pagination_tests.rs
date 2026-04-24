use mcp_server_atlassian::pagination::{
    PaginationType, extract_pagination_info, validate_page_size,
};
use pretty_assertions::assert_eq;
use serde_json::json;

// ---- Page-based (Bitbucket) ----

#[test]
fn page_basic_with_next_url() {
    let data = json!({
        "values": [{"a":1},{"a":2}],
        "page": 1,
        "pagelen": 2,
        "size": 10,
        "next": "https://api.bitbucket.org/2.0/repositories/foo?page=2&pagelen=2"
    });
    let p = extract_pagination_info(&data, PaginationType::Page).unwrap();
    assert_eq!(p.has_more, Some(true));
    assert_eq!(p.next_cursor.as_deref(), Some("2"));
    assert_eq!(p.count, Some(2));
    assert_eq!(p.page, Some(1));
    assert_eq!(p.size, Some(2));
    assert_eq!(p.total, Some(10));
}

#[test]
fn page_without_next_has_no_cursor() {
    let data = json!({
        "values": [{"a":1}],
        "page": 1,
        "pagelen": 50,
        "size": 1
    });
    let p = extract_pagination_info(&data, PaginationType::Page).unwrap();
    assert_eq!(p.has_more, Some(false));
    assert_eq!(p.next_cursor, None);
}

#[test]
fn page_with_available_placeholder_calculates_next() {
    let data = json!({
        "values": [],
        "page": 3,
        "pagelen": 25,
        "next": "available"
    });
    let p = extract_pagination_info(&data, PaginationType::Page).unwrap();
    assert_eq!(p.next_cursor.as_deref(), Some("4"));
}

#[test]
fn page_with_unparseable_next_falls_back_to_string_value() {
    let data = json!({
        "values": [],
        "page": 1,
        "pagelen": 10,
        "next": "token-xyz"
    });
    let p = extract_pagination_info(&data, PaginationType::Page).unwrap();
    assert_eq!(p.next_cursor.as_deref(), Some("token-xyz"));
}

// ---- Offset-based (Jira) ----

#[test]
fn offset_when_more_pages_exist() {
    let data = json!({
        "values": [{"a":1}],
        "startAt": 0,
        "maxResults": 50,
        "total": 100
    });
    let p = extract_pagination_info(&data, PaginationType::Offset).unwrap();
    assert_eq!(p.has_more, Some(true));
    assert_eq!(p.next_cursor.as_deref(), Some("50"));
    assert_eq!(p.total, Some(100));
}

#[test]
fn offset_falls_through_to_default_when_complete() {
    let data = json!({
        "values": [{"a":1}],
        "startAt": 50,
        "maxResults": 50,
        "total": 100
    });
    let p = extract_pagination_info(&data, PaginationType::Offset).unwrap();
    // Default branch kicks in when no offset shape matched.
    assert_eq!(p.has_more, Some(false));
    assert_eq!(p.count, Some(1));
}

// ---- Cursor-based (Confluence) ----

#[test]
fn cursor_extracts_cursor_param() {
    let data = json!({
        "results": [{"id":1},{"id":2}],
        "_links": {"next": "/wiki/rest/api/content?cursor=abc%3Ddef&limit=2"}
    });
    let p = extract_pagination_info(&data, PaginationType::Cursor).unwrap();
    assert_eq!(p.has_more, Some(true));
    assert_eq!(p.next_cursor.as_deref(), Some("abc=def"));
    assert_eq!(p.count, Some(2));
}

// ---- validate_page_size ----

#[test]
fn validate_page_size_default() {
    assert_eq!(validate_page_size(None), 50);
    assert_eq!(validate_page_size(Some(0)), 50);
}

#[test]
fn validate_page_size_clamps_above_max() {
    assert_eq!(validate_page_size(Some(500)), 100);
}

#[test]
fn validate_page_size_passes_valid() {
    assert_eq!(validate_page_size(Some(25)), 25);
    assert_eq!(validate_page_size(Some(100)), 100);
}
