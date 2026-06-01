//! Zoom Server-to-Server OAuth token exchange and cache.
//!
//! Zoom's S2S OAuth flow needs **no ongoing user reauthorization** — the app
//! holds static client credentials — but the bearer it issues is short-lived
//! (~1 hour), so the server must renew it automatically. This module owns
//! that lifecycle:
//!
//! - **Exchange** — `POST {token_url}` as `application/x-www-form-urlencoded`
//!   with `grant_type=account_credentials&account_id=…`, authenticated with
//!   HTTP Basic over `client_id:client_secret`.
//! - **Cache** — the issued token is kept per-[`TokenCache`] instance (one
//!   per [`ZoomVendor`](super::ZoomVendor)), keyed by the identity it was
//!   minted for so a reused vendor never serves a token across a credential
//!   or OAuth-host change.
//! - **Skew** — we expire the cache [`EXPIRY_SKEW`] early so we never race
//!   the edge of Zoom's reported lifetime.
//! - **No stampede** — the refresh path re-checks expiry after taking the
//!   write lock, so concurrent callers collapse to a single exchange.

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::{McpError, OriginalError, api_error, auth_invalid, unexpected};

/// Renew this far ahead of Zoom's reported expiry so an in-flight request
/// never carries a token that lapses mid-flight.
const EXPIRY_SKEW: Duration = Duration::from_mins(1);

/// Identity a cached token is valid for. A change in any field invalidates
/// the cache — defends a long-lived vendor instance against config reloads
/// and keeps test token-servers from bleeding state into one another.
///
/// The client secret is represented as a non-reversible fingerprint, not the
/// raw value: rotating the secret must invalidate the cached bearer (otherwise
/// a reused vendor instance would keep serving a token minted with the old
/// secret until expiry), but the plaintext secret should never live in this
/// struct — that keeps it out of the derived `Debug` and out of memory beyond
/// the exchange call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenKey {
    account_id: String,
    client_id: String,
    token_url: String,
    secret_fp: u64,
}

/// Stable in-process fingerprint of the client secret. Used only for cache-key
/// equality within a single run, so a non-cryptographic hash is sufficient;
/// the ~1/2^64 collision chance is negligible and the operator controls the
/// input (no adversarial collision concern).
fn secret_fingerprint(secret: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    secret.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug)]
struct Cached {
    key: TokenKey,
    token: String,
    /// Monotonic deadline (`Instant`, not wall-clock) so the TTL is immune to
    /// system-clock adjustments.
    expires_at: Instant,
}

/// Per-vendor-instance token cache.
#[derive(Debug, Default)]
pub struct TokenCache {
    inner: RwLock<Option<Cached>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a valid bearer for the given identity, exchanging client
    /// credentials only when the cache is empty, scoped to a different
    /// identity, or within [`EXPIRY_SKEW`] of expiry.
    pub async fn bearer(
        &self,
        client: &Client,
        account_id: &str,
        client_id: &str,
        client_secret: &str,
        token_url: &str,
    ) -> Result<String, McpError> {
        let key = TokenKey {
            account_id: account_id.to_owned(),
            client_id: client_id.to_owned(),
            token_url: token_url.to_owned(),
            secret_fp: secret_fingerprint(client_secret),
        };

        // Fast path: shared read lock, no exchange.
        {
            let guard = self.inner.read().await;
            if let Some(c) = guard.as_ref()
                && c.key == key
                && Instant::now() < c.expires_at
            {
                return Ok(c.token.clone());
            }
        }

        // Slow path: exclusive write lock. Re-check first — a concurrent
        // caller may have refreshed while we waited for the lock, so only one
        // exchange happens per expiry window (no stampede).
        let mut guard = self.inner.write().await;
        if let Some(c) = guard.as_ref()
            && c.key == key
            && Instant::now() < c.expires_at
        {
            return Ok(c.token.clone());
        }

        let (token, ttl) = exchange(client, &key, client_secret).await?;
        // Subtract the skew; never let the effective lifetime hit zero even if
        // Zoom returns an implausibly short TTL.
        let effective = ttl.saturating_sub(EXPIRY_SKEW).max(Duration::from_secs(1));
        *guard = Some(Cached {
            key,
            token: token.clone(),
            expires_at: Instant::now() + effective,
        });
        Ok(token)
    }
}

/// Perform the OAuth `account_credentials` exchange. Returns the raw token and
/// Zoom's reported lifetime (skew is applied by the caller).
async fn exchange(
    client: &Client,
    key: &TokenKey,
    client_secret: &str,
) -> Result<(String, Duration), McpError> {
    let basic = STANDARD.encode(format!("{}:{}", key.client_id, client_secret));
    // Encode the body ourselves (reqwest's `.form()` is feature-gated and this
    // crate trims default features) — same `form_urlencoded` the controller
    // layer uses for query strings.
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "account_credentials")
        .append_pair("account_id", &key.account_id)
        .finish();
    let response = client
        .post(&key.token_url)
        .header("Authorization", format!("Basic {basic}"))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| api_error(format!("Zoom OAuth token request failed: {e}"), None, None))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(classify_token_error(status, &body));
    }

    let parsed: TokenResponse = serde_json::from_str(&body).map_err(|e| {
        unexpected(
            format!("Zoom OAuth token response was not valid JSON: {e}"),
            Some(OriginalError::String(body.clone())),
        )
    })?;
    Ok((parsed.access_token, Duration::from_secs(parsed.expires_in)))
}

/// Map a failed token exchange to a typed error. Zoom's OAuth error envelope
/// is `{"reason": "...", "error": "..."}` — distinct from the API envelope.
fn classify_token_error(status: StatusCode, body: &str) -> McpError {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("reason")
                .and_then(Value::as_str)
                .or_else(|| v.get("error").and_then(Value::as_str))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                status
                    .canonical_reason()
                    .unwrap_or("Zoom OAuth error")
                    .to_owned()
            } else {
                trimmed.to_owned()
            }
        });

    let original = Some(OriginalError::String(body.to_owned()));
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let mut err = auth_invalid(format!(
            "Zoom OAuth token exchange rejected the client credentials: {detail}. Check \
             ZOOM_ACCOUNT_ID / ZOOM_CLIENT_ID / ZOOM_CLIENT_SECRET."
        ));
        err.status_code = Some(status.as_u16());
        err.original = original;
        err
    } else {
        api_error(
            format!("Zoom OAuth token exchange failed: {detail}"),
            Some(status.as_u16()),
            original,
        )
    }
}
