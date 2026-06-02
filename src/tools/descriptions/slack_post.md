Call a Slack Web API write method via HTTP POST. Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a bot/user OAuth token (`SLACK_TOKEN`, e.g. `xoxb-…`) sent as a Bearer token; no per-call auth is needed.

**Slack quirks you must know:**
- Endpoints are *methods* (`/chat.postMessage`), not REST resources. Almost all Slack writes are POST regardless of semantics.
- The request body is sent as JSON (`Content-Type: application/json`), which Slack accepts for Bearer-authenticated calls.
- A request can **fail with HTTP 200** and `{"ok": false, "error": "<code>"}`. This tool reclassifies `ok: false` as an error automatically.
- Writing requires the matching bot scope (e.g. `chat:write`) and the bot must be a member of the target channel.

**Common POST methods:**
- `/chat.postMessage` - send a message. Body: `{"channel": "C123", "text": "hi"}` (or `blocks`).
- `/chat.update` - edit a message. Body: `{"channel": "C123", "ts": "...", "text": "..."}`.
- `/chat.postEphemeral` - message visible only to one user. Body adds `{"user": "U123"}`.
- `/conversations.create` - create a channel. Body: `{"name": "my-channel"}`.
- `/conversations.invite` - add users. Body: `{"channel": "C123", "users": "U1,U2"}`.
- `/reactions.add` - add an emoji reaction. Body: `{"channel": "C123", "timestamp": "...", "name": "thumbsup"}`.

**Body:** pass the JSON request body as `body`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://api.slack.com/web
