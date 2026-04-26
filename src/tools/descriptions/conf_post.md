Create Confluence resources. Returns TOON format by default (token-efficient).

**IMPORTANT - Cost Optimization:**
- Use `jq` param to extract only needed fields from response (e.g., `jq: "{id: id, title: title}"`)
- Unfiltered responses include all metadata and are expensive!

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Create page:** `/wiki/api/v2/pages`
   body: `{"spaceId": "123456", "status": "current", "title": "Page Title", "parentId": "789", "body": {"representation": "storage", "value": "<p>Content</p>"}}`

2. **Create blog post:** `/wiki/api/v2/blogposts`
   body: `{"spaceId": "123456", "status": "current", "title": "Blog Title", "body": {"representation": "storage", "value": "<p>Content</p>"}}`

3. **Add label:** `/wiki/api/v2/pages/{id}/labels` - body: `{"name": "label-name"}`

4. **Add comment:** `/wiki/api/v2/pages/{id}/footer-comments`

API reference: https://developer.atlassian.com/cloud/confluence/rest/v2/
