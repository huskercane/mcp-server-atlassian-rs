#![allow(clippy::doc_markdown)]

//! Integration tests for NinjaOne console-session login.
//!
//! The three login endpoints and the API surface are stood up on one wiremock
//! instance, so the full path a `ninjaone_login` call takes —
//! `authentication-state` → `login` → `mfa-login` → cached `sessionKey` header
//! on the next tool call — runs with no network and no global state.

use std::collections::HashMap;

use mcp_server_atlassian::config::Config;
use mcp_server_atlassian::controllers::ninjaone::{NinjaOneContext, handle_read, login};
use mcp_server_atlassian::error::ErrorKind;
use mcp_server_atlassian::tools::args::{NinjaOneLoginArgs, NinjaOneReadArgs, OutputFormatArg};
use mcp_server_atlassian::transport::{HttpMethod, build_client};
use mcp_server_atlassian::vendor::ninjaone::NinjaOneVendor;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EMAIL: &str = "tech@example.com";
const PASSWORD: &str = "s3cret";
const SESSION_KEY: &str = "6be0db57-a551-471c-92cd-d58534ab8fc5";
/// A minted session is replayed exactly as the browser replays it: the
/// `sessionKey` cookie the console sets on `mfa-login`.
const SESSION_COOKIE: &str = "sessionKey=6be0db57-a551-471c-92cd-d58534ab8fc5";
const LOGIN_TOKEN: &str = "cde4f659-5b6f-426d-a016-0cd9b97ad714";

fn config(extra: &[(&str, &str)]) -> Config {
    let mut map = HashMap::from([
        ("NINJAONE_EMAIL".to_owned(), EMAIL.to_owned()),
        ("NINJAONE_PASSWORD".to_owned(), PASSWORD.to_owned()),
    ]);
    for (key, value) in extra {
        map.insert((*key).to_owned(), (*value).to_owned());
    }
    Config::from_map(map)
}

fn login_args() -> NinjaOneLoginArgs {
    NinjaOneLoginArgs {
        output_format: Some(OutputFormatArg::Json),
        ..NinjaOneLoginArgs::default()
    }
}

fn read_args() -> NinjaOneReadArgs {
    NinjaOneReadArgs {
        server: None,
        path: "/ws/webapp/sessionproperties".to_owned(),
        query_params: None,
        jq: None,
        output_format: Some(OutputFormatArg::Json),
    }
}

/// `POST /ws/account/authentication-state` → native password login, no
/// reCAPTCHA. Mirrors the captured tenant response.
async fn mount_auth_state(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/ws/account/authentication-state"))
        .and(body_json(json!({ "email": EMAIL })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authState": "NATIVE",
            "recaptchaRequired": false,
        })))
        .mount(server)
        .await;
}

/// `POST /ws/account/login` with the given response body.
fn login_mock(body: serde_json::Value) -> Mock {
    Mock::given(method("POST"))
        .and(path("/ws/account/login"))
        .and(body_json(json!({
            "email": EMAIL,
            "password": PASSWORD,
            "staySignedIn": false,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
}

fn mfa_required() -> serde_json::Value {
    json!({
        "resultCode": "MFA_REQUIRED",
        "loginToken": LOGIN_TOKEN,
        "mfaType": "TOTP",
        "available_mfa": {"TOTP": "yes"},
    })
}

fn session_success() -> serde_json::Value {
    json!({
        "resultCode": "SUCCESS",
        "succeeded": true,
        "forbidden": false,
        "sessionKey": SESSION_KEY,
        "maxAge": -1,
        "divisionUid": "17115c07-fe78-4563-a7a4-4c798decbacb",
        "appUserUid": "2194289a-33e7-4006-a153-c79ef0666472",
        "userType": "TECHNICIAN",
    })
}

#[tokio::test]
async fn mfa_login_mints_a_session_key_reused_by_later_calls() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .and(body_json(
            json!({ "loginToken": LOGIN_TOKEN, "code": "381164" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        // Exactly one exchange: the second tool call must reuse the cache.
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ws/webapp/sessionproperties"))
        .and(header("cookie", SESSION_COOKIE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attributes": {"id": 7}})))
        .expect(2)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.mfa_code = Some("381164".to_owned());
    let response = login(&ctx, &args).await.unwrap();
    assert!(response.content.contains("\"mfaUsed\": true"));
    assert!(response.content.contains("TECHNICIAN"));
    // Only the non-secret prefix is reported.
    assert!(response.content.contains("6be0db57"));
    assert!(!response.content.contains(SESSION_KEY));

    for _ in 0..2 {
        let read = handle_read(&ctx, HttpMethod::Get, &read_args())
            .await
            .unwrap();
        assert!(read.content.contains('7'));
        if let Some(path) = read.raw_response_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[tokio::test]
async fn login_without_mfa_returns_the_session_key_directly() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(session_success()).mount(&server).await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let response = login(&ctx, &login_args()).await.unwrap();
    assert!(response.content.contains("\"mfaUsed\": false"));
    assert!(response.content.contains("\"authenticated\": true"));
}

#[tokio::test]
async fn mfa_required_without_a_code_is_actionable() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let error = login(&ctx, &login_args()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("TOTP"));
    assert!(error.message.contains("mfaCode"));
}

#[tokio::test]
async fn a_rejected_password_is_reported_as_invalid_auth() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(json!({
        "resultCode": "FAILURE",
        "succeeded": false,
        "errorMessage": "Invalid credentials",
    }))
    .mount(&server)
    .await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let error = login(&ctx, &login_args()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("FAILURE"));
    assert!(error.message.contains("Invalid credentials"));
    assert!(!error.message.contains(PASSWORD));
}

#[tokio::test]
async fn a_wrong_mfa_code_is_reported_without_caching_a_session() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resultCode": "INVALID_MFA_CODE",
            "succeeded": false,
            "errorMessage": "Invalid code",
        })))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.mfa_code = Some("000000".to_owned());
    let error = login(&ctx, &args).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("INVALID_MFA_CODE"));

    // No session was cached, so a follow-up call must say so rather than
    // dispatching with a phantom key.
    let error = handle_read(&ctx, HttpMethod::Get, &read_args())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("ninjaone_login"));
}

#[tokio::test]
async fn a_minted_session_outranks_a_stale_configured_key() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(session_success()).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/ws/webapp/sessionproperties"))
        .and(header("cookie", SESSION_COOKIE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attributes": {"id": 7}})))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[("NINJAONE_SESSION_KEY", "stale-key-from-a-browser")]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    login(&ctx, &login_args()).await.unwrap();
    let read = handle_read(&ctx, HttpMethod::Get, &read_args())
        .await
        .unwrap();
    assert!(read.content.contains('7'));
    if let Some(path) = read.raw_response_path {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn an_expired_session_is_evicted_and_reported() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(session_success()).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/ws/webapp/sessionproperties"))
        .and(header("cookie", SESSION_COOKIE))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "resultCode": "FAILURE",
            "errorMessage": "Missing or empty sessionKey.",
        })))
        // The dead key must be replayed once and only once.
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    login(&ctx, &login_args()).await.unwrap();

    let error = handle_read(&ctx, HttpMethod::Get, &read_args())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("ninjaone_login"));

    let error = handle_read(&ctx, HttpMethod::Get, &read_args())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("No NinjaOne session is active"));
}

