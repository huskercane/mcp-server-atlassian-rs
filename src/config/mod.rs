//! Configuration loader with a three-source cascade that matches the TypeScript
//! reference (`src/utils/config.util.ts`).
//!
//! Priority (highest wins):
//! 1. Process environment variables
//! 2. `.env` file in the current working directory
//! 3. Global config file at `$HOME/.mcp/configs.json`
//!
//! Unlike the TS implementation, we never mutate `std::env` (Rust 2024
//! marks `std::env::set_var` as `unsafe` under multi-threaded contexts).
//! Instead we collect all three sources into an immutable snapshot. The
//! observable behavior (which value wins for a given key) is identical.
//!
//! ## Vendor scoping (added when Jira tools were introduced)
//!
//! Process env and `.env` are vendor-neutral and form the **shared** overlay
//! that both [`Config::get`] and [`Config::get_for`] read first.
//!
//! Global config sections are **vendor-scoped**. A user may define a
//! `bitbucket` section, a `jira` section, or both, and a vendor-specific key
//! in one section never leaks into another vendor's lookup.
//!
//! Lookup rules:
//!
//! - [`Config::get_for(vendor, key)`](Config::get_for): `shared` →
//!   `by_vendor[vendor]`. Reads the named vendor's section only; never
//!   crosses into a sibling vendor's section.
//! - [`Config::get(key)`](Config::get): `shared` → unambiguous vendor value.
//!   A key is "unambiguous" if exactly one vendor section defines it, OR all
//!   defining vendor sections agree on the same value (the copy-paste case).
//!   When vendor sections disagree, [`Config::get`] returns `None` and forces
//!   the caller to disambiguate via [`Config::get_for`].
//!
//! Shared keys (auth credentials, network timeout) read via [`Config::get`].
//! Vendor-specific keys (`BITBUCKET_DEFAULT_WORKSPACE`, `ATLASSIAN_SITE_NAME`)
//! read via [`Config::get_for`].

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::{debug, warn};

pub mod global;

/// Canonical vendor name for Bitbucket. Use these constants at call sites
/// instead of hard-coded strings so a typo becomes a compile error.
pub const VENDOR_BITBUCKET: &str = "bitbucket";

/// Canonical vendor name for Jira.
pub const VENDOR_JIRA: &str = "jira";

/// Immutable configuration snapshot assembled from all three sources.
///
/// Internally split into a vendor-neutral `shared` overlay (process env +
/// `.env`) and a per-vendor map populated from the global config file. See
/// the module docs for lookup rules.
#[derive(Debug, Clone, Default)]
pub struct Config {
    shared: HashMap<String, String>,
    by_vendor: BTreeMap<String, HashMap<String, String>>,
}

/// Builds the snapshot using the standard cascade. Calls to this function
/// read the filesystem (global config + `.env`) and `std::env`. It is
/// intentionally side-effect free.
pub fn load() -> Config {
    Config::load_from_sources(
        global::default_path().as_deref(),
        Some(Path::new(".env")),
        &env_map_from_process(),
    )
}

impl Config {
    /// Pure builder used by tests and by [`load`]. Priority is applied as
    /// follows:
    ///
    /// - Global config sections are read into `by_vendor`, keyed by the
    ///   canonical vendor name (e.g. `"bitbucket"`, `"jira"`). Multiple
    ///   aliases for the same vendor are merged with the higher-priority
    ///   alias winning per-key.
    /// - `.env` and process env are merged into the vendor-neutral `shared`
    ///   overlay. Process env wins over `.env`.
    ///
    /// - `global_path`: optional path to a `configs.json` file.
    /// - `dotenv_path`: optional path to a `.env` file.
    /// - `process_env`: caller-supplied view of `std::env::vars()`.
    pub fn load_from_sources(
        global_path: Option<&Path>,
        dotenv_path: Option<&Path>,
        process_env: &HashMap<String, String>,
    ) -> Self {
        let mut shared: HashMap<String, String> = HashMap::new();
        let mut by_vendor: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();

        // Global config: vendor-scoped sections.
        if let Some(path) = global_path
            && path.exists()
        {
            match global::read_all_vendors(path, crate::constants::PACKAGE_NAME) {
                Ok(map) => {
                    debug!(vendors = map.len(), "loaded global config sections");
                    by_vendor = map;
                }
                Err(err) => warn!(error = %err, "failed to read global config"),
            }
        }

        // .env: vendor-neutral, applied to shared.
        if let Some(path) = dotenv_path
            && path.exists()
        {
            match load_dotenv(path) {
                Ok(entries) => {
                    debug!(count = entries.len(), "loaded .env entries");
                    shared.extend(entries);
                }
                Err(err) => warn!(error = %err, "failed to read .env"),
            }
        }

        // Process env: vendor-neutral, highest-priority overlay on shared.
        for (k, v) in process_env {
            shared.insert(k.clone(), v.clone());
        }

        Self { shared, by_vendor }
    }

