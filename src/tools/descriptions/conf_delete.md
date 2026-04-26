Delete Confluence resources. Returns TOON format by default.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**
- `/wiki/api/v2/pages/{id}` - Delete page
- `/wiki/api/v2/blogposts/{id}` - Delete blog post
- `/wiki/api/v2/pages/{id}/labels/{label-id}` - Remove label
- `/wiki/api/v2/footer-comments/{id}` - Delete comment
- `/wiki/api/v2/attachments/{id}` - Delete attachment

Note: Most DELETE endpoints return 204 No Content on success.

API reference: https://developer.atlassian.com/cloud/confluence/rest/v2/
