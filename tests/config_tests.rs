//! Tests for the config cascade. Verifies priority order, all four alias
//! keys in `configs.json`, and the `.environments` map wiring.

use std::collections::HashMap;
use std::io::Write;

use mcp_server_atlassian_bitbucket::config::{
    Config, candidate_keys, extract_environments_for,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

const PKG: &str = "@huskercane/mcp-server-atlassian-bitbucket-rs";

fn write_global(dir: &TempDir, body: &serde_json::Value) -> std::path::PathBuf {
    let path = dir.path().join("configs.json");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(serde_json::to_vec_pretty(body).unwrap().as_slice())
        .unwrap();
    path
}

fn write_dotenv(dir: &TempDir, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(".env");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    path
}

// ---- candidate_keys ----

#[test]
fn candidate_keys_match_ts_priority() {
    let keys = candidate_keys(PKG);
    assert_eq!(
        keys,
        vec![
            "bitbucket".to_string(),
            "atlassian-bitbucket".to_string(),
            PKG.to_string(),
            "mcp-server-atlassian-bitbucket-rs".to_string(),
        ]
    );
}

#[test]
fn candidate_keys_for_unscoped_package() {
    let keys = candidate_keys("mcp-server-atlassian-bitbucket-rs");
    assert_eq!(
        keys,
        vec![
            "bitbucket".to_string(),
            "atlassian-bitbucket".to_string(),
            "mcp-server-atlassian-bitbucket-rs".to_string(),
            "mcp-server-atlassian-bitbucket-rs".to_string(),
        ]
    );
}

// ---- extract_environments_for (each alias) ----

#[test]
fn extract_via_short_key() {
    let doc = json!({
        "bitbucket": {
            "environments": {
                "ATLASSIAN_API_TOKEN": "short-key-token"
            }
        }
    });
    let entries = extract_environments_for(&doc, PKG);
    assert_eq!(entries.get("ATLASSIAN_API_TOKEN").unwrap(), "short-key-token");
}

#[test]
fn extract_via_product_key() {
    let doc = json!({
        "atlassian-bitbucket": {
            "environments": {
                "ATLASSIAN_API_TOKEN": "product-token"
            }
        }
    });
    let entries = extract_environments_for(&doc, PKG);
    assert_eq!(entries.get("ATLASSIAN_API_TOKEN").unwrap(), "product-token");
}

#[test]
fn extract_via_scoped_name() {
    let doc = json!({
        PKG: {
            "environments": {
                "ATLASSIAN_API_TOKEN": "scoped-token"
            }
        }
    });
    let entries = extract_environments_for(&doc, PKG);
    assert_eq!(entries.get("ATLASSIAN_API_TOKEN").unwrap(), "scoped-token");
}

#[test]
fn extract_via_unscoped_name() {
    let doc = json!({
        "mcp-server-atlassian-bitbucket-rs": {
            "environments": {
                "ATLASSIAN_API_TOKEN": "unscoped-token"
            }
        }
    });
    let entries = extract_environments_for(&doc, PKG);
    assert_eq!(entries.get("ATLASSIAN_API_TOKEN").unwrap(), "unscoped-token");
}

#[test]
fn priority_short_over_product_over_scoped_over_unscoped() {
    let doc = json!({
        "bitbucket": { "environments": { "K": "short" } },
        "atlassian-bitbucket": { "environments": { "K": "product" } },
        PKG: { "environments": { "K": "scoped" } },
        "mcp-server-atlassian-bitbucket-rs": { "environments": { "K": "unscoped" } }
    });
    let entries = extract_environments_for(&doc, PKG);
    assert_eq!(entries.get("K").unwrap(), "short");
}

#[test]
fn missing_environments_returns_empty() {
    let doc = json!({ "bitbucket": {} });
    let entries = extract_environments_for(&doc, PKG);
    assert!(entries.is_empty());
}

#[test]
fn coerces_non_string_environment_values() {
    let doc = json!({
        "bitbucket": {
            "environments": {
                "A_BOOL": true,
                "A_NUM": 42,
                "A_STR": "hello"
            }
        }
    });
    let entries = extract_environments_for(&doc, PKG);
    assert_eq!(entries.get("A_BOOL").unwrap(), "true");
    assert_eq!(entries.get("A_NUM").unwrap(), "42");
    assert_eq!(entries.get("A_STR").unwrap(), "hello");
}

// ---- full cascade (global -> dotenv -> process env) ----

#[test]
fn process_env_wins_over_dotenv_and_global() {
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": { "environments": { "X": "from-global", "Y": "g-y" } }
        }),
    );
    let dotenv = write_dotenv(&dir, "X=from-dotenv\nZ=d-z\n");

    let mut proc_env: HashMap<String, String> = HashMap::new();
    proc_env.insert("X".into(), "from-process".into());

    let cfg = Config::load_from_sources(Some(&global), Some(&dotenv), &proc_env);
    assert_eq!(cfg.get("X"), Some("from-process"));
    assert_eq!(cfg.get("Y"), Some("g-y"));
    assert_eq!(cfg.get("Z"), Some("d-z"));
}

#[test]
fn dotenv_wins_over_global_when_process_env_absent() {
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": { "environments": { "X": "from-global" } }
        }),
    );
    let dotenv = write_dotenv(&dir, "X=from-dotenv\n");
    let proc_env: HashMap<String, String> = HashMap::new();

    let cfg = Config::load_from_sources(Some(&global), Some(&dotenv), &proc_env);
    assert_eq!(cfg.get("X"), Some("from-dotenv"));
}

#[test]
fn falls_back_to_global_when_nothing_overrides() {
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "atlassian-bitbucket": { "environments": { "ATLASSIAN_API_TOKEN": "abc" } }
        }),
    );
    let proc_env: HashMap<String, String> = HashMap::new();

    let cfg = Config::load_from_sources(Some(&global), None, &proc_env);
    assert_eq!(cfg.get("ATLASSIAN_API_TOKEN"), Some("abc"));
}

#[test]
fn missing_sources_do_not_error() {
    let proc_env: HashMap<String, String> = HashMap::new();
    let cfg = Config::load_from_sources(None, None, &proc_env);
    assert!(cfg.is_empty());
}

// ---- typed getters ----

#[test]
fn typed_getters_behave_like_ts() {
    let mut m = HashMap::new();
    m.insert("S".into(), "hello".into());
    m.insert("B_TRUE".into(), "TRUE".into());
    m.insert("B_FALSE".into(), "no".into());
    m.insert("N".into(), "42".into());
    m.insert("N_BAD".into(), "nope".into());
    let cfg = Config::from_map(m);

    assert_eq!(cfg.get("S"), Some("hello"));
    assert_eq!(cfg.get_or("missing", "fallback"), "fallback");
    assert!(cfg.get_bool("B_TRUE", false));
    assert!(!cfg.get_bool("B_FALSE", true));
    assert!(cfg.get_bool("missing", true));
    assert_eq!(cfg.get_int("N", -1), 42);
    assert_eq!(cfg.get_int("N_BAD", -1), -1);
    assert_eq!(cfg.get_int("missing", 7), 7);
}
