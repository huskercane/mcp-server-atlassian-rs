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

// ---------------------------------------------------------------------------
// Per-server accounts
//
// A NinjaOne account decides which division and role a session gets, so an
// operator holds a different account per environment rather than one account
// for all of them. These lock the merge rule: a `NINJAONE_SERVERS` entry that
// names its own `email` is a self-contained principal, and the top-level
// NINJAONE_* login keys — which unlock a different account — never leak into
// it.
// ---------------------------------------------------------------------------

/// The `qa4-1` account: a different person, on a different environment, from
/// the top-level `EMAIL` / `PASSWORD` every config in this file also sets.
const QA4_EMAIL: &str = "qa4-operator@example.net";
const QA4_PASSWORD: &str = "qa4-s3cret";
const QA5_EMAIL: &str = "qa5-operator@example.net";

/// `authentication-state` + `login` mounted for one specific principal. The
/// `body_json` matchers are the assertion: a login that used any other email
/// or password matches nothing and fails the exchange.
async fn mount_login_for(server: &MockServer, email: &str, password: &str) {
    Mock::given(method("POST"))
        .and(path("/ws/account/authentication-state"))
        .and(body_json(json!({ "email": email })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authState": "NATIVE",
            "recaptchaRequired": false,
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ws/account/login"))
        .and(body_json(json!({
            "email": email,
            "password": password,
            "staySignedIn": false,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_server_entry_logs_in_as_its_own_account() {
    let server = MockServer::start().await;
    mount_login_for(&server, QA4_EMAIL, QA4_PASSWORD).await;

    let client = build_client().unwrap();
    let servers = json!({
        "qa4-1": {
            "url": server.uri(),
            "email": QA4_EMAIL,
            "password": QA4_PASSWORD,
        },
    })
    .to_string();
    // The top-level NINJAONE_EMAIL / NINJAONE_PASSWORD stay set throughout:
    // the point is that the entry's own account wins over them.
    let config = config(&[("NINJAONE_SERVERS", servers.as_str())]);
    let vendor = NinjaOneVendor::default();
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.server = Some("qa4-1".to_owned());
    let response = login(&ctx, &args).await.unwrap();

    assert!(response.content.contains("\"authenticated\": true"));
    // Which account the session belongs to is reported: on NinjaOne that is
    // what determines the division and role the session can see.
    assert!(response.content.contains(QA4_EMAIL));
    assert!(!response.content.contains(EMAIL));
    assert!(!response.content.contains(QA4_PASSWORD));
}

#[tokio::test]
async fn a_server_account_never_borrows_the_top_level_password() {
    let server = MockServer::start().await;
    let client = build_client().unwrap();
    // `email` without `password`: the top-level NINJAONE_PASSWORD belongs to a
    // different account, so sending it here would fail the login and count a
    // bad attempt against a real person's account.
    let servers = json!({ "qa4-1": { "url": server.uri(), "email": QA4_EMAIL } }).to_string();
    let config = config(&[("NINJAONE_SERVERS", servers.as_str())]);
    let vendor = NinjaOneVendor::default();
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.server = Some("qa4-1".to_owned());
    let error = login(&ctx, &args).await.unwrap_err();

    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains(QA4_EMAIL));
    // The fix names the field the operator has to add, not the top-level key
    // they did not use.
    assert!(
        error
            .message
            .contains("NINJAONE_SERVERS[\"qa4-1\"].password"),
        "unhelpful message: {}",
        error.message
    );
    // Nothing was sent: the missing password is caught before the exchange.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_server_entry_uses_its_own_totp_seed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ws/account/authentication-state"))
        .and(body_json(json!({ "email": QA4_EMAIL })))
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
        .respond_with(ResponseTemplate::new(200).set_body_json(session_success()))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    let servers = json!({
        "qa4-1": {
            "url": server.uri(),
            "email": QA4_EMAIL,
            "password": QA4_PASSWORD,
            "totpSecret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        },
    })
    .to_string();
    // A top-level command for the *other* account is configured and must be
    // ignored: an MFA source belongs to the account it was enrolled on.
    let config = config(&[
        ("NINJAONE_SERVERS", servers.as_str()),
        ("NINJAONE_TOTP_COMMAND", "echo 111111"),
    ]);
    let vendor = NinjaOneVendor::default();
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.server = Some("qa4-1".to_owned());
    login(&ctx, &args).await.unwrap();

    let submitted = server
        .received_requests()
        .await
        .expect("recorded requests")
        .into_iter()
        .find(|request| request.url.path() == "/ws/account/mfa-login")
        .expect("an mfa-login request was sent");
    let body: serde_json::Value = serde_json::from_slice(&submitted.body).unwrap();
    let code = body["code"].as_str().expect("a code was submitted");
    assert_ne!(code, "111111", "the other account's TOTP command was used");
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));
}

#[tokio::test]
async fn a_session_belongs_to_the_account_that_minted_it() {
    let server = MockServer::start().await;
    mount_login_for(&server, QA4_EMAIL, QA4_PASSWORD).await;
    Mock::given(method("GET"))
        .and(path("/ws/webapp/sessionproperties"))
        .and(header("cookie", SESSION_COOKIE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attributes": {"id": 7}})))
        .expect(1)
        .mount(&server)
        .await;

    let client = build_client().unwrap();
    // Two environments, two accounts. Both happen to be served by the same
    // mock, so only the principal distinguishes their sessions.
    let servers = json!({
        "qa4-1": { "url": server.uri(), "email": QA4_EMAIL, "password": QA4_PASSWORD },
        "qa5": { "url": server.uri(), "email": QA5_EMAIL, "password": "qa5-s3cret" },
    })
    .to_string();
    let config = config(&[("NINJAONE_SERVERS", servers.as_str())]);
    let vendor = NinjaOneVendor::default();
    let ctx = NinjaOneContext::new(&client, &config, &vendor);

    let mut args = login_args();
    args.server = Some("qa4-1".to_owned());
    login(&ctx, &args).await.unwrap();

    let mut read = read_args();
    read.server = Some("qa4-1".to_owned());
    let response = handle_read(&ctx, HttpMethod::Get, &read).await.unwrap();
    assert!(response.content.contains('7'));
    if let Some(path) = response.raw_response_path {
        let _ = std::fs::remove_file(path);
    }

    // The other environment has its own account and no session of its own, so
    // it must ask for a login rather than replay qa4-1's key as a different
    // person.
    read.server = Some("qa5".to_owned());
    let error = handle_read(&ctx, HttpMethod::Get, &read).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthMissing);
    assert!(error.message.contains("ninjaone_login"));
}