    /// Construct directly from a flat map. The map populates the
    /// vendor-neutral `shared` overlay, so all entries are visible from both
    /// [`get`](Self::get) and [`get_for`](Self::get_for). Useful for tests
    /// and library embedders that pass credentials in directly without a
    /// global config file.
    pub fn from_map(values: HashMap<String, String>) -> Self {
        Self {
            shared: values,
            by_vendor: BTreeMap::new(),
        }
    }

    /// Vendor-neutral lookup. Returns the value when:
    /// - `shared` (process env / `.env`) defines it, OR
    /// - exactly one vendor section defines it, OR
    /// - all vendor sections that define it agree on the value.
    ///
    /// Returns `None` when two or more vendor sections define the key with
    /// different values; the caller must then disambiguate via
    /// [`get_for`](Self::get_for).
    pub fn get(&self, key: &str) -> Option<&str> {
        if let Some(v) = self.shared.get(key) {
            return Some(v.as_str());
        }
        let mut found: Option<&str> = None;
        for vendor_map in self.by_vendor.values() {
            if let Some(v) = vendor_map.get(key) {
                match found {
                    None => found = Some(v.as_str()),
                    Some(prev) if prev == v.as_str() => {} // agreement counts as unambiguous
                    Some(_) => return None,                // disagreement → force get_for
                }
            }
        }
        found
    }

    /// Vendor-scoped lookup. Reads `shared` first, then the named vendor's
    /// section only. Never reads another vendor's section.
    ///
    /// Use this for keys that are vendor-specific by definition
    /// (`BITBUCKET_DEFAULT_WORKSPACE`, `ATLASSIAN_SITE_NAME`).
    pub fn get_for(&self, vendor: &str, key: &str) -> Option<&str> {
        if let Some(v) = self.shared.get(key) {
            return Some(v.as_str());
        }
        self.by_vendor
            .get(vendor)
            .and_then(|m| m.get(key))
            .map(String::as_str)
    }

    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_owned()
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get(key)
            .map_or(default, |v| v.eq_ignore_ascii_case("true"))
    }

    pub fn get_int(&self, key: &str, default: i64) -> i64 {
        self.get(key)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(default)
    }

    /// Test/inspection helper. Counts entries across `shared` and every
    /// vendor section; a key present in both is counted once per map.
    pub fn len(&self) -> usize {
        self.shared.len() + self.by_vendor.values().map(HashMap::len).sum::<usize>()
    }

    pub fn is_empty(&self) -> bool {
        self.shared.is_empty() && self.by_vendor.values().all(HashMap::is_empty)
    }
}

fn env_map_from_process() -> HashMap<String, String> {
    std::env::vars().collect()
}

fn load_dotenv(path: &Path) -> std::io::Result<HashMap<String, String>> {
    let iter = dotenvy::from_path_iter(path).map_err(std::io::Error::other)?;
    let mut map = HashMap::new();
    for entry in iter {
        let (k, v) = entry.map_err(std::io::Error::other)?;
        map.insert(k, v);
    }
    Ok(map)
}

