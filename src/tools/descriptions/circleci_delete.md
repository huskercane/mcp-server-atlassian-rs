Delete CircleCI resources via the CircleCI REST API v2 (HTTP DELETE). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a personal API token (`CIRCLECI_TOKEN`) sent as a Bearer token; no per-call auth is needed.

**Common DELETE endpoints:**
- `/project/{project-slug}/envvar/{name}` - delete a project environment variable.
- `/project/{project-slug}/schedule/{schedule-id}` - delete a scheduled pipeline.
- `/context/{context-id}` - delete a context.

**Project slug:** `<vcs>/<org>/<repo>` — e.g. `gh/acme/web`, `bb/acme/web`, or `circleci/<org-id>/<project-id>`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://circleci.com/docs/api/v2/
