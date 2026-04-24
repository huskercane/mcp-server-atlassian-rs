//! `JMESPath` filter wrapper. Mirrors TS `applyJqFilter`.
//!
//! - Empty / whitespace-only filter → pass-through.
//! - Valid expression → transformed JSON value.
//! - Invalid expression → wrap the original data plus an `_jqError` marker so
//!   the LLM can see what went wrong without the request failing outright.

use serde_json::{Value, json};

/// Apply `filter` to `data`. Returns the filtered value, or a diagnostic
/// envelope when the expression is invalid.
pub fn apply_jq_filter(data: &Value, filter: Option<&str>) -> Value {
    let Some(raw) = filter else {
        return data.clone();
    };
    let expr = raw.trim();
    if expr.is_empty() {
        return data.clone();
    }

    let parsed = match ::jmespath::compile(expr) {
        Ok(e) => e,
        Err(err) => return invalid_filter_envelope(data, expr, &err.to_string()),
    };

    match parsed.search(data.clone()) {
        Ok(var) => var_to_value(&var),
        Err(err) => invalid_filter_envelope(data, expr, &err.to_string()),
    }
}

fn var_to_value(var: &::jmespath::Variable) -> Value {
    // Round-trip through a JSON string — jmespath::Variable implements
    // `Serialize`, so this is infallible for any real result.
    serde_json::to_string(var).map_or(Value::Null, |s| {
        serde_json::from_str(&s).unwrap_or(Value::Null)
    })
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
