Read any Zoom data via the Zoom REST API v2. Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with Server-to-Server OAuth; the server mints and auto-renews the bearer token, so no per-call auth is needed.

**IMPORTANT - Cost Optimization:**
- ALWAYS use `jq` to filter response fields. Unfiltered responses are expensive.
- Use `page_size` query param to cap result count (e.g., `page_size: "30"`, max 300).
- To explore a schema, fetch ONE item with `page_size: "1"` and NO jq, then add a jq filter on later calls.

**Checking the schedule / listing / "search":** Zoom has no single search tool — listing and searching are just GETs against the right path:
- `/users/me/meetings` - your scheduled/upcoming meetings (use `type` query param: `scheduled`, `upcoming`, `upcoming_meetings`, `previous_meetings`)
- `/users/{userId}/meetings` - another user's meetings (host must be in your account)
- `/meetings/{meetingId}` - a single meeting's details (incl. `join_url`)
- `/users` - list account users (`status`, `role_id`, `page_size`, `next_page_token`)
- `/users/me` - the authenticated user
- `/contacts?search_key={query}` - search contacts (Contacts scope required)
- `/users/me/recordings` - cloud recordings (`from`/`to` date params)
- `/report/users/{userId}/meetings` - meeting reports (`from`/`to`)

**Pagination:** Zoom uses `page_size` + `next_page_token` (token-based) for most list endpoints. Pass the returned `next_page_token` as a query param to fetch the next page.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

**JQ examples:** `meetings[*].{id: id, topic: topic, start: start_time, join: join_url}`, `meetings[*].topic`, `users[*].email`

API reference: https://developers.zoom.us/docs/api/
