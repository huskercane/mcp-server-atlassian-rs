#![allow(clippy::doc_markdown)]

//! RFC 6238 conformance and `otpauth://` parsing for the in-process TOTP
//! generator.
//!
//! The vectors are the ones published in RFC 6238 Appendix B. They are the
//! whole point of this file: a TOTP implementation that is subtly wrong still
//! produces plausible six-digit numbers, and would only be caught by a real
//! NinjaOne login refusing them.

use mcp_server_devtools::error::ErrorKind;
use mcp_server_devtools::vendor::ninjaone::totp::TotpSpec;

/// RFC 6238's SHA1 seed is the ASCII string "12345678901234567890"; this is
/// its base32 encoding.
const SHA1_SEED_B32: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
/// The SHA256 test seed is that string repeated out to 32 bytes
/// ("12345678901234567890123456789012"). The trailing `=` padding is part of
/// the base32 encoding of a 32-byte value, so this doubles as a check that
/// padding is tolerated.
const SHA256_SEED_B32: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA====";

fn spec(uri: &str) -> TotpSpec {
    TotpSpec::parse(uri).expect("spec parses")
}

#[test]
fn rfc6238_sha1_vectors() {
    // 8 digits, as the RFC's table uses, so the full truncation output is
    // compared rather than the low six digits of it.
    let totp = spec(&format!(
        "otpauth://totp/rfc?secret={SHA1_SEED_B32}&digits=8"
    ));
    for (time, expected) in [
        (59_u64, "94287082"),
        (1_111_111_109, "07081804"),
        (1_111_111_111, "14050471"),
        (1_234_567_890, "89005924"),
        (2_000_000_000, "69279037"),
        (20_000_000_000, "65353130"),
    ] {
        assert_eq!(totp.code_at(time).unwrap(), expected, "at t={time}");
    }
}

#[test]
fn rfc6238_sha256_vectors() {
    let totp = spec(&format!(
        "otpauth://totp/rfc?secret={SHA256_SEED_B32}&digits=8&algorithm=SHA256"
    ));
    for (time, expected) in [
        (59_u64, "46119246"),
        (1_111_111_109, "68084774"),
        (1_234_567_890, "91819424"),
    ] {
        assert_eq!(totp.code_at(time).unwrap(), expected, "at t={time}");
    }
}

/// The defaults NinjaOne (and every authenticator app) actually uses: SHA1,
/// six digits, a 30-second step. The expected values are the low six digits of
/// the SHA1 vectors above, which is what an 8-digit truncation reduces to.
#[test]
fn six_digit_defaults_match_the_sha1_vectors() {
    let totp = spec(&format!("otpauth://totp/Ninja?secret={SHA1_SEED_B32}"));
    assert_eq!(totp.code_at(59).unwrap(), "287082");
    assert_eq!(totp.code_at(1_234_567_890).unwrap(), "005924");
}

#[test]
fn a_bare_base32_secret_is_accepted() {
    let totp = TotpSpec::parse(SHA1_SEED_B32).unwrap();
    assert_eq!(totp.code_at(59).unwrap(), "287082");
}

/// Authenticator apps display seeds in lowercase, space-separated groups, and
/// exports may carry base32 padding. All three must round-trip to the same key.
#[test]
fn secret_formatting_variations_decode_identically() {
    let expected = TotpSpec::parse(SHA1_SEED_B32).unwrap().code_at(59).unwrap();
    for variant in [
        "gezdgnbvgy3tqojqgezdgnbvgy3tqojq",
        "GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ",
        "GEZD-GNBV-GY3T-QOJQ-GEZD-GNBV-GY3T-QOJQ",
        "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ====",
    ] {
        assert_eq!(
            TotpSpec::parse(variant).unwrap().code_at(59).unwrap(),
            expected,
            "variant {variant} decoded differently"
        );
    }
}

/// A realistic Bitwarden export value: percent-encoded issuer-prefixed label,
/// issuer parameter, explicit defaults.
#[test]
fn a_password_manager_uri_parses() {
    let totp = spec(&format!(
        "otpauth://totp/NinjaOne%3Arohit%40example.com?secret={SHA1_SEED_B32}\
         &issuer=NinjaOne&algorithm=SHA1&digits=6&period=30"
    ));
    assert_eq!(totp.code_at(59).unwrap(), "287082");
}

#[test]
fn a_custom_period_changes_the_step() {
    let totp = spec(&format!(
        "otpauth://totp/rfc?secret={SHA1_SEED_B32}&digits=8&period=60"
    ));
    // t=59 with a 60s step is still counter 0, which is t=0..29's code under
    // the default 30s step.
    assert_eq!(totp.code_at(59).unwrap(), totp.code_at(0).unwrap());
    assert_ne!(totp.code_at(59).unwrap(), totp.code_at(60).unwrap());
}

#[test]
fn the_current_code_has_the_configured_shape() {
    let code = spec(&format!("otpauth://totp/Ninja?secret={SHA1_SEED_B32}"))
        .current_code()
        .unwrap();
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn a_counter_based_uri_is_refused() {
    let error = TotpSpec::parse(&format!(
        "otpauth://hotp/rfc?secret={SHA1_SEED_B32}&counter=1"
    ))
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("otpauth://totp"));
}

#[test]
fn a_uri_without_a_secret_is_refused() {
    let error = TotpSpec::parse("otpauth://totp/rfc?issuer=NinjaOne").unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("no `secret` parameter"));
}

/// Base32 has no `1`, `8`, or `9`. A typo must fail loudly at login rather
/// than silently producing codes the server rejects.
#[test]
fn a_malformed_secret_is_refused_without_echoing_it() {
    let error = TotpSpec::parse("NOTBASE32!!1").unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("not valid base32"));
    assert!(!error.message.contains("NOTBASE32"));
}

#[test]
fn an_out_of_range_digit_count_is_refused() {
    let error = TotpSpec::parse(&format!(
        "otpauth://totp/rfc?secret={SHA1_SEED_B32}&digits=12"
    ))
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("digits"));
}

/// The seed is a permanent credential and the struct is reachable from the
/// vendor, so `Debug` must not print it.
#[test]
fn debug_output_redacts_the_seed() {
    let rendered = format!(
        "{:?}",
        spec(&format!("otpauth://totp/x?secret={SHA1_SEED_B32}"))
    );
    assert!(rendered.contains("redacted"));
    assert!(!rendered.contains("12345678901234567890"));
    assert!(!rendered.contains(SHA1_SEED_B32));
}
