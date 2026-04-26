Read any Confluence data. Returns TOON format by default (30-60% fewer tokens than JSON).

**IMPORTANT - Cost Optimization:**
- ALWAYS use `jq` param to filter response fields. Unfiltered responses are very expensive!
- Use `limit` query param to restrict result count (e.g., `limit: "5"`)
- If unsure about available fields, first fetch ONE item with `limit: "1"` and NO jq filter to explore the schema, then use jq in subsequent calls

**Schema Discovery Pattern:**
1. First call: `path: "/wiki/api/v2/spaces", queryParams: {"limit": "1"}` (no jq) - explore available fields
2. Then use: `jq: "results[*].{id: id, key: key, name: name}"` - extract only what you need

**Output format:** TOON (default, token-efficient) or JSON (`outputFormat: "json"`)

**Common paths:**
- `/wiki/api/v2/spaces` - list spaces
- `/wiki/api/v2/pages` - list pages (use `space-id` query param)
- `/wiki/api/v2/pages/{id}` - get page details
- `/wiki/api/v2/pages/{id}/body` - get page body (`body-format`: storage, atlas_doc_format, view)
- `/wiki/rest/api/search` - search content (`cql` query param)

**JQ examples:** `results[*].id`, `results[0]`, `results[*].{id: id, title: title}`

API reference: https://developer.atlassian.com/cloud/confluence/rest/v2/
