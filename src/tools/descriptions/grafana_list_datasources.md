List the datasources configured in Grafana. Returns TOON format by default (30-60% fewer tokens than JSON).

Use this to discover the Loki datasource `uid` that `grafana_query_logs` requires. Each entry includes `id`, `uid`, `name`, and `type` (e.g. `loki`, `prometheus`).

Authenticates with a Grafana **service-account token** (`GRAFANA_TOKEN`) sent as `Authorization: Bearer`; `GRAFANA_URL` sets the base (e.g. `https://myorg.grafana.net` or `http://localhost:3000`).

**Typical use — find Loki datasources:**
- `jq`: `[?type=='loki'].{name: name, uid: uid}`

Then pass the chosen `uid` to `grafana_query_logs` as `datasourceUid`.

**Output format:** TOON (default) or JSON (`outputFormat: "json"`).

API reference: https://grafana.com/docs/grafana/latest/developers/http_api/data_source/#get-all-data-sources
