Fetch a page of results for a Splunk search job.

Pass the `sid` returned by `splunk_create_job`. Use `count` and `offset` to page large result sets. If the search has not completed, Splunk may return no final results yet.

The result body is streamed and normalized to an atomic canonical-NDJSON artifact with a hard byte limit, bounded head/tail preview, SHA-256 checksum, and manifest sidecar. `jq` is not applied to this streamed response.

Example: `{"sid":"1712345678.42","count":100,"offset":0,"jq":"rows"}`

Requires `SPLUNK_URL` and `SPLUNK_TOKEN`.
