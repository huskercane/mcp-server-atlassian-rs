Replace/update Zoom resources via PUT to the Zoom REST API v2. Pass the JSON request body in `body`.

Authenticates with Server-to-Server OAuth (auto-renewed bearer).

PUT is idempotent — used where Zoom expects a full-representation update. Most meeting edits use PATCH (see `zoom_patch`); PUT is used for settings-style endpoints, e.g.:
- `PUT /users/{userId}/settings` - update a user's settings
- `PUT /meetings/{meetingId}/status` - end/recover a meeting (`{"action": "end"}`)
- `PUT /meetings/{meetingId}/recordings/status` - recover recordings

Zoom write endpoints typically return `204 No Content` on success (empty body).

API reference: https://developers.zoom.us/docs/api/
