Partially update Bitbucket resources. Returns TOON format by default.

**IMPORTANT - Cost Optimization:** Use `jq` param to filter response fields.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Update PR title/description:** `/repositories/{workspace}/{repo}/pullrequests/{id}`
   body: `{"title": "New title", "description": "Updated description"}`

2. **Update PR reviewers:** `/repositories/{workspace}/{repo}/pullrequests/{id}`
   body: `{"reviewers": [{"uuid": "{user-uuid}"}]}`

3. **Update repository properties:** `/repositories/{workspace}/{repo}`
   body: `{"description": "New description"}`

4. **Update comment:** `/repositories/{workspace}/{repo}/pullrequests/{pr_id}/comments/{comment_id}`
   body: `{"content": {"raw": "Updated comment"}}`

The `/2.0` prefix is added automatically. API reference: https://developer.atlassian.com/cloud/bitbucket/rest/
