#![allow(clippy::doc_markdown)]

//! Shared CLI option types and JSON-parse helpers used by both the
//! [`bb`](crate::cli::bb) and [`jira`](crate::cli::jira) subcommand groups.
//!
//! The actual subcommand enums and dispatchers live in their respective
//! per-vendor modules.

use clap::ValueEnum;
use serde_json::Value;

use crate::error::McpError;
use crate::format::OutputFormat;
use crate::tools::args::QueryParams;

/// CLI-side mirror of [`OutputFormat`] with a [`ValueEnum`] derive so clap
/// can parse `--output-format toon|json` without coupling the vendor-neutral
/// format module to clap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[value(rename_all = "lower")]
pub enum OutputFormatFlag {
    /// Token-efficient tabular format (default; 30–60% fewer tokens than JSON).
    #[default]
    Toon,
    /// Pretty-printed JSON.
    Json,
}

impl From<OutputFormatFlag> for OutputFormat {
    fn from(flag: OutputFormatFlag) -> Self {
        match flag {
            OutputFormatFlag::Toon => Self::Toon,
            OutputFormatFlag::Json => Self::Json,
        }
    }
}

/// Args shared by all read-shaped verbs (GET, DELETE) on both vendors.
#[derive(Debug, Clone, clap::Args)]
pub struct ReadOpts {
    /// API endpoint path. For Bitbucket, omit the `/2.0` prefix (added
    /// automatically). For Jira, supply the full path including the API
    /// version, e.g. `/rest/api/3/myself`.
    #[arg(short = 'p', long = "path")]
    pub path: String,

    /// Query parameters as a JSON object string, e.g. `'{"pagelen":"25"}'`
    /// (Bitbucket) or `'{"jql":"project=PROJ"}'` (Jira).
    #[arg(short = 'q', long = "query-params")]
    pub query_params: Option<String>,

    /// JMESPath expression to filter or transform the response. Reduces
    /// token cost when only a subset of fields is needed.
    #[arg(long = "jq")]
    pub jq: Option<String>,

    /// Output format. Defaults to `toon` (token-efficient tabular). Use
    /// `json` for pretty-printed JSON.
    #[arg(long = "output-format", value_enum, default_value_t)]
    pub output_format: OutputFormatFlag,
}

/// Args shared by all write-shaped verbs (POST, PUT, PATCH) on both vendors.
#[derive(Debug, Clone, clap::Args)]
pub struct WriteOpts {
    /// API endpoint path. See [`ReadOpts::path`] for vendor-specific
    /// conventions.
    #[arg(short = 'p', long = "path")]
    pub path: String,

    /// Request body as a JSON object string. Top-level value must be an
    /// object (arrays and primitives are rejected).
    #[arg(short = 'b', long = "body")]
    pub body: String,

    /// Query parameters as a JSON object string.
    #[arg(short = 'q', long = "query-params")]
    pub query_params: Option<String>,

    /// JMESPath expression to filter or transform the response.
    #[arg(long = "jq")]
    pub jq: Option<String>,

    /// Output format. Defaults to `toon`.
    #[arg(long = "output-format", value_enum, default_value_t)]
    pub output_format: OutputFormatFlag,
}

/// Parse a JSON string that must decode to an object. Matches TS `parseJson`:
/// rejects arrays, null, or primitives.
pub fn parse_object(json: &str, field: &str) -> Result<Value, McpError> {
    let value: Value = serde_json::from_str(json).map_err(|_| {
        crate::error::unexpected(
            format!("Invalid JSON in --{field}. Please provide valid JSON."),
            None,
        )
    })?;
    if !value.is_object() {
        let kind = match &value {
            Value::Null => "null",
            Value::Array(_) => "array",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Object(_) => unreachable!(),
        };
        return Err(crate::error::unexpected(
            format!("Invalid --{field}: expected a JSON object, got {kind}."),
            None,
        ));
    }
    Ok(value)
}

/// Parse `--query-params` JSON into a string-to-string map. Anything else
/// surfaces as a JSON-validation error to the user.
pub fn parse_query_params(input: Option<&str>) -> Result<Option<QueryParams>, McpError> {
    let Some(raw) = input else { return Ok(None) };
    let value = parse_object(raw, "query-params")?;
    let obj = value.as_object().expect("parse_object guarantees object");
    let mut out = QueryParams::new();
    for (k, v) in obj {
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            _ => {
                return Err(crate::error::unexpected(
                    format!(
                        "Invalid --query-params: value for \"{k}\" must be a string, boolean, or number."
                    ),
                    None,
                ));
            }
        };
        out.insert(k.clone(), s);
    }
    Ok(Some(out))
}
