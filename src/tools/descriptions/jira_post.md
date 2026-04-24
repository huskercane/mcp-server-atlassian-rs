Create Jira resources. Returns TOON format by default (token-efficient).

**IMPORTANT - Cost Optimization:**
- Use `jq` param to extract only needed fields from response (e.g., `jq: "{key: key, id: id}"`)
- Unfiltered responses include all metadata and are expensive!

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Create issue:** `/rest/api/3/issue`
   body: `{"fields": {"project": {"key": "PROJ"}, "summary": "Issue title", "issuetype": {"name": "Task"}, "description": {"type": "doc", "version": 1, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Description"}]}]}}}`

2. **Add comment:** `/rest/api/3/issue/{issueIdOrKey}/comment`
   body: `{"body": {"type": "doc", "version": 1, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Comment text"}]}]}}`

3. **Add worklog:** `/rest/api/3/issue/{issueIdOrKey}/worklog`
   body: `{"timeSpentSeconds": 3600, "comment": {"type": "doc", "version": 1, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Work done"}]}]}}`

4. **Transition issue:** `/rest/api/3/issue/{issueIdOrKey}/transitions`
   body: `{"transition": {"id": "31"}}`

5. **Add attachment:** `/rest/api/3/issue/{issueIdOrKey}/attachments`
   Note: Requires multipart form data (complex - use Jira UI for attachments)

API reference: https://developer.atlassian.com/cloud/jira/platform/rest/v3/
