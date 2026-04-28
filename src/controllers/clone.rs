//! Repository clone controller. Mirrors
//! `src/controllers/atlassian.repositories.content.controller.ts`.
//!
//! Responsibilities (in order):
//! 1. Resolve workspace (arg override > env default > API default).
//! 2. Validate `repo_slug` against the path-traversal allow-list.
//! 3. Normalise `target_path` (absolute or resolved against CWD).
//! 4. Ensure the target directory exists and is writable.
//! 5. Fetch repository metadata and pick the preferred clone URL (SSH first,
//!    HTTPS fallback).
//! 6. Spawn `git clone <url> <target>/<repoSlug>` via [`crate::shell::execute`]
//!    without a shell.
//! 7. Return a markdown success block; on failure emit the protocol-specific
//!    troubleshooting template.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::{debug, warn};

use crate::auth::Credentials;
use crate::controllers::api::{BitbucketContext, ControllerResponse};
use crate::error::{McpError, unexpected};
use crate::shell;
use crate::tools::args::CloneArgs;
use crate::transport::{RequestOptions, ResponseBody, fetch};
use crate::workspace::resolve_default_workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Ssh,
    Https,
}

impl Protocol {
    fn display(self) -> &'static str {
        match self {
            Self::Ssh => "SSH",
            Self::Https => "HTTPS",
        }
    }
}

/// Regex for the allowed `repo_slug` alphabet. Ported from TS regex literal.
fn slug_is_valid(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Main entry point invoked by both the MCP tool and the CLI subcommand.
pub async fn handle_clone(
    ctx: &BitbucketContext<'_>,
    args: &CloneArgs,
) -> Result<ControllerResponse, McpError> {
    let workspace = resolve_workspace(ctx, args.workspace_slug.as_deref()).await?;

    validate_slug(&args.repo_slug)?;
    if args.target_path.is_empty() {
        return Err(unexpected("Target path is required".to_owned(), None));
    }

    let processed_target = resolve_target_path(&args.target_path);
    ensure_target_dir(&processed_target).await?;

    let metadata = fetch_repo_metadata(ctx, &workspace, &args.repo_slug).await?;
    let (clone_url, protocol) = pick_clone_url(&metadata)?;

    let clone_dir = processed_target.join(&args.repo_slug);
    if target_subdir_exists(&clone_dir).await {
        return Ok(ControllerResponse {
            content: format!(
                "Target directory `{}` already exists. Please choose a different target path or remove the existing directory.",
                clone_dir.display()
            ),
            raw_response_path: None,
        });
    }

    debug!(%clone_url, protocol = protocol.display(), "clone: invoking git");
    let clone_dir_str = clone_dir.to_string_lossy().into_owned();
    let output = shell::execute(
        "git",
        &["clone", &clone_url, &clone_dir_str],
        "cloning repository",
    )
    .await
    .map_err(|err| enrich_clone_error(err, protocol))?;

    Ok(ControllerResponse {
        content: success_message(
            &workspace,
            &args.repo_slug,
            &clone_dir,
            protocol,
            &output.stdout,
        ),
        raw_response_path: None,
    })
}

async fn resolve_workspace(
    ctx: &BitbucketContext<'_>,
    explicit: Option<&str>,
) -> Result<String, McpError> {
    if let Some(slug) = explicit
        && !slug.is_empty()
    {
        return Ok(slug.to_owned());
    }
    resolve_default_workspace(ctx).await.ok_or_else(|| {
        unexpected(
            "No default workspace found. Please provide a workspace slug.".to_owned(),
            None,
        )
    })
}

fn validate_slug(slug: &str) -> Result<(), McpError> {
    if slug.is_empty() {
        return Err(unexpected(
            "Repository slug is required".to_owned(),
            None,
        ));
    }
    if !slug_is_valid(slug) {
        return Err(unexpected(
            "Invalid repository slug: must contain only alphanumeric characters, hyphens, underscores, and dots.".to_owned(),
            None,
        ));
    }
    Ok(())
}

fn resolve_target_path(target: &str) -> PathBuf {
    let path = Path::new(target);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    }
}

