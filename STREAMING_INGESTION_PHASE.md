# Streaming Ingestion: Next Phase

The previous phase already implemented:

- A shared atomic stream-to-artifact writer with chunk-level decoded-byte limits, bounded head/tail previews, incremental SHA-256, same-filesystem `.part` files and atomic rename, and failed-write cleanup.
- Streamed ingestion for CircleCI action output, with bounded parallel downloads and fixed-buffer final assembly.
- Streamed ingestion for Splunk search and job-results responses.
- Streamed ingestion for Grafana/Loki query responses.
- A passing full test suite via `cargo test --all-features -j 1`.

## 1. Canonical NDJSON normalization

- Add vendor-specific streaming normalization adapters:
  - CircleCI: one canonical record per log line with project, job, step, action, sequence, optional extracted timestamp, and payload.
  - Splunk: map `json_rows` fields and rows into canonical records.
  - Loki: map each stream/value pair into canonical records with labels, nanosecond timestamp, and payload.
- Handle JSON and UTF-8 tokens crossing HTTP chunk boundaries.
- Enforce a maximum decoded record size.
- Never silently truncate oversized records. Follow an explicit fail-or-synthetic-record policy and mark completeness accordingly.

## 2. Time-partitioned ingestion

- Add bounded parallel time partitions for Splunk and Loki when callers provide usable absolute start/end bounds.
- Use precision-aware client-side half-open intervals `[start, end)`.
- Preserve existing behavior when bounds cannot be safely partitioned, such as unresolved relative Splunk times.
- Limit concurrency and enforce both per-partition and aggregate byte quotas.
- Keep current public tool contracts compatible where practical; document any necessary schema additions.

## 3. Bounded merge and deduplication

- Write each normalized partition to an atomic NDJSON part-file.
- Merge part-files lazily using a bounded K-way priority queue.
- Support chronological and reverse-chronological ordering.
- Enforce global result limits during merge.
- Deduplicate exact boundary records using a stable key such as timestamp, source identity, and record hash.
- Do not retain an unbounded deduplication set or reorder queue.

## 4. Explicit compression control

- Do not break generic non-log HTTP behavior.
- For streaming ingestion, use a client/path where Reqwest automatic gzip/Brotli/deflate/zstd decoding is disabled as supported by the enabled features.
- Count encoded HTTP payload bytes and decoded bytes separately.
- Decode incrementally with bounded buffers.
- Treat `Content-Length` only as an early rejection optimization.

## 5. Artifact manifest

Persist a JSON manifest coupled to the final artifact, including:

- artifact version and format;
- vendor and query interval;
- ordering;
- total records;
- encoded and decoded byte totals;
- SHA-256 for every canonical partition and final artifact;
- partition requested/succeeded/failed counts;
- deduplication policy and duplicate count;
- truncation/skipped-record diagnostics;
- completeness state and reason.

`completeness: "complete"` is allowed only when every partition succeeds, no limits or truncation occur, and normalization, deduplication, merge, checksum, and final commit all succeed.

## 6. Resource and lifecycle safeguards

- Enforce quotas continuously during partition writes and final merge.
- Bound individual heap records.
- Use same-filesystem temporary and final files.
- Clean incomplete parts on errors/cancellation and retain startup scavenging.
- Add request deadlines, idle-read timeouts, cancellation propagation, and bounded retry behavior.
- Account for temporary parts plus the final artifact in disk quotas.

## 7. Tests and documentation

Add focused tests for:

- chunked responses without `Content-Length`;
- compressed/decompressed byte ceilings;
- chunk-boundary UTF-8 and JSON framing;
- oversized records;
- atomic cleanup after failure;
- interval boundaries;
- out-of-order partitions;
- deduplication;
- ascending/descending merges;
- global limits;
- partial versus complete manifests;
- bounded preview and checksum correctness.

Run formatting, targeted tests, Clippy, and the full test suite with `-j 1` if Windows paging pressure recurs.

Do not claim the phase complete unless time partitioning, canonical NDJSON, bounded merge/deduplication, explicit decompression accounting, and persisted manifests are all implemented and verified. If a vendor API makes a requirement unsafe or ambiguous, preserve correctness, document the limitation, and report it explicitly.

## Implementation checkpoint (2026-08-21)

### Completed in this phase so far

- Added `src/ingestion.rs` with vendor-neutral canonical ingestion primitives:
  - `CanonicalRecord` with an optional nanosecond timestamp, source identity, payload, labels, and metadata;
  - a bounded, blocking JSON-array parser connected to async consumers through a bounded Tokio channel, preserving JSON and UTF-8 tokens across input-buffer boundaries;
  - explicit maximum canonical-record size enforcement;
  - precision-safe half-open interval generation;
  - atomic canonical NDJSON partition writing;
  - lazy K-way merge using one bounded heap record per partition;
  - chronological and reverse-chronological ordering;
  - adjacent boundary-record deduplication using timestamp, source, and payload/label hash;
  - global merge-result limits;
  - atomic manifest-sidecar persistence.
