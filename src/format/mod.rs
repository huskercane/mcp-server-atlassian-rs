//! Output formatting, filtering, and truncation helpers.

pub mod jmespath;
pub mod markdown;
pub mod truncation;

use serde::Serialize;
use serde_json::Value;
use toon_format::EncodeOptions;

/// How tool output should be rendered before being handed to the MCP client.
///
/// Default is [`OutputFormat::Toon`] to match the TS server, which promises
/// token-efficient TOON output in README/tool descriptions. On encode failure
/// the renderer falls back to pretty JSON — same behaviour as TS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Toon,
    Json,
}

impl OutputFormat {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("json") => Self::Json,
            _ => Self::Toon,
        }
    }
}

/// Render `data` as the requested output string. Falls back to pretty JSON if
/// TOON encoding fails. Matches TS `toOutputString`.
pub fn render(data: &Value, format: OutputFormat) -> String {
    let json_fallback = to_pretty_json(data);
    match format {
        OutputFormat::Json => json_fallback,
        OutputFormat::Toon => encode_toon(data).unwrap_or(json_fallback),
    }
}

/// Render with a caller-supplied serializable value. Same policy as
/// [`render`].
pub fn render_serializable<T: Serialize>(data: &T, format: OutputFormat) -> String {
    let json_fallback = serde_json::to_string_pretty(data).unwrap_or_default();
    match format {
        OutputFormat::Json => json_fallback,
        OutputFormat::Toon => encode_toon_serializable(data).unwrap_or(json_fallback),
    }
}

/// Pretty JSON with 2-space indent — matches TS `JSON.stringify(value, null, 2)`.
pub fn to_pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn encode_toon(value: &Value) -> Option<String> {
    toon_format::encode(value, &EncodeOptions::default()).ok()
}

fn encode_toon_serializable<T: Serialize>(value: &T) -> Option<String> {
    toon_format::encode(value, &EncodeOptions::default()).ok()
}
