Replace Jira resources (full update). Returns TOON format by default.

**IMPORTANT - Cost Optimization:** Use `jq` param to extract only needed fields from response

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Update issue (full):** `/rest/api/3/issue/{issueIdOrKey}`
   body: `{"fields": {"summary": "New title", "description": {...}, "assignee": {"accountId": "..."}}}`

2. **Update project:** `/rest/api/3/project/{projectIdOrKey}`
   body: `{"name": "New Project Name", "description": "Updated description"}`

3. **Set issue property:** `/rest/api/3/issue/{issueIdOrKey}/properties/{propertyKey}`
   body: `{"value": "property value"}`

Note: PUT replaces the entire resource. For partial updates, prefer PATCH.

API reference: https://developer.atlassian.com/cloud/jira/platform/rest/v3/
