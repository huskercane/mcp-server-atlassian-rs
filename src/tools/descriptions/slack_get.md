Read any Slack data via the Slack Web API (GET). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a bot/user OAuth token (`SLACK_TOKEN`, e.g. `xoxb-…`) sent as a Bearer token; no per-call auth is needed.

**IMPORTANT - Cost Optimization:**
- ALWAYS use `jq` to filter response fields. Unfiltered responses are expensive.
- To explore a schema, fetch ONE item (or a single page) with NO jq, then add a jq filter on later calls.

**Slack quirks you must know:**
- Endpoints are *methods* (`/conversations.list`, `/users.info`), not REST resources.
- A request can **fail with HTTP 200** and `{"ok": false, "error": "<code>"}` in the body. This tool reclassifies `ok: false` as an error automatically, so a successful result always has `ok: true`.
- Most read methods take inputs as **query params** (`queryParams`), not a body.

**Common GET methods:**
- `/auth.test` - verify the token and see the authed user/team
- `/conversations.list` - channels (`types=public_channel,private_channel`, `limit`, `cursor`)
- `/conversations.history` - messages in a channel (`channel`, `limit`, `oldest`, `latest`)
- `/conversations.replies` - a thread (`channel`, `ts`)
- `/users.list` / `/users.info` - users (`user` for info)
- `/users.conversations` - channels a user is in

**Pagination:** Slack uses cursor pagination. Read `response_metadata.next_cursor` and pass it back as the `cursor` query param.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

**JQ examples:** `channels[*].{id: id, name: name}`, `members[*].name`, `messages[*].{user: user, text: text, ts: ts}`

API reference: https://api.slack.com/web
