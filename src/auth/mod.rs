//! Credential resolution and HTTP Basic auth header construction.
//!
//! Mirrors the logic in TS `transport.util.ts` (credential selection block)
//! and the README-documented env var names. Two conventions are supported,
//! with the Atlassian API token taking priority when both sets are present.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::config::Config;
use crate::error::{McpError, auth_missing};

/// Resolved Bitbucket credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credentials {
    /// `ATLASSIAN_USER_EMAIL` + `ATLASSIAN_API_TOKEN`.
    /// Shared across Jira/Confluence/Bitbucket; preferred when present.
    AtlassianApiToken { email: String, token: String },

    /// `ATLASSIAN_BITBUCKET_USERNAME` + `ATLASSIAN_BITBUCKET_APP_PASSWORD`.
    /// Bitbucket-specific fallback.
    BitbucketAppPassword { username: String, password: String },
}

impl Credentials {
    /// Resolve credentials from a [`Config`]. Returns `None` when neither
    /// convention is fully populated; the caller decides whether this is an
    /// error (server boot) or allowed (CLI help, version, etc.).
    pub fn resolve(config: &Config) -> Option<Self> {
        if let (Some(email), Some(token)) = (
            config.get("ATLASSIAN_USER_EMAIL"),
            config.get("ATLASSIAN_API_TOKEN"),
        ) && !email.is_empty()
            && !token.is_empty()
        {
            return Some(Self::AtlassianApiToken {
                email: email.to_owned(),
                token: token.to_owned(),
            });
        }

        if let (Some(username), Some(password)) = (
            config.get("ATLASSIAN_BITBUCKET_USERNAME"),
            config.get("ATLASSIAN_BITBUCKET_APP_PASSWORD"),
        ) && !username.is_empty()
            && !password.is_empty()
        {
            return Some(Self::BitbucketAppPassword {
                username: username.to_owned(),
                password: password.to_owned(),
            });
        }

        None
    }

    /// Same as [`resolve`] but errors with [`auth_missing`] when no credentials
    /// are present. Matches TS boot behavior.
    pub fn require(config: &Config) -> Result<Self, McpError> {
        Self::resolve(config).ok_or_else(|| {
            auth_missing(
                "Authentication credentials are missing. Set ATLASSIAN_USER_EMAIL + \
                 ATLASSIAN_API_TOKEN, or ATLASSIAN_BITBUCKET_USERNAME + \
                 ATLASSIAN_BITBUCKET_APP_PASSWORD.",
            )
        })
    }

    /// `Authorization: Basic <base64>` header value.
    pub fn basic_auth_header(&self) -> String {
        format!("Basic {}", self.basic_auth_payload())
    }

    /// Base64-encoded `user:secret` payload without the `Basic ` prefix.
    pub fn basic_auth_payload(&self) -> String {
        let raw = match self {
            Self::AtlassianApiToken { email, token } => format!("{email}:{token}"),
            Self::BitbucketAppPassword { username, password } => {
                format!("{username}:{password}")
            }
        };
        STANDARD.encode(raw.as_bytes())
    }

    /// Identifier part (email or username), useful for log lines without
    /// leaking the secret.
    pub fn principal(&self) -> &str {
        match self {
            Self::AtlassianApiToken { email, .. } => email,
            Self::BitbucketAppPassword { username, .. } => username,
        }
    }
}
