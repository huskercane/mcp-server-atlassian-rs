//! Pagination extraction and validation. Mirrors `src/utils/pagination.util.ts`.
//!
//! Bitbucket paginates most endpoints by page/pagelen with a `next` URL;
//! some older endpoints use cursors; Jira (not used here) uses offsets.
//! The extractor supports all three so the controller layer can stay
//! vendor-agnostic when we add Jira/Confluence siblings later.

use serde_json::Value;
use tracing::warn;
use url::Url;

use crate::constants::data_limits::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};

/// Pagination flavour used by the API being called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginationType {
    Page,
    Offset,
    Cursor,
}

/// Extracted pagination info. Mirrors TS `ResponsePagination`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ResponsePagination {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Extract pagination info from a Bitbucket/Jira/Confluence response. Returns
/// `None` when the shape doesn't match any known pagination style.
pub fn extract_pagination_info(data: &Value, kind: PaginationType) -> Option<ResponsePagination> {
    let obj = data.as_object()?;

    let pagination = match kind {
        PaginationType::Page => extract_page(obj),
        PaginationType::Offset => extract_offset(obj),
        PaginationType::Cursor => extract_cursor(obj),
    };

    pagination.or_else(|| default_when_results_present(obj))
}

fn extract_page(obj: &serde_json::Map<String, Value>) -> Option<ResponsePagination> {
    let page = obj.get("page").and_then(Value::as_u64)?;
    let pagelen = obj.get("pagelen").and_then(Value::as_u64)?;
    let next_raw = obj.get("next");
    let has_more = next_raw.is_some_and(|v| !matches!(v, Value::Null));

    let next_cursor = if has_more {
        compute_next_cursor(page, next_raw)
    } else {
        None
    };

    let values_count = obj
        .get("values")
        .and_then(Value::as_array)
        .map_or(0, |a| a.len() as u64);

    Some(ResponsePagination {
        next_cursor,
        has_more: Some(has_more),
        count: Some(values_count),
        page: Some(page),
        size: Some(pagelen),
        total: obj.get("size").and_then(Value::as_u64),
    })
}

fn extract_offset(obj: &serde_json::Map<String, Value>) -> Option<ResponsePagination> {
    let start_at = obj.get("startAt").and_then(Value::as_u64);
    let max_results = obj.get("maxResults").and_then(Value::as_u64);
    let total = obj.get("total").and_then(Value::as_u64);
    let count = obj
        .get("values")
        .and_then(Value::as_array)
        .map(|a| a.len() as u64);

    if let (Some(start), Some(max), Some(total)) = (start_at, max_results, total)
        && start + max < total
    {
        return Some(ResponsePagination {
            next_cursor: Some((start + max).to_string()),
            has_more: Some(true),
            count,
            total: Some(total),
            ..Default::default()
        });
    }

    if let Some(next) = obj.get("nextPage").and_then(Value::as_str) {
        return Some(ResponsePagination {
            next_cursor: Some(next.to_owned()),
            has_more: Some(true),
            count,
            ..Default::default()
        });
    }

    None
}

fn extract_cursor(obj: &serde_json::Map<String, Value>) -> Option<ResponsePagination> {
    let next = obj.get("_links")?.get("next")?.as_str()?;
    let cursor = extract_cursor_param(next)?;
    let count = obj
        .get("results")
        .and_then(Value::as_array)
        .map(|a| a.len() as u64);
    Some(ResponsePagination {
        next_cursor: Some(cursor),
        has_more: Some(true),
        count,
        ..Default::default()
    })
}

fn default_when_results_present(obj: &serde_json::Map<String, Value>) -> Option<ResponsePagination> {
    let count = obj
        .get("results")
        .or_else(|| obj.get("values"))
        .and_then(Value::as_array)
        .map(|a| a.len() as u64);
    count.map(|c| ResponsePagination {
        has_more: Some(false),
        count: Some(c),
        ..Default::default()
    })
}

fn compute_next_cursor(current_page: u64, next: Option<&Value>) -> Option<String> {
    match next {
        Some(Value::String(s)) if s.contains("://") => Url::parse(s)
            .ok()
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, _)| k == "page")
                    .map(|(_, v)| v.into_owned())
            })
            .or_else(|| Some((current_page + 1).to_string())),
        Some(Value::String(s)) if s == "available" => Some((current_page + 1).to_string()),
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn extract_cursor_param(next: &str) -> Option<String> {
    // Confluence cursor pagination sends URLs like `/wiki/rest/api/?cursor=abc&limit=25`.
    // We also accept bare query strings for flexibility.
    if let Ok(url) = Url::parse(next)
        && let Some((_, v)) = url.query_pairs().find(|(k, _)| k == "cursor")
    {
        return Some(v.into_owned());
    }
    next.split(&['?', '&'][..])
        .find_map(|segment| segment.strip_prefix("cursor="))
        .map(percent_decode)
}

fn percent_decode(input: &str) -> String {
    // url crate's Url doesn't expose a cheap standalone decoder. Do the
    // common-case replacement by re-parsing as a dummy URL query.
    let stub = format!("http://x/?v={input}");
    Url::parse(&stub)
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "v")
                .map(|(_, v)| v.into_owned())
        })
        .unwrap_or_else(|| input.to_owned())
}

/// Clamp a user-supplied page size to the configured range, substituting a
/// default when missing/zero. Matches TS `validatePageSize`.
pub fn validate_page_size(requested: Option<u32>) -> u32 {
    match requested {
        Some(n) if n > 0 && n <= MAX_PAGE_SIZE => n,
        Some(n) if n > MAX_PAGE_SIZE => {
            warn!(requested = n, max = MAX_PAGE_SIZE, "clamping requested page size");
            MAX_PAGE_SIZE
        }
        _ => DEFAULT_PAGE_SIZE,
    }
}

/// Validate that a received page didn't exceed the configured maximum
/// (CWE-770). Matches TS `validatePaginationLimits`.
pub fn validate_pagination_limits(pagination: &ResponsePagination) -> bool {
    let item_count = u32::try_from(pagination.count.unwrap_or(0)).unwrap_or(u32::MAX);
    let page_size = u32::try_from(pagination.size.unwrap_or(0)).unwrap_or(u32::MAX);
    if item_count > MAX_PAGE_SIZE || page_size > MAX_PAGE_SIZE {
        warn!(
            item_count,
            page_size, max = MAX_PAGE_SIZE, "response exceeds page-size limit"
        );
        return false;
    }
    true
}
