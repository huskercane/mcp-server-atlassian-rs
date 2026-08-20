# mcp-server-atlassian (Rust)

Single-binary MCP server (`mcp-atlassian`) exposing Atlassian (Bitbucket / Jira /
Confluence) plus Zoom, CircleCI, Slack, Postman, edX, and New Relic as MCP tools.
Ports of the TS reference servers with byte-for-byte parity on tool descriptions,
schemas, output formats, and error envelopes — preserve that parity when touching
anything LLM-facing.

## Toolchain & baseline (match it, don't fragment it)

- Rust is **pinned**: `rust-toolchain.toml` → `1.95.0`, `edition = "2024"`,
  `rust-version = "1.95"`. Use stable only — no nightly/unstable/preview features.
- Dependencies are **exact-pinned** (`=x.y.z`) on purpose. When adding/upgrading a
  dep, pin it the same way and update `Cargo.lock` deliberately; don't loosen
  existing pins to a range.
- "Use latest features" means latest *stable* idioms within this pinned baseline
  (edition 2024 patterns, current async runtime) — it does **not** mean bumping to
  newer toolchain/crate versions ad hoc. Raising the baseline is its own reviewed change.

## Build / test / lint (CI gates — all must pass with zero warnings)

```bash
cargo build                                   # default features (keychain on)
cargo build --no-default-features             # headless / keychain-off path
cargo clippy --all-targets -- -D warnings     # warnings are errors; pedantic is on
cargo test                                    # full suite (integration-heavy)
cargo fmt --all                               # rustfmt is the formatter of record
cargo deny check                              # license + advisory gate (deny.toml)
```

CI (`.github/workflows/rust.yml`) runs build + clippy + test on a
ubuntu/macos/windows matrix; Linux also builds `--no-default-features`. "No
compile-time warning or error" is enforced by `-D warnings` — clippy `all` +
`pedantic` are `warn` (see `[lints]` in `Cargo.toml`); a handful are explicitly
allowed there, so prefer fixing over adding new `#[allow(...)]`.

## Releasing (one version number, not three)

`Cargo.toml`'s `version` is the **single source of truth**. `src/constants.rs::VERSION`
derives from it via `env!("CARGO_PKG_VERSION")`, and that constant is what MCP clients
see in `get_info()`, what `--version` prints, and what the HTTP health banner shows.
Never write a version literal back into it — `binary_tests::version_reported_to_users_is_the_crate_version`
fails if you do. (Unrelated to the MCP *protocol* version, which is negotiated
separately.)

To cut a release:

1. Bump `version` in `Cargo.toml`, and build so `Cargo.lock` picks it up.
2. Commit both, and get them onto `main`.
3. Tag `v<version>` on `main` — the tag must match `Cargo.toml` exactly.
4. Push the tag. `.github/workflows/release.yml` triggers on `v*` and builds the
   Linux/macOS/Windows matrix (~8 minutes).

`scripts/check-release-tag.sh` enforces step 3, wired as a `PreToolUse` hook in
`.claude/settings.json`: it denies a tag-creating command whose version disagrees
with `Cargo.toml`, and ignores everything else (listing, deleting, log ranges, and
commands that merely mention tagging). It reads the hook JSON payload on stdin, so
to check by hand:

```bash
echo '{"tool_input":{"command":"git tag -a v1.2.3 -m x"}}' | scripts/check-release-tag.sh
```

This exists because the three numbers had silently diverged: releases reached
`v0.10.0` while `Cargo.toml` said `0.8.0` and the reported version said `3.1.0`
(inherited from the TS reference server at port time), so a released binary
introduced itself to every MCP client under a version that was never released.

## Layout

- `src/vendor/` — per-vendor HTTP clients (auth headers, request/response shapes).
- `src/controllers/` — orchestration between tools and vendors.
- `src/tools/` — MCP tool definitions (`rmcp`), schemas, descriptions.
- `src/transport/` — stdio + streamable-HTTP (axum) transports.
- `src/auth/`, `src/config/` — credentials (OS keychain via `keyring`, gated by the
  `keychain` feature) and config/env loading.
- `src/format/`, `src/pagination.rs`, `src/error.rs` — output formatting, paging, error envelope.
- `tests/` — integration tests (`*_vendor_tests.rs`, `*_controller_tests.rs`, etc.)
  using `wiremock` for HTTP and `assert_cmd` for the binary. This is where new
  behavior gets covered.

## Conventions & priorities

- **Priority order: correctness > security > performance > brevity.** Don't trade
  away correctness for a micro-optimization; don't log secrets or tokens.
- `unsafe_code = "deny"` in production. Test files that must mutate `std::env`
  (now `unsafe` in edition 2024) opt in locally via `#![allow(unsafe_code)]` — keep
  that confined to tests.
- **Async**: this is a tokio (multi-thread) app — use async for I/O (HTTP, fs,
  process). Keep CPU-bound/sync prep synchronous. For an infallible sync-prep +
  single tail `.await`, prefer `fn -> impl Future<Output = T> + Send` over
  `async fn`; do **not** convert when there's a `?` before the await, a branch on
  the awaited result, multiple sequenced awaits, or a fixed signature (`#[tool]`,
  axum handler, trait method).
- **Tests**: new public fn / bug fix / behavior change ⇒ a test. Prefer integration
  tests against real behavior (wiremock/assert_cmd) over mock-heavy unit tests. A
  bug-fix test must fail on the unfixed code and pass on the fixed code.
- **Parity**: tool names, descriptions, schemas, and error envelopes mirror the TS
  servers. Changing them is a deliberate, called-out change — not incidental.
