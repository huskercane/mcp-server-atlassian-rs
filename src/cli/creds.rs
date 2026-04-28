//! Credential management CLI (`mcp-atlassian creds …`).
//!
//! Stores Atlassian API tokens / Bitbucket app-passwords in the OS keychain
//! so they don't have to live in plaintext in `~/.mcp/configs.json` or in
//! the launcher's `.mcp.json` `env` block.
//!
//! Subcommands:
//! - `set --kind ... --principal ...` — read secret from stdin (no echo if
//!   tty), store it in the keychain, verify roundtrip.
//! - `get --kind ... --principal ...` — confirm presence by printing a
//!   redacted last-4 fingerprint.
//! - `rm  --kind ... --principal ...` — delete the keychain entry.
//! - `migrate [--force]` — read `~/.mcp/configs.json`, copy plaintext
//!   secrets to the keychain, replace each with the literal `"keychain"`
//!   sentinel, write a `.bak` alongside. See [`migrate`] for the full
//!   atomicity contract.
//!
//! Lookup at runtime is implemented in [`crate::auth::Credentials::resolve_with`].
//!
//! `creds list` is intentionally absent — `keyring`'s `Entry` API has no
//! portable enumeration. Inspect entries via the OS-native UI: Keychain
//! Access on macOS, `credwiz.exe` on Windows, `seahorse` on Linux. Look
//! for the `mcp-server-atlassian.*` service prefix.

use clap::{Args, Subcommand};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde_json::Value;

use crate::auth::keychain::{KeychainBackend, KeychainError, OsKeychain, SecretKind};
use crate::config::{self, VENDOR_BITBUCKET, VENDOR_CONFLUENCE, VENDOR_JIRA};
use crate::constants::PACKAGE_NAME;
use crate::error::{McpError, unexpected};

/// Maximum supported secret length. Bound by Windows Credential Manager,
/// which caps the credential blob at 2560 bytes (DPAPI overhead included).
/// We reject earlier with a clear message rather than letting the OS error.
const MAX_SECRET_BYTES: usize = 2048;

/// Verbs exposed under `mcp-atlassian creds …`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Store a secret in the OS keychain.
    Set(SetOpts),
    /// Confirm a secret exists; prints a redacted fingerprint (last 4).
    Get(SelectOpts),
    /// Delete a secret from the OS keychain.
    Rm(SelectOpts),
    /// Migrate plaintext secrets from `~/.mcp/configs.json` into the OS
    /// keychain and replace them with the `"keychain"` sentinel.
    Migrate(MigrateOpts),
}

/// Shared `--kind` + `--vendor` + `--principal` selector.
#[derive(Debug, Args)]
pub struct SelectOpts {
    /// Secret kind: `api-token` (Atlassian Cloud) or `app-password`
    /// (Bitbucket).
    #[arg(long, value_parser = parse_kind)]
    pub kind: SecretKind,
    /// Atlassian product the secret belongs to: `bitbucket`, `jira`, or
    /// `confluence`. The same email may have a different token per
    /// vendor, so the slot is vendor-scoped.
    #[arg(long, value_parser = parse_vendor)]
    pub vendor: String,
    /// Account identifier — email for `api-token`, username for
    /// `app-password`. Same string `Credentials::principal()` returns.
    #[arg(long)]
    pub principal: String,
}

#[derive(Debug, Args)]
pub struct SetOpts {
    #[arg(long, value_parser = parse_kind)]
    pub kind: SecretKind,
    #[arg(long, value_parser = parse_vendor)]
    pub vendor: String,
    #[arg(long)]
    pub principal: String,
    /// When set, read the secret from stdin as a plain line without
    /// disabling tty echo. Useful in scripts; default behaviour
    /// auto-detects a tty.
    #[arg(long)]
    pub from_stdin: bool,
}

#[derive(Debug, Args)]
pub struct MigrateOpts {
    /// Overwrite an existing keychain entry whose value differs from the
    /// configs.json value. Without this flag, that case is a hard error
    /// (stale-clobber guard) — runtime auth picks up the keychain value,
    /// so silently overwriting it with a stale plaintext token would
    /// regress live authentication.
    #[arg(long)]
    pub force: bool,
}

