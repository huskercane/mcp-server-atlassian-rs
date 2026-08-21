Fetch raw CircleCI step logs for a job. Use this after `circleci_get /workflow/{workflow-id}/job` gives you a job's `job_number`.

Inputs:
- `projectSlug`: CircleCI project slug in v2 form, e.g. `gh/acme/web` or `bb/acme/web`.
- `jobNumber`: Numeric `job_number` from the workflow job list.
- `stepNumber` (optional): fetch only one one-based step number.
- `failedOnly` (optional): fetch only failed/non-zero-exit actions, avoiding downloads for successful actions.
- `condensed` (optional): return error-like lines with surrounding context.
- `contextLines` (optional): surrounding lines for condensed output (default 3, maximum 20).

This tool uses CircleCI's older build-details API to discover per-step output URLs, then fetches and flattens those step outputs into readable log text. `circleci/<org-id>/<project-id>` slugs are not supported because CircleCI's older log endpoint is VCS-path based.

The complete selected logs are atomically saved to a process-owned temporary file. The response includes its local path, opaque artifact ID, and HTTP download path. HTTP clients can resume with standard Range requests; other MCP clients can call `artifact_read` repeatedly with `nextOffset`. The inline response contains a diagnostic summary plus either a small head/large tail preview or condensed error context. Temporary artifacts are deleted on graceful process exit; abandoned artifacts are removed the next time the server starts.

Selected action outputs are fetched with bounded concurrency while preserving CircleCI order. Transient connection/timeouts and HTTP 429/502/503/504 responses are retried up to three attempts with backoff and `Retry-After` support.

Action bodies stream directly into atomic part artifacts with chunk-level byte accounting. The ordered final JSON artifact is assembled from those files with a fixed-size copy buffer, avoiding retention of complete action bodies and a second full serialized copy in memory.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).
