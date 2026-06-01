Create Zoom resources via POST to the Zoom REST API v2. Pass the JSON request body in `body`.

Authenticates with Server-to-Server OAuth (auto-renewed bearer).

**Create a meeting:** `POST /users/me/meetings`
```json
{
  "topic": "Project sync",
  "type": 2,
  "start_time": "2026-06-02T15:00:00Z",
  "duration": 30,
  "timezone": "UTC",
  "settings": { "join_before_host": true, "waiting_room": false }
}
```
- `type`: 1 = instant, 2 = scheduled, 3 = recurring (no fixed time), 8 = recurring (fixed time).
- `start_time` is ISO-8601. Provide `timezone` for local times.
- The response includes `id`, `join_url`, and `start_url`. **Starting a meeting is not an API action** — the host opens the returned `start_url` (it launches the Zoom client). Hand that URL to the user; the API cannot press "start" for them.

**Other common creates:**
- `POST /meetings/{meetingId}/registrants` - add a registrant
- `POST /users/me/meetings/{meetingId}/invite_links` - generate join links
- `POST /users` - create a user (account admin)

**Tip:** filter the response with `jq` (e.g. `{id: id, join: join_url, start: start_url}`) to avoid returning the full payload.

API reference: https://developers.zoom.us/docs/api/
