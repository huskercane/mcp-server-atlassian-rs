Partially update a Postman resource via the Postman API (HTTP PATCH). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a Postman API key (`POSTMAN_API_KEY`) sent in the `X-API-Key` header; no per-call auth is needed.

PATCH applies a partial update — send only the fields you want to change. To replace a whole resource use `postman_put`.

**Common PATCH endpoints:**
- `/workspaces/{id}` - rename or re-describe a workspace. Body: `{"workspace": {"name": "New name"}}`.
- `/collections/{uid}` - some collection metadata updates are accepted as PATCH.

**Body:** pass the partial JSON request body as `body`, wrapped in the resource's top-level key.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://learning.postman.com/docs/developer/postman-api/
