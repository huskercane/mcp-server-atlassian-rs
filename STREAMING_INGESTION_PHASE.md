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

## Bounded merge and final-artifact checkpoint (2026-08-21)

The bounded merge and final multi-partition artifact slice is implemented and
committed as `5a034ae`.

### Completed

- Splunk and Loki partitioned requests now return one atomic canonical NDJSON
  artifact and one atomic final manifest. The interim
  `canonical_ndjson_partition_set` status artifact is no longer produced.
- Every retained partition is streamed through pre-merge validation. Validation
  requires the part and sidecar to exist and verifies artifact version, vendor,
  canonical format, single-partition commit state, manifest relationships,
  exact record and byte counts, declared ordering, canonical NDJSON framing,
  bounded record size, and SHA-256. Missing, malformed, changed, or inconsistent
  parts fail the entire operation.
- Final assembly uses the existing lazy K-way priority queue with at most one
  bounded record per partition. Ordering is chronological or
  reverse-chronological with deterministic partition/sequence tie handling.
- Loki's caller limit is applied once during final ordered merge. Each retained
  partition is still treated conservatively if its upstream response reaches
  the vendor request limit; the final manifest is `partial` in that case.
- Exact cross-partition boundary duplicates use timestamp, source identity, and
  payload/labels SHA-256. The boundary marker remains bounded to one record.
  Same-timestamp distinct records and duplicates within one partition remain.
- Checked quota enforcement covers retained canonical bytes, conservative
  projected-final bytes, in-progress final bytes, and retained-parts-plus-final
  disk use. The shared artifact writer now rejects checked-counter overflow.
- Merge cancellation is explicit and cannot commit a final artifact after the
  cancellation token is observed.
- Final manifests persist the exact query interval, ordering, output records,
  encoded and decoded transfer accounting, final bytes and SHA-256, partition
  bytes and SHA-256 values, partition counts, limit diagnostics, deduplication
  policy/count, skipped/truncated counts, diagnostics, and conservative
  completeness.
- Cleanup is transactional. Failed validation, merge, quota, cancellation,
  final write, or manifest commit removes incomplete outputs and retained
  partitions. Manifest partials are removed on failure. A final-manifest commit
  failure removes the already-committed final artifact. Successful partitions
  are removed only after both final artifact and manifest commits succeed.
- Single-request Splunk and Loki behavior remains on the existing route, and
  CircleCI manifests were revalidated after shared artifact changes.

### Verification

- `cargo fmt --all`
- Focused ingestion, Splunk controller, Grafana controller, CircleCI controller,
  raw-artifact, tool-schema, and streaming-transport tests
- `cargo check --all-features`
- `cargo test --all-features -j 1` (full suite passed)
- `git diff --check`

`cargo clippy --all-features --all-targets -- -D warnings` still reports exactly
the documented five pre-existing warnings and no warning from this slice: two
conversion warnings in `src/transport/response_cache.rs`, `too_many_lines` and
`map_unwrap_or` in the generic transport, and `map_unwrap_or` in the NinjaOne
vendor.

## Bounded parallel partition acquisition and deterministic fault-injection checkpoint (2026-08-21)

Implemented and committed as `17e0c59`.

### Completed

- Splunk and Loki partition acquisition is bounded to four concurrent requests,
  while retaining the existing 2-16 planned-partition eligibility and all safe
  single-request fallbacks.
- Completed partitions are stored by planned index, so out-of-order transport
  completion cannot change merge order or final-manifest partition order.
- A shared cancellation token is propagated through transport and normalization.
  The first transport, normalization, quota, deadline, or cancellation failure
  stops new scheduling, cancels outstanding work, drains in-flight futures, and
  returns the first failure.
- Concurrent encoded/decoded transfer accounting remains checked and atomic.
  Retained temporary/projected disk accounting is also checked atomically,
  including overflow and quota rejection paths.
- Cleanup is transactional across transport inputs, canonical parts, sidecars,
  final artifacts, final partials, and manifest partials. Committed artifacts
  are unregistered when removed, preventing stale registry entries.
- Test-only fault seams cover partition/final write and commit failures,
  manifest write/sync/rename failures, cancellation during merge, and checked
  transfer/disk-counter overflow. Production behavior is unchanged without an
  injected fault.