#[tokio::test]
async fn a_federated_account_is_refused_before_the_password_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ws/account/authentication-state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authState": "SAML",
            "recaptchaRequired": false,
        })))
        .mount(&server)
        .await;
    // No /ws/account/login mock: reaching it would 404 the test.

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let error = login(&ctx, &login_args()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("SAML"));
    assert!(error.message.contains("NINJAONE_SESSION_KEY"));
}

#[tokio::test]
async fn a_recaptcha_tenant_is_told_what_is_missing_and_accepts_a_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ws/account/authentication-state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authState": "NATIVE",
            "recaptchaRequired": true,
        })))
        .mount(&server)
        .await;
    login_mock(mfa_required()).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .and(body_json(json!({
            "loginToken": LOGIN_TOKEN,
            "code": "381164",
            "recaptchaToken": "0cAFcWeA",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.mfa_code = Some("381164".to_owned());
    let error = login(&ctx, &args).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("recaptchaToken"));

    args.recaptcha_token = Some("0cAFcWeA".to_owned());
    let response = login(&ctx, &args).await.unwrap();
    assert!(response.content.contains("\"authenticated\": true"));
}

#[tokio::test]
async fn login_honours_a_server_alias_path_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/console/ws/account/authentication-state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authState": "NATIVE",
            "recaptchaRequired": false,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/console/ws/account/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let servers = json!({ "qa": { "url": server.uri(), "prefix": "/console" } }).to_string();
    let config = config(&[("NINJAONE_SERVERS", servers.as_str())]);
    let vendor = NinjaOneVendor::default();
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.server = Some("qa".to_owned());
    let response = login(&ctx, &args).await.unwrap();
    assert!(response.content.contains("\"authenticated\": true"));
}

#[tokio::test]
async fn login_requires_server_held_credentials() {
    let server = MockServer::start().await;
    let client = build_client().unwrap();
    let config = Config::from_map(HashMap::new());
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let error = login(&ctx, &login_args()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("NINJAONE_EMAIL"));
}

#[tokio::test]
async fn a_403_does_not_throw_away_a_working_session() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(session_success()).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/ws/webapp/sessionproperties"))
        .and(header("cookie", SESSION_COOKIE))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "resultCode": "FAILURE",
            "errorMessage": "Insufficient permissions.",
        })))
        // Both calls must still carry the session: a 403 is about permissions,
        // not about the key.
        .expect(2)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    login(&ctx, &login_args()).await.unwrap();
    for _ in 0..2 {
        let error = handle_read(&ctx, HttpMethod::Get, &read_args())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
        assert!(error.message.contains("Insufficient permissions."));
        assert!(!error.message.contains("ninjaone_login"));
    }
}

