//! Generic NinjaOne request controller.

use reqwest::Client;
use serde_json::{Value, json};

use crate::config::Config;
use crate::controllers::api::{ControllerResponse, HandleContext, dispatch_with_creds};
use crate::error::{McpError, auth_invalid};
use crate::format::{OutputFormat, jmespath::apply_jq_filter, render};
use crate::tools::args::{NinjaOneLoginArgs, NinjaOneReadArgs, NinjaOneWriteArgs, QueryParams};
use crate::transport::HttpMethod;
use crate::vendor::Vendor;
use crate::vendor::ninjaone::{AuthSource, NinjaOneVendor};

pub struct NinjaOneContext<'a> {
    pub client: &'a Client,
    pub config: &'a Config,
    pub vendor: &'a NinjaOneVendor,
}

impl<'a> NinjaOneContext<'a> {
    pub fn new(client: &'a Client, config: &'a Config, vendor: &'a NinjaOneVendor) -> Self {
        Self {
            client,
            config,
            vendor,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn request(
    ctx: &NinjaOneContext<'_>,
    server: Option<&str>,
    method: HttpMethod,
    path: &str,
    query: Option<&QueryParams>,
    body: Option<Value>,
    jq: Option<&str>,
    output_format: OutputFormat,
) -> Result<ControllerResponse, McpError> {
    // Resolve credentials through the server-scoped vendor: a minted session
    // key is cached per base URL, so the alias must be applied before lookup.
    let vendor = ctx.vendor.for_server(server);
    let auth = vendor.resolve_auth(ctx.config).await?;
    let handle = HandleContext::new(ctx.client, ctx.config, &vendor);
    let result = dispatch_with_creds(
        &handle,
        &auth.credentials,
        method,
        path,
        query,
        body,
        jq,
        output_format,
    )
    .await;

    match result {
        Err(error) if auth.source == AuthSource::LoginSession && is_unauthorized(&error) => {
            // The minted key is dead (NinjaOne expires console sessions server
            // side). Evict it so the next call reports "log in again" instead
            // of replaying a key that can never succeed.
            vendor.invalidate_session(ctx.config).await;
            Err(session_expired(&error))
        }
        other => other,
    }
}

/// Only `401` means "this session key is not accepted". A `403` is an
/// authorization decision about a valid session, so evicting on it would throw
/// away a working key and send the operator to re-enter an MFA code for
/// nothing.
fn is_unauthorized(error: &McpError) -> bool {
    error.status_code == Some(401)
}

fn session_expired(error: &McpError) -> McpError {
    let mut expired = auth_invalid(format!(
        "The NinjaOne session key minted by ninjaone_login is no longer valid ({}). Call \
         ninjaone_login again with the current multi-factor code.",
        error.message
    ));
    expired.status_code = error.status_code;
    expired.original.clone_from(&error.original);
    expired
}

pub async fn handle_read(
    ctx: &NinjaOneContext<'_>,
    method: HttpMethod,
    args: &NinjaOneReadArgs,
) -> Result<ControllerResponse, McpError> {
    request(
        ctx,
        args.server.as_deref(),
        method,
        &args.path,
        args.query_params.as_ref(),
        None,
        args.jq.as_deref(),
        args.output_format.map_or(OutputFormat::Toon, Into::into),
    )
    .await
}

pub async fn handle_write(
    ctx: &NinjaOneContext<'_>,
    method: HttpMethod,
    args: &NinjaOneWriteArgs,
) -> Result<ControllerResponse, McpError> {
    request(
        ctx,
        args.server.as_deref(),
        method,
        &args.path,
        args.query_params.as_ref(),
        Some(args.body.clone()),
        args.jq.as_deref(),
        args.output_format.map_or(OutputFormat::Toon, Into::into),
    )
    .await
}

/// Mint a console session key for the configured NinjaOne principal.
///
/// The session key never leaves the process: the response reports only the
/// non-secret facts (which server, which account, which division, whether MFA
/// was used, and an 8-character prefix so a human can correlate the session
/// with a browser one). The account matters to the caller: each server can be
/// configured with its own principal, and that principal is what decides the
/// division and role the session sees.
pub async fn login(
    ctx: &NinjaOneContext<'_>,
    args: &NinjaOneLoginArgs,
) -> Result<ControllerResponse, McpError> {
    let vendor = ctx.vendor.for_server(args.server.as_deref());
    let outcome = vendor
        .login(
            ctx.client,
            ctx.config,
            args.mfa_code.as_deref(),
            args.recaptcha_token.as_deref(),
        )
        .await?;

    // Warm the session-properties cache immediately. Besides verifying that
    // NinjaOne accepts the newly minted cookie, this captures division/user
    // context needed by later console and database-discovery workflows.
    // The shared response cache isolates this entry by the session credential
    // fingerprint and never writes it to disk.
    if let Err(error) = request(
        ctx,
        args.server.as_deref(),
        HttpMethod::Get,
        "/webapp/sessionproperties",
        None,
        None,
        None,
        OutputFormat::Json,
    )
    .await
    {
        tracing::debug!(%error, "ninjaone: session-properties cache warm failed");
    }

    // A configured NINJAONE_ACCESS_TOKEN still outranks a console session, so
    // say plainly which credential the next ninjaone_* call will carry rather
    // than letting a successful login imply it is the one in use.
    let active = match vendor.resolve_auth(ctx.config).await?.source {
        AuthSource::AccessToken => "NINJAONE_ACCESS_TOKEN",
        AuthSource::LoginSession => "login session",
        AuthSource::StaticSessionKey => "NINJAONE_SESSION_KEY",
        AuthSource::SessionCookie => "NINJAONE_SESSION_COOKIE",
    };

    let summary = json!({
        "authenticated": true,
        "activeCredential": active,
        "server": args.server.clone(),
        "baseUrl": vendor.base_url(ctx.config)?,
        "email": outcome.email,
        "mfaUsed": outcome.mfa_used,
        "mfaType": outcome.mfa_type,
        "sessionKeyPreview": format!("{}…", outcome.session_key_preview),
        "divisionUid": outcome.division_uid,
        "appUserUid": outcome.app_user_uid,
        "userType": outcome.user_type,
        "note": "The session key is held in memory for this server process only. \
                 Session properties were fetched and cached for later calls. Restarting the MCP \
                 server requires a new login.",
    });

    let filtered = apply_jq_filter(&summary, args.jq.as_deref());
    Ok(ControllerResponse {
        content: render(
            &filtered,
            args.output_format.map_or(OutputFormat::Toon, Into::into),
        ),
        raw_response_path: None,
    })
}
