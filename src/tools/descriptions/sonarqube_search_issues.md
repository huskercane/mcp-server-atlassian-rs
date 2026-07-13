Search SonarQube/SonarCloud **issues** (bugs, vulnerabilities, code smells) for a project — the specific lines behind a quality-gate failure. Returns TOON format by default (30-60% fewer tokens than JSON).

Calls `GET /api/issues/search`. Each returned issue carries its `rule`, `severity`, `type`, `message`, `component` (file), and `line`, so "2 new bugs" becomes "unclosed resource at `Foo.java:41`".

Authenticates with a Sonar **user token** (`SONARQUBE_TOKEN`) sent as `Authorization: Bearer`; `SONARQUBE_URL` sets the base. No per-call auth is needed.

**Parameters:**
- `componentKeys` (required): usually the project key, e.g. `my-org_my-repo`. Comma-separate for multiple.
- `branch` **or** `pullRequest` (mutually exclusive): scope to that analysis. For a red PR build, pass the `pullRequest` id to get exactly the issues that failed it.
- `types`: `BUG`, `VULNERABILITY`, `CODE_SMELL` (comma-separated).
- `severities`: `INFO`, `MINOR`, `MAJOR`, `CRITICAL`, `BLOCKER`.
- `statuses`: e.g. `OPEN,CONFIRMED,REOPENED`.
- `resolved`: `false` for open issues only.
- `pageSize`: Sonar's `ps` (max 500) — keep small.

**SonarCloud:** pass `organization`. Omit it for self-hosted SonarQube.

**IMPORTANT - Cost Optimization:** use `jq` to keep only what you need, e.g. `issues[*].{rule: rule, msg: message, file: component, line: line}`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://next.sonarqube.com/sonarqube/web_api/api/issues/search