async fn ensure_target_dir(path: &Path) -> Result<(), McpError> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(unexpected(
                    format!(
                        "Cannot access target directory {}: not a directory",
                        path.display()
                    ),
                    None,
                ));
            }
            if let Err(err) = writable_check(path).await {
                return Err(unexpected(
                    format!(
                        "Permission denied: You don't have write access to the target directory: {} ({err})",
                        path.display()
                    ),
                    None,
                ));
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            debug!(path = %path.display(), "creating target directory");
            tokio::fs::create_dir_all(path).await.map_err(|err| {
                unexpected(
                    format!(
                        "Failed to create target directory {}: {err}. Please ensure you have write permissions to the parent directory.",
                        path.display()
                    ),
                    None,
                )
            })
        }
        Err(err) => Err(unexpected(
            format!("Cannot access target directory {}: {err}", path.display()),
            None,
        )),
    }
}

async fn writable_check(path: &Path) -> std::io::Result<()> {
    // `tokio::fs` doesn't expose an access(2) wrapper; attempt a probe write
    // of a harmless file into the directory.
    let probe = path.join(".mcp-clone-writable-probe");
    tokio::fs::write(&probe, b"").await?;
    let _ = tokio::fs::remove_file(&probe).await;
    Ok(())
}

async fn target_subdir_exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok_and(|m| m.is_dir())
}

async fn fetch_repo_metadata(
    ctx: &BitbucketContext<'_>,
    workspace: &str,
    repo: &str,
) -> Result<Value, McpError> {
    let handle = ctx.handle();
    let creds = Credentials::require_for_async(handle.config, handle.vendor.name()).await?;
    let path = format!("/2.0/repositories/{workspace}/{repo}");
    let response = fetch(
        handle.client,
        handle.vendor,
        &creds,
        handle.config,
        &path,
        RequestOptions::default(),
    )
    .await?;
    match response.data {
        ResponseBody::Json(v) => Ok(v),
        _ => Err(unexpected(
            format!("Unexpected non-JSON body fetching {path}"),
            None,
        )),
    }
}

fn pick_clone_url(metadata: &Value) -> Result<(String, Protocol), McpError> {
    let links = metadata
        .get("links")
        .and_then(|l| l.get("clone"))
        .and_then(Value::as_array);
    let Some(entries) = links else {
        return Err(unexpected(
            "Could not find a valid clone URL for the repository".to_owned(),
            None,
        ));
    };
    if let Some(url) = url_with_name(entries, "ssh") {
        return Ok((url, Protocol::Ssh));
    }
    if let Some(url) = url_with_name(entries, "https") {
        warn!("SSH clone URL not found, falling back to HTTPS");
        return Ok((url, Protocol::Https));
    }
    Err(unexpected(
        "Could not find a valid clone URL for the repository".to_owned(),
        None,
    ))
}

fn url_with_name(entries: &[Value], name: &str) -> Option<String> {
    entries.iter().find_map(|entry| {
        let obj = entry.as_object()?;
        if obj.get("name").and_then(Value::as_str)? != name {
            return None;
        }
        obj.get("href")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn enrich_clone_error(err: McpError, protocol: Protocol) -> McpError {
    let troubleshooting = match protocol {
        Protocol::Ssh => {
            "\n\n**Troubleshooting SSH Clone Issues:**\n\
             1. Ensure you have SSH keys set up with Bitbucket\n\
             2. Check if your SSH agent is running: `eval \"$(ssh-agent -s)\"; ssh-add`\n\
             3. Verify connectivity: `ssh -T git@bitbucket.org`\n\
             4. Try using HTTPS instead (modify your tool call with a different repository URL)"
        }
        Protocol::Https => {
            "\n\n**Troubleshooting HTTPS Clone Issues:**\n\
             1. Check your Bitbucket credentials\n\
             2. Ensure the target directory is writable\n\
             3. Try running the command manually to see detailed errors"
        }
    };
    let mut enriched = err;
    enriched.message = format!("{}{troubleshooting}", enriched.message);
    enriched
}

fn success_message(
    workspace: &str,
    repo: &str,
    target: &Path,
    protocol: Protocol,
    stdout: &str,
) -> String {
    format!(
        "Successfully cloned repository `{workspace}/{repo}` to `{target}` using {proto}.\n\n\
         **Details:**\n\
         - **Repository**: {workspace}/{repo}\n\
         - **Clone Protocol**: {proto}\n\
         - **Target Location**: {target}\n\n\
         **Output:**\n```\n{stdout}\n```\n\n\
         **Note**: If this is your first time cloning with SSH, ensure your SSH keys are set up correctly.",
        target = target.display(),
        proto = protocol.display(),
    )
}
