# mcp-server-atlassian (Rust port)

Rust implementation of the Atlassian MCP servers — connects AI assistants (Claude Desktop, Cursor, Continue, Cline, any MCP client) to **Bitbucket Cloud, Jira Cloud, and Confluence Cloud** through a single binary. Ports [`@aashari/mcp-server-atlassian-bitbucket`](https://github.com/aashari/mcp-server-atlassian-bitbucket), [`@aashari/mcp-server-atlassian-jira`](https://github.com/aashari/mcp-server-atlassian-jira), and [`@aashari/mcp-server-atlassian-confluence`](https://github.com/aashari/mcp-server-atlassian-confluence) with byte-for-byte parity on tool descriptions, schemas, output formats, and error envelopes.

This directory does **not** ship to npm. It builds a single static-ish binary: `mcp-atlassian`.

## Why a Rust port

- No Node.js runtime dependency.
- ~13 MB release binary vs. ~120 MB `node_modules` tree per product.
- Cold-start in milliseconds instead of hundreds.
- One binary serves Bitbucket, Jira, and Confluence — instead of running three Node processes side-by-side, you get one MCP server exposing all 16 tools (six `bb_*`, five `jira_*`, five `conf_*`).
- Identical LLM-facing tool descriptions and output formats — drop-in replacement for the TS packages in an MCP client config.

## Download prebuilt binaries

Grab the latest release for your platform from the [GitHub Releases page](https://github.com/huskercane/mcp-server-atlassian-rs/releases/latest):

| Platform | Archive |
|---|---|
| Linux (x86_64) | `mcp-atlassian-linux-x86_64.tar.gz` |
| macOS (Intel) | `mcp-atlassian-macos-x86_64.tar.gz` |
| macOS (Apple Silicon) | `mcp-atlassian-macos-aarch64.tar.gz` |
| Windows (x86_64) | `mcp-atlassian-windows-x86_64.zip` |

Each archive ships the `mcp-atlassian` binary and a `.sha256` checksum sibling. On macOS you may need to clear the quarantine bit: `xattr -d com.apple.quarantine ./mcp-atlassian`.

## Build from source

```bash
git clone https://github.com/huskercane/mcp-server-atlassian-rs.git
cd mcp-server-atlassian-rs
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

Create an Atlassian API token with the scopes you need (Bitbucket, Jira, and/or Confluence). The TS README has step-by-step screenshots: see [Get Your Bitbucket Credentials](https://github.com/aashari/mcp-server-atlassian-bitbucket#1-get-your-bitbucket-credentials).

### Environment variables

| Variable | Purpose | Vendor scope |
|---|---|---|
| `ATLASSIAN_USER_EMAIL` | Atlassian account email (recommended auth) | all |
| `ATLASSIAN_API_TOKEN` | Scoped API token starting with `ATATT` | all |
| `ATLASSIAN_BITBUCKET_USERNAME` | Legacy fallback: Bitbucket username | bb only |
| `ATLASSIAN_BITBUCKET_APP_PASSWORD` | Legacy fallback: App Password | bb only |
| `BITBUCKET_DEFAULT_WORKSPACE` | Default workspace slug used when a tool/CLI call omits it | bb only |
| `ATLASSIAN_SITE_NAME` | Atlassian site shortname (e.g. `mycompany` for `mycompany.atlassian.net`). **Required** before invoking any `jira_*` or `conf_*` tool; only checked at tool-call time, so a Bitbucket-only setup boots without it. Jira and Confluence point at the same Atlassian site, so populating it under either the `jira` or `confluence` section of `~/.mcp/configs.json` works for both — duplication is unnecessary. | jira + conf |
| `TRANSPORT_MODE` | `stdio` (default) or `http` | shared |
| `PORT` | HTTP transport listening port (default `3000`, bound to `127.0.0.1`) | shared |
| `DEBUG` | Glob filter for debug logs (e.g. `DEBUG=*`) | shared |

Tokens can also be written to `~/.mcp/configs.json`. The Rust port supports per-vendor sections (`bitbucket`, `atlassian-bitbucket`, `jira`, `atlassian-jira`, `confluence`, `atlassian-confluence`) so each product's keys stay isolated:

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
  },
  "confluence": {
    "environments": {
      "ATLASSIAN_SITE_NAME": "mycompany"
    }
  }
}
```

Shared keys (e.g. `ATLASSIAN_API_TOKEN`) can live in any section — when the same key appears in multiple sections with the **same value** it's resolved unambiguously; if the values disagree, you must scope the lookup explicitly via `get_for(vendor, key)`. Process env and `.env` always take priority over the global file.

`ATLASSIAN_SITE_NAME` gets a narrower fallback specifically for the Jira ↔ Confluence case: defining it under either section satisfies both vendors. The fallback is a deliberate two-vendor allow-list; unrelated sections (e.g. `bitbucket`) never leak into the lookup.

### Storing credentials in the OS keychain (desktop only)

If you'd rather not keep API tokens or app passwords in plaintext on disk, store them in the OS keychain and put the literal string `"keychain"` in their place. Supported on **macOS** (Keychain Services), **Windows** (Credential Manager), and **Linux desktop** (GNOME Keyring or KWallet, auto-unlocked at login).

> **Headless / CI / SSH-only Linux is out of scope.** Keychain backends require a logged-in desktop session with a keyring agent running. For server-style deployments either keep using env vars in your launcher, or build with `--no-default-features` to compile without the `keyring` dependency entirely.

#### Resolution order

When the binary needs a credential, it tries each source in priority order; the first hit wins:

1. Process environment variable (e.g. `ATLASSIAN_API_TOKEN`)
2. `.env` file in the current working directory
3. `~/.mcp/configs.json`
4. **OS keychain** — consulted in two cases:
   - **Explicit**: a previous source returned the literal string `"keychain"` (the sentinel). The principal (email/username) is read from the same cascade. A missing keychain entry is a hard auth error — it tells you the configuration intent didn't match reality.
   - **Implicit**: the secret is absent from every source above but the principal is set. Useful if you've migrated and deleted the field outright. A miss falls through silently.

`Config::get` itself is unaware of the keychain; the expansion happens entirely inside `auth::Credentials::resolve_with`. Non-secret keys (`ATLASSIAN_SITE_NAME`, `BITBUCKET_DEFAULT_WORKSPACE`, etc.) never trigger keychain reads.

#### CLI

```bash
# Store a token (no echo when stdin is a tty; pipes work too).
mcp-atlassian creds set --kind api-token --principal you@company.com
mcp-atlassian creds set --kind app-password --principal your-bb-username

# Confirm an entry exists (prints the last 4 chars only).
mcp-atlassian creds get --kind api-token --principal you@company.com

# Remove an entry.
mcp-atlassian creds rm --kind api-token --principal you@company.com

# One-shot migration: read tokens from ~/.mcp/configs.json, copy them to the
# keychain, replace each with the "keychain" sentinel, and write a .bak.
mcp-atlassian creds migrate

# `creds migrate --force` overrides the stale-clobber guard: when the keychain
# already holds a different value than configs.json, force overwrites it
# (logged with both fingerprints). Without --force, that's a hard error so a
# rotated keychain entry can't be silently regressed by a stale file value.
mcp-atlassian creds migrate --force
```

There is no `creds list` — `keyring`'s `Entry` API has no portable enumeration. Inspect entries through the OS-native UI: **Keychain Access** on macOS, **`credwiz.exe`** on Windows, **`seahorse`** on Linux. Look for the `mcp-server-atlassian.api-token` and `mcp-server-atlassian.app-password` services.

#### After migrating

`~/.mcp/configs.json` will look like this:

```json
{
  "bitbucket": {
    "environments": {
      "ATLASSIAN_USER_EMAIL": "you@company.com",
      "ATLASSIAN_API_TOKEN": "keychain"
    }
  }
}
```

Restart your MCP client. The first time the server resolves the credential it logs an info-level breadcrumb (`source=keychain, kind=api-token, principal=…`) so you can confirm the keychain path was taken. After validating, delete the `.bak` file `creds migrate` left behind.

#### Platform notes

- **macOS unsigned dev builds**: `keyring` keys ACLs by code signature, so every `cargo build` produces a new signature and Keychain prompts to re-grant access. Click *Always Allow* per rebuild, or install a release-signed binary at `~/.cargo/bin/mcp-atlassian` and invoke that. Same pattern as 1Password CLI, AWS Vault, and `gh`.
- **Windows**: silent under the user account that ran `creds set`. If your MCP client runs as a different user, the keychain entry won't be visible — re-run `creds set` from that account.
- **Linux desktop**: `libsecret` and `dbus` need to be available at build time. On Debian/Ubuntu: `sudo apt install libsecret-1-dev libdbus-1-dev`. The keyring agent (GNOME Keyring or KWallet) must be running and unlocked at runtime; both auto-unlock at GUI login on standard desktop distros.
- **Headless Linux**: build with `cargo build --release --no-default-features` to drop the keyring dep entirely. The CLI subcommands and sentinel resolution still compile but every keychain operation returns `KeychainError::Unavailable`. Keep using env vars / `~/.mcp/configs.json` plaintext in this mode.

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

Sixteen tools across three vendor families. Tool names match the TS references one-to-one.

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

### Confluence (`conf_*`)

| Tool | Annotations | Use |
|---|---|---|
| `conf_get` | read-only, idempotent | GET any Confluence API endpoint |
| `conf_post` | mutating | POST to any endpoint (e.g. create page, comment) |
| `conf_put` | mutating, idempotent | PUT to any endpoint (e.g. replace page content) |
| `conf_patch` | mutating | PATCH any endpoint (partial updates) |
| `conf_delete` | destructive, idempotent | DELETE any endpoint |

Confluence paths pass through verbatim — supply the full `/wiki/api/v2/...` (preferred) or `/wiki/rest/api/...` (CQL search) path. Shares `ATLASSIAN_SITE_NAME` with Jira; missing site name surfaces as an authentication error at call time. Confluence treats 403 as `API_ERROR/Access denied` (not `auth_invalid` like Jira), preserving the upstream TS asymmetry.

### Shared inputs

All API tools accept `path` (required), `queryParams` (optional JSON map), `jq` (optional JMESPath filter to reduce token cost), and `outputFormat` (`toon` default, `json` alternative).

## CLI usage

Three subcommand groups — one per vendor — keep the verbs unambiguous:

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

# Confluence
./mcp-atlassian conf get --path "/wiki/api/v2/spaces"

./mcp-atlassian conf get \
    --path "/wiki/rest/api/search" \
    --query-params '{"cql":"type=page AND space=DEV","limit":"10"}' \
    --jq 'results[*].{id:id,title:title}'

./mcp-atlassian conf post \
    --path "/wiki/api/v2/pages" \
    --body '{"spaceId":"123456","status":"current","title":"New Page","body":{"representation":"storage","value":"<p>Hello</p>"}}'
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

Byte-for-byte parity is preserved across **all three** TS servers on everything an MCP client or a CLI consumer can observe:
- Tool names, descriptions, input schemas, and annotations.
- Output formats (TOON default; JSON fallback) and error envelope shape (Bitbucket's four shapes, Jira's `errorMessages`/`errors` envelope plus OAuth-style and flat `message`, and Confluence's v2 `title`/`detail`, GraphQL-style `errors[]`, legacy `errorMessages`, and `statusCode`+`message` shapes).
- Truncation rules (40,000-char threshold, trailing-newline cut, raw-response save path).
- Config cascade (`os env > .env > ~/.mcp/configs.json`) and all alias keys for every vendor.
- HTTP transport behavior (Origin check, CORS mirror, 1 MB body cap, reaper cadence).
- Path normalisation: Bitbucket auto-prepends `/2.0`; Jira and Confluence pass through verbatim.

The `--output-format` flag, which the TS Bitbucket CLI lacked, is now available on every verb across all three vendor groups (parity with the TS Jira CLI).

Internally, configuration loading is read-only rather than mutating `std::env` (which is `unsafe` in Rust 2024 editions under threading). The observable behavior — which value wins for a given key — is identical, with the addition that vendor-scoped global-config sections no longer leak across products.

## License

ISC, matching the TS reference.
