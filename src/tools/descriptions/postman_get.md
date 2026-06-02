Read any Postman data via the Postman API (GET). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a Postman API key (`POSTMAN_API_KEY`) sent in the `X-API-Key` header; no per-call auth is needed.

**IMPORTANT - Cost Optimization:**
- ALWAYS use `jq` to filter response fields. Collection and workspace payloads are large.
- To explore a schema, fetch ONE item (e.g. a single collection) with NO jq, then add a jq filter on later calls.

**Common GET endpoints:**
- `/me` - the authenticated user and plan
- `/workspaces` - list workspaces; `/workspaces/{id}` for one (includes its collections/environments)
- `/collections` - list collections (each has a `uid`); `/collections/{uid}` for the full collection
- `/environments` - list environments; `/environments/{uid}` for one
- `/mocks`, `/monitors` - mock servers and monitors
- `/apis` - APIs (Postman API builder)

**IDs:** most item endpoints take a `uid` of the form `{ownerId}-{guid}`, returned in the list responses.

**Pagination:** some collection endpoints accept `limit` and `offset` query params; otherwise responses are returned whole.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

**JQ examples:** `collections[*].{uid: uid, name: name}`, `workspaces[*].name`, `environments[*].id`

API reference: https://learning.postman.com/docs/developer/postman-api/
