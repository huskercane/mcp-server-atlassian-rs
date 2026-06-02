Create CircleCI resources via the CircleCI REST API v2 (HTTP POST). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a personal API token (`CIRCLECI_TOKEN`) sent as a Bearer token; no per-call auth is needed.

**Common POST endpoints:**
- `/project/{project-slug}/pipeline` - trigger a pipeline. Body: `{"branch": "main"}` or `{"tag": "v1.0"}`, optionally `{"parameters": {...}}`.
- `/workflow/{workflow-id}/cancel` - cancel a running workflow (no body).
- `/workflow/{workflow-id}/rerun` - rerun a workflow. Body may include `{"from_failed": true}` or `{"jobs": [...]}`.
- `/project/{project-slug}/job/{job-number}/cancel` - cancel a job (no body).
- `/project/{project-slug}/envvar` - create an env var. Body: `{"name": "FOO", "value": "bar"}`.

**Project slug:** `<vcs>/<org>/<repo>` — e.g. `gh/acme/web`, `bb/acme/web`, or `circleci/<org-id>/<project-id>`.

**Body:** pass the JSON request body as `body`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://circleci.com/docs/api/v2/
