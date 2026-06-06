Create an edX/Open edX discussion thread.

Uses `POST /api/discussion/v1/threads/`.

Required fields are `courseId`, `topicId`, `type` (`discussion` or `question`),
`title`, and `rawBody`. Some courses may block posting during blackouts or after
course/discussion closure. Requires `EDX_ACCESS_TOKEN` and course discussion
access.