- Added non-empty Splunk and Grafana/Loki controller coverage for delayed
  out-of-order completion, deterministic ordering, ascending and descending
  output, boundary deduplication, same-timestamp distinct records, global
  limits, final checksums/accounting, complete versus limited manifests,
  middle-partition failure, cancellation, and orphan cleanup.

### Verification

- `cargo fmt --all`
- Focused ingestion, Splunk controller, Grafana controller, CircleCI controller,
  raw-artifact, tool-schema, and streaming-transport tests
- `cargo check --all-features`
- `cargo test --all-features -j 1` (full suite passed)
- `git diff --check`
- `cargo clippy --all-features --all-targets -- -D warnings` reaches the project
  and reports exactly the five documented pre-existing warnings: two conversion
  warnings in `src/transport/response_cache.rs`, `too_many_lines` and
  `map_unwrap_or` in the generic transport, and `map_unwrap_or` in the NinjaOne
  vendor. This slice adds no Clippy warnings.

The bounded parallel acquisition and deterministic fault-injection slice is
complete. The broader streaming-ingestion phase is also complete through
planner, normalization, transport, partitioning, bounded merge/deduplication,
final manifests, lifecycle cleanup, and this parallel-acquisition checkpoint.
Any future work is optional hardening or addressing the separately documented
Clippy baseline; no required implementation item remains in the handoff below.

## Continuous disk reservation production-hardening checkpoint (2026-08-21)

### Completed

- Replaced post-commit retained-part accounting with one checked atomic disk
  reservation shared by every concurrently active transport-input, canonical
  partition, final-artifact, and manifest write in a streaming-ingestion
  transaction. Chunk writers reserve before each filesystem write; manifest
  writers reserve their complete serialized size before creating the partial.
- Aggregate reservations cannot exceed the configured disk ceiling even while
  transport and normalization writers overlap. Checked addition rejects quota
  exhaustion and overflow without changing the counter; checked subtraction
  rejects underflow and double release. Peak reservation is observable for
  deterministic tests.
- Reservation ownership follows the artifact lifecycle. An incomplete writer
  releases after partial cleanup, commit transfers ownership to the artifact
  registry, manifest commit attaches the sidecar reservation, replacement
  releases the superseded sidecar exactly once, and artifact removal deletes
  artifact/manifest/manifest-partial files before unregistering and releasing.
- Successful partition-to-final transitions remove partition artifacts and
  sidecars only after final artifact and manifest commits, leaving exactly the
  live final artifact plus final manifest reserved. Failure paths return every
  shared reservation to zero and retain no artifact registry entry.
- Preserved the independent final-artifact projected-space rule: canonical
  retained-part bytes must still fit alongside a conservatively projected
  final artifact before merge begins. The continuous reservation then enforces
  actual final and manifest writes.
- Applied the same shared reservation lifecycle to single-response Splunk,
  Splunk job results, Grafana/Loki, and CircleCI ingestion without changing any
  public tool schema. CircleCI transport inputs are now removed after final
  normalization, and final-manifest failure removes the committed final
  artifact transactionally.
- Added the internal/server `STREAMING_PARTITION_CONCURRENCY` configuration.
  It defaults to four, accepts only 1-16, and is always capped by the planned
  partition count. Invalid or out-of-range values fall back to four; public
  partition controls and every existing partition-eligibility fallback remain
  unchanged.
- Added deterministic tests for reservation quota/overflow/underflow,
  concurrent-writer peak bounds, write and commit rollback, double-release
  prevention, manifest write/sync/rename rollback, successful final/manifest
  live reservation accounting, sidecar cleanup, and zero reservations after
  failure. The transport and normalization paths share this tested artifact
  writer and registry lifecycle.

### Verification

- `cargo fmt --all`
- Focused ingestion, Splunk normalization/controller, Grafana/Loki
  normalization/controller, CircleCI controller, raw-artifact, tool-schema,
  and streaming-transport tests, including non-empty multipart ordering in
  both directions
- `cargo check --all-features`
- `cargo test --all-features -j 1` (full suite passed after the final release
  lifecycle correction)
