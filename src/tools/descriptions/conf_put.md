Replace Confluence resources (full update). Returns TOON format by default.

**IMPORTANT - Cost Optimization:**
- Use `jq` param to extract only needed fields from response
- Example: `jq: "{id: id, version: version.number}"`

**Output format:** TOON (default) or JSON (`outputFormat: "json"`)

**Common operations:**

1. **Update page:** `/wiki/api/v2/pages/{id}`
   body: `{"id": "123", "status": "current", "title": "Updated Title", "spaceId": "456", "body": {"representation": "storage", "value": "<p>Content</p>"}, "version": {"number": 2}}`
   Note: version.number must be incremented

2. **Update blog post:** `/wiki/api/v2/blogposts/{id}`

Note: PUT replaces entire resource. Version number must be incremented.

API reference: https://developer.atlassian.com/cloud/confluence/rest/v2/
