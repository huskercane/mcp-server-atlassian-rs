# mcp-server-atlassian (Rust port)

Rust implementation of the Atlassian MCP servers — connects AI assistants (Claude Desktop, Cursor, Continue, Cline, any MCP client) to **Bitbucket Cloud and Jira Cloud** through a single binary. Ports both [`@aashari/mcp-server-atlassian-bitbucket`](https://github.com/aashari/mcp-server-atlassian-bitbucket) and [`@aashari/mcp-server-atlassian-jira`](https://github.com/aashari/mcp-server-atlassian-jira) with byte-for-byte parity on tool descriptions, schemas, output formats, and error envelopes.

This directory does **not** ship to npm. It builds a single static-ish binary: `mcp-atlassian`.

## Why a Rust port

- No Node.js runtime dependency.
- ~13 MB release binary vs. ~120 MB `node_modules` tree.
- Cold-start in milliseconds instead of hundreds.
- One binary serves both Bitbucket and Jira — instead of running two Node processes side-by-side, you get one MCP server exposing all 11 tools (six `bb_*`, five `jira_*`).
- Identical LLM-facing tool descriptions and output formats — drop-in replacement for the TS packages in an MCP client config.

## Build from source

```bash
git clone https://github.com/aashari/mcp-server-atlassian-bitbucket.git
cd mcp-server-atlassian-bitbucket/rust
cargo build --release
```

The binary lands at `target/release/mcp-atlassian`. Requires Rust 1.85 or later.

Optional checks:
```bash
cargo test                                   # full test suite
cargo clippy --all-targets -- -D warnings   # lint gate (pedantic)
cargo deny check                             # license + advisory check
```

## Credentials

Create an Atlassian API token with the scopes you need (Bitbucket and/or Jira). The TS README has step-by-step screenshots: see [Get Your Bitbucket Credentials](https://github.com/aashari/mcp-server-atlassian-bitbucket#1-get-your-bitbucket-credentials).

### Environment variables

| Variable | Purpose | Vendor scope |
|---|---|---|
| `ATLASSIAN_USER_EMAIL` | Atlassian account email (recommended auth) | both |
| `ATLASSIAN_API_TOKEN` | Scoped API token starting with `ATATT` | both |
| `ATLASSIAN_BITBUCKET_USERNAME` | Legacy fallback: Bitbucket username | bb only |
| `ATLASSIAN_BITBUCKET_APP_PASSWORD` | Legacy fallback: App Password | bb only |
| `BITBUCKET_DEFAULT_WORKSPACE` | Default workspace slug used when a tool/CLI call omits it | bb only |
| `ATLASSIAN_SITE_NAME` | Jira site shortname (e.g. `mycompany` for `mycompany.atlassian.net`). **Required** before invoking any `jira_*` tool; only checked at tool-call time, so a Bitbucket-only setup boots without it. | jira only |
| `TRANSPORT_MODE` | `stdio` (default) or `http` | shared |
| `PORT` | HTTP transport listening port (default `3000`, bound to `127.0.0.1`) | shared |
| `DEBUG` | Glob filter for debug logs (e.g. `DEBUG=*`) | shared |

Tokens can also be written to `~/.mcp/configs.json`. The Rust port supports per-vendor sections (`bitbucket`, `atlassian-bitbucket`, `jira`, `atlassian-jira`) so Bitbucket-only and Jira-only keys stay isolated:

```json
{
  "bitbucket": {
    "environments": {
      "ATLASSIAN_USER_EMAIL": "you@company.com",
      "ATLASSIAN_API_TOKEN": "ATATT...",
      "BITBUCKET_DEFAULT_WORKSPACE": "acme"
    }
  },
  "jira": {
    "environments": {
      "ATLASSIAN_SITE_NAME": "mycompany"
    }
  }
}
```

Shared keys (e.g. `ATLASSIAN_API_TOKEN`) can live in either section — when the same key appears in both with the **same value** it's resolved unambiguously; if the values disagree, you must scope the lookup explicitly via `get_for(vendor, key)`. Process env and `.env` always take priority over the global file.

## MCP client configuration

### Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "bitbucket": {
      "command": "/absolute/path/to/mcp-atlassian",
      "env": {
        "ATLASSIAN_USER_EMAIL": "your.email@company.com",
        "ATLASSIAN_API_TOKEN": "ATATT..."
      }
    }
  }
}
```

Restart Claude Desktop. The server appears in the status bar. Stdio transport is the default — no `TRANSPORT_MODE` needed.

### Any MCP-compatible client

Point the client at the binary. Stdio is the default transport. If your client uses streamable HTTP, run the binary with `TRANSPORT_MODE=http` and point the client at `http://127.0.0.1:3000/mcp`.

## Available tools

Eleven tools across two vendor families. Tool names match the TS references one-to-one.

### Bitbucket (`bb_*`)