- Converted CircleCI action output to canonical NDJSON:
  - one canonical record per log line;
  - project, job, step, action, and sequence metadata;
  - optional RFC3339 timestamp extraction;
  - explicit failure for oversized canonical records.
- Added a CircleCI manifest sidecar containing record totals, checksums, partition counts, byte totals, diagnostics, deduplication policy, and completeness state.
- CircleCI manifests now use measured encoded transfer bytes and decoded bytes.
  They report `complete` only when every requested action download succeeds and
  canonical normalization, checksum, artifact commit, and manifest commit all
  succeed without truncation or a limit.
- Added focused unit coverage for interval boundaries, ascending merge/deduplication, descending merge, global limits, and oversized records.
- Fixed Windows manifest commits by syncing through a writable file handle, closing it, and then atomically renaming it.
- Added a dedicated streaming Reqwest client with automatic gzip, Brotli,
  deflate, and zstd decompression disabled while preserving the generic client.
- Added bounded incremental decoding for identity, gzip, Brotli, deflate, and
  zstd bodies with separate encoded/decoded accounting, continuous quotas,
  early-only `Content-Length` rejection, idle-read timeout, one end-to-end
  deadline, cancellation propagation, and bounded pre-body retries.
- Routed CircleCI signed action-output downloads through the explicit streaming
  transport. CircleCI manifests now use measured transfer/decompressed totals
  and are complete only when every requested action succeeds and all canonical
  commits succeed.
- Added Splunk `json_rows` canonical normalization for search export and job
  results. The bounded adapter validates one field declaration and streams rows
  through a bounded channel into atomic canonical NDJSON, with explicit row
  shape/width, timestamp, JSON, and record-size failures. `_time` accepts only
  RFC3339 or explicit epoch seconds through nanosecond precision; source
  identity uses only `source`, `sourcetype`, `host`, and `index`. Successful
  single responses persist checksummed manifests with encoded/decoded totals.
- Documented Splunk ambiguities instead of inferring semantics: `_time` is the
  only recognized timestamp field; missing `_raw` uses the complete scalar row
  as a deterministic payload for transforming/statistical searches; missing
  source fields remain absent. Relative search bounds still use exactly one
  request and no time partitioning has been introduced.
- Splunk normalization failures never skip or truncate rows. Missing,
  duplicate, empty, or out-of-order field declarations, row-width mismatches,
  nested field values, malformed JSON, invalid timestamps, oversized rows, and
  oversized canonical records fail explicitly. Canonical `.part` files are
  removed on failure, and a manifest-commit failure removes the committed
  canonical artifact rather than returning an unmanifested success.
- Added single-response Grafana/Loki canonical normalization. A nested bounded
  Serde visitor validates the successful `streams` envelope, retains only the
  current bounded label map, and sends value tuples through a bounded channel,
  preserving JSON tokens and multibyte UTF-8 across reader boundaries.
- Loki records preserve the exact payload, complete validated label map, and a
  conservatively parsed non-negative integer nanosecond timestamp. Invalid or
  duplicate labels, unsupported result types, malformed envelopes/tuples/JSON,
  invalid or overflowing timestamps, oversized source tuples, and oversized
  canonical records fail without skipping, coercion, rounding, or truncation.
- Loki source identity uses only the documented conventional label allowlist
  (`service_name`, `namespace`, `job`, `app`, `container`, `pod`, `host`,
  `instance`, and `filename`) in fixed order. With none present it is explicitly
  `loki:unknown`; the complete labels remain in the record.
- Grafana query responses continue through the dedicated streaming transport
  with separate encoded/decoded accounting, then commit canonical NDJSON and a
  checksummed single-partition manifest atomically. Canonical partials are
  removed on normalization/write/commit failures, and manifest failure removes
  the committed canonical artifact.

### Verification completed

- `cargo fmt --all`
- `cargo check --all-features`
- `cargo test --all-features ingestion::tests -j 1`
- `cargo test --all-features controllers::grafana::normalization_tests -j 1`
- `cargo test --all-features --test grafana_controller_tests -j 1`
- `cargo test --all-features --test splunk_controller_tests -j 1`
- `cargo test --all-features --test streaming_transport_tests -j 1`
- `cargo test --all-features --test circleci_controller_tests logs_fetches_build_details_and_flattens_action_output -j 1`
- `cargo test --all-features --test circleci_controller_tests logs_failed_only_skips_successful_outputs_and_condenses_errors -j 1`
- `git diff --check`

