Fetch raw CircleCI step logs for a job. Use this after `circleci_get /workflow/{workflow-id}/job` gives you a job's `job_number`.

Inputs:
- `projectSlug`: CircleCI project slug in v2 form, e.g. `gh/acme/web` or `bb/acme/web`.
- `jobNumber`: Numeric `job_number` from the workflow job list.

This tool uses CircleCI's older build-details API to discover per-step output URLs, then fetches and flattens those step outputs into readable log text. `circleci/<org-id>/<project-id>` slugs are not supported because CircleCI's older log endpoint is VCS-path based.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).
