Generic `GET` against the SonarQube/SonarCloud Web API for endpoints without a dedicated tool. Returns TOON format by default (30-60% fewer tokens than JSON).

Use the dedicated tools first — `sonarqube_quality_gate` (why a build failed) and `sonarqube_search_issues` (the offending lines). Reach for this for the long tail:
- `/api/projects/search` — discover project keys.
- `/api/measures/component` — coverage, duplication, ncloc, etc. (`metricKeys=coverage,duplicated_lines_density`).
- `/api/hotspots/search` — security hotspots.
- `/api/ce/task` — raw compute-engine task status by `id`.

Authenticates with a Sonar **user token** (`SONARQUBE_TOKEN`) sent as `Authorization: Bearer`; `SONARQUBE_URL` sets the base. No per-call auth is needed.

**Parameters:**
- `path`: the API endpoint path starting with `/`, e.g. `/api/measures/component`.
- `queryParams`: key-value pairs, e.g. `{"component": "my-project", "metricKeys": "coverage,bugs"}`.
- `jq`: JMESPath to keep only the fields you need (always use this to cut token costs).

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://next.sonarqube.com/sonarqube/web_api
