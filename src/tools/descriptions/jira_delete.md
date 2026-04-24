Delete Jira resources. Returns TOON format by default.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Delete issue:** `/rest/api/3/issue/{issueIdOrKey}`
   Query param: `deleteSubtasks=true` to delete subtasks

2. **Delete comment:** `/rest/api/3/issue/{issueIdOrKey}/comment/{commentId}`

3. **Delete worklog:** `/rest/api/3/issue/{issueIdOrKey}/worklog/{worklogId}`

4. **Delete attachment:** `/rest/api/3/attachment/{attachmentId}`

5. **Remove watcher:** `/rest/api/3/issue/{issueIdOrKey}/watchers`
   Query param: `accountId={accountId}`

Note: Most DELETE endpoints return 204 No Content on success.

API reference: https://developer.atlassian.com/cloud/jira/platform/rest/v3/
