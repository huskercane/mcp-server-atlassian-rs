Create a response/comment in an edX/Open edX discussion thread.

Uses `POST /api/discussion/v1/comments/`.

Provide `threadId` to respond to a thread, or `parentId` for a nested comment
when the target Open edX instance supports that shape. `rawBody` is required.
Requires `EDX_ACCESS_TOKEN` and discussion posting access.
