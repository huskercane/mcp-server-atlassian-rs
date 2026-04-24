Delete Bitbucket resources. Returns TOON format by default.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Delete branch:** `/repositories/{workspace}/{repo}/refs/branches/{branch_name}`
2. **Delete PR comment:** `/repositories/{workspace}/{repo}/pullrequests/{pr_id}/comments/{comment_id}`
3. **Decline PR:** `/repositories/{workspace}/{repo}/pullrequests/{id}/decline`
4. **Remove PR approval:** `/repositories/{workspace}/{repo}/pullrequests/{id}/approve`
5. **Delete repository:** `/repositories/{workspace}/{repo}` (caution: irreversible)

Note: Most DELETE endpoints return 204 No Content on success.

The `/2.0` prefix is added automatically. API reference: https://developer.atlassian.com/cloud/bitbucket/rest/
