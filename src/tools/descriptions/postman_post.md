Create Postman resources via the Postman API (HTTP POST). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a Postman API key (`POSTMAN_API_KEY`) sent in the `X-API-Key` header; no per-call auth is needed.

**Common POST endpoints:**
- `/collections` - create a collection. Body: `{"collection": { ...collection schema... }}`. Add `?workspace={id}` to place it in a workspace.
- `/environments` - create an environment. Body: `{"environment": {"name": "Prod", "values": [...]}}`.
- `/workspaces` - create a workspace. Body: `{"workspace": {"name": "Team", "type": "team"}}`.
- `/mocks`, `/monitors` - create a mock server / monitor.
- `/import/openapi` - import an OpenAPI spec into a collection.

**Body:** pass the JSON request body as `body`. Postman wraps most resources in a top-level key (`collection`, `environment`, `workspace`).

**Query params:** several creates accept `?workspace={id}` to target a workspace.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://learning.postman.com/docs/developer/postman-api/