| Tool | Annotations | Use |
|---|---|---|
| `bb_get` | read-only, idempotent | GET any Bitbucket API endpoint |
| `bb_post` | mutating | POST to any endpoint |
| `bb_put` | mutating, idempotent | PUT to any endpoint |
| `bb_patch` | mutating | PATCH any endpoint |
| `bb_delete` | destructive, idempotent | DELETE any endpoint |
| `bb_clone` | mutating | Clone a repository over SSH (falling back to HTTPS) |

The `/2.0` API prefix is prepended automatically.

### Jira (`jira_*`)

| Tool | Annotations | Use |
|---|---|---|
| `jira_get` | read-only, idempotent | GET any Jira API endpoint |
| `jira_post` | mutating | POST to any endpoint (e.g. create issue, comment) |
| `jira_put` | mutating, idempotent | PUT to any endpoint |
| `jira_patch` | mutating | PATCH any endpoint (partial issue update) |
| `jira_delete` | destructive, idempotent | DELETE any endpoint |

Jira paths pass through verbatim — supply the full `/rest/api/3/...` path. Requires `ATLASSIAN_SITE_NAME` to be set; missing site name surfaces as an authentication error at call time.

### Shared inputs

All API tools accept `path` (required), `queryParams` (optional JSON map), `jq` (optional JMESPath filter to reduce token cost), and `outputFormat` (`toon` default, `json` alternative).

## CLI usage

Two subcommand groups — one per vendor — keep the verbs unambiguous:

```bash
# Bitbucket
./mcp-atlassian bb get --path "/workspaces"

./mcp-atlassian bb get \
    --path "/repositories/acme" \
    --query-params '{"pagelen":"10"}' \
    --jq 'values[].{slug:slug,language:language}'

./mcp-atlassian bb post \
    --path "/repositories/acme/website/pullrequests" \
    --body '{"title":"Fix login","source":{"branch":{"name":"fix-login"}},"destination":{"branch":{"name":"main"}}}'

./mcp-atlassian bb clone --repo-slug website --target-path ~/work

# Jira
./mcp-atlassian jira get --path "/rest/api/3/myself"

./mcp-atlassian jira get \
    --path "/rest/api/3/search/jql" \
    --query-params '{"jql":"project=PROJ AND status=\"In Progress\"","maxResults":"10"}' \
    --jq 'issues[*].{key:key,summary:fields.summary}'

./mcp-atlassian jira post \
    --path "/rest/api/3/issue" \
    --body '{"fields":{"project":{"key":"PROJ"},"summary":"New task","issuetype":{"name":"Task"}}}'
```

Every verb accepts `--output-format toon|json` (default `toon`, parity with the TS Jira CLI). `--help` on any subcommand lists flags and expected input shapes.

### Deprecated top-level Bitbucket verbs

The original CLI exposed Bitbucket verbs without the `bb` prefix (`./mcp-atlassian get …`). Those are kept as hidden aliases for one release and emit a stderr deprecation notice when invoked. Migrate scripts to the explicit `bb` form before the next major release.

## Transports

- **stdio (default)**: MCP client spawns the binary, reads JSON-RPC framed by newlines on stdout, writes on stdin. Ctrl-D / stdin-EOF triggers a clean exit.
- **streamable HTTP**: `TRANSPORT_MODE=http ./mcp-atlassian`. Binds `127.0.0.1:${PORT:-3000}`. Endpoints:
  - `GET /` — plaintext health banner.
  - `POST /mcp` — MCP initialize + JSON-RPC calls. Returns `Mcp-Session-Id` on first call; subsequent calls must echo it.
  - `GET /mcp` — SSE stream for a session.
  - `DELETE /mcp` — tear a session down.
  
  Origin allowlist: only `http(s)://{localhost|127.0.0.1|[::1]}[:port]`. Request body cap: 1 MB. Idle sessions are reaped after 30 minutes of inactivity.

Both transports respond cleanly to `SIGINT` / `SIGTERM`: in-flight HTTP sessions drain, stdio flushes its transport, then the process exits 0.

## Compatibility with the TS references

Byte-for-byte parity is preserved across **both** TS servers on everything an MCP client or a CLI consumer can observe:
- Tool names, descriptions, input schemas, and annotations.
- Output formats (TOON default; JSON fallback) and error envelope shape (Bitbucket's four shapes plus Jira's `errorMessages`/`errors` envelope, OAuth-style, and flat `message`).
- Truncation rules (40,000-char threshold, trailing-newline cut, raw-response save path).
- Config cascade (`os env > .env > ~/.mcp/configs.json`) and all alias keys for both vendors.
- HTTP transport behavior (Origin check, CORS mirror, 1 MB body cap, reaper cadence).
- Path normalisation: Bitbucket auto-prepends `/2.0`; Jira passes through verbatim.

The `--output-format` flag, which the TS Bitbucket CLI lacked, is now available on every verb on both vendor groups (parity with the TS Jira CLI).

Internally, configuration loading is read-only rather than mutating `std::env` (which is `unsafe` in Rust 2024 editions under threading). The observable behavior — which value wins for a given key — is identical, with the addition that vendor-scoped global-config sections no longer leak across products.

## License

ISC, matching the TS reference.
