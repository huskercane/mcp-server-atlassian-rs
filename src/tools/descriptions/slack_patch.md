Send an HTTP PATCH to the Slack Web API. Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a bot/user OAuth token (`SLACK_TOKEN`, e.g. `xoxb-…`) sent as a Bearer token; no per-call auth is needed.

**Note:** The Slack Web API is almost entirely GET and POST — there is rarely a reason to use PATCH. To edit a message use `slack_post` with `/chat.update`. This verb exists for completeness; a request can still fail with HTTP 200 and `{"ok": false, "error": "<code>"}`, which is reclassified as an error automatically.

**Body:** pass the JSON request body as `body`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://api.slack.com/web
