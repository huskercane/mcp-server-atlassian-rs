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

/// Arguments for `newrelic_query`.
///
/// New Relic's only API is NerdGraph (a single GraphQL endpoint), so this is a
/// bespoke tool rather than the five generic REST verbs the other vendors use.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewRelicQueryArgs {
    /// NerdGraph GraphQL document to execute. NRQL queries are run by wrapping
    /// them here, e.g.
    /// `{ actor { account(id: 123) { nrql(query: "SELECT count(*) FROM Transaction SINCE 1 hour ago") { results } } } }`.
    pub query: String,

    /// Optional GraphQL variables object, referenced by `$name` in `query`.
    /// Example: `{"id": 1234567, "q": "SELECT average(duration) FROM Transaction"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<Value>,

    /// JMESPath expression to filter/transform the response. IMPORTANT: always
    /// use this to extract only needed fields and reduce token costs.
    /// Example: "data.actor.account.nrql.results".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq: Option<String>,

    /// Output format: "toon" (default, 30-60% fewer tokens) or "json".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormatArg>,
}

/// Arguments for `grafana_query_logs`.
///
/// Reads logs by running a LogQL query against a Loki datasource through
/// Grafana's datasource proxy. The datasource UID identifies which Loki backend
/// to query; discover it with `grafana_list_datasources`. Time-range and limit
/// knobs map directly onto Loki's `query_range` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrafanaQueryLogsArgs {
    /// UID of the Loki datasource to query. Find it via
    /// `grafana_list_datasources` (the `uid` field of a `type: "loki"` entry).
    pub datasource_uid: String,

    /// LogQL query, e.g. `{app="api"} |= "error"` or
    /// `sum by (level) (count_over_time({app="api"}[5m]))`.
    pub query: String,

    /// Range start: RFC3339 (`2024-01-01T00:00:00Z`) or Unix nanoseconds.
    /// Defaults to Loki's default window (last hour) when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,

    /// Range end: RFC3339 or Unix nanoseconds. Defaults to "now" when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,

    /// Max number of log lines to return (Loki defaults to 100). Keep this small
    /// to reduce token costs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    /// Scan direction: `backward` (newest first, default) or `forward`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,

    /// Query resolution step for metric queries, e.g. `30s` or `1m`. Only
    /// meaningful for LogQL metric queries; ignored for log selectors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,

    /// JMESPath expression to filter/transform the response. IMPORTANT: always
    /// use this to extract only needed fields and reduce token costs.
    /// Example: "data.result[*].values".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq: Option<String>,

    /// Output format: "toon" (default, 30-60% fewer tokens) or "json".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormatArg>,
}

/// Arguments for `grafana_list_datasources`.
///
/// Lists configured Grafana datasources so the caller can discover a Loki
/// datasource's UID for `grafana_query_logs`. Filter to Loki entries with `jq`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrafanaListDatasourcesArgs {
    /// JMESPath expression to filter/transform the response. Example to find
    /// Loki datasources: `[?type=='loki'].{name: name, uid: uid}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq: Option<String>,

    /// Output format: "toon" (default) or "json".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormatArg>,
}

/// Shared optional output controls for typed edX discussion read tools.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdxDiscussionOutputArgs {
    /// JMESPath expression to filter/transform the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq: Option<String>,

    /// Output format: "toon" (default) or "json".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormatArg>,
}

/// Arguments for `edx_discussion_course`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdxDiscussionCourseArgs {
    /// Course key, e.g. "course-v1:edX+DemoX+Demo_Course".
    pub course_id: String,

    #[serde(default, flatten)]
    pub output: EdxDiscussionOutputArgs,
}

/// Arguments for `edx_discussion_topics`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdxDiscussionTopicsArgs {
    /// Course key, e.g. "course-v1:edX+DemoX+Demo_Course".
    pub course_id: String,

    #[serde(default, flatten)]
    pub output: EdxDiscussionOutputArgs,
}

/// Arguments for `edx_discussion_threads`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdxDiscussionThreadsArgs {
    /// Course key. Required by the edX Discussion API.
    pub course_id: String,

    /// Retrieve threads only within this topic. Mutually exclusive with
    /// `following` and `textSearch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<String>,

    /// Retrieve only threads the user is following.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub following: Option<bool>,

    /// Retrieve only unread threads or unanswered question threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<EdxDiscussionThreadView>,

    /// Full-text search query for matching threads/comments. Mutually
    /// exclusive with `topicId` and `following`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_search: Option<String>,

    /// Order threads by last activity, comment count, or vote count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by: Option<EdxDiscussionThreadOrderBy>,

    /// Sort direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_direction: Option<EdxDiscussionOrderDirection>,

    /// Number of threads per page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,

    /// Page number to retrieve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,

    #[serde(default, flatten)]
    pub output: EdxDiscussionOutputArgs,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdxDiscussionThreadView {
    Unread,
    Unanswered,
}

impl EdxDiscussionThreadView {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Unanswered => "unanswered",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdxDiscussionThreadOrderBy {
    LastActivityAt,
    CommentCount,
    VoteCount,
}

impl EdxDiscussionThreadOrderBy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LastActivityAt => "last_activity_at",
            Self::CommentCount => "comment_count",
            Self::VoteCount => "vote_count",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EdxDiscussionOrderDirection {
    Asc,
    Desc,
}

impl EdxDiscussionOrderDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// Arguments for `edx_discussion_thread_create`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdxDiscussionThreadCreateArgs {
    /// Course key.
    pub course_id: String,

    /// Topic ID to create the thread in.
    pub topic_id: String,

    /// Thread type: "discussion" or "question".
    #[serde(rename = "type")]
    pub thread_type: EdxDiscussionThreadType,

    /// Thread title.
    pub title: String,

    /// Raw body. May contain Markdown, HTML, and MathJax markup supported by
    /// the edX discussion service.
    pub raw_body: String,

    /// Optional cohort/group ID. Privileged roles may set this explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<i64>,

    #[serde(default, flatten)]
    pub output: EdxDiscussionOutputArgs,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EdxDiscussionThreadType {
    Discussion,
    Question,
}

impl EdxDiscussionThreadType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discussion => "discussion",
            Self::Question => "question",
        }
    }
}

/// Arguments for `edx_discussion_comments`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdxDiscussionCommentsArgs {
    /// Thread ID whose comments/responses should be listed.
    pub thread_id: String,

    /// Retrieve only endorsed or non-endorsed comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endorsed: Option<bool>,

    /// Number of comments per page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,

    /// Page number to retrieve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,

    #[serde(default, flatten)]
    pub output: EdxDiscussionOutputArgs,
}

/// Arguments for `edx_discussion_comment_create`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdxDiscussionCommentCreateArgs {
    /// Thread ID to respond to. Omit only when creating a child comment under
    /// `parentId`, if the target Open edX instance supports that form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,

    /// Parent comment ID for a nested comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,

    /// Raw body. May contain Markdown, HTML, and MathJax markup supported by
    /// the edX discussion service.
    pub raw_body: String,

    #[serde(default, flatten)]
    pub output: EdxDiscussionOutputArgs,
}
