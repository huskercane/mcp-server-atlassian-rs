Report a SonarQube/SonarCloud **quality gate** result — the failing conditions that explain *why a CI build failed Sonar*. Returns TOON format by default (30-60% fewer tokens than JSON).

Calls `GET /api/qualitygates/project_status`. The response's `projectStatus.status` is `OK`/`ERROR`, and `projectStatus.conditions[]` lists each condition with its `metricKey`, `comparator`, `errorThreshold`, and the `actualValue` — e.g. `new_coverage` `LT` `80` with `actualValue: 63.4`. That is the literal failure reason.

Authenticates with a Sonar **user token** (`SONARQUBE_TOKEN`) sent as `Authorization: Bearer`; `SONARQUBE_URL` sets the base (`https://sonar.mycorp.com` or `https://sonarcloud.io`). No per-call auth is needed.

**Identify the analysis with exactly one of:**
- `analysisId` — used as-is.
- `ceTaskId` — the scanner compute-engine task id printed in the CI log's `report-task.txt` (grab it from `circleci_logs`). This tool resolves it to an `analysisId` via `/api/ce/task` for you, so you can go straight from a CircleCI log to the gate result.
- `projectKey` — with optional `branch` **or** `pullRequest` (mutually exclusive). Alone, it reports the main branch's latest analysis.

**Typical flow after a red build:** `circleci_logs` (find the failed Sonar step and its `ceTaskId`) → `sonarqube_quality_gate` (failing conditions) → `sonarqube_search_issues` (the exact offending lines).

**SonarCloud:** pass `organization`. Omit it for self-hosted SonarQube.

**IMPORTANT - Cost Optimization:** use `jq` to keep only what you need, e.g. `projectStatus.conditions[?status=='ERROR']`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://next.sonarqube.com/sonarqube/web_api/api/qualitygates/project_status
