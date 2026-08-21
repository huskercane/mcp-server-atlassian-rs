# Log-shaped tool truncation

## Status

- **`circleci_logs`: fixed.** `controllers/circleci.rs::handle_logs` now calls
  `raw_response::save_artifact` (populates `raw_response_path`, previously
  hardcoded `None`) and `render_log_summary`, which does head+tail
  (`HEAD_CHARS = 5_000`, `TAIL_CHARS = 30_000`) instead of head-only, plus an
  `args.condensed` flag (PASSED/FAILED lines + surrounding context) and an
  artifact-resume path (`artifact_read`/HTTP Range) for the full log. This
  matches what the standalone `circleci` CLI's `job output get --condensed`
  already did, but now built into the tool itself.

- **Fixed: generic large responses now preserve the head and tail.**
  `truncate_for_ai` (`src/format/truncation.rs`) keeps the first 5,000 bytes
  and uses the remainder of its 40,000-byte preview budget for the response
  tail. It is called by every tool going through the generic response path,
  including `grafana_query_logs`, `splunk_search`, and
  `splunk_job_results`. For JSON-shaped API
  responses (`jira_get`, `sonarqube_search_issues`, etc.) head-truncation is
  usually tolerable — the important fields are near the top of the JSON.
  For genuinely log-shaped tools, the useful final results/errors/summary
  are therefore retained in the in-context preview. This fixes:
  - `splunk_search` (`splunk::search`) / `splunk_job_results` (`splunk::job_results`) —
    a search/job results dump ending in the aggregated/final rows.
  - `grafana_query_logs` (`grafana::query_logs`) — a log stream, same shape
    as CircleCI step output.

  These do already get a `raw_response_path` via the generic path's
  `raw_response::save(...)` (confirmed in `transport/mod.rs:270`), so unlike
  circleci_logs's old state there's always a full-content fallback file —
  but the *in-context preview* itself still shows the least useful part
  first for a large log/search result.

## Implemented follow-up

The generic helper was changed instead of adding controller-specific paths.
Small responses are unchanged, while every oversized response gains the same
head/tail behavior and retains its existing full-response artifact fallback.
