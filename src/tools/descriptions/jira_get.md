Read any Jira data. Returns TOON format by default (30-60% fewer tokens than JSON).

**IMPORTANT - Cost Optimization:**
- ALWAYS use `jq` param to filter response fields. Unfiltered responses are very expensive!
- Use `maxResults` query param to restrict result count (e.g., `maxResults: "5"`)
- If unsure about available fields, first fetch ONE item with `maxResults: "1"` and NO jq filter to explore the schema, then use jq in subsequent calls

**Schema Discovery Pattern:**
1. First call: `path: "/rest/api/3/search/jql", queryParams: {"maxResults": "1", "jql": "project=PROJ"}` (no jq) - explore available fields
2. Then use: `jq: "issues[*].{key: key, summary: fields.summary, status: fields.status.name}"` - extract only what you need

**Output format:** TOON (default, token-efficient) or JSON (`outputFormat: "json"`)

**Common paths:**
- `/rest/api/3/project` - list all projects
- `/rest/api/3/project/{projectKeyOrId}` - get project details
- `/rest/api/3/search/jql` - search issues with JQL (use `jql` query param). NOTE: `/rest/api/3/search` is deprecated!
- `/rest/api/3/issue/{issueIdOrKey}` - get issue details
- `/rest/api/3/issue/{issueIdOrKey}/comment` - list issue comments
- `/rest/api/3/issue/{issueIdOrKey}/worklog` - list issue worklogs
- `/rest/api/3/issue/{issueIdOrKey}/transitions` - get available transitions
- `/rest/api/3/user/search` - search users (use `query` param)
- `/rest/api/3/status` - list all statuses
- `/rest/api/3/issuetype` - list issue types
- `/rest/api/3/priority` - list priorities

**JQ examples:** `issues[*].key`, `issues[0]`, `issues[*].{key: key, summary: fields.summary}`

**Example JQL queries:** `project=PROJ`, `assignee=currentUser()`, `status="In Progress"`, `created >= -7d`

API reference: https://developer.atlassian.com/cloud/jira/platform/rest/v3/
