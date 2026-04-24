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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::{debug, warn};

pub mod global;

/// Immutable configuration snapshot assembled from all three sources.
#[derive(Debug, Clone, Default)]
pub struct Config {
    values: HashMap<String, String>,
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
    /// Pure builder used by tests and by [`load`]. Priority is applied by
    /// overlaying sources in order: global -> dotenv -> process env.
    ///
    /// - `global_path`: optional path to a `configs.json` file.
    /// - `dotenv_path`: optional path to a `.env` file.
    /// - `process_env`: caller-supplied view of `std::env::vars()`.
    pub fn load_from_sources(
        global_path: Option<&Path>,
        dotenv_path: Option<&Path>,
        process_env: &HashMap<String, String>,
    ) -> Self {
        let mut values: HashMap<String, String> = HashMap::new();

        // 3. Global config (lowest priority; only keys missing elsewhere win)
        if let Some(path) = global_path
            && path.exists()
        {
            match global::read(path, crate::constants::PACKAGE_NAME) {
                Ok(entries) => {
                    debug!(count = entries.len(), "loaded global config entries");
                    values.extend(entries);
                }
                Err(err) => warn!(error = %err, "failed to read global config"),
            }
        }

        // 2. .env file
        if let Some(path) = dotenv_path
            && path.exists()
        {
            match load_dotenv(path) {
                Ok(entries) => {
                    debug!(count = entries.len(), "loaded .env entries");
                    for (k, v) in entries {
                        values.insert(k, v);
                    }
                }
                Err(err) => warn!(error = %err, "failed to read .env"),
            }
        }

        // 1. Process env (highest priority)
        for (k, v) in process_env {
            values.insert(k.clone(), v.clone());
        }

        Self { values }
    }

    /// Construct directly from a map. Useful for tests and library embedders.
    pub fn from_map(values: HashMap<String, String>) -> Self {
        Self { values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
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

    /// Test/inspection helper.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
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
/// package. Exposed for tests; production code should prefer [`global::read`].
pub fn extract_environments_for(
    root: &Value,
    package_name: &str,
) -> HashMap<String, String> {
    let keys = candidate_keys(package_name);
    for key in &keys {
        let Some(section) = root.get(key.as_str()).and_then(Value::as_object) else {
            continue;
        };
        let Some(env) = section.get("environments").and_then(Value::as_object) else {
            continue;
        };
        return env
            .iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) => Some((k.clone(), s.clone())),
                Value::Bool(b) => Some((k.clone(), b.to_string())),
                Value::Number(n) => Some((k.clone(), n.to_string())),
                _ => None,
            })
            .collect();
    }
    HashMap::new()
}

/// Aliases we try inside `configs.json`, in priority order. Matches TS
/// `loadFromGlobalConfig` key probing logic.
pub fn candidate_keys(package_name: &str) -> Vec<String> {
    let short = "bitbucket".to_string();
    let product = "atlassian-bitbucket".to_string();
    let full = package_name.to_string();
    let unscoped = package_name
        .split_once('/')
        .map_or_else(|| package_name.to_string(), |(_, rest)| rest.to_string());
    vec![short, product, full, unscoped]
}

/// Cross-platform `~/.mcp/configs.json` resolver.
pub fn default_global_path() -> Option<PathBuf> {
    global::default_path()
}
