Delete a Postman resource via the Postman API (HTTP DELETE). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a Postman API key (`POSTMAN_API_KEY`) sent in the `X-API-Key` header; no per-call auth is needed.

**DESTRUCTIVE — deletion is permanent.** Confirm the `uid`/`id` before calling.

**Common DELETE endpoints:**
- `/collections/{uid}` - delete a collection
- `/environments/{uid}` - delete an environment
- `/workspaces/{id}` - delete a workspace
- `/mocks/{id}`, `/monitors/{id}` - delete a mock server / monitor

The path identifies the resource; no body is sent. The response typically echoes the deleted resource's id.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://learning.postman.com/docs/developer/postman-api/
