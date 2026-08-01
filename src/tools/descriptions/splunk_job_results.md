Fetch a page of results for a Splunk search job.

Pass the `sid` returned by `splunk_create_job`. Use `count` and `offset` to page large result sets. If the search has not completed, Splunk may return no final results yet.

Example: `{"sid":"1712345678.42","count":100,"offset":0,"jq":"rows"}`

Requires `SPLUNK_URL` and `SPLUNK_TOKEN`.
