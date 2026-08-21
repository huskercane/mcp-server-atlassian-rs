Run a bounded SPL search and return results as they become available.

Uses Splunk's versioned `/services/search/v2/jobs/export` endpoint with `json_rows` output. Include `earliestTime` and `latestTime` for indexed-data searches; Splunk REST searches otherwise default to all time. Keep result sets small with SPL commands such as `head`, `fields`, `table`, or aggregation commands.

The export body is consumed incrementally and normalized to an atomic canonical-NDJSON artifact with a hard stream-level byte limit, bounded head/tail preview, SHA-256 checksum, and manifest sidecar. This protects chunked responses without `Content-Length`. `jq` is not applied to streamed exports; filter the downloaded artifact instead.

Example: `{"search":"search index=main error | stats count by host | head 20","earliestTime":"-15m","latestTime":"now","jq":"rows"}`

Requires `SPLUNK_URL` and `SPLUNK_TOKEN`.

`timePartitions` (2-16) is an opt-in preview for plain event searches only (`search ...` with no pipeline or embedded time modifier). Splunk documents the event-time window as `earliest <= _time < latest`; exact RFC3339 or epoch-second bounds are therefore translated directly into adjacent half-open ranges using nine-digit decimal epoch seconds. Transforming/pipelined searches, relative, missing, invalid, reversed, or precision-ambiguous bounds retain the unchanged single-request path because partitioning could change SPL semantics. Until bounded merge/deduplication lands, the returned artifact is a conservative `partial` partition-set status naming the atomic, checksummed canonical parts, not a final combined artifact.
