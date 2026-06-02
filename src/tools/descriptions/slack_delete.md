Send an HTTP DELETE to the Slack Web API. Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a bot/user OAuth token (`SLACK_TOKEN`, e.g. `xoxb-…`) sent as a Bearer token; no per-call auth is needed.

**Note:** Slack does not delete via the HTTP DELETE verb — deletion is a POST *method* instead. Prefer `slack_post`:
- `/chat.delete` - delete a message. Body: `{"channel": "C123", "ts": "..."}`.
- `/reactions.remove` - remove a reaction. Body: `{"channel": "C123", "timestamp": "...", "name": "thumbsup"}`.

This verb exists for completeness and takes no body. A request can fail with HTTP 200 and `{"ok": false, "error": "<code>"}`, which is reclassified as an error automatically.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://api.slack.com/web
