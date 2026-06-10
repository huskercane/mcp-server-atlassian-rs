Query New Relic via the NerdGraph GraphQL API. Returns TOON format by default (30-60% fewer tokens than JSON).

NerdGraph is New Relic's single GraphQL endpoint — NRQL queries, entity search, dashboards, alerts, and account data are all driven through it. This one tool POSTs your GraphQL document (plus optional `variables`) to `/graphql`.

Authenticates with a New Relic **User API key** (`NEW_RELIC_API_KEY`) sent in the `API-Key` header; no per-call auth is needed. EU-region accounts must set `NEW_RELIC_REGION=eu`.

**IMPORTANT - Cost Optimization:**
- ALWAYS request only the fields you need in the GraphQL selection set, and use `jq` to trim the response further.
- To explore, request a small selection first, then widen it on later calls.

**NerdGraph quirks you must know:**
- A request can **fail with HTTP 200** and a top-level `errors` array (query syntax, NRQL, or permission failures). This tool reclassifies a non-empty `errors` array as an error automatically, so a successful result has no `errors`.
- You need the numeric **account id** for most queries. Find it with: `{ actor { accounts { id name } } }`.

**Running NRQL** (the common case) — wrap the NRQL string in a NerdGraph query:
```graphql
{ actor { account(id: 1234567) { nrql(query: "SELECT count(*) FROM Transaction SINCE 1 hour ago") { results } } } }
```
Prefer `variables` for the account id and NRQL string instead of string-interpolating them:
- query: `query($id: Int!, $q: Nrql!) { actor { account(id: $id) { nrql(query: $q) { results } } } }`
- variables: `{"id": 1234567, "q": "SELECT average(duration) FROM Transaction TIMESERIES SINCE 1 day ago"}`

**Other common queries:**
- List accounts: `{ actor { accounts { id name } } }`
- Entity search: `{ actor { entitySearch(query: "domain = 'APM' AND name LIKE '%api%'") { results { entities { guid name entityType } } } } }`
- Current user: `{ actor { user { name email } } }`

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

**JQ examples:** `data.actor.account.nrql.results`, `data.actor.entitySearch.results.entities[*].{guid: guid, name: name}`

API reference: https://docs.newrelic.com/docs/apis/nerdgraph/get-started/introduction-new-relic-nerdgraph/
