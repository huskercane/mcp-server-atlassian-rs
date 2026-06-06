Get edX/Open edX discussion topics for a course.

Uses `GET /api/discussion/v1/course_topics/{course_id}/`.

Returns courseware topics and non-courseware topics, including topic IDs and
thread-list URLs. Requires `EDX_ACCESS_TOKEN`, and the user behind the token
must have course discussion access.

Use `jq` to extract only the topic names/IDs you need.
