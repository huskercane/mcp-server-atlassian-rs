// The field-level doc strings below travel as the JSON Schema descriptions
// published to MCP clients; their wording is pinned to the TS reference.
#![allow(clippy::doc_markdown)]

//! Argument types for the MCP tools. Mirrors the Zod schemas in
//! `src/tools/atlassian.api.types.ts` so the JSON Schema published over MCP
//! matches the reference implementation.
//!
//! Struct naming deliberately keeps camelCase JSON field names (`queryParams`,
//! `outputFormat`) because those are part of the tool's public contract and
//! are referenced verbatim by LLM prompts and TS tests.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::format::OutputFormat;

/// Serializable/deserializable `OutputFormat` for tool arg surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormatArg {
    #[default]
    Toon,
    Json,
}

impl From<OutputFormatArg> for OutputFormat {
    fn from(value: OutputFormatArg) -> Self {
        match value {
            OutputFormatArg::Toon => Self::Toon,
            OutputFormatArg::Json => Self::Json,
        }
    }
}

/// Query parameter map. `BTreeMap` is used so the generated JSON Schema has
/// a deterministic shape and URL encoding is stable order (important for the
/// raw-response log and test fixtures).
pub type QueryParams = BTreeMap<String, String>;

/// Arguments for `bb_get` / `bb_delete` (no body).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadArgs {
    /// The Bitbucket API endpoint path (without base URL). Must start with "/".
    /// Examples: "/workspaces", "/repositories/{workspace}/{repo_slug}",
    /// "/repositories/{workspace}/{repo_slug}/pullrequests/{id}"
    pub path: String,

    /// Optional query parameters as key-value pairs.
    /// Examples: {"pagelen": "25", "page": "2", "q": "state=\"OPEN\"",
    /// "fields": "values.title,values.state"}
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_params: Option<QueryParams>,

    /// JMESPath expression to filter/transform the response. IMPORTANT:
    /// always use this to extract only needed fields and reduce token costs.
    /// Examples: "values[*].{name: name, slug: slug}",
    /// "values[0]", "values[*].name". See https://jmespath.org
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq: Option<String>,

    /// Output format: "toon" (default, 30-60% fewer tokens) or "json".
    /// TOON is optimized for LLMs with tabular arrays and minimal syntax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormatArg>,
}

/// Arguments for `bb_clone`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloneArgs {
    /// Bitbucket workspace slug containing the repository. If not provided,
    /// the tool will use your default workspace (either configured via
    /// `BITBUCKET_DEFAULT_WORKSPACE` or the first workspace in your account).
    /// Example: "myteam"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_slug: Option<String>,

    /// Repository name/slug to clone. This is the short name of the
    /// repository. Example: "project-api"
    pub repo_slug: String,

    /// Directory path where the repository will be cloned. IMPORTANT:
    /// Absolute paths are strongly recommended (e.g., "/home/user/projects"
    /// or "C:\\Users\\name\\projects"). Relative paths will be resolved
    /// relative to the server's working directory, which may not be what you
    /// expect. The repository will be cloned into a subdirectory at
    /// targetPath/repoSlug. Make sure you have write permissions to this
    /// location.
    pub target_path: String,
}

/// Arguments for `bb_post` / `bb_put` / `bb_patch` (with body).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteArgs {
    /// The Bitbucket API endpoint path (without base URL). Must start with "/".
    pub path: String,

    /// Request body as a JSON object. Structure depends on the endpoint.
    /// Example for PR:
    /// `{"title": "My PR", "source": {"branch": {"name": "feature"}}}`
    pub body: Value,

    /// Optional query parameters as key-value pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_params: Option<QueryParams>,

    /// JMESPath expression to filter/transform the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq: Option<String>,

    /// Output format: "toon" (default) or "json".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormatArg>,
}
