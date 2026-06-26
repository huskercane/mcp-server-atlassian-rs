# mcp-server-atlassian (Rust port)

Rust implementation of the Atlassian MCP servers — connects AI assistants (Claude Desktop, Cursor, Continue, Cline, any MCP client) to **Bitbucket Cloud, Jira Cloud, and Confluence Cloud** through a single binary. Ports [`@aashari/mcp-server-atlassian-bitbucket`](https://github.com/aashari/mcp-server-atlassian-bitbucket), [`@aashari/mcp-server-atlassian-jira`](https://github.com/aashari/mcp-server-atlassian-jira), and [`@aashari/mcp-server-atlassian-confluence`](https://github.com/aashari/mcp-server-atlassian-confluence) with byte-for-byte parity on tool descriptions, schemas, output formats, and error envelopes.

The same binary also exposes eight more products as **native additions** (not TS ports), each with its own auth model:

- **Zoom Cloud** (`zoom_*`) — [Server-to-Server OAuth](https://developers.zoom.us/docs/internal-apps/s2s-oauth/): exchanges static client credentials for a short-lived bearer and **auto-renews it** (no ongoing user reauthorization).
- **CircleCI** (`circleci_*`) — a single [personal API token](https://circleci.com/docs/managing-api-tokens/) sent as a Bearer token, the scheme CircleCI's [v2 API](https://circleci.com/docs/api/v2/) recommends.
- **Slack** (`slack_*`) — a bot/user [OAuth token](https://api.slack.com/authentication/token-types) (`xoxb-…`) as a Bearer token. Its Web API returns `200 OK` with `{"ok": false, "error": …}` for logical failures, which this server reclassifies as a proper error.
- **Postman** (`postman_*`) — the one vendor that authenticates outside the `Authorization` header: its [API key](https://learning.postman.com/docs/developer/postman-api/authentication/) rides in `X-API-Key`.
- **edX / Open edX discussions** (`edx_discussion_*`) — a bearer token against `https://courses.edx.org` by default, or another LMS host via `EDX_API_BASE`.
- **New Relic** (`newrelic_query`) — drives NerdGraph (a single GraphQL endpoint) with a User API key in the `API-Key` header.
- **Grafana** (`grafana_*`) — reads logs by proxying [LogQL](https://grafana.com/docs/loki/latest/query/) to a [Loki](https://grafana.com/docs/loki/latest/) datasource through Grafana's [datasource proxy](https://grafana.com/docs/grafana/latest/developers/http_api/data_source/#data-source-proxy-calls), authenticated with a [service-account token](https://grafana.com/docs/grafana/latest/administration/service-accounts/) as a Bearer token. Works the same for self-hosted Grafana and Grafana Cloud (only `GRAFANA_URL` differs).
- **WRDS** (`wrds_*`) — the one vendor with **no REST API**: [Wharton Research Data Services](https://wrds-www.wharton.upenn.edu/) is a **PostgreSQL** database (CRSP, Compustat, IBES, TAQ, …), so it connects directly to `wrds-pgdata.wharton.upenn.edu:9737` over SSL (the access path the official [`wrds` Python package](https://pypi.org/project/wrds/) wraps) and exposes read-only SQL plus library/table/column discovery. Being PostgreSQL rather than HTTP, it is gated behind a Cargo feature (`wrds`, on by default) — build `--no-default-features` to drop the Postgres client entirely.

This directory does **not** ship to npm. It builds a single static-ish binary: `mcp-atlassian`.

## Why a Rust port

- No Node.js runtime dependency.
- ~13 MB release binary vs. ~120 MB `node_modules` tree per product.
- Cold-start in milliseconds instead of hundreds.
- One binary serves Bitbucket, Jira, Confluence, Zoom, CircleCI, Slack, Postman, edX discussions, New Relic, Grafana, and WRDS — instead of running separate Node processes side-by-side, you get one MCP server exposing all 49 tools (six `bb_*`, five `jira_*`, five `conf_*`, five `zoom_*`, five `circleci_*`, five `slack_*`, five `postman_*`, six `edx_discussion_*`, one `newrelic_query`, two `grafana_*`, four `wrds_*`). The four `wrds_*` tools are feature-gated (`wrds`, on by default); a `--no-default-features` build omits them and the Postgres dependency.
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

The binary lands at `target/release/mcp-atlassian`. Requires Rust 1.96 or later (pinned in `rust-toolchain.toml`).

By default the build includes the WRDS PostgreSQL integration (`wrds` feature). To build without it — dropping the `tokio-postgres` dependency tree entirely (headless / non-WRDS deployments, or to shrink the binary by ~0.5–1 MB):

```bash
cargo build --release --no-default-features --features keychain   # WRDS off, keychain on
cargo build --release --no-default-features                        # WRDS off, keychain off (headless)
```

Optional checks:
```bash
cargo test                                   # full test suite
cargo clippy --all-targets -- -D warnings   # lint gate (pedantic)
cargo deny check                             # license + advisory check
```

## Credentials

Create an Atlassian API token with the scopes you need (Bitbucket, Jira, and/or Confluence). The TS README has step-by-step screenshots: see [Get Your Bitbucket Credentials](https://github.com/aashari/mcp-server-atlassian-bitbucket#1-get-your-bitbucket-credentials).

Zoom is separate: it does **not** use an Atlassian token. Create a [Server-to-Server OAuth app](https://developers.zoom.us/docs/internal-apps/s2s-oauth/) and use its account ID, client ID, and client secret (`ZOOM_*` below). These never go through the OS keychain — they are read as plaintext from the `zoom` config section or environment.

CircleCI is also separate: create a [personal API token](https://circleci.com/docs/managing-api-tokens/) (CircleCI → User Settings → Personal API Tokens) and set it as `CIRCLECI_TOKEN` (below). Like Zoom, it is read as plaintext from the `circleci` config section or environment and never goes through the OS keychain.

Slack is separate too: create a Slack app, install it to your workspace, and copy its bot token (`xoxb-…`) or user token (`xoxp-…`) into `SLACK_TOKEN` (below). The token's [scopes](https://api.slack.com/scopes) gate what the `slack_*` tools can do. Read as plaintext from the `slack` config section or environment; never goes through the OS keychain.

Postman is separate too: create an [API key](https://learning.postman.com/docs/developer/postman-api/authentication/) (Postman → Account Settings → API Keys) and set it as `POSTMAN_API_KEY` (below). Unlike every other vendor it is sent in the `X-API-Key` header, not `Authorization`. Read as plaintext from the `postman` config section or environment; never goes through the OS keychain.

edX is separate too: set `EDX_ACCESS_TOKEN` to a bearer token that can access the target course discussions. The default LMS base is `https://courses.edx.org`; set `EDX_API_BASE` for another Open edX instance. Discussion endpoints still enforce course enrollment/forum-role access and course discussion availability.

New Relic is separate too: create a [User API key](https://docs.newrelic.com/docs/apis/intro-apis/new-relic-api-keys/) (New Relic → user menu → API keys → User key) and set it as `NEW_RELIC_API_KEY`. Unlike every other vendor it is sent in the `API-Key` header, and its only API is **NerdGraph** (a single GraphQL endpoint), so the integration exposes one `newrelic_query` tool rather than five REST verbs. EU-region accounts must set `NEW_RELIC_REGION=eu`. Read as plaintext from the `newrelic` config section or environment; never goes through the OS keychain.

Grafana is separate too: create a [service-account token](https://grafana.com/docs/grafana/latest/administration/service-accounts/) (Grafana → Administration → Service accounts) and set it as `GRAFANA_TOKEN`, plus `GRAFANA_URL` for your instance base (e.g. `https://myorg.grafana.net` or `http://localhost:3000`). The token is sent as `Authorization: Bearer`. "Reading logs from Grafana" runs a LogQL query against a Loki datasource via Grafana's datasource proxy, so you first discover the Loki datasource `uid` with `grafana_list_datasources`, then pass it to `grafana_query_logs`. Read as plaintext from the `grafana` config section or environment; never goes through the OS keychain.

WRDS is separate too, and unlike every other vendor it is **not HTTP** — it is a PostgreSQL connection. Set `WRDS_USERNAME` and `WRDS_PASSWORD` to your [WRDS account](https://wrds-www.wharton.upenn.edu/) credentials; the host, port, and database default to the WRDS Cloud values (`wrds-pgdata.wharton.upenn.edu`, `9737`, `wrds`) and only need `WRDS_HOST` / `WRDS_PORT` / `WRDS_DBNAME` for a mirror or a local test database. The connection always uses SSL (`WRDS_SSLMODE` defaults to `require`). Access reflects your institution's WRDS subscriptions, and the account is read-only — this server additionally forces every session read-only and wraps each query so only a single `SELECT` runs. Read as plaintext from the `wrds` config section or environment; never goes through the OS keychain. Requires the binary to be built with the `wrds` feature (the default).

### Environment variables

| Variable | Purpose | Vendor scope |
|---|---|---|
| `ATLASSIAN_USER_EMAIL` | Atlassian account email (recommended auth) | all |
| `ATLASSIAN_API_TOKEN` | Scoped API token starting with `ATATT` | all |
| `ATLASSIAN_BITBUCKET_USERNAME` | Legacy fallback: Bitbucket username | bb only |
| `ATLASSIAN_BITBUCKET_APP_PASSWORD` | Legacy fallback: App Password | bb only |
| `BITBUCKET_DEFAULT_WORKSPACE` | Default workspace slug used when a tool/CLI call omits it | bb only |
| `ATLASSIAN_SITE_NAME` | Atlassian site shortname (e.g. `mycompany` for `mycompany.atlassian.net`). **Required** before invoking any `jira_*` or `conf_*` tool; only checked at tool-call time, so a Bitbucket-only setup boots without it. Jira and Confluence point at the same Atlassian site, so populating it under either the `jira` or `confluence` section of `~/.mcp/configs.json` works for both — duplication is unnecessary. | jira + conf |
| `ZOOM_ACCOUNT_ID` | Zoom Server-to-Server OAuth account ID. **Required** before invoking any `zoom_*` tool; only checked at tool-call time, so a non-Zoom setup boots without it. | zoom only |
| `ZOOM_CLIENT_ID` | Zoom S2S OAuth app client ID. | zoom only |
| `ZOOM_CLIENT_SECRET` | Zoom S2S OAuth app client secret. | zoom only |
| `CIRCLECI_TOKEN` | CircleCI personal API token, sent as `Authorization: Bearer`. **Required** before invoking any `circleci_*` tool; only checked at tool-call time, so a non-CircleCI setup boots without it. | circleci only |
| `SLACK_TOKEN` | Slack bot/user OAuth token (`xoxb-…` / `xoxp-…`), sent as `Authorization: Bearer`. **Required** before invoking any `slack_*` tool; only checked at tool-call time, so a non-Slack setup boots without it. | slack only |
| `POSTMAN_API_KEY` | Postman API key, sent in the `X-API-Key` header (not `Authorization`). **Required** before invoking any `postman_*` tool; only checked at tool-call time, so a non-Postman setup boots without it. | postman only |
| `EDX_ACCESS_TOKEN` | edX/Open edX bearer token for discussion API requests. **Required** before invoking any `edx_discussion_*` tool; only checked at tool-call time, so a non-edX setup boots without it. | edx only |
| `EDX_API_BASE` | Optional LMS base URL for edX/Open edX discussion APIs. Defaults to `https://courses.edx.org`; set this for another Open edX host. | edx only |
| `NEW_RELIC_API_KEY` | New Relic User API key, sent in the `API-Key` header (not `Authorization`). **Required** before invoking `newrelic_query`; only checked at tool-call time, so a non-New-Relic setup boots without it. | newrelic only |
| `NEW_RELIC_REGION` | New Relic data-center region: `us` (default) or `eu`. EU accounts must set `eu`, which targets `https://api.eu.newrelic.com`. | newrelic only |
| `NEW_RELIC_API_BASE` | Optional explicit NerdGraph base URL override (takes priority over `NEW_RELIC_REGION`). | newrelic only |
| `GRAFANA_URL` | Grafana instance base URL (e.g. `https://myorg.grafana.net` or `http://localhost:3000`). **Required** before invoking any `grafana_*` tool; only checked at tool-call time, so a non-Grafana setup boots without it. | grafana only |
| `GRAFANA_TOKEN` | Grafana service-account token (or API key), sent as `Authorization: Bearer`. **Required** before invoking any `grafana_*` tool; only checked at tool-call time. | grafana only |
| `WRDS_USERNAME` | WRDS account username for the PostgreSQL connection. **Required** before invoking any `wrds_*` tool; only checked at tool-call time, so a non-WRDS setup boots without it. | wrds only |
| `WRDS_PASSWORD` | WRDS account password. **Required** before invoking any `wrds_*` tool. | wrds only |
| `WRDS_HOST` | WRDS Postgres host. Defaults to `wrds-pgdata.wharton.upenn.edu`; override for a mirror or local test DB. | wrds only |
| `WRDS_PORT` | WRDS Postgres port. Defaults to `9737`. | wrds only |
| `WRDS_DBNAME` | WRDS database name. Defaults to `wrds`. | wrds only |
| `WRDS_SSLMODE` | TLS mode: `require` (default), `prefer`, or `disable` (local testing only). WRDS requires SSL. | wrds only |
| `TRANSPORT_MODE` | `stdio` (default) or `http` | shared |
| `PORT` | HTTP transport listening port (default `3000`, bound to `127.0.0.1`) | shared |
| `DEBUG` | Glob filter for debug logs (e.g. `DEBUG=*`) | shared |

Tokens can also be written to `~/.mcp/configs.json`. The Rust port supports per-vendor sections (`bitbucket`, `atlassian-bitbucket`, `jira`, `atlassian-jira`, `confluence`, `atlassian-confluence`, `zoom`, `mcp-server-zoom`, `circleci`, `circle-ci`, `mcp-server-circleci`, `slack`, `mcp-server-slack`, `postman`, `mcp-server-postman`, `edx`, `openedx`, `open-edx`, `mcp-server-edx`, `newrelic`, `new-relic`, `mcp-server-newrelic`, `grafana`, `mcp-server-grafana`, `wrds`, `mcp-server-wrds`) so each product's keys stay isolated:

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
  },
  "zoom": {
    "environments": {
      "ZOOM_ACCOUNT_ID": "abc123...",
      "ZOOM_CLIENT_ID": "client-id...",
      "ZOOM_CLIENT_SECRET": "client-secret..."
    }
  },
  "circleci": {
    "environments": {
      "CIRCLECI_TOKEN": "CCIPRJ_..."
    }
  },
  "slack": {
    "environments": {
      "SLACK_TOKEN": "xoxb-..."
    }
  },
  "postman": {
    "environments": {
      "POSTMAN_API_KEY": "PMAK-..."
    }
  },
  "edx": {
    "environments": {
      "EDX_ACCESS_TOKEN": "eyJ...",
      "EDX_API_BASE": "https://courses.edx.org"
    }
  },
  "newrelic": {
    "environments": {
      "NEW_RELIC_API_KEY": "NRAK-...",
      "NEW_RELIC_REGION": "us"
    }
  },
  "grafana": {
    "environments": {
      "GRAFANA_URL": "https://myorg.grafana.net",
      "GRAFANA_TOKEN": "glsa_..."
    }
  },
  "wrds": {
    "environments": {
      "WRDS_USERNAME": "your-wrds-username",
      "WRDS_PASSWORD": "your-wrds-password"
    }
  }
}
```

Credential keys (`ATLASSIAN_API_TOKEN`, `ATLASSIAN_USER_EMAIL`, `ATLASSIAN_BITBUCKET_*`, `ZOOM_*`, `CIRCLECI_TOKEN`, `SLACK_TOKEN`, `POSTMAN_API_KEY`, `EDX_ACCESS_TOKEN`, `NEW_RELIC_API_KEY`, `GRAFANA_TOKEN`, `WRDS_USERNAME`/`WRDS_PASSWORD`) are resolved **per vendor** — each section keeps its own. The same email may hold three independent Atlassian Cloud API tokens (one per product), and runtime auth picks the right one based on which vendor is serving the request. Non-credential shared keys can live in any section; if values disagree you must scope the lookup explicitly via `get_for(vendor, key)`. Process env and `.env` always take priority over the global file.

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

Keychain entries are scoped by `(kind, vendor, principal)` — the service name carries the vendor suffix (`mcp-server-atlassian.api-token.bitbucket`, `.jira`, `.confluence`), so the same email can hold a different token in each slot. `Config::get_for` itself is unaware of the keychain; the expansion happens entirely inside `auth::Credentials::resolve_with_for(config, backend, vendor)`. Non-secret keys (`ATLASSIAN_SITE_NAME`, `BITBUCKET_DEFAULT_WORKSPACE`, etc.) never trigger keychain reads.

#### CLI

```bash
# Store a token. `--vendor` is required: api-token slots are per-product,
# so the same email can hold one Bitbucket-scoped token, one Jira-scoped
# token, etc. (no echo when stdin is a tty; pipes work too).
mcp-atlassian creds set --kind api-token --vendor bitbucket --principal you@company.com
mcp-atlassian creds set --kind api-token --vendor jira       --principal you@company.com
mcp-atlassian creds set --kind api-token --vendor confluence --principal you@company.com

# App-passwords are Bitbucket-only.
mcp-atlassian creds set --kind app-password --vendor bitbucket --principal your-bb-username

# Confirm an entry exists (prints the last 4 chars only).
mcp-atlassian creds get --kind api-token --vendor bitbucket --principal you@company.com

# Remove an entry.
mcp-atlassian creds rm --kind api-token --vendor bitbucket --principal you@company.com

# One-shot migration: walk each vendor section in ~/.mcp/configs.json,
# copy each token to its own scoped keychain entry, replace each section's
# secret with the "keychain" sentinel, and write a .bak. Disagreement on
# the secret value across vendors is fine — three sections with three
# tokens produce three independent keychain entries.
mcp-atlassian creds migrate

# `creds migrate --force` overrides the stale-clobber guard: when the keychain
# already holds a different value than configs.json, force overwrites it
# (logged with both fingerprints). Without --force, that's a hard error so a
# rotated keychain entry can't be silently regressed by a stale file value.
mcp-atlassian creds migrate --force
```

There is no `creds list` — `keyring`'s `Entry` API has no portable enumeration. Inspect entries through the OS-native UI: **Keychain Access** on macOS, **`credwiz.exe`** on Windows, **`seahorse`** on Linux. Look for service names of the form `mcp-server-atlassian.api-token.<vendor>` or `mcp-server-atlassian.app-password.bitbucket`.

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

Restart your MCP client. The first time the server resolves the credential it logs an info-level breadcrumb (`source=keychain, kind=api-token, vendor=…, principal=…`) so you can confirm the keychain path was taken and which scope hit. After validating, delete the `.bak` file `creds migrate` left behind.

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

Forty-nine tools across eleven vendor families. The Atlassian tool names (`bb_*`, `jira_*`, `conf_*`) match the TS references one-to-one; the `zoom_*`, `circleci_*`, `slack_*`, `postman_*`, `edx_discussion_*`, `newrelic_query`, `grafana_*`, and `wrds_*` tools are native additions with no TS port. The four `wrds_*` tools require the `wrds` feature (default on).

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

### Zoom (`zoom_*`)

| Tool | Annotations | Use |
|---|---|---|
| `zoom_get` | read-only, idempotent | GET any Zoom API v2 endpoint (schedule, meeting details, users, "search") |
| `zoom_post` | mutating | POST to any endpoint (e.g. create a meeting) |
| `zoom_put` | mutating, idempotent | PUT to any endpoint (e.g. end a meeting, update settings) |
| `zoom_patch` | mutating | PATCH any endpoint (e.g. reschedule a meeting) |
| `zoom_delete` | destructive, idempotent | DELETE any endpoint (e.g. cancel a meeting) |

Zoom paths pass through verbatim relative to the `https://api.zoom.us/v2` base — supply e.g. `/users/me/meetings` (no version segment). There is **no separate search tool**: listing and searching are just `zoom_get` against the right path (`/users/me/meetings`, `/contacts?search_key=…`, `/users`, …). Authenticates with Server-to-Server OAuth; credentials (`ZOOM_ACCOUNT_ID` + `ZOOM_CLIENT_ID` + `ZOOM_CLIENT_SECRET`) are read from the `zoom` config section / environment (plaintext — the OS-keychain sentinel is Atlassian-only). Missing credentials surface as an authentication error at call time, so a non-Zoom deployment boots without them. The bearer is cached per server instance and renewed 60s before expiry.

> **Starting a meeting** is not a REST action: `zoom_post /users/me/meetings` returns a `start_url` the host opens to launch the Zoom client. **Reminders** are likewise not an API call — drive them from your scheduler (e.g. a cron/loop agent that polls `zoom_get /users/me/meetings`), not from this server.

### CircleCI (`circleci_*`)

| Tool | Annotations | Use |
|---|---|---|
| `circleci_get` | read-only, idempotent | GET any CircleCI API v2 endpoint (pipelines, workflows, jobs, insights, `/me`) |
| `circleci_post` | mutating | POST to any endpoint (e.g. trigger a pipeline, cancel/rerun a workflow) |
| `circleci_put` | mutating, idempotent | PUT to any endpoint (rarely used in v2) |
| `circleci_patch` | mutating | PATCH any endpoint (e.g. update a scheduled pipeline) |
| `circleci_delete` | destructive, idempotent | DELETE any endpoint (e.g. remove an env var, schedule, or context) |

CircleCI paths pass through verbatim relative to the `https://circleci.com/api/v2` base — supply e.g. `/project/{project-slug}/pipeline` (no version segment), where `project-slug` is `<vcs>/<org>/<repo>` (e.g. `gh/acme/web`) or `circleci/<org-id>/<project-id>`. There is **no separate search tool**: listing is just `circleci_get` against the right path. Authenticates with a personal API token (`CIRCLECI_TOKEN`, read from the `circleci` config section / environment as plaintext — the OS-keychain sentinel is Atlassian-only) sent as `Authorization: Bearer`. Missing the token surfaces as an authentication error at call time, so a non-CircleCI deployment boots without it. Pagination is token-based: pass a response's `next_page_token` back as the `page-token` query param.

### Slack (`slack_*`)

| Tool | Annotations | Use |
|---|---|---|
| `slack_get` | read-only, idempotent | GET any Slack Web API method (`/conversations.list`, `/users.info`, `/auth.test`, …) |
| `slack_post` | mutating | POST to any method (e.g. `/chat.postMessage`, `/conversations.create`, `/reactions.add`) |
| `slack_put` | mutating, idempotent | PUT to any method (rarely used — Slack is GET/POST) |
| `slack_patch` | mutating | PATCH any method (rarely used) |
| `slack_delete` | destructive, idempotent | DELETE verb (rarely used — Slack deletes via POST methods like `/chat.delete`) |

Slack endpoints are *methods* (`/conversations.list`), passed verbatim relative to the `https://slack.com/api` base. Almost everything is GET (query params) or POST (JSON body); the PUT/PATCH/DELETE verbs exist for completeness but Slack rarely uses them. Authenticates with a bot/user OAuth token (`SLACK_TOKEN`, read from the `slack` config section / environment as plaintext — the OS-keychain sentinel is Atlassian-only) sent as `Authorization: Bearer`. Missing the token surfaces as an authentication error at call time, so a non-Slack deployment boots without it. **Slack's defining quirk:** the Web API returns `200 OK` even on logical failures, signalling the real outcome with `{"ok": false, "error": "<code>"}` in the body — this server inspects that envelope and reclassifies `ok: false` as a typed error (auth codes → authentication error, `ratelimited` → 429, `*_not_found` → 404), so a successful tool result always means `ok: true`. Pagination is cursor-based: read `response_metadata.next_cursor` and pass it back as the `cursor` query param.

### Postman (`postman_*`)

| Tool | Annotations | Use |
|---|---|---|
| `postman_get` | read-only, idempotent | GET any Postman API endpoint (`/me`, `/workspaces`, `/collections`, `/environments`, …) |
| `postman_post` | mutating | POST to any endpoint (e.g. create a collection, environment, or workspace) |
| `postman_put` | mutating, idempotent | PUT to any endpoint (replace a collection / environment) |
| `postman_patch` | mutating | PATCH any endpoint (e.g. rename a workspace) |
| `postman_delete` | destructive, idempotent | DELETE any endpoint (remove a collection, environment, workspace, …) |

Postman paths pass through verbatim relative to the `https://api.getpostman.com` base — supply e.g. `/collections` or `/collections/{uid}` (item endpoints take a `uid` of the form `{ownerId}-{guid}`). Authenticates with an API key (`POSTMAN_API_KEY`, read from the `postman` config section / environment as plaintext — the OS-keychain sentinel is Atlassian-only) sent in the **`X-API-Key`** header rather than `Authorization` — Postman is the only vendor here that authenticates outside the standard auth header. Missing the key surfaces as an authentication error at call time, so a non-Postman deployment boots without it. Most write payloads are wrapped in a top-level resource key (`{"collection": {…}}`, `{"environment": {…}}`).

### New Relic (`newrelic_query`)

| Tool | Annotations | Use |
|---|---|---|
| `newrelic_query` | mutating, open-world | Run any NerdGraph (GraphQL) query against New Relic — NRQL queries, entity search, dashboards, alerts, account data |

New Relic's only API is **NerdGraph**, a single GraphQL endpoint, so unlike the REST vendors there are no five verbs — just one tool that POSTs a GraphQL document (and optional `variables`) to `/graphql`. NRQL queries are run by wrapping them in NerdGraph, e.g. `{ actor { account(id: 123) { nrql(query: "SELECT count(*) FROM Transaction SINCE 1 hour ago") { results } } } }`. Find your account id with `{ actor { accounts { id name } } }`. Authenticates with a User API key (`NEW_RELIC_API_KEY`, read from the `newrelic` config section / environment as plaintext — the OS-keychain sentinel is Atlassian-only) sent in the **`API-Key`** header. Missing the key surfaces as an authentication error at call time, so a non-New-Relic deployment boots without it. **NerdGraph's defining quirk:** query, validation, and most permission failures come back as `200 OK` with a top-level `errors` array — this server reclassifies a non-empty `errors` array as a typed error, so a successful tool result has no `errors`. EU-region accounts must set `NEW_RELIC_REGION=eu`. The `newrelic_query` tool is marked mutating because NerdGraph mutations (creating dashboards, alert policies, …) share the same endpoint as reads.

### Grafana (`grafana_*`)

| Tool | Annotations | Use |
|---|---|---|
| `grafana_query_logs` | read-only, idempotent | Read logs by running a LogQL query against a Loki datasource via Grafana's datasource proxy |
| `grafana_list_datasources` | read-only, idempotent | List configured datasources to discover a Loki datasource's `uid` |

Grafana is a query/visualization layer, not a log store — "reading logs from Grafana" means running a **LogQL** query against a **Loki** datasource through Grafana's **datasource proxy** (`GET /api/datasources/proxy/uid/{uid}/loki/api/v1/query_range`). First call `grafana_list_datasources` and copy the `uid` of an entry whose `type` is `loki` (filter with `jq`: `[?type=='loki'].{name: name, uid: uid}`), then pass it to `grafana_query_logs` as `datasourceUid` along with a `query` (LogQL) and optional `start`/`end`/`limit`/`direction`/`step`. Authenticates with a service-account token (`GRAFANA_TOKEN`, read from the `grafana` config section / environment as plaintext — the OS-keychain sentinel is Atlassian-only) sent as `Authorization: Bearer`; the instance base comes from `GRAFANA_URL`. Both are checked at call time, so a non-Grafana deployment boots without them. The same two tools work unchanged against self-hosted Grafana and Grafana Cloud. A bad LogQL query comes back from Loki as an HTTP error and is surfaced as a typed error.

### WRDS (`wrds_*`)

| Tool | Annotations | Use |
|---|---|---|
| `wrds_query` | read-only, idempotent | Run a read-only SQL `SELECT` against WRDS (e.g. `SELECT permno, date, ret FROM crsp.dsf WHERE …`) |
| `wrds_list_libraries` | read-only, idempotent | List the WRDS libraries (PostgreSQL schemas) your account can access |
| `wrds_list_tables` | read-only, idempotent | List the tables/views inside one library |
| `wrds_describe_table` | read-only, idempotent | Describe a table's columns (name, type, nullability) |

WRDS ([Wharton Research Data Services](https://wrds-www.wharton.upenn.edu/)) is a **PostgreSQL** data platform for finance/accounting/economics research, not an HTTP API — so these tools connect directly to `wrds-pgdata.wharton.upenn.edu:9737` over SSL (the access path the official `wrds` Python package wraps). A WRDS "library" is a Postgres **schema** and a dataset is a `library.table` (e.g. `crsp.dsf`, `comp.funda`, `ff.factors_daily`). The typical flow is **discover then query**: `wrds_list_libraries` → `wrds_list_tables` → `wrds_describe_table` to learn exact column names, then `wrds_query` with a tight `WHERE` and small `rowLimit` (WRDS tables are huge). Authenticates with a WRDS username + password (`WRDS_USERNAME` / `WRDS_PASSWORD`, read from the `wrds` config section / environment as plaintext — the OS-keychain sentinel is Atlassian-only); missing credentials surface as an authentication error at call time, so a non-WRDS deployment boots without them. **Safety:** every session is forced read-only (`default_transaction_read_only = on`) with a statement timeout, and `wrds_query` wraps the caller's SQL in a subquery so only a single `SELECT`/`VALUES` can run — writes, DDL, and multi-statement bodies are rejected. PostgreSQL renders each result set to JSON server-side (`to_jsonb`), so `jq`/`outputFormat` behave exactly as they do for the HTTP vendors. These tools require the `wrds` Cargo feature (on by default); a `--no-default-features` build omits them.

### Shared inputs

All API tools accept `path` (required), `queryParams` (optional JSON map), `jq` (optional JMESPath filter to reduce token cost), and `outputFormat` (`toon` default, `json` alternative). The exceptions are `newrelic_query`, which takes `query` (GraphQL string) and optional `variables` instead of `path`/`queryParams`; the `grafana_*` tools, which take typed inputs (`datasourceUid`/`query`/range knobs for `grafana_query_logs`; no path for either); and the `wrds_*` tools, which take typed inputs (`sql` + optional `rowLimit` for `wrds_query`; `library`/`table` for the discovery tools; no path) and still honour `jq`/`outputFormat`.

## Cross-vendor workflows

This server ships **primitives, not orchestration**: it exposes raw HTTP verbs per vendor, and the *agent* chains them. There is no `do_everything(PROJ-123)` tool. The recipes below spell out the path patterns an agent needs, because the links *between* systems (Jira→PR, PR→build) are not stored anywhere — they're matched by issue-dev-panel data and by branch/commit.

### From a Jira key to its PR, review state, and build

Given `PROJ-123`:

1. **Read the issue + get its numeric id.** `jira_get /rest/api/3/issue/PROJ-123` with `jq: "{id: id, key: key, summary: fields.summary, status: fields.status.name}"`. The dev-panel API in the next step needs the **numeric** `id`, not the key.
2. **Find linked branches/PRs** (requires the Bitbucket–Jira integration to be enabled). `jira_get /rest/dev-status/latest/issue/detail` with `queryParams: {"issueId": "<numeric id>", "applicationType": "bitbucket", "dataType": "pullrequest"}` → returns `detail[].pullRequests[]` with each PR's `id`, `url`, `status`, and `source.branch`. Use `dataType: "branch"` for branches or `"repository"` for commits.
3. **Read the PR and its review state.** `bb_get /repositories/{workspace}/{repo}/pullrequests/{pr-id}` for the PR; `bb_get .../pullrequests/{pr-id}/activity` for approvals and review activity.
4. **Write a comment back.** `bb_post /repositories/{workspace}/{repo}/pullrequests/{pr-id}/comments` with `body: {"content": {"raw": "Build is green ✅"}}`.
5. **Check build status for the PR's branch.** CircleCI is keyed by branch, not by PR: `circleci_get /project/{slug}/pipeline` with `queryParams: {"branch": "<source.branch from step 2>"}` → take the latest pipeline `id` → `circleci_get /pipeline/{id}/workflow` → `circleci_get /workflow/{workflow-id}/job`. Each job carries `status` and `job_number`. (`{slug}` is `<vcs>/<org>/<repo>`, e.g. `gh/acme/web`.)

### Why did the build fail?

From the failed job's `job_number` (step 5 above):

- **Which step / how it failed:** `circleci_get /project/{slug}/job/{job-number}` → job status, duration, executor.
- **Failed test details:** `circleci_get /project/{slug}/{job-number}/tests` → `items[]` with `name`, `result`, `message`, `file` — the cleanest "reason" **when the job stores test results** (`store_test_results`).
- **Raw step logs are out of scope here.** They live behind CircleCI's older v1.1 API or per-step S3 `output_url`s — neither sits under the fixed `…/api/v2` base this tool targets, so fetch those outside the server if you need full log text.

> These chains are only as reliable as the underlying setup: step 2 needs the Jira↔Bitbucket app installed, and steps 5–6 assume the repo actually builds on CircleCI for that branch. The agent infers the PR→pipeline link from the branch name; it is not a stored relationship.

## CLI usage

Three subcommand groups — one per Atlassian vendor — keep the verbs unambiguous. (Zoom, CircleCI, Slack, Postman, edX, New Relic, Grafana, and WRDS are MCP-only: there is no CLI group for them, so `zoom_*`, `circleci_*`, `slack_*`, `postman_*`, `edx_discussion_*`, `newrelic_query`, `grafana_*`, and `wrds_*` are reachable through an MCP client, not the command line.)

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
