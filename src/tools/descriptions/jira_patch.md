Partially update Jira resources. Returns TOON format by default.

**IMPORTANT - Cost Optimization:** Use `jq` param to filter response fields.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Update issue fields:** `/rest/api/3/issue/{issueIdOrKey}`
   body: `{"fields": {"summary": "Updated title"}}` (only updates specified fields)

2. **Update comment:** `/rest/api/3/issue/{issueIdOrKey}/comment/{commentId}`
   body: `{"body": {"type": "doc", "version": 1, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Updated comment"}]}]}}`

3. **Update worklog:** `/rest/api/3/issue/{issueIdOrKey}/worklog/{worklogId}`
   body: `{"timeSpentSeconds": 7200}`

Note: PATCH only updates the fields you specify, leaving others unchanged.

API reference: https://developer.atlassian.com/cloud/jira/platform/rest/v3/
