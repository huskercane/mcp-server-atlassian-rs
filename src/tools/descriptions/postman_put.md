Replace a Postman resource via the Postman API (HTTP PUT). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a Postman API key (`POSTMAN_API_KEY`) sent in the `X-API-Key` header; no per-call auth is needed.

PUT replaces the **entire** resource — send the full object, not a partial. For partial edits use `postman_patch` where the endpoint supports it.

**Common PUT endpoints:**
- `/collections/{uid}` - replace a collection. Body: `{"collection": { ...full collection schema... }}`.
- `/environments/{uid}` - replace an environment. Body: `{"environment": {"name": "...", "values": [...]}}`.
- `/mocks/{id}`, `/monitors/{id}` - replace a mock server / monitor.

**Body:** pass the full JSON request body as `body`, wrapped in the resource's top-level key.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://learning.postman.com/docs/developer/postman-api/
