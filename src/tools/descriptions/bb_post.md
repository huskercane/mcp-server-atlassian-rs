Create Bitbucket resources. Returns TOON format by default (token-efficient).

**IMPORTANT - Cost Optimization:**
- Use `jq` param to extract only needed fields from response (e.g., `jq: "{id: id, title: title}"`)
- Unfiltered responses include all metadata and are expensive!

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Create PR:** `/repositories/{workspace}/{repo}/pullrequests`
   body: `{"title": "...", "source": {"branch": {"name": "feature"}}, "destination": {"branch": {"name": "main"}}}`

2. **Add PR comment:** `/repositories/{workspace}/{repo}/pullrequests/{id}/comments`
   body: `{"content": {"raw": "Comment text"}}`

3. **Approve PR:** `/repositories/{workspace}/{repo}/pullrequests/{id}/approve`
   body: `{}`

4. **Request changes:** `/repositories/{workspace}/{repo}/pullrequests/{id}/request-changes`
   body: `{}`

5. **Merge PR:** `/repositories/{workspace}/{repo}/pullrequests/{id}/merge`
   body: `{"merge_strategy": "squash"}` (strategies: merge_commit, squash, fast_forward)

The `/2.0` prefix is added automatically. API reference: https://developer.atlassian.com/cloud/bitbucket/rest/