- `git diff --check`

`cargo clippy --all-features --all-targets -- -D warnings` reaches the project
and reports exactly the documented five pre-existing warnings and no warning
from this slice: `cast_possible_truncation` and `cast_possible_wrap` in
`src/transport/response_cache.rs`, `too_many_lines` and `map_unwrap_or` in the
generic transport, and `map_unwrap_or` in `src/vendor/ninjaone/mod.rs`.

## Historical handoff: bounded parallel partition acquisition and deterministic fault injection

Keep every planner, normalization, transport, merge, quota, manifest, and
single-request invariant above. Do not broaden partition eligibility or infer
ambiguous time bounds.

- Replace the deliberately sequential partition acquisition loop with bounded
  parallel acquisition. Keep concurrency small and configurable within the
  existing 2-16 partition ceiling.
- Store completed parts by planned partition index so out-of-order request
  completion cannot alter deterministic merge ordering or manifest ordering.
- On the first transport, normalization, quota, deadline, or cancellation
  failure, cancel outstanding partition work, schedule nothing new, await task
  shutdown, and transactionally remove every transport input, canonical part,
  sidecar, final partial, and manifest partial.
- Preserve bounded pre-body retries and never retry a response after body
  consumption. Preserve encoded/decoded aggregate accounting under concurrent
  updates and checked overflow.
- Introduce narrow test-only fault-injection seams for partition write/commit,
  final write/commit, manifest write/sync/rename, cancellation during merge,
  and checked-counter overflow. Production behavior must remain unchanged when
  no fault is injected.
- Add focused tests for genuinely out-of-order completion, middle-partition
  failure with in-flight work, cancellation propagation, final-write failure,
  manifest-sync/rename failure, per-partition and aggregate quotas,
  projected-final overflow, checked transfer/disk-counter overflow, and absence
  of orphaned files or registered artifacts after every failure.
- Add non-empty controller tests for ascending and descending multi-partition
  outputs, cross-boundary duplicates, same-timestamp distinct records, global
  limits in both directions, final SHA-256/accounting, and complete versus
  limited manifests.
- Re-run CircleCI manifest tests if shared ingestion, transport, artifact, or
  manifest code changes. Do not modify unrelated files to silence the known
  five-warning Clippy baseline.

Run `cargo fmt --all`, the focused ingestion/vendor/artifact/transport tests,
`cargo check --all-features`, Clippy with `-D warnings`,
`cargo test --all-features -j 1`, and `git diff --check`. Do not claim this next
slice complete unless concurrency remains bounded, failure cancellation is
transactional, and the full suite passes.

## Server-wide streaming disk coordination checkpoint (2026-08-21)

Implemented and committed as `6def9e0`. Treat this checkpoint and that commit
as authoritative for subsequent optional hardening.

### Completed

- Replaced transaction-local aggregate disk ceilings with one process-wide
  streaming disk coordinator. Every Splunk search/job-results, Grafana/Loki,
  CircleCI action-output, transport-input, canonical-part, final-artifact, and
  manifest writer now participates in the same server ceiling while retaining
  an independent per-transaction byte total and the existing per-request byte
  quotas.
- Added private checked transaction and reservation identifiers. Each writer
  grows one narrow lease before physical writes and transfers that exact lease
  to the artifact registry on commit. A transaction cannot release another
  transaction's bytes; release underflow, duplicate release, owner mismatch,
  identifier overflow, reservation overflow, transaction overflow, and global
  overflow all fail explicitly without changing accounting.
- Reservation contention uses strict FIFO head-of-line fairness. Acquisition
  waits are cancellation-aware and bounded by an absolute transaction
  deadline. Cancellation and deadline races roll back a grant atomically, so a
  timed-out waiter cannot strand bytes or disturb the holder that blocked it.
- Preserved the final-artifact projected-space rule independently of the live
  server-wide reservation ceiling. Successful final artifacts retain exactly
  their artifact and manifest leases until artifact removal or process-session
  cleanup; partition artifacts transfer no ownership until final artifact and
  manifest commit both succeed.