### Verification caveat

- `cargo clippy --all-features --all-targets -- -D warnings` reaches the project
  but remains blocked by five pre-existing warnings outside this completed
  slice: conversion lints in `src/transport/response_cache.rs`, a
  `too_many_lines` and `map_unwrap_or` warning in the generic transport, and a
  `map_unwrap_or` warning in the NinjaOne vendor. The two warnings introduced
  by the compression/accounting work were fixed, and the Splunk slice adds no
  new Clippy failures. The Loki slice likewise adds no new Clippy failures; the
  command still reports exactly this five-warning baseline.
- The full `cargo test --all-features -j 1` suite has not been rerun after this
  slice. Run it after the remaining vendor normalization and integration work.

### Superseded handoff

The former “safe time partitioning only” handoff was completed by the
time-partitioning checkpoint below. The authoritative next slice is now bounded
merge integration, cross-partition boundary deduplication, and final manifest
commit. Do not repeat or broadly rewrite the completed partition planner,
vendor boundary translation, strict normalization adapters, or dedicated
streaming transport.

### Important worktree note

The repository contained extensive pre-existing user modifications before this checkpoint. Preserve them. Do not reset, restore, or broadly rewrite the modified streaming/controller/transport files. Inspect `git status` and focused diffs before every overlapping edit.

## Time-partitioning checkpoint (2026-08-21)

The safe time-partitioning-only slice is implemented without invoking the
existing merge or deduplication code.

- Added backward-compatible optional `timePartitions` controls (2-16) to
  Splunk export searches and Grafana/Loki range queries. Omitted, out-of-range,
  or unsafe controls retain the exact single-request route.
- Exact RFC3339 and vendor-native numeric bounds are parsed without floating
  point conversion. Loki numeric bounds must be unsigned integer nanoseconds;
  Splunk numeric bounds are epoch seconds with at most nine fractional digits.
  Missing, relative, invalid, reversed, overflowing, over-precision, or
  otherwise ambiguous bounds fall back to one request.
- Client partitions are checked, contiguous nanosecond `[start,end)` ranges.
  Width and aggregate counters use checked arithmetic.
- Vendor boundary semantics are directly compatible with the client model:
  Loki documents `start <= timestamp < end` at
  https://grafana.com/docs/loki/latest/reference/loki-http-api/#query-logs-within-a-range-of-time;
  Splunk documents `earliest <= _time < latest` at
  https://help.splunk.com/en/splunk-enterprise/search/spl-search-reference/10.4/time-format-variables-and-modifiers/time-modifiers.
  Loki receives exact integer nanoseconds; Splunk receives exact epoch seconds
  with nine fractional digits. No endpoint is rounded or adjusted.
- Splunk partitioning is restricted to plain `search ...` event searches with
  no pipeline or embedded time modifiers. Transforming SPL is not distributive
  over time ranges and therefore stays on one request.
- Loki partitioning additionally requires an explicit global limit. Partitions
  execute in the requested forward/backward order with a decreasing remaining
  limit, so the global limit is not independently applied to every partition.
  Omitting a limit stays on one request because Loki's implicit per-request cap
  cannot prove completeness across partitions.
- Execution is deliberately bounded to one active partition in this slice.
  This preserves ordered global-limit semantics and ensures a failure, quota
  violation, or cancellation prevents every later request from being
  scheduled. Each request retains the dedicated transport deadline,
  idle-timeout, cancellation, bounded pre-body retry, no-retry-after-body, and
  explicit decompression behavior.
- Added shared checked aggregate encoded/decoded counters enforced for every
  transport chunk, in addition to the existing continuous per-request limits.
  Canonical parts retain the per-partition continuous write ceiling; aggregate
  canonical temporary/projected-final bytes are checked after every atomic
  part commit before another request is scheduled.
- Every successful response passes through the strict existing Splunk or Loki
  canonical adapter. Each retained NDJSON part is atomic and has its own
  checksum manifest. The atomic partition-set status maps each exact interval
  to its artifact and sidecar path and reports `completeness: partial` because
  merge, boundary deduplication, and final assembly have not run.
- Transport inputs, incomplete canonical files, successfully retained parts,
  and their sidecars are cleaned on the applicable transport, normalization,
  quota, status-write, or status-commit failure paths. No partial success is
  returned as complete.

### Next required item

Implement bounded merge integration and cross-partition boundary
deduplication. Enforce the true aggregate final-disk ceiling during that merge,
replace the interim partition-set status with the final multi-partition
manifest, broaden lifecycle/scavenging integration, and then run the full test
suite. Do not claim the overall streaming-ingestion phase complete until those
items and full-suite verification are finished.

### Time-partitioning verification

