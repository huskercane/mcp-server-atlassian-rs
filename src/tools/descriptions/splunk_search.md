Run a bounded SPL search and return results as they become available.

Uses Splunk's versioned `/services/search/v2/jobs/export` endpoint with `json_rows` output. Include `earliestTime` and `latestTime` for indexed-data searches; Splunk REST searches otherwise default to all time. Keep result sets small with SPL commands such as `head`, `fields`, `table`, or aggregation commands.

Example: `{"search":"search index=main error | stats count by host | head 20","earliestTime":"-15m","latestTime":"now","jq":"rows"}`

Requires `SPLUNK_URL` and `SPLUNK_TOKEN`.
