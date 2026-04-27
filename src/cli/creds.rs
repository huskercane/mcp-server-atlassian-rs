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
use std::collections::HashMap;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde_json::Value;

use crate::auth::keychain::{KeychainBackend, KeychainError, OsKeychain, SecretKind};
use crate::config::{self, Config, Resolved};
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

/// Shared `--kind` + `--principal` selector.
#[derive(Debug, Args)]
pub struct SelectOpts {
    /// Secret kind: `api-token` (Atlassian Cloud) or `app-password`
    /// (Bitbucket).
    #[arg(long, value_parser = parse_kind)]
    pub kind: SecretKind,
    /// Account identifier — email for `api-token`, username for
    /// `app-password`. Same string `Credentials::principal()` returns.
    #[arg(long)]
    pub principal: String,
}

#[derive(Debug, Args)]
pub struct SetOpts {
    #[arg(long, value_parser = parse_kind)]
    pub kind: SecretKind,
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

/// `creds set` handler. Public so tests can call it directly.
#[allow(clippy::needless_pass_by_value)]
pub fn set(backend: &dyn KeychainBackend, opts: SetOpts) -> Result<(), McpError> {
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
        .set(opts.kind, &opts.principal, &secret)
        .map_err(|e| unexpected(format!("keychain set failed: {e}"), None))?;

    // Verify roundtrip — catches mock backends that silently no-op.
    match backend.get(opts.kind, &opts.principal) {
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
        "stored {} for principal {} (service={})",
        opts.kind,
        opts.principal,
        opts.kind.service()
    );
    Ok(())
}

/// `creds get` handler. Prints a redacted fingerprint to confirm presence
/// without leaking the secret.
#[allow(clippy::needless_pass_by_value)]
pub fn get(backend: &dyn KeychainBackend, opts: SelectOpts) -> Result<(), McpError> {
    match backend.get(opts.kind, &opts.principal) {
        Ok(Some(secret)) => {
            println!(
                "{} for {} is set ({})",
                opts.kind,
                opts.principal,
                fingerprint(&secret)
            );
            Ok(())
        }
        Ok(None) => Err(unexpected(
            format!(
                "no keychain entry for kind={}, principal={}",
                opts.kind, opts.principal
            ),
            None,
        )),
        Err(e) => Err(unexpected(format!("keychain get failed: {e}"), None)),
    }
}

/// `creds rm` handler.
#[allow(clippy::needless_pass_by_value)]
pub fn rm(backend: &dyn KeychainBackend, opts: SelectOpts) -> Result<(), McpError> {
    match backend.delete(opts.kind, &opts.principal) {
        Ok(()) => {
            println!("removed {} for {}", opts.kind, opts.principal);
            Ok(())
        }
        Err(KeychainError::NotFound) => Err(unexpected(
            format!(
                "no keychain entry to remove for kind={}, principal={}",
                opts.kind, opts.principal
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

/// Render a `Config::resolve` `Ambiguous` payload for an error message
/// without echoing raw values. Each entry is shown as `vendor=****abcd`.
/// Used by `creds migrate` so a disagreement on `ATLASSIAN_API_TOKEN` etc.
/// cannot leak the conflicting secrets to stderr/logs.
fn fmt_ambiguous_redacted(values: &[(&str, &str)]) -> String {
    values
        .iter()
        .map(|(vendor, val)| format!("{vendor}={}", fingerprint(val)))
        .collect::<Vec<_>>()
        .join(", ")
}

// ----------------------------------------------------------------------------
// `creds migrate`
// ----------------------------------------------------------------------------

/// Outcome of a successful migration. Pure data so tests can assert on it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrateOutcome {
    /// Credentials newly written to the keychain (or already-present and
    /// confirmed equal — both end with the file rewritten to `"keychain"`).
    pub migrated: Vec<(SecretKind, String)>,
    /// Reasons specific candidates were skipped without error.
    pub skipped: Vec<MigrateSkip>,
    /// Path of the `.bak` file created next to the rewritten configs.json.
    /// Always present on success; the caller is expected to delete it
    /// after validating the migration.
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MigrateSkip {
    /// Sentinel was already present and the keychain entry verified.
    AlreadyMigrated { kind: SecretKind, principal: String },
    /// Plaintext value matches an existing keychain entry; no write needed,
    /// file still rewritten to `"keychain"`.
    InSync { kind: SecretKind, principal: String },
    /// Neither principal nor secret is set anywhere.
    NotConfigured { kind: SecretKind },
    /// Principal is set but secret is missing (no plaintext to migrate).
    /// Mirrors the runtime auth fallback semantics.
    PartiallyConfigured { kind: SecretKind },
}

/// Migrate plaintext secrets in `configs_path` to the keychain. Pure — does
/// not read process env or `.env`. Public so tests call it directly with
/// an in-memory backend and a tempdir-relative path.
///
/// On success, writes `<configs_path>.bak` (full original) and
/// atomic-replaces `configs_path` with secrets replaced by the literal
/// `"keychain"` sentinel.
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

    // File-only Config — no .env, no process env. We must mirror runtime
    // resolution but on the file alone, so migration doesn't pick up
    // env-sourced credentials it has no business writing to disk.
    let cfg = Config::load_from_sources(Some(configs_path), None, &HashMap::new());

    let candidates = [
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
    ];
    let aliases = config::vendor_aliases(PACKAGE_NAME);

    // Plan every candidate before any keychain write so we can fail fast on
    // misconfiguration without leaving partial state.
    let mut planned: Vec<PlannedAction> = Vec::new();
    let mut skipped: Vec<MigrateSkip> = Vec::new();

    for candidate in &candidates {
        match plan_candidate(candidate, &cfg, &json, &aliases)? {
            CandidateOutcome::Migrate(plan) => planned.push(plan),
            CandidateOutcome::Skip(reason) => skipped.push(reason),
        }
    }

    // Execute keychain writes with rollback on failure.
    let mut applied: Vec<RollbackEntry> = Vec::new();
    let mut migrated: Vec<(SecretKind, String)> = Vec::new();
    let mut to_rewrite: Vec<&PlannedAction> = Vec::new();

    for plan in &planned {
        let prior = backend
            .get(plan.kind, &plan.principal)
            .map_err(|e| unexpected(format!("keychain pre-read failed: {e}"), None))?;

        match (&prior, plan.action) {
            (Some(existing), PlannedKind::WriteFromPlaintext) if existing == &plan.value => {
                // Keychain already has this exact value — no write, but the
                // file still gets the sentinel.
                skipped.push(MigrateSkip::InSync {
                    kind: plan.kind,
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
                             principal={} (plaintext={}, keychain={}). The keychain \
                             value looks fresh; the configs.json value looks stale. \
                             Re-run with --force to overwrite the keychain, or remove \
                             the secret from configs.json to discard the plaintext.",
                            plan.kind,
                            plan.principal,
                            fingerprint(&plan.value),
                            fingerprint(existing),
                        ),
                        None,
                    ));
                }
                tracing::warn!(
                    kind = %plan.kind,
                    principal = plan.principal.as_str(),
                    plaintext_fp = fingerprint(&plan.value),
                    keychain_fp = fingerprint(existing),
                    "--force: overwriting existing keychain entry"
                );
                if let Err(e) = backend.set(plan.kind, &plan.principal, &plan.value) {
                    rollback(&applied, backend);
                    return Err(unexpected(format!("keychain set failed: {e}"), None));
                }
                if let Err(e) = verify_set(backend, plan) {
                    rollback(&applied, backend);
                    return Err(e);
                }
                applied.push(RollbackEntry {
                    kind: plan.kind,
                    principal: plan.principal.clone(),
                    prior: prior.clone(),
                });
                migrated.push((plan.kind, plan.principal.clone()));
                to_rewrite.push(plan);
            }
            (_, PlannedKind::WriteFromPlaintext) => {
                // Either no prior or non-conflict; do the write.
                if let Err(e) = backend.set(plan.kind, &plan.principal, &plan.value) {
                    rollback(&applied, backend);
                    return Err(unexpected(format!("keychain set failed: {e}"), None));
                }
                if let Err(e) = verify_set(backend, plan) {
                    rollback(&applied, backend);
                    return Err(e);
                }
                applied.push(RollbackEntry {
                    kind: plan.kind,
                    principal: plan.principal.clone(),
                    prior: prior.clone(),
                });
                migrated.push((plan.kind, plan.principal.clone()));
                to_rewrite.push(plan);
            }
            (Some(s), PlannedKind::VerifySentinel) if !s.is_empty() => {
                // Sentinel + non-empty entry — no-op for keychain side; the
                // file already has the sentinel.
                skipped.push(MigrateSkip::AlreadyMigrated {
                    kind: plan.kind,
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
                        "config has \"keychain\" sentinel for kind={}, principal={}, \
                         but {detail}. Run \
                         `mcp-atlassian creds set --kind {} --principal {}` \
                         or remove the sentinel from configs.json.",
                        plan.kind, plan.principal, plan.kind, plan.principal,
                    ),
                    None,
                ));
            }
        }
    }

    // Rewrite the JSON: for every recognized alias section of every
    // canonical vendor we just migrated, set environments[secret_key] =
    // "keychain" if currently set. Unrecognized top-level sections stay
    // untouched.
    if !to_rewrite.is_empty() {
        for plan in &to_rewrite {
            rewrite_aliases_to_sentinel(&mut json, plan.secret_key, &aliases);
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
        for (kind, principal) in &outcome.migrated {
            println!("  - {kind} for {principal}");
        }
    }
    for skip in &outcome.skipped {
        match skip {
            MigrateSkip::AlreadyMigrated { kind, principal } => {
                println!("  (already migrated) {kind} for {principal}");
            }
            MigrateSkip::InSync { kind, principal } => {
                println!("  (in sync; rewriting file) {kind} for {principal}");
            }
            MigrateSkip::NotConfigured { kind } => {
                println!("  (not configured) {kind}");
            }
            MigrateSkip::PartiallyConfigured { kind } => {
                println!(
                    "  (partial; principal set but secret missing) {kind}; \
                     leaving as-is per runtime auth fallback rules"
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
    principal: String,
    prior: Option<String>,
}

#[allow(clippy::too_many_lines)]
fn plan_candidate(
    candidate: &Candidate,
    cfg: &Config,
    json: &Value,
    aliases: &[(&'static str, Vec<String>)],
) -> Result<CandidateOutcome, McpError> {
    // (3a) Raw alias-group inspection — runs first so a sentinel in one
    // alias hiding plaintext in another is still caught.
    let alias_state = inspect_aliases(json, candidate.secret_key, aliases)?;
    if let AliasInspection::Conflict { canonical, found } = &alias_state {
        return Err(unexpected(
            format!(
                "alias conflict for {} within canonical vendor `{}`: {}. Reconcile \
                 manually before migrating.",
                candidate.secret_key,
                canonical,
                found
                    .iter()
                    .map(|(alias, val)| format!("{alias}={}", fingerprint(val)))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            None,
        ));
    }

    // (3b) Resolve via Config::resolve and dispatch.
    let principal = match cfg.resolve(candidate.principal_key) {
        Resolved::Resolved("") => {
            return Err(unexpected(
                format!(
                    "{} is set to an empty value; cannot migrate {}",
                    candidate.principal_key, candidate.secret_key
                ),
                None,
            ));
        }
        Resolved::Resolved(p) => p,
        Resolved::Ambiguous { values } => {
            // Defensive redaction: the principal is normally email/username
            // (non-secret), but a misconfigured file might have a token in
            // that slot. Cheaper to fingerprint than to leak.
            return Err(unexpected(
                format!(
                    "{} disagrees across vendor sections [{}]; reconcile before migrating",
                    candidate.principal_key,
                    fmt_ambiguous_redacted(&values),
                ),
                None,
            ));
        }
        Resolved::Missing => {
            // Principal absent. Two subcases:
            //   - secret says "keychain" → hard error (sentinel implies opt-in).
            //   - secret has plaintext  → hard error (we'd leave plaintext on disk).
            //   - secret missing too    → not configured, skip.
            //   - secret empty          → not configured, skip.
            return match cfg.resolve(candidate.secret_key) {
                Resolved::Missing | Resolved::Resolved("") => {
                    Ok(CandidateOutcome::Skip(MigrateSkip::NotConfigured {
                        kind: candidate.kind,
                    }))
                }
                Resolved::Resolved("keychain") => Err(unexpected(
                    format!(
                        "config sets {}=\"keychain\" but {} is missing; cannot migrate",
                        candidate.secret_key, candidate.principal_key
                    ),
                    None,
                )),
                Resolved::Resolved(_) => Err(unexpected(
                    format!(
                        "plaintext {} is set in configs.json but {} is missing; \
                         migration cannot move it safely. Add {} or remove {} first.",
                        candidate.secret_key,
                        candidate.principal_key,
                        candidate.principal_key,
                        candidate.secret_key
                    ),
                    None,
                )),
                Resolved::Ambiguous { values } => Err(unexpected(
                    format!(
                        "{} disagrees across vendor sections [{}]; reconcile before migrating",
                        candidate.secret_key,
                        fmt_ambiguous_redacted(&values),
                    ),
                    None,
                )),
            };
        }
    };

    match cfg.resolve(candidate.secret_key) {
        Resolved::Missing => Ok(CandidateOutcome::Skip(MigrateSkip::PartiallyConfigured {
            kind: candidate.kind,
        })),
        Resolved::Resolved("") => Err(unexpected(
            format!("{} is set to an empty string; nothing to migrate", candidate.secret_key),
            None,
        )),
        Resolved::Resolved("keychain") => Ok(CandidateOutcome::Migrate(PlannedAction {
            kind: candidate.kind,
            principal: principal.to_owned(),
            secret_key: candidate.secret_key,
            value: String::new(),
            action: PlannedKind::VerifySentinel,
        })),
        Resolved::Resolved(s) => Ok(CandidateOutcome::Migrate(PlannedAction {
            kind: candidate.kind,
            principal: principal.to_owned(),
            secret_key: candidate.secret_key,
            value: s.to_owned(),
            action: PlannedKind::WriteFromPlaintext,
        })),
        Resolved::Ambiguous { values } => Err(unexpected(
            format!(
                "{} disagrees across vendor sections [{}]; reconcile before migrating",
                candidate.secret_key,
                fmt_ambiguous_redacted(&values),
            ),
            None,
        )),
    }
}

#[derive(Debug)]
enum AliasInspection {
    /// All alias values for this canonical vendor are equal-or-absent.
    Uniform,
    /// Aliases of one canonical vendor have different non-empty values.
    Conflict {
        canonical: &'static str,
        found: Vec<(String, String)>,
    },
}

/// Walk the raw JSON for every recognized alias of every canonical vendor
/// and validate that within each canonical vendor, the value of
/// `secret_key` is uniform (all equal, or absent). Also enforces the type
/// guard: secret values must be JSON strings.
fn inspect_aliases(
    json: &Value,
    secret_key: &str,
    aliases: &[(&'static str, Vec<String>)],
) -> Result<AliasInspection, McpError> {
    for (canonical, alias_list) in aliases {
        let mut found: Vec<(String, String)> = Vec::new();
        for alias in alias_list {
            let val = json
                .get(alias)
                .and_then(|s| s.get("environments"))
                .and_then(|env| env.get(secret_key));
            match val {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if s.is_empty() => {}
                Some(Value::String(s)) => found.push((alias.clone(), s.clone())),
                Some(other) => {
                    return Err(unexpected(
                        format!(
                            "{secret_key} in section `{alias}` is a JSON {} value; \
                             migrate refuses to coerce. Quote the value as a string in \
                             configs.json first.",
                            json_type_name(other)
                        ),
                        None,
                    ));
                }
            }
        }
        if found.len() > 1 {
            let first = &found[0].1;
            if !found.iter().all(|(_, v)| v == first) {
                return Ok(AliasInspection::Conflict {
                    canonical,
                    found,
                });
            }
        }
    }
    Ok(AliasInspection::Uniform)
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

fn rewrite_aliases_to_sentinel(
    json: &mut Value,
    secret_key: &str,
    aliases: &[(&'static str, Vec<String>)],
) {
    let alias_set: Vec<String> = aliases
        .iter()
        .flat_map(|(_, list)| list.iter().cloned())
        .collect();
    let Some(root) = json.as_object_mut() else {
        return;
    };
    for alias in alias_set {
        let Some(section) = root.get_mut(&alias).and_then(|s| s.as_object_mut()) else {
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
    match backend.get(plan.kind, &plan.principal) {
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
            Some(prior) => backend.set(entry.kind, &entry.principal, prior),
            None => match backend.delete(entry.kind, &entry.principal) {
                Ok(()) | Err(KeychainError::NotFound) => Ok(()),
                Err(e) => Err(e),
            },
        };
        if let Err(e) = result {
            tracing::error!(
                kind = %entry.kind,
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

    fn select(kind: SecretKind, principal: &str) -> SelectOpts {
        SelectOpts {
            kind,
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
        let err = get(&kc, select(SecretKind::ApiToken, "alice@x")).unwrap_err();
        assert!(err.message.contains("no keychain entry"));
    }

    #[test]
    fn get_handler_succeeds_when_entry_exists() {
        let kc = InMemoryKeychain::new();
        kc.set(SecretKind::ApiToken, "alice@x", "tok").unwrap();
        // Just exercise the success path — stdout isn't captured here, but
        // the function returning Ok(()) means the entry was found.
        get(&kc, select(SecretKind::ApiToken, "alice@x")).unwrap();
    }

    #[test]
    fn rm_handler_removes_entry() {
        let kc = InMemoryKeychain::new();
        kc.set(SecretKind::ApiToken, "alice@x", "tok").unwrap();
        rm(&kc, select(SecretKind::ApiToken, "alice@x")).unwrap();
        assert!(kc.is_empty());
    }

    #[test]
    fn rm_handler_errors_on_missing() {
        let kc = InMemoryKeychain::new();
        let err = rm(&kc, select(SecretKind::ApiToken, "alice@x")).unwrap_err();
        assert!(err.message.contains("no keychain entry to remove"));
    }
}
