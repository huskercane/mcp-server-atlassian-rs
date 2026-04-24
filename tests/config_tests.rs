//! Tests for the config cascade. Verifies priority order, all four alias
//! keys in `configs.json`, and the `.environments` map wiring.

use std::collections::HashMap;
use std::io::Write;

use mcp_server_atlassian::config::{
    Config, VENDOR_BITBUCKET, VENDOR_JIRA, candidate_keys, extract_all_vendor_sections,
    extract_environments_for,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

const PKG: &str = "@huskercane/mcp-server-atlassian";

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
            "mcp-server-atlassian".to_string(),
        ]
    );
}

#[test]
fn candidate_keys_for_unscoped_package() {
    let keys = candidate_keys("mcp-server-atlassian");
    assert_eq!(
        keys,
        vec![
            "bitbucket".to_string(),
            "atlassian-bitbucket".to_string(),
            "mcp-server-atlassian".to_string(),
            "mcp-server-atlassian".to_string(),
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
        "mcp-server-atlassian": {
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
        "mcp-server-atlassian": { "environments": { "K": "unscoped" } }
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

// ---- vendor-scoped lookup (get_for + get unambiguity rule) ----

#[test]
fn single_bitbucket_section_visible_via_get() {
    // A user with only a `bitbucket` section in global config should see
    // its values through the vendor-neutral `get` API. Nothing changes for
    // existing single-vendor users.
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": {
                "environments": {
                    "ATLASSIAN_API_TOKEN": "abc",
                    "BITBUCKET_DEFAULT_WORKSPACE": "myws"
                }
            }
        }),
    );
    let cfg = Config::load_from_sources(Some(&global), None, &HashMap::new());

    assert_eq!(cfg.get("ATLASSIAN_API_TOKEN"), Some("abc"));
    assert_eq!(
        cfg.get_for(VENDOR_BITBUCKET, "BITBUCKET_DEFAULT_WORKSPACE"),
        Some("myws")
    );
}

#[test]
fn agreeing_vendor_sections_are_unambiguous_for_get() {
    // Common case: user copy-pastes shared credentials into both vendor
    // sections. `get` should still return the (agreed) value rather than
    // forcing every shared-key call site through `get_for`.
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": { "environments": { "ATLASSIAN_API_TOKEN": "shared" } },
            "jira":      { "environments": { "ATLASSIAN_API_TOKEN": "shared" } }
        }),
    );
    let cfg = Config::load_from_sources(Some(&global), None, &HashMap::new());

    assert_eq!(cfg.get("ATLASSIAN_API_TOKEN"), Some("shared"));
}

#[test]
fn conflicting_vendor_sections_force_get_for() {
    // If the same key has different values across vendor sections, `get`
    // refuses to guess and returns None. The caller must disambiguate via
    // `get_for(vendor, key)`.
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": { "environments": { "ATLASSIAN_API_TOKEN": "bb-token" } },
            "jira":      { "environments": { "ATLASSIAN_API_TOKEN": "jira-token" } }
        }),
    );
    let cfg = Config::load_from_sources(Some(&global), None, &HashMap::new());

    assert_eq!(cfg.get("ATLASSIAN_API_TOKEN"), None);
    assert_eq!(
        cfg.get_for(VENDOR_BITBUCKET, "ATLASSIAN_API_TOKEN"),
        Some("bb-token")
    );
    assert_eq!(
        cfg.get_for(VENDOR_JIRA, "ATLASSIAN_API_TOKEN"),
        Some("jira-token")
    );
}

#[test]
fn process_env_overrides_vendor_section_conflicts() {
    // The shared overlay (process env / .env) takes priority over any
    // vendor section, so a process-env override resolves an otherwise
    // ambiguous key for both `get` and `get_for`.
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": { "environments": { "ATLASSIAN_API_TOKEN": "bb-token" } },
            "jira":      { "environments": { "ATLASSIAN_API_TOKEN": "jira-token" } }
        }),
    );
    let mut proc_env = HashMap::new();
    proc_env.insert("ATLASSIAN_API_TOKEN".into(), "from-process".into());

    let cfg = Config::load_from_sources(Some(&global), None, &proc_env);

    assert_eq!(cfg.get("ATLASSIAN_API_TOKEN"), Some("from-process"));
    assert_eq!(
        cfg.get_for(VENDOR_BITBUCKET, "ATLASSIAN_API_TOKEN"),
        Some("from-process")
    );
    assert_eq!(
        cfg.get_for(VENDOR_JIRA, "ATLASSIAN_API_TOKEN"),
        Some("from-process")
    );
}

