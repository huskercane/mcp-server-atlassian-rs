Read any Bitbucket data. Returns TOON format by default (30-60% fewer tokens than JSON).

**IMPORTANT - Cost Optimization:**
- ALWAYS use `jq` param to filter response fields. Unfiltered responses are very expensive!
- Use `pagelen` query param to restrict result count (e.g., `pagelen: "5"`)
- If unsure about available fields, first fetch ONE item with `pagelen: "1"` and NO jq filter to explore the schema, then use jq in subsequent calls

**Schema Discovery Pattern:**
1. First call: `path: "/workspaces", queryParams: {"pagelen": "1"}` (no jq) - explore available fields
2. Then use: `jq: "values[*].{slug: slug, name: name, uuid: uuid}"` - extract only what you need

**Output format:** TOON (default, token-efficient) or JSON (`outputFormat: "json"`)

**Common paths:**
- `/workspaces` - list workspaces
- `/repositories/{workspace}` - list repos in workspace
- `/repositories/{workspace}/{repo}` - get repo details
- `/repositories/{workspace}/{repo}/pullrequests` - list PRs
- `/repositories/{workspace}/{repo}/pullrequests/{id}` - get PR details
- `/repositories/{workspace}/{repo}/pullrequests/{id}/comments` - list PR comments
- `/repositories/{workspace}/{repo}/pullrequests/{id}/diff` - get PR diff
- `/repositories/{workspace}/{repo}/refs/branches` - list branches
- `/repositories/{workspace}/{repo}/commits` - list commits
- `/repositories/{workspace}/{repo}/src/{commit}/{filepath}` - get file content
- `/repositories/{workspace}/{repo}/diff/{source}..{destination}` - compare branches/commits

**Query params:** `pagelen` (page size), `page` (page number), `q` (filter), `sort` (order), `fields` (sparse response)

**Example filters (q param):** `state="OPEN"`, `source.branch.name="feature"`, `title~"bug"`

**JQ examples:** `values[*].slug`, `values[0]`, `values[*].{name: name, uuid: uuid}`

The `/2.0` prefix is added automatically. API reference: https://developer.atlassian.com/cloud/bitbucket/rest/
