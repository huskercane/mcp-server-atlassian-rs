Partially update Zoom resources via PATCH to the Zoom REST API v2. Pass only the fields to change in `body`.

Authenticates with Server-to-Server OAuth (auto-renewed bearer).

**Update (reschedule) a meeting:** `PATCH /meetings/{meetingId}`
```json
{ "start_time": "2026-06-03T16:00:00Z", "duration": 45, "topic": "Project sync (moved)" }
```
- Only the supplied fields change. Use `occurrence_id` as a query param to edit a single occurrence of a recurring meeting.

**Other common patches:**
- `PATCH /webinars/{webinarId}` - update a webinar
- `PATCH /users/{userId}` - update user profile fields

Zoom returns `204 No Content` on success (empty body).

API reference: https://developers.zoom.us/docs/api/