/// Entry point used by [`crate::cli::run`]. Production callers go through
/// here, which constructs an [`OsKeychain`]. Tests call the per-subcommand
/// functions directly with an [`InMemoryKeychain`](crate::auth::InMemoryKeychain).
///
/// `async` is kept to match the `bb`/`jira`/`conf` dispatch signatures so
/// the `cli::run` match arms compose uniformly; the body itself doesn't
/// await anything yet.
#[allow(clippy::unused_async)]
pub async fn dispatch(command: Command) -> Result<(), McpError> {
    let backend = OsKeychain::new();
    match command {
        Command::Set(opts) => set(&backend, opts),
        Command::Get(opts) => get(&backend, opts),
        Command::Rm(opts) => rm(&backend, opts),
        Command::Migrate(opts) => {
            let path = config::default_global_path().ok_or_else(|| {
                unexpected("could not resolve $HOME/.mcp/configs.json path", None)
            })?;
            let outcome = migrate_with(&backend, &path, opts.force)?;
            print_migrate_summary(&outcome);
            Ok(())
        }
    }
}

fn parse_kind(s: &str) -> Result<SecretKind, String> {
    SecretKind::parse(s).ok_or_else(|| {
        format!(
            "unknown kind '{s}': use one of 'api-token' or 'app-password' \
             (env-var spellings ATLASSIAN_API_TOKEN / ATLASSIAN_BITBUCKET_APP_PASSWORD \
             also accepted)"
        )
    })
}

fn parse_vendor(s: &str) -> Result<String, String> {
    match s {
        VENDOR_BITBUCKET | VENDOR_JIRA | VENDOR_CONFLUENCE => Ok(s.to_owned()),
        _ => Err(format!(
            "unknown vendor '{s}': use one of '{VENDOR_BITBUCKET}', '{VENDOR_JIRA}', \
             or '{VENDOR_CONFLUENCE}'"
        )),
    }
}

/// Reject `--kind app-password --vendor <not-bitbucket>`. Atlassian
/// app-passwords are a Bitbucket-only auth scheme; runtime auth (see
/// `Credentials::resolve_with_for`) will never read an app-password
/// keychain entry under another vendor's scope, so creating, reading,
/// or deleting one is dead state. Surface this at CLI parse time
/// instead of letting the backend round-trip succeed and mislead.
fn ensure_kind_vendor_combo(kind: SecretKind, vendor: &str) -> Result<(), McpError> {
    if matches!(kind, SecretKind::AppPassword) && vendor != VENDOR_BITBUCKET {
        return Err(unexpected(
            format!(
                "kind=app-password is Bitbucket-only; vendor `{vendor}` does not \
                 support it. Drop --vendor or pass --vendor {VENDOR_BITBUCKET}."
            ),
            None,
        ));
    }
    Ok(())
}

/// `creds set` handler. Public so tests can call it directly.
#[allow(clippy::needless_pass_by_value)]
pub fn set(backend: &dyn KeychainBackend, opts: SetOpts) -> Result<(), McpError> {
    ensure_kind_vendor_combo(opts.kind, &opts.vendor)?;
    if opts.principal.is_empty() {
        return Err(unexpected("--principal must not be empty", None));
    }

    let secret = read_secret(opts.from_stdin)?;
    if secret.is_empty() {
        return Err(unexpected("secret read from stdin was empty", None));
    }
    if secret.len() > MAX_SECRET_BYTES {
        return Err(unexpected(
            format!(
                "secret is {} bytes; refusing to store more than {MAX_SECRET_BYTES} bytes \
                 (Windows Credential Manager caps at ~2560 bytes including overhead)",
                secret.len()
            ),
            None,
        ));
    }

    backend
        .set(opts.kind, &opts.vendor, &opts.principal, &secret)
        .map_err(|e| unexpected(format!("keychain set failed: {e}"), None))?;

    // Verify roundtrip — catches mock backends that silently no-op.
    match backend.get(opts.kind, &opts.vendor, &opts.principal) {
        Ok(Some(stored)) if stored == secret => {}
        Ok(Some(_)) => {
            return Err(unexpected(
                "keychain readback returned a different value than was just written; \
                 the backend may be unreliable. Refusing to claim success.",
                None,
            ));
        }
        Ok(None) => {
            return Err(unexpected(
                "keychain readback returned no entry after a successful set; \
                 the backend appears to be a no-op stub. Refusing to claim success.",
                None,
            ));
        }
        Err(e) => {
            return Err(unexpected(
                format!("keychain readback failed after successful set: {e}"),
                None,
            ));
        }
    }

    println!(
        "stored {} for vendor {} principal {} (service={})",
        opts.kind,
        opts.vendor,
        opts.principal,
        opts.kind.service_for(&opts.vendor)
    );
    Ok(())
}

