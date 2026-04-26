Partially update Confluence resources. Returns TOON format by default.

**IMPORTANT - Cost Optimization:** Use `jq` param to filter response fields.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Update space:** `/wiki/api/v2/spaces/{id}`
   body: `{"name": "New Name", "description": {"plain": {"value": "Desc", "representation": "plain"}}}`

2. **Update comment:** `/wiki/api/v2/footer-comments/{id}`

Note: Confluence v2 API primarily uses PUT for updates.

API reference: https://developer.atlassian.com/cloud/confluence/rest/v2/