#[test]
fn vendor_specific_keys_stay_isolated() {
    // A key defined only in one vendor's section must not be visible to
    // another vendor's `get_for` call. This is the load-bearing guarantee
    // for keys like `BITBUCKET_DEFAULT_WORKSPACE` and `ATLASSIAN_SITE_NAME`.
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": {
                "environments": { "BITBUCKET_DEFAULT_WORKSPACE": "myws" }
            },
            "jira": {
                "environments": { "ATLASSIAN_SITE_NAME": "mysite" }
            }
        }),
    );
    let cfg = Config::load_from_sources(Some(&global), None, &HashMap::new());

    assert_eq!(
        cfg.get_for(VENDOR_BITBUCKET, "BITBUCKET_DEFAULT_WORKSPACE"),
        Some("myws")
    );
    assert_eq!(
        cfg.get_for(VENDOR_JIRA, "ATLASSIAN_SITE_NAME"),
        Some("mysite")
    );
    // Cross-vendor lookups must miss.
    assert_eq!(
        cfg.get_for(VENDOR_JIRA, "BITBUCKET_DEFAULT_WORKSPACE"),
        None
    );
    assert_eq!(cfg.get_for(VENDOR_BITBUCKET, "ATLASSIAN_SITE_NAME"), None);

    // `get` still resolves them because each is unambiguous (only one
    // vendor defines it). Call sites *should* still prefer `get_for`, but
    // the rule does not punish unambiguous singletons.
    assert_eq!(
        cfg.get("BITBUCKET_DEFAULT_WORKSPACE"),
        Some("myws")
    );
    assert_eq!(cfg.get("ATLASSIAN_SITE_NAME"), Some("mysite"));
}

#[test]
fn get_for_falls_back_to_shared_but_not_to_other_vendors() {
    // Shared overlay (process env) is the right place to put credentials
    // when you only have one set; `get_for` honours that. A value that
    // only exists in another vendor's section must not be reachable.
    let dir = TempDir::new().unwrap();
    let global = write_global(
        &dir,
        &json!({
            "bitbucket": { "environments": { "BB_ONLY_KEY": "bb-value" } }
        }),
    );
    let mut proc_env = HashMap::new();
    proc_env.insert("ATLASSIAN_API_TOKEN".into(), "shared-token".into());

    let cfg = Config::load_from_sources(Some(&global), None, &proc_env);

    // Shared fallback works for every vendor.
    assert_eq!(
        cfg.get_for(VENDOR_JIRA, "ATLASSIAN_API_TOKEN"),
        Some("shared-token")
    );
    assert_eq!(
        cfg.get_for(VENDOR_BITBUCKET, "ATLASSIAN_API_TOKEN"),
        Some("shared-token")
    );
    // Bitbucket-only key is not reachable from Jira.
    assert_eq!(cfg.get_for(VENDOR_JIRA, "BB_ONLY_KEY"), None);
    assert_eq!(
        cfg.get_for(VENDOR_BITBUCKET, "BB_ONLY_KEY"),
        Some("bb-value")
    );
}

#[test]
fn extract_all_vendor_sections_canonicalises_aliases() {
    // The richer extractor merges per-vendor aliases (with higher-priority
    // alias winning per-key) and groups them under a canonical vendor name.
    let doc = json!({
        "bitbucket":          { "environments": { "K": "from-short", "ONLY_SHORT": "x" } },
        "atlassian-bitbucket":{ "environments": { "K": "from-product", "ONLY_PRODUCT": "y" } },
        "atlassian-jira":     { "environments": { "ATLASSIAN_SITE_NAME": "mysite" } }
    });
    let map = extract_all_vendor_sections(&doc, PKG);

    let bb = map.get(VENDOR_BITBUCKET).unwrap();
    assert_eq!(bb.get("K").map(String::as_str), Some("from-short"));
    assert_eq!(bb.get("ONLY_SHORT").map(String::as_str), Some("x"));
    assert_eq!(bb.get("ONLY_PRODUCT").map(String::as_str), Some("y"));

    let jira = map.get(VENDOR_JIRA).unwrap();
    assert_eq!(
        jira.get("ATLASSIAN_SITE_NAME").map(String::as_str),
        Some("mysite")
    );
}

#[test]
fn ts_jira_package_aliases_resolve_to_jira_section() {
    // Migration guarantee: a user who set up the TS Jira server keyed
    // their global config under "@aashari/mcp-server-atlassian-jira" or
    // its unscoped form. Both must continue to land in the canonical
    // "jira" section after migration to the Rust crate.
    let doc = json!({
        "@aashari/mcp-server-atlassian-jira": {
            "environments": { "ATLASSIAN_SITE_NAME": "from-scoped-key" }
        }
    });
    let map = extract_all_vendor_sections(&doc, PKG);
    let jira = map.get(VENDOR_JIRA).expect("jira section resolved");
    assert_eq!(
        jira.get("ATLASSIAN_SITE_NAME").map(String::as_str),
        Some("from-scoped-key")
    );

    let doc = json!({
        "mcp-server-atlassian-jira": {
            "environments": { "ATLASSIAN_SITE_NAME": "from-unscoped-key" }
        }
    });
    let map = extract_all_vendor_sections(&doc, PKG);
    let jira = map.get(VENDOR_JIRA).expect("jira section resolved");
    assert_eq!(
        jira.get("ATLASSIAN_SITE_NAME").map(String::as_str),
        Some("from-unscoped-key")
    );
}

#[test]
fn ts_bitbucket_package_aliases_resolve_to_bitbucket_section() {
    // Symmetric guarantee for the TS Bitbucket server's package names.
    let doc = json!({
        "@aashari/mcp-server-atlassian-bitbucket": {
            "environments": { "BITBUCKET_DEFAULT_WORKSPACE": "from-ts-bitbucket" }
        }
    });
    let map = extract_all_vendor_sections(&doc, PKG);
    let bb = map.get(VENDOR_BITBUCKET).expect("bitbucket section resolved");
    assert_eq!(
        bb.get("BITBUCKET_DEFAULT_WORKSPACE").map(String::as_str),
        Some("from-ts-bitbucket")
    );
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
