//! Global MCP config file reader (`$HOME/.mcp/configs.json`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Default location of the global config file, if we can resolve `$HOME`.
pub fn default_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".mcp").join("configs.json"))
}

/// Read the global config JSON and return a flat `{env-var: value}` map for the
/// first matching package alias. Returns an empty map when the file exists but
/// none of the candidate aliases match, matching TS behavior (logged, silent).
pub fn read(
    path: &Path,
    package_name: &str,
) -> Result<HashMap<String, String>, std::io::Error> {
    let bytes = std::fs::read(path)?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(super::extract_environments_for(&root, package_name))
}
