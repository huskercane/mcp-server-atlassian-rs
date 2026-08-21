#![allow(clippy::doc_markdown)]

//! NinjaOne web-console session login (`sessionKey` acquisition).
//!
//! The private `/ws/...` console endpoints authenticate with a `sessionKey`
//! that the browser obtains through three plain JSON POSTs — no browser
//! machinery is involved, so the server can perform the same exchange:
//!
//! 1. `POST /ws/account/authentication-state` `{"email"}` →
//!    `{"authState":"NATIVE","recaptchaRequired":false}`. Tells us whether the
//!    principal is a native (password) login or federated, and whether the
//!    tenant demands a reCAPTCHA token.
//! 2. `POST /ws/account/login` `{"email","password","staySignedIn"}` → either
//!    `{"resultCode":"SUCCESS","sessionKey":…}` when MFA is off, or
//!    `{"resultCode":"MFA_REQUIRED","loginToken":…,"mfaType":"TOTP"}`.
//! 3. `POST /ws/account/mfa-login` `{"loginToken","code"}` →
//!    `{"resultCode":"SUCCESS","sessionKey":…}`, alongside
//!    `Set-Cookie: sessionKey=…;Path=/;Secure;HttpOnly;SameSite=Lax`. The body
//!    field and the cookie carry the same value, so replaying the body field
//!    as that cookie reproduces exactly what the browser sends.
//!
//! The issued key is cached **in memory only**, per (base URL, email), for the
//! life of the process: it is a session secret, so it never touches disk. A
//! restart therefore needs a fresh MFA code, and a `401` from any `/ws/...`
//! call evicts the entry so the next tool call reports an actionable
//! "session expired" error rather than looping on a dead key.
//!
//! The `email`/`password`/`mfa_code` inputs are already resolved by the time
//! they arrive here — from the top-level `NINJAONE_*` keys or from the selected
//! `NINJAONE_SERVERS` entry, which is how one operator holds a different
//! account per environment. This module only performs the exchange; where the
//! credentials came from is [`super`]'s concern.

use std::collections::HashMap;
use std::fmt;

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::debug;

use super::{HTTP_LOG_TARGET, sanitized_http_json, sanitized_http_text};
use crate::error::{McpError, OriginalError, api_error, auth_invalid, unexpected};

const AUTH_STATE_PATH: &str = "/ws/account/authentication-state";
const LOGIN_PATH: &str = "/ws/account/login";
const MFA_LOGIN_PATH: &str = "/ws/account/mfa-login";

/// Identity a cached session key belongs to. Both fields matter: one process
/// may hold sessions for several `NINJAONE_SERVERS` aliases, and the same
/// alias may be re-authenticated as a different principal after a config
/// reload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionId {
    base_url: String,
    email: String,
}

/// Inputs for one login attempt. Credentials always come from server-held
/// config; only the one-time MFA code (and an optional reCAPTCHA token) may
/// originate from a tool argument.
#[derive(Debug, Clone, Copy)]
pub struct LoginRequest<'a> {
    pub base_url: &'a str,
    pub email: &'a str,
    pub password: &'a str,
    pub mfa_code: Option<&'a str>,
    pub recaptcha_token: Option<&'a str>,
}

/// Non-secret result of a successful login. The session key itself stays in
/// the cache; callers get a short prefix so a human can correlate the session
/// with a browser one without the full secret entering tool output or logs.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    /// The account the session belongs to. Reported back because on NinjaOne
    /// the account *is* the access boundary — division and role come from it —
    /// so "which login is this session" is not a detail a caller can infer.
    pub email: String,
    pub session_key_preview: String,
    pub mfa_used: bool,
    pub mfa_type: Option<String>,
    pub division_uid: Option<String>,
    pub app_user_uid: Option<String>,
    pub user_type: Option<String>,
}

/// Process-lifetime session-key cache, shared by every clone of a
/// [`NinjaOneVendor`](super::NinjaOneVendor) through an `Arc`.
#[derive(Default)]
pub struct SessionCache {
    inner: RwLock<HashMap<SessionId, String>>,
}

/// Redacting `Debug`: the map values are live session secrets, and
/// `NinjaOneVendor` derives `Debug`, so the default derive would print them
/// into any diagnostic that formats the vendor.
impl fmt::Debug for SessionCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionCache")
            .field("entries", &self.inner.try_read().map_or(0, |g| g.len()))
            .finish()
    }
}