- Hardened partial-write cleanup. A write error conservatively keeps its lease
  until the partial is physically deleted. Failed deletion retains an internal
  orphan lease for later filesystem reconciliation instead of releasing bytes
  early. Artifact removal likewise releases registry leases only after the
  artifact, manifest, and manifest partial are gone.
- Made reserved manifest replacement portable and transactional on Windows by
  using a same-directory backup/rename/remove sequence. The superseded
  sidecar lease is released only after its physical backup is removed, and
  replacement rollback restores the prior sidecar on failure.
- Session cleanup now clears registry and orphan leases only after the process
  artifact directory is physically removed. Reconciliation removes missing
  registry entries, missing sidecar leases, and cleaned orphan leases.
  Abandoned-session scavenging retains directories whose PID is live and is
  deliberately conservative when process inspection itself fails, preventing
  deletion of artifacts owned by another live process.
- Preserved all public tool schemas, partition eligibility and exact bound
  handling, `STREAMING_PARTITION_CONCURRENCY`, its default of four, and its
  1-16 bound.
- Added deterministic focused coverage for independent Splunk/Loki
  transactions sharing one ceiling, CircleCI competition, FIFO wake order,
  peak bounds, quota exhaustion and overflow, cancellation/deadline rollback,
  transaction isolation, double-release rejection, acquisition rollback,
  artifact/manifest ownership transitions and replacement, failed cleanup,
  live-process scavenging, and registry/filesystem/reservation reconciliation.

### Verification

- `cargo fmt --all`
- Focused ingestion, Splunk normalization/controller, Grafana/Loki
  normalization/controller, CircleCI controller, raw-artifact, tool-schema,
  and streaming-transport tests, including non-empty multipart ordering in
  both directions
- `cargo check --all-features`
- `cargo test --all-features -j 1` (full suite passed)
- `git diff --check`

`cargo clippy --all-features --all-targets -- -D warnings` reaches the project
and reports exactly the documented five pre-existing warnings and no warning
from this slice: `cast_possible_truncation` and `cast_possible_wrap` in
`src/transport/response_cache.rs`, `too_many_lines` and `map_unwrap_or` in the
generic transport, and `map_unwrap_or` in `src/vendor/ninjaone/mod.rs`.

## Historical handoff: download-safe artifact retention and reclamation

Treat the server-wide streaming disk coordination checkpoint above and commit
`6def9e0` as authoritative. Preserve every completed planner,
partition-eligibility, normalization, transport, merge, quota, manifest,
cancellation, cleanup, ordering, reservation, fairness, ownership, and
single-request invariant. Do not broaden Splunk or Loki partition eligibility,
infer ambiguous time bounds, change public tool schemas, or modify unrelated
files to address the documented Clippy baseline.

The next optional slice should prevent successful downloadable streaming
artifacts from holding the process-wide disk ceiling indefinitely while making
artifact reclamation safe under concurrent downloads.

- Add an internal lifecycle state and download/read pin for committed streaming
  artifacts. A range read or metadata lookup that will lead to a read must pin
  the exact registry generation before opening the file and release the pin
  exactly once after the read finishes, fails, or is cancelled.
- Add bounded, configurable retention for successful streaming artifacts and
  manifests. Keep the setting internal/server-side and preserve current public
  tool schemas. Choose and document conservative defaults and strict bounds;
  invalid values must fall back deterministically.
- Expiration must be based on a monotonic/testable committed-at timestamp, with
  deterministic oldest-first ordering and artifact ID as a stable tie-breaker.
  Do not evict an unexpired artifact merely to satisfy a waiting reservation.
- An expired pinned artifact must enter a non-readable pending-delete state but
  remain physically present and fully reserved until the final pin is released.
  New reads must fail consistently once deletion begins; existing pinned reads
  must complete safely.
- Delete the artifact, manifest, manifest partial, and any replacement/cleanup
  sidecars before unregistering or releasing their exact server-wide leases.
  A physical deletion failure must retain registry ownership and reservations
  and be retried by a later bounded sweep without busy looping.
- A successful reclamation must wake FIFO disk-reservation waiters through the
  existing coordinator. It must not bypass transaction ownership, deadline, or
  cancellation checks and must never release another artifact's lease.