- `cargo fmt --all`
- `cargo test --all-features ingestion::tests -j 1`
- `cargo test --all-features controllers::splunk::normalization_tests -j 1`
- `cargo test --all-features controllers::grafana::normalization_tests -j 1`
- `cargo test --all-features --test splunk_controller_tests -j 1`
- `cargo test --all-features --test grafana_controller_tests -j 1`
- `cargo test --all-features --test streaming_transport_tests -j 1`
- `cargo test --all-features --test tool_schema_tests -j 1`
- `cargo check --all-features`
- `git diff --check`

Clippy again reports exactly the documented five-warning baseline: two
conversion warnings in `transport/response_cache.rs`, the generic transport
`too_many_lines` and `map_unwrap_or` warnings, and the NinjaOne
`map_unwrap_or` warning. This slice adds no Clippy warning. The full suite was
not run because bounded merge/final assembly and the remaining lifecycle work
are intentionally still pending.

## Next implementation slice: bounded merge and final multi-partition artifacts

Treat the time-partitioning checkpoint above as authoritative. Integrate only
the already-normalized Splunk and Loki partition artifacts into the existing
bounded merge primitives. Do not redesign safe partition selection, relax any
single-request fallback, or expand partition eligibility in this slice.

- Replace the interim partition-set status artifact with one atomic canonical
  NDJSON final artifact plus an atomic final manifest. Do not materialize all
  records or an entire partition in memory during assembly.
- Use a bounded K-way merge with at most one bounded record per open partition
  in the priority queue. Preserve chronological and reverse-chronological
  ordering exactly, including deterministic tie handling.
- Apply the caller's result limit once, globally, during ordered merge. A limit
  reached intentionally must be represented in completeness/diagnostics and
  must not be confused with an upstream or normalization failure.
- Deduplicate only exact cross-partition boundary duplicates using the stable
  canonical identity (`timestamp`, source identity, and payload/labels hash).
  Keep deduplication state bounded. Never suppress distinct records sharing a
  timestamp, and never silently deduplicate records inside one partition.
- Validate each retained partition and its checksum/manifest relationship
  before merging. A missing, changed, malformed, or uncommitted part must fail
  explicitly; it must never be skipped.
- Continuously enforce checked aggregate quotas for retained canonical parts,
  the in-progress final artifact, projected final bytes, and total temporary
  plus final disk usage. Do not rely on filesystem metadata as the only
  in-progress final-write enforcement.
- Preserve cancellation, request deadlines, idle timeouts, bounded pre-body
  retry behavior, and the rule that a response is never retried after body
  consumption. Stop scheduling/processing new work after failure,
  cancellation, or quota violation.
- Make cleanup transactional and unambiguous. Remove incomplete final files,
  manifest partials, transport inputs, and canonical partition artifacts after
  every failed merge/checksum/commit/cancellation path. Remove successful
  partition artifacts only after both the final artifact and final manifest
  commit successfully. If final-manifest commit fails, remove the committed
  final artifact and retain no success result.
- Final manifests must include the exact query interval, requested ordering,
  total output records, encoded and decoded transfer totals, canonical
  partition byte totals and SHA-256 values, final bytes and SHA-256,
  requested/succeeded/failed partition counts, global-limit diagnostics,
  deduplication policy and duplicate count, skipped/truncated diagnostics, and
  a conservative completeness state/reason.
- `completeness: complete` is allowed only when all required partitions,
  normalization, checksum validation, merge, deduplication, quota checks, final
  artifact commit, and manifest commit succeed, and no result limit or
  truncation changes the complete result. Partial partition success alone is
  never complete.
- Preserve existing single-request output contracts and manifests. Revalidate
  CircleCI manifests if shared ingestion, transport, artifact, or manifest code
  changes. Do not modify unrelated files to address the known Clippy baseline.

Add focused tests for ascending and descending multi-partition merges,
out-of-order completion, exact boundary duplicates, same-timestamp distinct
records, bounded heap/dedup state, global limits in both directions, empty
partitions, checksum mismatch, missing parts, malformed canonical NDJSON,
middle-partition failure, cancellation during merge, final-write and manifest
commit failures, per-partition plus aggregate disk quotas, projected-final and
checked-counter overflow, transactional cleanup, final checksums/accounting,
complete versus limited/partial/failed manifests, and unchanged single-request
Splunk/Loki contracts.

Run:

- `cargo fmt --all`
- targeted ingestion, Splunk controller, Grafana controller, raw-artifact, and
  streaming-transport tests
- `cargo check --all-features`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo test --all-features -j 1`
- `git diff --check`

Preserve and report the exact five-warning pre-existing Clippy baseline. Do not
claim the overall streaming-ingestion phase complete unless this slice passes
the full suite and the final artifact/manifest lifecycle is demonstrably
transactional.