/// `creds get` handler. Prints a redacted fingerprint to confirm presence
/// without leaking the secret.
#[allow(clippy::needless_pass_by_value)]
pub fn get(backend: &dyn KeychainBackend, opts: SelectOpts) -> Result<(), McpError> {
    ensure_kind_vendor_combo(opts.kind, &opts.vendor)?;
    match backend.get(opts.kind, &opts.vendor, &opts.principal) {
        Ok(Some(secret)) => {
            println!(
                "{} for vendor {} principal {} is set ({})",
                opts.kind,
                opts.vendor,
                opts.principal,
                fingerprint(&secret)
            );
            Ok(())
        }
        Ok(None) => Err(unexpected(
            format!(
                "no keychain entry for kind={}, vendor={}, principal={}",
                opts.kind, opts.vendor, opts.principal
            ),
            None,
        )),
        Err(e) => Err(unexpected(format!("keychain get failed: {e}"), None)),
    }
}

/// `creds rm` handler.
#[allow(clippy::needless_pass_by_value)]
pub fn rm(backend: &dyn KeychainBackend, opts: SelectOpts) -> Result<(), McpError> {
    ensure_kind_vendor_combo(opts.kind, &opts.vendor)?;
    match backend.delete(opts.kind, &opts.vendor, &opts.principal) {
        Ok(()) => {
            println!(
                "removed {} for vendor {} principal {}",
                opts.kind, opts.vendor, opts.principal
            );
            Ok(())
        }
        Err(KeychainError::NotFound) => Err(unexpected(
            format!(
                "no keychain entry to remove for kind={}, vendor={}, principal={}",
                opts.kind, opts.vendor, opts.principal
            ),
            None,
        )),
        Err(e) => Err(unexpected(format!("keychain delete failed: {e}"), None)),
    }
}

/// Read a secret from stdin. Hides input when stdin is a tty (uses
/// `rpassword`); reads a plain line when stdin is piped, so scripted
/// setups (`echo $TOKEN | mcp-atlassian creds set ...`) still work.
fn read_secret(force_plain: bool) -> Result<String, McpError> {
    let stdin_is_tty = io::stdin().is_terminal();
    if stdin_is_tty && !force_plain {
        let secret = rpassword::prompt_password("Secret (input hidden): ")
            .map_err(|e| unexpected(format!("failed to read secret: {e}"), None))?;
        Ok(secret)
    } else {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| unexpected(format!("failed to read stdin: {e}"), None))?;
        // Strip a single trailing newline if present so `echo $TOKEN | ...`
        // doesn't accidentally store a token-with-newline.
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        Ok(buf)
    }
}

/// Last-4 fingerprint, with a fixed prefix so short secrets aren't fully
/// printed. Used for `creds get` and for `creds migrate --force` warning
/// log lines.
pub(crate) fn fingerprint(secret: &str) -> String {
    let len = secret.chars().count();
    if len <= 4 {
        return "****".to_string();
    }
    let tail: String = secret.chars().skip(len - 4).collect();
    format!("****{tail}")
}

// ----------------------------------------------------------------------------
// `creds migrate`
// ----------------------------------------------------------------------------