- Integrate the sweeper with graceful server shutdown and process-session
  cleanup. Stop scheduling sweeps, prevent new pins, drain active pins and
  deletion work within a bounded shutdown deadline, then use the existing
  conservative cleanup/reconciliation rules. Forced termination remains the
  next process's abandoned-session responsibility.
- Keep abandoned-session scavenging PID-safe and conservative when process
  inspection fails. Never delete a directory owned by a live process.
- Preserve `STREAMING_PARTITION_CONCURRENCY`, its default of four, and its 1-16
  bound. Preserve per-request quotas, per-transaction accounting, the
  final-artifact projected-space rule, and strict FIFO reservation fairness.

Add deterministic test-only clocks, sweep triggers, pin guards, and deletion
fault seams. Add focused tests for:

- expired versus unexpired successful Splunk, Loki, and CircleCI artifacts;
- concurrent full and range downloads while expiration becomes eligible;
- an expired pinned artifact deferring physical deletion and lease release;
- rejection of new reads after the pending-delete transition;
- exact artifact-plus-manifest reservation release after the final pin;
- deterministic oldest-first and artifact-ID tie ordering;
- a FIFO reservation waiter waking only after successful physical reclamation;
- deletion failure retaining files, registry state, and exact reservations;
- retry success reconciling filesystem, registry, pins, and reservations;
- manifest replacement and manifest-partial cleanup during expiration;
- cancellation and shutdown while reads, sweeps, and reservation waits overlap;
- session cleanup with live, failed, expired, and pending-delete artifacts;
- abandoned-session reconciliation without deleting live-process artifacts;
- peak server-wide reserved bytes never exceeding the ceiling;
- no artifact, partial, sidecar, manifest partial, stale pin, or registry entry
  remaining after completed deletion and transactional failure cleanup;
- unchanged public tool schemas and unchanged single-request and multipart
  Splunk/Loki behavior.

Re-run non-empty Splunk and Grafana/Loki multipart tests in both ordering
directions, CircleCI manifest tests, raw-artifact and range-download tests,
tool-schema tests, HTTP artifact-download tests, and streaming-transport tests.

Run:

- `cargo fmt --all`
- focused ingestion, reservation-coordinator, Splunk controller, Grafana
  controller, CircleCI controller, raw-artifact, HTTP artifact-download,
  tool-schema, and streaming-transport tests
