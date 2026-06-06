List comments/responses for an edX/Open edX discussion thread.

Uses `GET /api/discussion/v1/comments/` with `thread_id`.

Supports `endorsed`, `pageSize`, and `page`. Requires `EDX_ACCESS_TOKEN`; the
user behind the token must have access to the thread's course discussion.

Use `jq` to extract compact fields such as authors, bodies, vote counts, and
child comments.