/// Outcome of a successful migration. Pure data so tests can assert on it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrateOutcome {
    /// Credentials newly written to the keychain (or already-present and
    /// confirmed equal — both end with the file rewritten to `"keychain"`).
    pub migrated: Vec<MigrateRecord>,
    /// Reasons specific candidates were skipped without error.
    pub skipped: Vec<MigrateSkip>,
    /// Path of the `.bak` file created next to the rewritten configs.json.
    /// Always present on success; the caller is expected to delete it
    /// after validating the migration.
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MigrateRecord {
    pub kind: SecretKind,
    pub vendor: String,
    pub principal: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MigrateSkip {
    /// Sentinel was already present and the keychain entry verified.
    AlreadyMigrated {
        kind: SecretKind,
        vendor: String,
        principal: String,
    },
    /// Plaintext value matches an existing keychain entry; no write needed,
    /// file still rewritten to `"keychain"`.
    InSync {
        kind: SecretKind,
        vendor: String,
        principal: String,
    },
    /// Neither principal nor secret is set in this vendor section.
    NotConfigured { kind: SecretKind, vendor: String },
    /// Principal is set but secret is missing (no plaintext to migrate).
    /// Mirrors the runtime auth fallback semantics.
    PartiallyConfigured { kind: SecretKind, vendor: String },
}

/// Migrate plaintext secrets in `configs_path` to the keychain. Pure — does
/// not read process env or `.env`. Public so tests call it directly with
/// an in-memory backend and a tempdir-relative path.
///
/// Each canonical vendor section (`bitbucket` / `jira` / `confluence`) is
/// migrated independently. The same email may have a different token per
/// vendor — that is the supported model — so cross-vendor disagreement on
/// a secret value is *not* an error. Three vendor sections with three
/// different tokens produce three keychain entries, each scoped by vendor.
///
/// On success, writes `<configs_path>.bak` (full original) and
/// atomic-replaces `configs_path` with each migrated secret replaced by
/// the literal `"keychain"` sentinel within its own vendor section.
///
/// On failure, neither the file nor the keychain is left in an
/// intermediate state: any keychain writes performed earlier in the run
/// are rolled back to their prior values (or deleted if no prior value
/// existed).
#[allow(clippy::too_many_lines)]
pub fn migrate_with(
    backend: &dyn KeychainBackend,
    configs_path: &Path,
    force: bool,
) -> Result<MigrateOutcome, McpError> {
    if !configs_path.exists() {
        return Err(unexpected(
            format!(
                "no global config at {}; nothing to migrate",
                configs_path.display()
            ),
            None,
        ));
    }

    // Read + parse the file up front so we can fail before touching the
    // keychain if it isn't valid JSON.
    let raw = std::fs::read(configs_path)
        .map_err(|e| unexpected(format!("read {}: {e}", configs_path.display()), None))?;
    let mut json: Value = serde_json::from_slice(&raw).map_err(|e| {
        unexpected(
            format!(
                "{} is not valid JSON: {e}. Fix the file or pass an alternate path.",
                configs_path.display()
            ),
            None,
        )
    })?;

    let aliases = config::vendor_aliases(PACKAGE_NAME);

    // Plan every (vendor, candidate) tuple before any keychain write so we
    // can fail fast on misconfiguration without leaving partial state.
    let mut planned: Vec<PlannedAction> = Vec::new();
    let mut skipped: Vec<MigrateSkip> = Vec::new();

    for (canonical, alias_list) in &aliases {
        // App-passwords are Bitbucket-only; runtime auth never reads them
        // for any other vendor, so migrating them under jira/confluence
        // would silently store dead state.
        let candidates: &[Candidate] = if *canonical == VENDOR_BITBUCKET {
            &[
                Candidate {
                    kind: SecretKind::ApiToken,
                    principal_key: "ATLASSIAN_USER_EMAIL",
                    secret_key: "ATLASSIAN_API_TOKEN",
                },
                Candidate {
                    kind: SecretKind::AppPassword,
                    principal_key: "ATLASSIAN_BITBUCKET_USERNAME",
                    secret_key: "ATLASSIAN_BITBUCKET_APP_PASSWORD",
                },
            ]
        } else {
            &[Candidate {
                kind: SecretKind::ApiToken,
                principal_key: "ATLASSIAN_USER_EMAIL",
                secret_key: "ATLASSIAN_API_TOKEN",
            }]
        };

        for candidate in candidates {
            match plan_candidate(candidate, canonical, alias_list, &json)? {
                CandidateOutcome::Migrate(plan) => planned.push(plan),
                CandidateOutcome::Skip(reason) => skipped.push(reason),
            }
        }
    }

    // Execute keychain writes with rollback on failure.
    let mut applied: Vec<RollbackEntry> = Vec::new();
    let mut migrated: Vec<MigrateRecord> = Vec::new();
    let mut to_rewrite: Vec<&PlannedAction> = Vec::new();

    for plan in &planned {
        let prior = backend
            .get(plan.kind, &plan.vendor, &plan.principal)
            .map_err(|e| unexpected(format!("keychain pre-read failed: {e}"), None))?;

        match (&prior, plan.action) {
            (Some(existing), PlannedKind::WriteFromPlaintext) if existing == &plan.value => {
                // Keychain already has this exact value — no write, but the
                // file still gets the sentinel.
                skipped.push(MigrateSkip::InSync {
                    kind: plan.kind,
                    vendor: plan.vendor.clone(),
                    principal: plan.principal.clone(),
                });
                to_rewrite.push(plan);
            }
            (Some(existing), PlannedKind::WriteFromPlaintext) if existing != &plan.value => {
                if !force {
                    rollback(&applied, backend);
                    return Err(unexpected(
                        format!(
                            "keychain already has a different value for kind={}, \
                             vendor={}, principal={} (plaintext={}, keychain={}). The \
                             keychain value looks fresh; the configs.json value looks \
                             stale. Re-run with --force to overwrite the keychain, or \
                             remove the secret from the `{}` section to discard the \
                             plaintext.",
                            plan.kind,
                            plan.vendor,
                            plan.principal,
                            fingerprint(&plan.value),
                            fingerprint(existing),
                            plan.vendor,
                        ),
                        None,
                    ));
                }
                tracing::warn!(
                    kind = %plan.kind,
                    vendor = plan.vendor.as_str(),
                    principal = plan.principal.as_str(),
                    plaintext_fp = fingerprint(&plan.value),
                    keychain_fp = fingerprint(existing),
                    "--force: overwriting existing keychain entry"
                );
                if let Err(e) =
                    backend.set(plan.kind, &plan.vendor, &plan.principal, &plan.value)
                {
                    rollback(&applied, backend);
                    return Err(unexpected(format!("keychain set failed: {e}"), None));
                }
                if let Err(e) = verify_set(backend, plan) {
                    rollback(&applied, backend);
                    return Err(e);
                }
                applied.push(RollbackEntry {
                    kind: plan.kind,
                    vendor: plan.vendor.clone(),
                    principal: plan.principal.clone(),
                    prior: prior.clone(),
                });
                migrated.push(MigrateRecord {
                    kind: plan.kind,
                    vendor: plan.vendor.clone(),
                    principal: plan.principal.clone(),
                });
                to_rewrite.push(plan);
            }
            (_, PlannedKind::WriteFromPlaintext) => {
                // Either no prior or non-conflict; do the write.
                if let Err(e) =
                    backend.set(plan.kind, &plan.vendor, &plan.principal, &plan.value)
                {
                    rollback(&applied, backend);
                    return Err(unexpected(format!("keychain set failed: {e}"), None));
                }
                if let Err(e) = verify_set(backend, plan) {
                    rollback(&applied, backend);
                    return Err(e);
                }
                applied.push(RollbackEntry {
                    kind: plan.kind,
                    vendor: plan.vendor.clone(),
                    principal: plan.principal.clone(),
                    prior: prior.clone(),
                });
                migrated.push(MigrateRecord {
                    kind: plan.kind,
                    vendor: plan.vendor.clone(),
                    principal: plan.principal.clone(),
                });
                to_rewrite.push(plan);
            }
            (Some(s), PlannedKind::VerifySentinel) if !s.is_empty() => {
                // Sentinel + non-empty entry — no-op for keychain side; the
                // file already has the sentinel.
                skipped.push(MigrateSkip::AlreadyMigrated {
                    kind: plan.kind,
                    vendor: plan.vendor.clone(),
                    principal: plan.principal.clone(),
                });
            }
            (_, PlannedKind::VerifySentinel) => {
                // Either no entry, or an empty-string entry. Runtime auth
                // rejects empty secrets at src/auth/mod.rs (the
                // `Some(s) if !s.is_empty()` guard on the sentinel path),
                // so claiming "already migrated" here would set the user
                // up for a hard 401 on the next request. Surface the
                // problem now instead.
                rollback(&applied, backend);
                let detail = match &prior {
                    Some(_) => "the keychain entry exists but is empty",
                    None => "no keychain entry exists",
                };
                return Err(unexpected(
                    format!(
                        "config has \"keychain\" sentinel for kind={}, vendor={}, \
                         principal={}, but {detail}. Run \
                         `mcp-atlassian creds set --kind {} --vendor {} --principal {}` \
                         or remove the sentinel from configs.json.",
                        plan.kind,
                        plan.vendor,
                        plan.principal,
                        plan.kind,
                        plan.vendor,
                        plan.principal,
                    ),
                    None,
                ));
            }
        }
    }

    // Rewrite the JSON: for every alias of each migrated vendor, set
    // environments[secret_key] = "keychain" if currently a non-empty
    // string. Other vendors' sections are not touched.
    if !to_rewrite.is_empty() {
        for plan in &to_rewrite {
            let alias_list = aliases
                .iter()
                .find(|(c, _)| *c == plan.vendor)
                .map_or(&[][..], |(_, list)| list.as_slice());
            rewrite_vendor_aliases_to_sentinel(&mut json, plan.secret_key, alias_list);
        }
    }

    // Backup before file rewrite.
    let backup_path = configs_path.with_file_name(format!(
        "{}.bak",
        configs_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("configs.json")
    ));
    if !to_rewrite.is_empty() {
        if let Err(e) = std::fs::copy(configs_path, &backup_path) {
            rollback(&applied, backend);
            return Err(unexpected(
                format!("write backup at {}: {e}", backup_path.display()),
                None,
            ));
        }

        // Atomic replace pinned to the target's parent directory so the
        // tmp file stays on the same filesystem (atomicwrites's default
        // tmpdir is `./.tmp` which can land elsewhere).
        let parent = configs_path
            .parent()
            .ok_or_else(|| unexpected("configs_path has no parent dir", None))?;
        let af = AtomicFile::new_with_tmpdir(configs_path, AllowOverwrite, parent);
        let body = serde_json::to_vec_pretty(&json)
            .map_err(|e| unexpected(format!("serialize config: {e}"), None))?;
        if let Err(e) = af.write(|f| f.write_all(&body)) {
            rollback(&applied, backend);
            return Err(unexpected(
                format!("atomic write of {}: {e}", configs_path.display()),
                None,
            ));
        }
    }

    Ok(MigrateOutcome {
        migrated,
        skipped,
        backup_path: if to_rewrite.is_empty() {
            None
        } else {
            Some(backup_path)
        },
    })
}

fn print_migrate_summary(outcome: &MigrateOutcome) {
    if outcome.migrated.is_empty() {
        println!("nothing to migrate; no plaintext secrets found");
    } else {
        println!(
            "migrated {} credential(s) to OS keychain",
            outcome.migrated.len()
        );
        for rec in &outcome.migrated {
            println!("  - {} for vendor {} principal {}", rec.kind, rec.vendor, rec.principal);
        }
    }
    for skip in &outcome.skipped {
        match skip {
            MigrateSkip::AlreadyMigrated {
                kind,
                vendor,
                principal,
            } => {
                println!("  (already migrated) {kind} for vendor {vendor} principal {principal}");
            }
            MigrateSkip::InSync {
                kind,
                vendor,
                principal,
            } => {
                println!(
                    "  (in sync; rewriting file) {kind} for vendor {vendor} principal {principal}"
                );
            }
            MigrateSkip::NotConfigured { kind, vendor } => {
                println!("  (not configured) {kind} for vendor {vendor}");
            }
            MigrateSkip::PartiallyConfigured { kind, vendor } => {
                println!(
                    "  (partial; principal set but secret missing) {kind} for vendor \
                     {vendor}; leaving as-is per runtime auth fallback rules"
                );
            }
        }
    }
    if let Some(bak) = &outcome.backup_path {
        println!(
            "backup at {} — delete it once the server authenticates",
            bak.display()
        );
    }
}

// ---- internals ----

struct Candidate {
    kind: SecretKind,
    principal_key: &'static str,
    secret_key: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedKind {
    /// Write the captured plaintext value into the keychain.
    WriteFromPlaintext,
    /// Sentinel already present in file; verify the keychain has an entry.
    VerifySentinel,
}

#[derive(Debug)]
struct PlannedAction {
    kind: SecretKind,
    vendor: String,
    principal: String,
    secret_key: &'static str,
    value: String, // plaintext; empty for VerifySentinel
    action: PlannedKind,
}

enum CandidateOutcome {
    Migrate(PlannedAction),
    Skip(MigrateSkip),
}

#[derive(Debug)]
struct RollbackEntry {
    kind: SecretKind,
    vendor: String,
    principal: String,
    prior: Option<String>,
}

/// Read a string-valued env entry from the canonical vendor's section,
/// merging across the vendor's alias list. The first alias in priority
/// order with a non-empty string wins. Within a single canonical vendor,
/// disagreement between alias spellings is still a conflict — that is a
/// copy-paste / migration mistake, not a per-product choice — and is
/// surfaced as `Err`.
fn read_vendor_string<'a>(
    json: &'a Value,
    canonical: &str,
    alias_list: &[String],
    key: &str,
) -> Result<Option<&'a str>, McpError> {
    let mut chosen: Option<(&str, &str)> = None;
    let mut conflicts: Vec<(String, String)> = Vec::new();
    for alias in alias_list {
        let Some(val) = json
            .get(alias)
            .and_then(|s| s.get("environments"))
            .and_then(|env| env.get(key))
        else {
            continue;
        };
        match val {
            Value::Null => {}
            Value::String(s) if s.is_empty() => {}
            Value::String(s) => match chosen {
                None => chosen = Some((alias.as_str(), s.as_str())),
                Some((_, prev)) if prev == s.as_str() => {}
                Some(_) => conflicts.push((alias.clone(), s.clone())),
            },
            other => {
                return Err(unexpected(
                    format!(
                        "{key} in section `{alias}` is a JSON {} value; migrate refuses \
                         to coerce. Quote the value as a string in configs.json first.",
                        json_type_name(other)
                    ),
                    None,
                ));
            }
        }
    }

    if !conflicts.is_empty()
        && let Some((first_alias, first_val)) = chosen
    {
        let mut all = vec![(first_alias.to_owned(), first_val.to_owned())];
        all.extend(conflicts);
        let rendered = all
            .iter()
            .map(|(a, v)| format!("{a}={}", fingerprint(v)))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(unexpected(
            format!(
                "alias conflict for {key} within canonical vendor `{canonical}`: \
                 {rendered}. Reconcile manually before migrating.",
            ),
            None,
        ));
    }

    Ok(chosen.map(|(_, v)| v))
}