- `cargo check --all-features`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo test --all-features -j 1`
- `git diff --check`

Preserve and report the exact documented five-warning pre-existing Clippy
baseline: `cast_possible_truncation` and `cast_possible_wrap` in
`src/transport/response_cache.rs`, `too_many_lines` and `map_unwrap_or` in the
generic transport, and `map_unwrap_or` in `src/vendor/ninjaone/mod.rs`. Update
this document with a dated checkpoint only after implementation and the full
verification pass. Commit the completed slice on `main`, but do not push.

## Download-safe artifact retention and reclamation checkpoint (2026-08-22)

### Completed

- Added private registry generations, readable/pending-delete lifecycle state,
  and checked read-pin counts. `artifact_read` and HTTP full/range downloads
  pin the exact generation before opening the file and retain the guard until
  the read completes, fails, or its response/future is dropped.
- Expiration atomically makes an artifact non-readable. Existing pinned reads
  may finish, new reads receive the existing not-found/expired response, and
  files plus reservations remain live until the last exact-generation pin is
  released. Final-pin release schedules reclamation exactly once; stale or
  duplicate releases cannot affect another generation.
- Added internal `STREAMING_ARTIFACT_RETENTION_SECONDS` configuration with a
  conservative one-hour default and strict five-minute through seven-day
  bounds. Added `STREAMING_ARTIFACT_SWEEP_INTERVAL_SECONDS` with a one-minute
  default and strict five-second through one-hour bounds. Invalid, missing, or
  out-of-range values fall back deterministically to their defaults. Public
  tool schemas are unchanged.
- Retention age uses a monotonic process clock and begins only after the final
  manifest reservation successfully attaches. Provisional transport inputs,
  canonical parts, and final artifacts still owned by a live transaction are
  not retention-eligible.
- Each pass considers at most 64 artifacts in deterministic oldest-first order
  with artifact ID as the stable tie-breaker. Pinned pending entries do not
  consume future sweep slots, so they cannot starve later expired artifacts.
  Unexpired artifacts are never evicted to satisfy reservation waiters.
- Reclamation removes the artifact, artifact partial, manifest, manifest
  partial, and manifest replacement/cleanup sidecars before unregistering the
  exact registry generation and releasing its artifact/manifest leases. A
  physical deletion failure retains pending registry ownership and the exact
  reservations for a later bounded retry.
- Successful physical reclamation releases through the existing server-wide
  coordinator, preserving transaction ownership, checked arithmetic, strict
  FIFO waiter ordering, cancellation, deadlines, per-request quotas,
  per-transaction accounting, and the projected-final-artifact rule.
- HTTP and stdio transports start the retention worker and perform bounded
  graceful shutdown. Shutdown stops new pins and sweeps, waits up to five
  seconds for active reads, aborts a stuck sweep, drains eligible deletion
  work, and then applies the existing conservative session cleanup and
  reconciliation. A session with active pins remains for abandoned-session
  recovery rather than being deleted underneath a live read.
- Missing-file reconciliation now recognizes manifest replacement sidecars.
  Abandoned-session scavenging remains PID-safe and treats process-inspection
  failure conservatively, so it does not delete another live process's
  artifact directory.
- Preserved Splunk and Loki partition eligibility and exact bound handling,
  every normalization/transport/merge/manifest/cancellation invariant,
  `STREAMING_PARTITION_CONCURRENCY`, its default of four and 1-16 bound, and
  all single-request behavior.

### Focused coverage

- Added deterministic committed-time and targeted sweep seams, exact pin
  guards, deletion-failure injection, and deletion-order observation.
- Covered expired Splunk versus unexpired Loki artifacts, pinned CircleCI
  artifacts, full/range HTTP downloads during expiration, response-drop
  cancellation, failed file opens, pending-delete read rejection, exact
  artifact-plus-manifest release, deterministic ordering, bounded passes,
  pinned-entry non-starvation, FIFO waiter wake-up, deletion failure/retry,
  manifest partial/replacement cleanup, live transaction exclusion, session
  cleanup with active pins, missing-file reconciliation, and PID-safe
  abandoned-session cleanup.
- Existing transaction rollback, manifest transition, reservation overflow,
  owner isolation, double-release, peak-bound, cleanup, and controller tests
  continue to cover failure isolation and absence of transactional orphans.

### Verification

- `cargo fmt --all`
- Focused ingestion, reservation-coordinator, Splunk normalization/controller,
  Grafana/Loki normalization/controller, CircleCI controller, raw-artifact,
  HTTP artifact-download, binary shutdown, tool-schema, and
  streaming-transport tests, including non-empty multipart ordering in both
  directions
- `cargo check --all-features`
- `cargo test --all-features -j 1` (full suite passed)
- `git diff --check`

`cargo clippy --all-features --all-targets -- -D warnings` reaches the project
and reports exactly the documented five pre-existing warnings and no warning
from this slice: `cast_possible_truncation` and `cast_possible_wrap` in
`src/transport/response_cache.rs`, `too_many_lines` and `map_unwrap_or` in the
generic transport, and `map_unwrap_or` in `src/vendor/ninjaone/mod.rs`.

## Clippy baseline cleanup and v0.14.0 release checkpoint (2026-08-22)

- Removed the five previously documented Clippy warnings with checked cache
  configuration conversions, direct `map_or`/`map_or_else` handling, and
  focused NinjaOne request/response logging helpers that keep the generic
  transport function bounded.
- Corrected the subsequently unmasked test-only case-sensitive extension
  comparison and made the retention waiter test deadline independent of
  parallel test-scheduler delays.
- Bumped the crate and lockfile package version from `0.13.0` to `0.14.0` for
  the completed streaming-ingestion release. Public tool schemas and runtime
  ingestion behavior are unchanged by this cleanup.
- `cargo fmt --all`, `cargo check --all-features`,
  `cargo clippy --all-features --all-targets -- -D warnings`,
  `cargo test --all-features -j 1`, and `git diff --check` pass. Clippy now
  completes with zero warnings across all features and targets.
