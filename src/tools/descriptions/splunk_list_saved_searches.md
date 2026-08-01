List reports, alerts, and other saved searches visible to the authenticated Splunk user.

Use `search` to apply a Splunk collection filter and `count`/`offset` for pagination. Filter the response with JMESPath to reduce token usage.

Example: `{"count":50,"jq":"entry[*].{name:name,search:content.search,disabled:content.disabled}"}`

Requires `SPLUNK_URL` and `SPLUNK_TOKEN`.