#[tokio::test]
async fn login_reports_when_an_access_token_still_outranks_the_session() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(session_success()).mount(&server).await;

    let client = build_client().unwrap();
    let config = config(&[("NINJAONE_ACCESS_TOKEN", "public-api-token")]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let response = login(&ctx, &login_args()).await.unwrap();
    assert!(response.content.contains("NINJAONE_ACCESS_TOKEN"));
}

// ---------------------------------------------------------------------------
// MFA code sourcing
//
// `echo` is the stand-in for a vault CLI: the point under test is that the
// server runs whatever prints a code and never learns what produced it. These
// use `echo` rather than a shell builtin peculiar to one platform so the same
// assertions hold on the Windows leg of the CI matrix.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_configured_command_supplies_the_code_without_a_tool_argument() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .and(body_json(
            json!({ "loginToken": LOGIN_TOKEN, "code": "381164" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[("NINJAONE_TOTP_COMMAND", "echo 381164")]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    // No mfaCode argument: the command is the only source.
    let response = login(&ctx, &login_args()).await.unwrap();
    assert!(response.content.contains("\"mfaUsed\": true"));
}

#[tokio::test]
async fn an_explicit_code_overrides_the_configured_command() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .and(body_json(
            json!({ "loginToken": LOGIN_TOKEN, "code": "999999" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[("NINJAONE_TOTP_COMMAND", "echo 381164")]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.mfa_code = Some("999999".to_owned());
    login(&ctx, &args).await.unwrap();
}

#[tokio::test]
async fn a_per_server_command_wins_over_the_global_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ws/account/authentication-state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authState": "NATIVE",
            "recaptchaRequired": false,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ws/account/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mfa_required()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .and(body_json(
            json!({ "loginToken": LOGIN_TOKEN, "code": "222222" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let servers = json!({
        "qa": { "url": server.uri(), "totpCommand": "echo 222222" },
    })
    .to_string();
    let config = config(&[
        ("NINJAONE_SERVERS", servers.as_str()),
        ("NINJAONE_TOTP_COMMAND", "echo 111111"),
    ]);
    let vendor = NinjaOneVendor::default();
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.server = Some("qa".to_owned());
    login(&ctx, &args).await.unwrap();
}

#[tokio::test]
async fn a_failing_command_reports_its_stderr() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;

    let client = build_client().unwrap();
    let config = config(&[(
        "NINJAONE_TOTP_COMMAND",
        "echo You are not logged in. 1>&2 && exit 1",
    )]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let error = login(&ctx, &login_args()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("NINJAONE_TOTP_COMMAND"));
    assert!(error.message.contains("not logged in"));
}

#[tokio::test]
async fn a_command_printing_junk_is_refused_without_echoing_it() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;

    let client = build_client().unwrap();
    let secret = "x".repeat(64);
    let command = format!("echo {secret}");
    let config = config(&[("NINJAONE_TOTP_COMMAND", command.as_str())]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let error = login(&ctx, &login_args()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    // Whatever the command printed came from a secret-bearing tool; the error
    // reports the length, never the content.
    assert!(!error.message.contains(&secret));
    assert!(error.message.contains("64 characters"));
}

#[tokio::test]
async fn a_configured_seed_generates_the_code_in_process() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;
    // The code depends on the wall clock, so the body cannot be pinned here;
    // it is asserted from the recorded request below, and the exact digits are
    // covered against the RFC vectors in ninjaone_totp_tests.rs.
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[(
        "NINJAONE_TOTP_SECRET",
        "otpauth://totp/NinjaOne:tech@example.com\
         ?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=NinjaOne",
    )]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    // No mfaCode argument and no external command: the seed is the only source.
    let response = login(&ctx, &login_args()).await.unwrap();
    assert!(response.content.contains("\"mfaUsed\": true"));

    let submitted = server
        .received_requests()
        .await
        .expect("recorded requests")
        .into_iter()
        .find(|request| request.url.path() == "/ws/account/mfa-login")
        .expect("an mfa-login request was sent");
    let body: serde_json::Value = serde_json::from_slice(&submitted.body).unwrap();
    let code = body["code"].as_str().expect("a code was submitted");
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));
}

#[tokio::test]
async fn a_vault_command_is_preferred_over_a_stored_seed() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/ws/account/mfa-login"))
        .and(body_json(
            json!({ "loginToken": LOGIN_TOKEN, "code": "424242" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let config = config(&[
        ("NINJAONE_TOTP_COMMAND", "echo 424242"),
        ("NINJAONE_TOTP_SECRET", "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"),
    ]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    login(&ctx, &login_args()).await.unwrap();
}

#[tokio::test]
async fn a_malformed_seed_fails_the_login_with_a_clear_message() {
    let server = MockServer::start().await;
    mount_auth_state(&server).await;
    login_mock(mfa_required()).mount(&server).await;

    let client = build_client().unwrap();
    let config = config(&[("NINJAONE_TOTP_SECRET", "not-a-valid-secret!!")]);
    let vendor = NinjaOneVendor::with_base_url(server.uri());
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let error = login(&ctx, &login_args()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("NINJAONE_TOTP_SECRET"));
}
