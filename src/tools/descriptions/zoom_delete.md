Delete Zoom resources via DELETE to the Zoom REST API v2.

Authenticates with Server-to-Server OAuth (auto-renewed bearer).

**Cancel/delete a meeting:** `DELETE /meetings/{meetingId}`
- Optional query params: `occurrence_id` (delete a single occurrence of a recurring meeting), `schedule_for_reminder=true` (email the host/alternative hosts), `cancel_meeting_reminder=true` (notify registrants).

**Other common deletes:**
- `DELETE /meetings/{meetingId}/registrants/{registrantId}` - remove a registrant
- `DELETE /users/{userId}` - delete/deactivate a user (account admin; may need `action` query param)
- `DELETE /meetings/{meetingId}/recordings` - delete cloud recordings

Zoom returns `204 No Content` on success (empty body). This is a destructive, irreversible operation — confirm the target before calling.

API reference: https://developers.zoom.us/docs/api/
