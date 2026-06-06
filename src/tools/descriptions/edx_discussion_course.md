Get edX/Open edX discussion metadata for a course.

Uses `GET /api/discussion/v1/courses/{course_id}/`.

Requires `EDX_ACCESS_TOKEN`. The user behind the token must have access to the
course. Set `EDX_API_BASE` for non-edx.org Open edX LMS hosts.

Use `jq` to keep responses small, for example:
- `{enabled: discussions_enabled, topics: topics_url}`
- `{threads: thread_list_url, following: following_thread_list_url}`
