# mcp-server-atlassian-bitbucket (Rust port)

Rust implementation of [`@aashari/mcp-server-atlassian-bitbucket`](https://github.com/aashari/mcp-server-atlassian-bitbucket) — an MCP server that connects AI assistants (Claude Desktop, Cursor, Continue, Cline, any MCP client) to Bitbucket Cloud. Same six tools, same six CLI commands, same wire protocol as the TypeScript reference.

This directory does **not** ship to npm. It builds a single static-ish binary: `mcp-atlassian-bitbucket`.

## Why a Rust port

- No Node.js runtime dependency.
- ~13 MB release binary vs. ~120 MB `node_modules` tree.
- Cold-start in milliseconds instead of hundreds.
- Identical LLM-facing tool descriptions and output formats — drop-in replacement for the TS package in an MCP client config.

## Build from source

```bash
git clone https://github.com/aashari/mcp-server-atlassian-bitbucket.git
cd mcp-server-atlassian-bitbucket/rust
cargo build --release
```

The binary lands at `target/release/mcp-atlassian-bitbucket`. Requires Rust 1.85 or later.

Optional checks:
```bash
cargo test                                   # full test suite
cargo clippy --all-targets -- -D warnings   # lint gate (pedantic)
cargo deny check                             # license + advisory check
```

## Credentials

Create an Atlassian API token with Bitbucket scopes (recommended) or, as a legacy fallback, a Bitbucket App Password. The TS README has step-by-step screenshots: see [Get Your Bitbucket Credentials](https://github.com/aashari/mcp-server-atlassian-bitbucket#1-get-your-bitbucket-credentials).

### Environment variables

| Variable | Purpose |
|---|---|
| `ATLASSIAN_USER_EMAIL` | Atlassian account email (recommended auth) |
| `ATLASSIAN_API_TOKEN` | Scoped API token starting with `ATATT` |
| `ATLASSIAN_BITBUCKET_USERNAME` | Legacy fallback: Bitbucket username |
| `ATLASSIAN_BITBUCKET_APP_PASSWORD` | Legacy fallback: App Password |
| `BITBUCKET_DEFAULT_WORKSPACE` | Default workspace slug used when a tool/CLI call omits it |
| `TRANSPORT_MODE` | `stdio` (default) or `http` |
| `PORT` | HTTP transport listening port (default `3000`, bound to `127.0.0.1`) |
| `DEBUG` | Glob filter for debug logs (e.g. `DEBUG=*`) |

Bitbucket tokens can also be written to `~/.mcp/configs.json` — see the [TS README's config-file section](https://github.com/aashari/mcp-server-atlassian-bitbucket#alternative-configuration-file). The Rust port reads the same file with the same alias keys.

## MCP client configuration

### Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "bitbucket": {
      "command": "/absolute/path/to/mcp-atlassian-bitbucket",
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

Identical to the TS version:

| Tool | Annotations | Use |
|---|---|---|
| `bb_get` | read-only, idempotent | GET any Bitbucket API endpoint |
| `bb_post` | mutating | POST to any endpoint |
| `bb_put` | mutating, idempotent | PUT to any endpoint |
| `bb_patch` | mutating | PATCH any endpoint |
| `bb_delete` | destructive, idempotent | DELETE any endpoint |
| `bb_clone` | mutating | Clone a repository over SSH (falling back to HTTPS) |

All API tools support `path` (required), `queryParams` (optional JSON map), `jq` (optional JMESPath filter to reduce token cost), and `outputFormat` (`toon` default, `json` alternative). The `/2.0` prefix is added automatically.

## CLI usage

Same subcommand surface as the TS binary:

```bash
# List workspaces
./mcp-atlassian-bitbucket get --path "/workspaces"

# List repos in a workspace, trimmed with a JMESPath filter
./mcp-atlassian-bitbucket get \
    --path "/repositories/acme" \
    --query-params '{"pagelen":"10"}' \
    --jq 'values[].{slug:slug,language:language}'

# Create a pull request
./mcp-atlassian-bitbucket post \
    --path "/repositories/acme/website/pullrequests" \
    --body '{"title":"Fix login","source":{"branch":{"name":"fix-login"}},"destination":{"branch":{"name":"main"}}}'

# Clone a repo (SSH preferred, HTTPS fallback)
./mcp-atlassian-bitbucket clone --repo-slug website --target-path ~/work
```

`--help` on any subcommand lists flags and expected input shapes.

## Transports

- **stdio (default)**: MCP client spawns the binary, reads JSON-RPC framed by newlines on stdout, writes on stdin. Ctrl-D / stdin-EOF triggers a clean exit.
- **streamable HTTP**: `TRANSPORT_MODE=http ./mcp-atlassian-bitbucket`. Binds `127.0.0.1:${PORT:-3000}`. Endpoints:
  - `GET /` — plaintext health banner.
  - `POST /mcp` — MCP initialize + JSON-RPC calls. Returns `Mcp-Session-Id` on first call; subsequent calls must echo it.
  - `GET /mcp` — SSE stream for a session.
  - `DELETE /mcp` — tear a session down.
  
  Origin allowlist: only `http(s)://{localhost|127.0.0.1|[::1]}[:port]`. Request body cap: 1 MB. Idle sessions are reaped after 30 minutes of inactivity.

Both transports respond cleanly to `SIGINT` / `SIGTERM`: in-flight HTTP sessions drain, stdio flushes its transport, then the process exits 0.

## Compatibility with the TS reference

Byte-for-byte parity is preserved on everything an MCP client or a CLI consumer can observe:
- Tool names, descriptions, input schemas, and annotations.
- Output formats (TOON default; JSON fallback) and error envelope shape.
- Truncation rules (40,000-char threshold, trailing-newline cut, raw-response save path).
- Config cascade (`os env > .env > ~/.mcp/configs.json`) and all four alias keys.
- HTTP transport behavior (Origin check, CORS mirror, 1 MB body cap, reaper cadence).
- CLI subcommands, flags, and short forms.

Internally, configuration loading is read-only rather than mutating `std::env` (which is `unsafe` in Rust 2024 editions under threading). The observable behavior — which value wins for a given key — is identical.

## License

ISC, matching the TS reference.
