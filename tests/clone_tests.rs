// Tests mutate the process-wide `PATH` to install a shim `git`, which is
// `unsafe` under the 2024 edition. `#[serial]` + `path_lock()` serialize the
// mutations; this `allow` is scoped to the test file.
#![allow(unsafe_code)]

//! End-to-end clone controller tests. Intercepts the metadata fetch via
//! wiremock and replaces `git` with a test shim on PATH so the clone
//! invocation is deterministic without network or real-repo setup.

use std::collections::HashMap;
use std::path::Path;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::api::BitbucketContext;
use mcp_server_atlassian::controllers::handle_clone;
use mcp_server_atlassian::tools::args::CloneArgs;
use mcp_server_atlassian::transport::build_client;
use mcp_server_atlassian::vendor::bitbucket::BitbucketVendor;
use mcp_server_atlassian::workspace::WorkspaceCache;
use serde_json::json;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ATLASSIAN_USER_EMAIL".into(), "alice@example.com".into());
    m.insert("ATLASSIAN_API_TOKEN".into(), "tok".into());
    m
}

/// Install a shim `git` in `shim_dir` that records its invocation in a
/// sibling marker file and exits 0. Returns the original PATH for restore.
fn install_git_shim(shim_dir: &Path, marker: &Path, exit_code: i32) -> String {
    let git_path = shim_dir.join("git");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{marker}'\nexit {exit_code}\n",
        marker = marker.display()
    );
    std::fs::write(&git_path, script).unwrap();
    let mut perms = std::fs::metadata(&git_path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&git_path, perms).unwrap();

    let original = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{original}", shim_dir.display());
    // SAFETY: test-only single-threaded mutation protected by `path_lock()`.
    // The `unsafe` is required under the Rust 2024 edition.
    unsafe {
        std::env::set_var("PATH", new_path);
    }
    original
}

fn restore_path(original: String) {
    unsafe {
        std::env::set_var("PATH", original);
    }
}

#[tokio::test]
#[serial]
async fn successful_clone_prefers_ssh_and_invokes_git() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "links": {
                "clone": [
                    {"href": "https://bitbucket.org/acme/widget.git", "name": "https"},
                    {"href": "git@bitbucket.org:acme/widget.git", "name": "ssh"}
                ]
            }
        })))
        .mount(&server)
        .await;

    let bin_dir = TempDir::new().unwrap();
    let marker = bin_dir.path().join("args.log");
    let original_path = install_git_shim(bin_dir.path(), &marker, 0);

    let clone_root = TempDir::new().unwrap();
    let args = CloneArgs {
        workspace_slug: Some("acme".into()),
        repo_slug: "widget".into(),
        target_path: clone_root.path().display().to_string(),
    };

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);
    let resp = handle_clone(&ctx, &args).await.unwrap();

    restore_path(original_path);

    assert!(resp.content.contains("using SSH"));
    assert!(resp.content.contains("acme/widget"));
    let recorded = std::fs::read_to_string(&marker).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines[0], "clone");
    assert_eq!(lines[1], "git@bitbucket.org:acme/widget.git");
    assert!(lines[2].ends_with("/widget"));
}

#[tokio::test]
#[serial]
async fn falls_back_to_https_when_no_ssh_link() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "links": {
                "clone": [
                    {"href": "https://bitbucket.org/acme/widget.git", "name": "https"}
                ]
            }
        })))
        .mount(&server)
        .await;

    let bin_dir = TempDir::new().unwrap();
    let marker = bin_dir.path().join("args.log");
    let original_path = install_git_shim(bin_dir.path(), &marker, 0);

    let clone_root = TempDir::new().unwrap();
    let args = CloneArgs {
        workspace_slug: Some("acme".into()),
        repo_slug: "widget".into(),
        target_path: clone_root.path().display().to_string(),
    };

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);
    let resp = handle_clone(&ctx, &args).await.unwrap();

    restore_path(original_path);

    assert!(resp.content.contains("using HTTPS"));
    let recorded = std::fs::read_to_string(&marker).unwrap();
    assert!(recorded.contains("https://bitbucket.org/acme/widget.git"));
}

#[tokio::test]
#[serial]
async fn invalid_slug_is_rejected_before_network() {
    let server = MockServer::start().await;
    // Intentionally no mocks: reaching the network would produce a 404, but
    // the slug validation should short-circuit before that.
    let clone_root = TempDir::new().unwrap();
    let args = CloneArgs {
        workspace_slug: Some("acme".into()),
        repo_slug: "bad/slug".into(),
        target_path: clone_root.path().display().to_string(),
    };

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);
    let err = handle_clone(&ctx, &args).await.unwrap_err();
    assert!(err.message.contains("Invalid repository slug"));
}

#[tokio::test]
#[serial]
async fn existing_subdir_returns_info_not_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "links": {
                "clone": [
                    {"href": "git@bitbucket.org:acme/widget.git", "name": "ssh"}
                ]
            }
        })))
        .mount(&server)
        .await;

    let clone_root = TempDir::new().unwrap();
    let subdir = clone_root.path().join("widget");
    std::fs::create_dir_all(&subdir).unwrap();

    let args = CloneArgs {
        workspace_slug: Some("acme".into()),
        repo_slug: "widget".into(),
        target_path: clone_root.path().display().to_string(),
    };

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);
    let resp = handle_clone(&ctx, &args).await.unwrap();
    assert!(resp.content.contains("already exists"));
}

#[tokio::test]
#[serial]
async fn git_failure_surfaces_ssh_troubleshooting() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2.0/repositories/acme/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "links": {
                "clone": [
                    {"href": "git@bitbucket.org:acme/widget.git", "name": "ssh"}
                ]
            }
        })))
        .mount(&server)
        .await;

    let bin_dir = TempDir::new().unwrap();
    let marker = bin_dir.path().join("args.log");
    let original_path = install_git_shim(bin_dir.path(), &marker, 128);

    let clone_root = TempDir::new().unwrap();
    let args = CloneArgs {
        workspace_slug: Some("acme".into()),
        repo_slug: "widget".into(),
        target_path: clone_root.path().display().to_string(),
    };

    let client = build_client().unwrap();
    let config = Config::from_map(creds());
    let vendor = BitbucketVendor::with_base_url(server.uri());
    let cache = WorkspaceCache::new();
    let ctx = BitbucketContext::new(&client, &config, &vendor, &cache);
    let err = handle_clone(&ctx, &args).await.unwrap_err();

    restore_path(original_path);

    assert!(err.message.contains("Troubleshooting SSH Clone Issues"));
}