impl SessionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cached key for this identity, if one was minted.
    pub async fn get(&self, base_url: &str, email: &str) -> Option<String> {
        let session_key = self.inner.read().await.get(&id(base_url, email)).cloned();
        debug!(
            target: HTTP_LOG_TARGET,
            pid = std::process::id(),
            %base_url,
            %email,
            hit = session_key.is_some(),
            "ninjaone session cache lookup"
        );
        session_key
    }

    /// Drop the cached key for this identity. Called after a `401` so the next
    /// tool call fails with a "log in again" message instead of replaying a
    /// key the server has already discarded.
    pub async fn invalidate(&self, base_url: &str, email: &str) {
        if self
            .inner
            .write()
            .await
            .remove(&id(base_url, email))
            .is_some()
        {
            debug!(%base_url, "ninjaone: dropped expired session key");
        }
    }

    /// Run the full login exchange and cache the resulting session key.
    ///
    /// Always performs a fresh exchange — re-running the login tool with a new
    /// MFA code is how an operator deliberately replaces a stale session, so
    /// this must not short-circuit on a cached entry.
    pub async fn login(
        &self,
        client: &Client,
        request: LoginRequest<'_>,
    ) -> Result<LoginOutcome, McpError> {
        match fetch_auth_state(client, request).await {
            Ok(state) => {
                if let Some(auth_state) = state.auth_state.as_deref()
                    && !auth_state.eq_ignore_ascii_case("NATIVE")
                {
                    return Err(auth_invalid(format!(
                        "NinjaOne reports authState `{auth_state}` for {}: automated login supports \
                         native email/password accounts only. Use NINJAONE_SESSION_KEY with a key \
                         copied from a browser session instead.",
                        request.email
                    )));
                }
                if state.recaptcha_required.unwrap_or(false) && request.recaptcha_token.is_none() {
                    return Err(auth_invalid(format!(
                        "NinjaOne requires a reCAPTCHA token for {}. Supply one via the login tool's \
                         `recaptchaToken` argument (copy it from a browser login), or use \
                         NINJAONE_SESSION_KEY.",
                        request.email
                    )));
                }
            }
            Err(error) if error.status_code == Some(StatusCode::NOT_FOUND.as_u16()) => {
                // Some appliance/older-host deployments do not expose this optional
                // browser pre-flight endpoint. The login endpoint remains authoritative
                // and returns a typed rejection for unsupported principals or credentials.
                debug!(
                    base_url = %request.base_url,
                    "ninjaone: authentication-state endpoint unavailable; continuing with login"
                );
            }
            Err(error) => return Err(error),
        }

        let login = post_step(
            client,
            &url(request.base_url, LOGIN_PATH),
            &json!({
                "email": request.email,
                "password": request.password,
                "staySignedIn": false,
            }),
            "login",
        )
        .await?;

        let (response, mfa_used) = match login.result_code.as_deref() {
            Some("SUCCESS") => (login, false),
            Some("MFA_REQUIRED") => (self.complete_mfa(client, request, &login).await?, true),
            _ => return Err(login_rejected(&login, request.email)),
        };

        let session_key = response.session_key.clone().ok_or_else(|| {
            unexpected(
                "NinjaOne login succeeded but returned no sessionKey",
                Some(OriginalError::String(
                    response.result_code.clone().unwrap_or_default(),
                )),
            )
        })?;

        let outcome = LoginOutcome {
            email: request.email.to_owned(),
            session_key_preview: preview(&session_key),
            mfa_used,
            mfa_type: response.mfa_type.clone(),
            division_uid: response.division_uid.clone(),
            app_user_uid: response.app_user_uid.clone(),
            user_type: response.user_type.clone(),
        };
        self.inner
            .write()
            .await
            .insert(id(request.base_url, request.email), session_key);
        debug!(
            target: HTTP_LOG_TARGET,
            pid = std::process::id(),
            base_url = %request.base_url,
            email = %request.email,
            mfa_used,
            "ninjaone session cached"
        );
        Ok(outcome)
    }

    /// Second leg of an MFA login: exchange `loginToken` + one-time code for
    /// the session key.
    async fn complete_mfa(
        &self,
        client: &Client,
        request: LoginRequest<'_>,
        login: &LoginResponse,
    ) -> Result<LoginResponse, McpError> {
        let code = request
            .mfa_code
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| {
                auth_invalid(format!(
                    "NinjaOne requires a {} multi-factor code for {}. Call ninjaone_login again \
                 with `mfaCode` set to the current code.",
                    login.mfa_type.as_deref().unwrap_or("multi-factor"),
                    request.email
                ))
            })?;
        let login_token = login.login_token.as_deref().ok_or_else(|| {
            unexpected(
                "NinjaOne asked for a multi-factor code but returned no loginToken",
                None,
            )
        })?;

        let mut body = json!({ "loginToken": login_token, "code": code });
        if let Some(token) = request.recaptcha_token
            && let Some(map) = body.as_object_mut()
        {
            map.insert("recaptchaToken".to_owned(), Value::String(token.to_owned()));
        }

        let response = post_step(
            client,
            &url(request.base_url, MFA_LOGIN_PATH),
            &body,
            "mfa-login",
        )
        .await?;
        if response.result_code.as_deref() == Some("SUCCESS") {
            Ok(response)
        } else {
            Err(login_rejected(&response, request.email))
        }
    }
}

