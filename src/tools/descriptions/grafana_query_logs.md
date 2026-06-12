Read logs from Grafana by running a LogQL query against a Loki datasource. Returns TOON format by default (30-60% fewer tokens than JSON).

Grafana itself does not store logs — it queries a Loki backend. This tool runs your LogQL through Grafana's **datasource proxy** (`/api/datasources/proxy/uid/{uid}/loki/api/v1/query_range`), so Grafana's auth and datasource configuration stay in charge. Works the same for self-hosted Grafana and Grafana Cloud.

Authenticates with a Grafana **service-account token** (`GRAFANA_TOKEN`) sent as `Authorization: Bearer`; `GRAFANA_URL` sets the base (e.g. `https://myorg.grafana.net` or `http://localhost:3000`). No per-call auth is needed.

**You need the Loki datasource `uid`.** Discover it with `grafana_list_datasources` (look for an entry with `type: "loki"` and copy its `uid`), then pass it as `datasourceUid`.

**IMPORTANT - Cost Optimization:**
- Set a small `limit` (default 100) and a tight time range (`start`/`end`).
- Use `jq` to keep only the fields you need from the response.

**LogQL examples:**
- Log lines: `{app="api"} |= "error"`
- Filter out noise: `{namespace="prod"} != "healthcheck"`
- Metric over time: `sum by (level) (count_over_time({app="api"}[5m]))`

**Parameters:**
- `start` / `end`: RFC3339 (`2024-01-01T00:00:00Z`) or Unix nanoseconds. Omit to use Loki's defaults (last hour → now).
- `limit`: max log lines (default 100).
- `direction`: `backward` (newest first, default) or `forward`.
- `step`: resolution for metric queries (e.g. `30s`); ignored for plain log selectors.

**Response shape:** Loki returns `{ "status": "success", "data": { "resultType": "streams"|"matrix", "result": [ { "stream": {labels}, "values": [[ts, line], …] } ] } }`. A bad LogQL query comes back as an HTTP error with a `{"error": …}` message.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

**JQ examples:** `data.result[*].values`, `data.result[*].{labels: stream, lines: values}`

API reference: https://grafana.com/docs/loki/latest/reference/loki-http-api/#query-logs-within-a-range-of-time
