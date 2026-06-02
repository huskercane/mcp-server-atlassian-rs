Partially update CircleCI resources via the CircleCI REST API v2 (HTTP PATCH). Returns TOON format by default (30-60% fewer tokens than JSON).

Authenticates with a personal API token (`CIRCLECI_TOKEN`) sent as a Bearer token; no per-call auth is needed.

**Common PATCH endpoints:**
- `/project/{project-slug}/schedule/{schedule-id}` - update a scheduled pipeline (name, timetable, parameters).

**Project slug:** `<vcs>/<org>/<repo>` — e.g. `gh/acme/web`, `bb/acme/web`, or `circleci/<org-id>/<project-id>`.

**Body:** pass the JSON request body as `body`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://circleci.com/docs/api/v2/