/// Parse a `configs.json` value (typically deserialised from disk) for a given
/// package, returning a flat env map for the first matching alias.
///
/// Preserved for back-compat with the original single-vendor TS port; the
/// internal loader uses [`extract_all_vendor_sections`] instead. Exposed for
/// tests; production code should prefer [`global::read_all_vendors`] via
/// [`Config::load_from_sources`].
pub fn extract_environments_for(
    root: &Value,
    package_name: &str,
) -> HashMap<String, String> {
    let keys = candidate_keys(package_name);
    for key in &keys {
        if let Some(env) = read_environments_at(root, key) {
            return env;
        }
    }
    HashMap::new()
}

/// Read every recognised vendor section out of the global config root and
/// return a `canonical-vendor → env-map` table. Per-vendor alias merging
/// uses the same priority order as [`extract_environments_for`] (higher
/// priority overrides per-key).
///
/// `package_name` is used to extend the Bitbucket vendor's alias list with
/// the crate's published package name and its unscoped form, so a user with
/// a section keyed by the full package name still resolves.
pub fn extract_all_vendor_sections(
    root: &Value,
    package_name: &str,
) -> BTreeMap<String, HashMap<String, String>> {
    let mut out: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();

    for (canonical, aliases) in vendor_aliases(package_name) {
        // Apply aliases in REVERSE priority so that higher-priority alias
        // values overwrite lower-priority ones in the merged map.
        let mut merged: HashMap<String, String> = HashMap::new();
        for alias in aliases.iter().rev() {
            if let Some(env) = read_environments_at(root, alias) {
                merged.extend(env);
            }
        }
        if !merged.is_empty() {
            out.insert(canonical.to_string(), merged);
        }
    }

    out
}

/// Look up `root[key].environments` and coerce the values into strings.
/// Returns `None` when the section or its `environments` map is missing.
fn read_environments_at(root: &Value, key: &str) -> Option<HashMap<String, String>> {
    let section = root.get(key).and_then(Value::as_object)?;
    let env = section.get("environments").and_then(Value::as_object)?;
    Some(
        env.iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) => Some((k.clone(), s.clone())),
                Value::Bool(b) => Some((k.clone(), b.to_string())),
                Value::Number(n) => Some((k.clone(), n.to_string())),
                _ => None,
            })
            .collect(),
    )
}

/// Aliases we try inside `configs.json`, in priority order. Matches TS
/// `loadFromGlobalConfig` key probing logic. Used by the back-compat
/// [`extract_environments_for`] entry point.
pub fn candidate_keys(package_name: &str) -> Vec<String> {
    let short = "bitbucket".to_string();
    let product = "atlassian-bitbucket".to_string();
    let full = package_name.to_string();
    let unscoped = package_name
        .split_once('/')
        .map_or_else(|| package_name.to_string(), |(_, rest)| rest.to_string());
    vec![short, product, full, unscoped]
}

/// Per-vendor alias lists in priority order (highest priority first).
///
/// The Bitbucket vendor inherits the crate's package-name-derived aliases
/// for back-compat with the original single-vendor TS port; the Jira vendor
/// only uses its short and product aliases (a Jira section keyed by a
/// package name would belong to a separate Jira-only crate, not this one).
fn vendor_aliases(package_name: &str) -> Vec<(&'static str, Vec<String>)> {
    let bitbucket_aliases = {
        let mut v = vec![
            "bitbucket".to_string(),
            "atlassian-bitbucket".to_string(),
            package_name.to_string(),
        ];
        let unscoped = package_name
            .split_once('/')
            .map_or_else(|| package_name.to_string(), |(_, rest)| rest.to_string());
        if !v.iter().any(|a| a == &unscoped) {
            v.push(unscoped);
        }
        v
    };
    let jira_aliases = vec!["jira".to_string(), "atlassian-jira".to_string()];

    vec![
        (VENDOR_BITBUCKET, bitbucket_aliases),
        (VENDOR_JIRA, jira_aliases),
    ]
}

/// Cross-platform `~/.mcp/configs.json` resolver.
pub fn default_global_path() -> Option<PathBuf> {
    global::default_path()
}
