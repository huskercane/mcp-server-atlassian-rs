//! `JMESPath` filter wrapper. Mirrors TS `applyJqFilter`.
//!
//! - Empty / whitespace-only filter → pass-through.
//! - Valid expression → transformed JSON value.
//! - Invalid expression → wrap the original data plus an `_jqError` marker so
//!   the LLM can see what went wrong without the request failing outright.

use std::borrow::Cow;

use serde_json::{Value, json};

/// Apply `filter` to `data`. Returns the filtered value, or a diagnostic
/// envelope when the expression is invalid.
///
/// Returns a [`Cow`] because the overwhelmingly common case is "no filter
/// supplied", where the result *is* `data`. Returning an owned `Value` there
/// forced a deep clone of the entire response on every tool call — ~14 MB and
/// 175k allocations on a 2 MB payload, for a value the caller only ever reads.
/// Callers that pass the result straight to
/// [`render`](crate::format::render) need no change: `&Cow<'_, Value>` derefs
/// to `&Value`.
pub fn apply_jq_filter<'a>(data: &'a Value, filter: Option<&str>) -> Cow<'a, Value> {
    let Some(raw) = filter else {
        return Cow::Borrowed(data);
    };
    let expr = raw.trim();
    if expr.is_empty() {
        return Cow::Borrowed(data);
    }

    let parsed = match ::jmespath::compile(expr) {
        Ok(e) => e,
        Err(err) => return Cow::Owned(invalid_filter_envelope(data, expr, &err.to_string())),
    };

    // `search` takes `impl ToJmespath`, which is implemented for `&Value` via
    // the blanket `Serialize` impl — it re-materialises the tree either way, so
    // handing it a borrow avoids one redundant deep clone.
    match parsed.search(data) {
        Ok(var) => Cow::Owned(var_to_value(&var)),
        Err(err) => Cow::Owned(invalid_filter_envelope(data, expr, &err.to_string())),
    }
}

fn var_to_value(var: &::jmespath::Variable) -> Value {
    // `jmespath::Variable` implements `Serialize`, so this is infallible for
    // any real result. `to_value` goes straight to a `Value`; the previous
    // round-trip through a JSON *string* serialised and then re-parsed the
    // entire result for no gain.
    serde_json::to_value(var).unwrap_or(Value::Null)
}

fn invalid_filter_envelope(data: &Value, expr: &str, _reason: &str) -> Value {
    // TS shape: `{_jqError: "Invalid JMESPath expression: <expr>", _originalData: <data>}`.
    // The TS version does not include the parser's own message, so we keep it
    // only in logs (caller may log `reason`).
    json!({
        "_jqError": format!("Invalid JMESPath expression: {expr}"),
        "_originalData": data.clone(),
    })
}