fn id(base_url: &str, email: &str) -> SessionId {
    SessionId {
        base_url: base_url.to_owned(),
        email: email.to_owned(),
    }
}

fn url(base_url: &str, path: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/ws") && path.starts_with("/ws/") {
        format!("{base_url}{}", &path[3..])
    } else {
        format!("{base_url}{path}")
    }
}

/// First 8 characters of the session key (it is a UUID, so this is the first
/// group). Enough to correlate two sessions, useless as a credential.
fn preview(session_key: &str) -> String {
    session_key.chars().take(8).collect()
}

/// Pre-flight probe. A failure here is reported as-is: if the tenant cannot
/// even classify the principal, the subsequent login would fail anyway and a
/// vague error at step 2 would be harder to act on.
async fn fetch_auth_state(
    client: &Client,
    request: LoginRequest<'_>,
) -> Result<AuthStateResponse, McpError> {
    let body = json!({ "email": request.email });
    let text = send(
        client,
        &url(request.base_url, AUTH_STATE_PATH),
        &body,
        "authentication-state",
    )
    .await?;
    serde_json::from_str(&text).map_err(|error| {
        unexpected(
            format!("NinjaOne authentication-state response was not valid JSON: {error}"),
            Some(OriginalError::String(text)),
        )
    })
}

async fn post_step(
    client: &Client,
    url: &str,
    body: &Value,
    step: &str,
) -> Result<LoginResponse, McpError> {
    let text = send(client, url, body, step).await?;
    serde_json::from_str(&text).map_err(|error| {
        unexpected(
            format!("NinjaOne {step} response was not valid JSON: {error}"),
            Some(OriginalError::String(text)),
        )
    })
}

/// POST JSON and return the raw body, mapping transport and non-2xx failures
/// to typed errors. The request body carries the password on the `login` step,
/// so nothing here logs or echoes it.
async fn send(client: &Client, url: &str, body: &Value, step: &str) -> Result<String, McpError> {
    debug!(
        target: HTTP_LOG_TARGET,
        method = "POST",
        %url,
        step,
        body = %sanitized_http_json(body),
        "ninjaone HTTP request"
    );
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .json(body)
        .send()
        .await
        .map_err(|error| {
            api_error(
                format!("NinjaOne {step} request failed: {error}"),
                None,
                None,
            )
        })?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    debug!(
        target: HTTP_LOG_TARGET,
        method = "POST",
        %url,
        step,
        status = status.as_u16(),
        body = %sanitized_http_text(&text),
        "ninjaone HTTP response"
    );
    if status.is_success() {
        Ok(text)
    } else {
        Err(step_error(step, status, &text))
    }
}

fn step_error(step: &str, status: StatusCode, body: &str) -> McpError {
    let detail = envelope_message(body).unwrap_or_else(|| {
        status
            .canonical_reason()
            .unwrap_or("NinjaOne login error")
            .to_owned()
    });
    let original = Some(OriginalError::String(body.to_owned()));
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let mut error = auth_invalid(format!(
            "NinjaOne {step} rejected the credentials: {detail}"
        ));
        error.status_code = Some(status.as_u16());
        error.original = original;
        error
    } else {
        api_error(
            format!("NinjaOne {step} failed: {detail}"),
            Some(status.as_u16()),
            original,
        )
    }
}

/// A `200` carrying a non-success `resultCode` — wrong password, expired
/// login token, bad MFA code. NinjaOne signals these in the body, not the
/// status, so they must be reclassified here.
fn login_rejected(response: &LoginResponse, email: &str) -> McpError {
    let code = response.result_code.as_deref().unwrap_or("UNKNOWN");
    let detail = response
        .error_message
        .as_deref()
        .map_or_else(String::new, |message| format!(": {message}"));
    auth_invalid(format!(
        "NinjaOne login for {email} returned `{code}`{detail}. Check the email and password \
         configured for this server (NINJAONE_EMAIL / NINJAONE_PASSWORD, or the `email` / \
         `password` fields of its NINJAONE_SERVERS entry) and, for an MFA account, that the \
         code is current."
    ))
}

fn envelope_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .get("errorMessage")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|message| !message.is_empty())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStateResponse {
    auth_state: Option<String>,
    recaptcha_required: Option<bool>,
}

/// Shared shape of the `login` and `mfa-login` responses. Both endpoints
/// answer with a `resultCode` envelope; which fields are populated depends on
/// whether MFA interrupted the exchange.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    result_code: Option<String>,
    session_key: Option<String>,
    login_token: Option<String>,
    mfa_type: Option<String>,
    division_uid: Option<String>,
    app_user_uid: Option<String>,
    user_type: Option<String>,
    error_message: Option<String>,
}
