List or search edX/Open edX discussion threads.

Uses `GET /api/discussion/v1/threads/` with `course_id` and optional filters.

Common filters:
- `topicId`: threads in a specific topic
- `textSearch`: search matching thread/comment text
- `following`: only followed threads
- `view`: `unread` or `unanswered`
- `orderBy`: `last_activity_at`, `comment_count`, or `vote_count`

`topicId`, `following`, and `textSearch` are mutually exclusive in the edX
Discussion API. Requires `EDX_ACCESS_TOKEN` and course discussion access.
