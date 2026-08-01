Start an asynchronous Splunk SPL search and return its search ID (`sid`).

Use this for searches that may take longer or produce results that should be paged. Pass the returned `sid` to `splunk_job_results`. Include explicit `earliestTime` and `latestTime` bounds for indexed-data searches.

Example: `{"search":"search index=main sourcetype=access_combined | stats count by status","earliestTime":"-24h@h","latestTime":"now","jq":"sid"}`

Requires `SPLUNK_URL` and `SPLUNK_TOKEN`.
