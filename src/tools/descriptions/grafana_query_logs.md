Read logs from Grafana by running a LogQL query against a Loki datasource. Returns TOON format by default (30-60% fewer tokens than JSON).

The upstream response is consumed with explicit encoded/decoded byte limits, including chunked and compressed responses, then normalized incrementally to atomic canonical NDJSON with a bounded preview, SHA-256 checksum, and manifest sidecar. Because the response is never materialized as one JSON value, `jq` is not applied on this streamed path; filter the downloaded artifact instead.

Only Loki's successful `streams` result shape is accepted. Matrix, vector, scalar, malformed, duplicate, or ambiguous structures fail the request; records are never skipped, coerced, rounded, or truncated. Each canonical record preserves the complete validated stream-label map, exact payload, and a non-negative base-10 nanosecond timestamp that fits in `u64`. Fractional, signed, negative, or overflowing timestamps are rejected.

Source identity uses only the conventional Loki labels `service_name`, `namespace`, `job`, `app`, `container`, `pod`, `host`, `instance`, and `filename`, in that fixed order. Loki does not mandate a universal source-label set: when none is present, the source is `loki:unknown`, while the full label map is still preserved. Arbitrary tenant labels are deliberately not promoted into identity. Loki/Grafana may add unrelated envelope fields, which are ignored; canonical log normalization does not infer logs from metric result types or alternate tuple shapes.

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

`timePartitions` (2-16) is an opt-in preview of safe partition ingestion. It activates only when both bounds are exact RFC3339 or integer Unix-nanosecond instants and an explicit global `limit` is present. Loki documents `start` as inclusive and `end` as exclusive, so each request maps directly to an adjacent client interval `[start,end)` without rounding. Partitions are processed in the requested direction with a decreasing remaining limit. Until bounded merge/deduplication lands, the returned artifact is a conservative `partial` partition-set status that names atomic, checksummed canonical parts; it is not a final combined log artifact. Relative, missing, fractional numeric, invalid, reversed, or otherwise ambiguous bounds use the unchanged single-request path.