fn plan_candidate(
    candidate: &Candidate,
    canonical: &str,
    alias_list: &[String],
    json: &Value,
) -> Result<CandidateOutcome, McpError> {
    let principal = read_vendor_string(json, canonical, alias_list, candidate.principal_key)?;
    let secret = read_vendor_string(json, canonical, alias_list, candidate.secret_key)?;

    match (principal, secret) {
        (None, None) => Ok(CandidateOutcome::Skip(MigrateSkip::NotConfigured {
            kind: candidate.kind,
            vendor: canonical.to_owned(),
        })),
        (None, Some("keychain")) => Err(unexpected(
            format!(
                "vendor `{canonical}` sets {}=\"keychain\" but {} is missing; cannot migrate",
                candidate.secret_key, candidate.principal_key
            ),
            None,
        )),
        (None, Some(_)) => Err(unexpected(
            format!(
                "plaintext {} is set in vendor `{canonical}` but {} is missing; \
                 migration cannot move it safely. Add {} or remove {} first.",
                candidate.secret_key,
                candidate.principal_key,
                candidate.principal_key,
                candidate.secret_key
            ),
            None,
        )),
        (Some(_), None) => Ok(CandidateOutcome::Skip(MigrateSkip::PartiallyConfigured {
            kind: candidate.kind,
            vendor: canonical.to_owned(),
        })),
        (Some(principal), Some("keychain")) => {
            Ok(CandidateOutcome::Migrate(PlannedAction {
                kind: candidate.kind,
                vendor: canonical.to_owned(),
                principal: principal.to_owned(),
                secret_key: candidate.secret_key,
                value: String::new(),
                action: PlannedKind::VerifySentinel,
            }))
        }
        (Some(principal), Some(secret)) => Ok(CandidateOutcome::Migrate(PlannedAction {
            kind: candidate.kind,
            vendor: canonical.to_owned(),
            principal: principal.to_owned(),
            secret_key: candidate.secret_key,
            value: secret.to_owned(),
            action: PlannedKind::WriteFromPlaintext,
        })),
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Replace `secret_key` with the `"keychain"` sentinel inside every alias
/// of the supplied vendor. Other vendors' sections are left untouched.
fn rewrite_vendor_aliases_to_sentinel(json: &mut Value, secret_key: &str, alias_list: &[String]) {
    let Some(root) = json.as_object_mut() else {
        return;
    };
    for alias in alias_list {
        let Some(section) = root.get_mut(alias).and_then(|s| s.as_object_mut()) else {
            continue;
        };
        let Some(env) = section
            .get_mut("environments")
            .and_then(|e| e.as_object_mut())
        else {
            continue;
        };
        if let Some(Value::String(s)) = env.get(secret_key)
            && !s.is_empty()
        {
            env.insert(secret_key.to_string(), Value::String("keychain".to_string()));
        }
    }
}

fn verify_set(backend: &dyn KeychainBackend, plan: &PlannedAction) -> Result<(), McpError> {
    match backend.get(plan.kind, &plan.vendor, &plan.principal) {
        Ok(Some(s)) if s == plan.value => Ok(()),
        Ok(Some(_)) => Err(unexpected(
            "keychain readback returned a different value than was just written; \
             refusing to claim success",
            None,
        )),
        Ok(None) => Err(unexpected(
            "keychain readback returned no entry after a successful set; \
             backend appears to be a stub",
            None,
        )),
        Err(e) => Err(unexpected(
            format!("keychain readback failed: {e}"),
            None,
        )),
    }
}

fn rollback(applied: &[RollbackEntry], backend: &dyn KeychainBackend) {
    // Reverse order — restore most recent first so a partial restore
    // failure leaves earlier entries un-touched.
    for entry in applied.iter().rev() {
        let result = match &entry.prior {
            Some(prior) => backend.set(entry.kind, &entry.vendor, &entry.principal, prior),
            None => match backend.delete(entry.kind, &entry.vendor, &entry.principal) {
                Ok(()) | Err(KeychainError::NotFound) => Ok(()),
                Err(e) => Err(e),
            },
        };
        if let Err(e) = result {
            tracing::error!(
                kind = %entry.kind,
                vendor = entry.vendor.as_str(),
                principal = entry.principal.as_str(),
                error = %e,
                "rollback failed; keychain may be in an inconsistent state"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::InMemoryKeychain;

    fn select(kind: SecretKind, vendor: &str, principal: &str) -> SelectOpts {
        SelectOpts {
            kind,
            vendor: vendor.to_owned(),
            principal: principal.to_owned(),
        }
    }

    #[test]
    fn fingerprint_redacts_long_secrets_to_last_four() {
        assert_eq!(fingerprint("abcdef-1234"), "****1234");
        assert_eq!(fingerprint("abcdefghij"), "****ghij");
    }

    #[test]
    fn fingerprint_masks_short_secrets_completely() {
        assert_eq!(fingerprint(""), "****");
        assert_eq!(fingerprint("abc"), "****");
        assert_eq!(fingerprint("abcd"), "****");
    }

    #[test]
    fn get_handler_reports_missing_entry() {
        let kc = InMemoryKeychain::new();
        let err = get(&kc, select(SecretKind::ApiToken, "bitbucket", "alice@x")).unwrap_err();
        assert!(err.message.contains("no keychain entry"));
    }

    #[test]
    fn get_handler_succeeds_when_entry_exists() {
        let kc = InMemoryKeychain::new();
        kc.set(SecretKind::ApiToken, "bitbucket", "alice@x", "tok")
            .unwrap();
        get(&kc, select(SecretKind::ApiToken, "bitbucket", "alice@x")).unwrap();
    }

    #[test]
    fn rm_handler_removes_entry() {
        let kc = InMemoryKeychain::new();
        kc.set(SecretKind::ApiToken, "bitbucket", "alice@x", "tok")
            .unwrap();
        rm(&kc, select(SecretKind::ApiToken, "bitbucket", "alice@x")).unwrap();
        assert!(kc.is_empty());
    }

    #[test]
    fn rm_handler_errors_on_missing() {
        let kc = InMemoryKeychain::new();
        let err = rm(&kc, select(SecretKind::ApiToken, "bitbucket", "alice@x")).unwrap_err();
        assert!(err.message.contains("no keychain entry to remove"));
    }

    #[test]
    fn app_password_rejected_for_non_bitbucket_vendors() {
        let kc = InMemoryKeychain::new();
        for v in ["jira", "confluence"] {
            let err = get(&kc, select(SecretKind::AppPassword, v, "bobby")).unwrap_err();
            assert!(err.message.contains("Bitbucket-only"), "{}", err.message);
            let err = rm(&kc, select(SecretKind::AppPassword, v, "bobby")).unwrap_err();
            assert!(err.message.contains("Bitbucket-only"), "{}", err.message);
            let opts = SetOpts {
                kind: SecretKind::AppPassword,
                vendor: v.to_owned(),
                principal: "bobby".into(),
                from_stdin: true,
            };
            let err = set(&kc, opts).unwrap_err();
            assert!(err.message.contains("Bitbucket-only"), "{}", err.message);
        }
        // The keychain must remain untouched — guard fired before any backend call.
        assert!(kc.is_empty());
    }
}
