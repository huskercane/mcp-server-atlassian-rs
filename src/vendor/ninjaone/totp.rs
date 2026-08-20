#![allow(clippy::doc_markdown)]

//! RFC 6238 TOTP, for operators who configure a seed rather than delegate to a
//! vault CLI.
//!
//! Accepts either the full `otpauth://totp/...?secret=...` URI a password
//! manager exports, or a bare base32 secret. The URI's `algorithm`, `digits`,
//! and `period` parameters are honoured; RFC defaults (SHA1 / 6 / 30) apply
//! when absent, which is what NinjaOne issues.
//!
//! **This puts a long-lived seed in the config file.** That is a deliberate
//! trade — it removes every external moving part, at the cost of the property
//! [`NINJAONE_TOTP_COMMAND`](super::mfa) has of never letting the seed leave
//! the vault. The command is tried first for that reason.
//!
//! Base32 is decoded here rather than pulled in as a dependency: it is an
//! encoding, not a cryptographic primitive. The HMAC and hashes are
//! `RustCrypto` implementations — nothing security-relevant is hand-rolled.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};

use crate::error::{McpError, auth_invalid, unexpected};

/// RFC 6238 allows 6–8 digits. Anything else is a malformed URI rather than a
/// exotic-but-valid configuration, and the truncation maths assumes this range.
const DIGIT_RANGE: std::ops::RangeInclusive<u32> = 6..=8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}

/// A parsed TOTP configuration.
///
/// `Debug` is redacted: this holds the raw seed, and it is reachable from the
/// vendor struct, so a derived `Debug` would print a permanent credential into
/// any diagnostic that formats it.
#[derive(Clone)]
pub struct TotpSpec {
    secret: Vec<u8>,
    algorithm: Algorithm,
    digits: u32,
    period: u64,
}

impl fmt::Debug for TotpSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TotpSpec")
            .field("algorithm", &self.algorithm)
            .field("digits", &self.digits)
            .field("period", &self.period)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl TotpSpec {
    /// Parse an `otpauth://totp/...` URI or a bare base32 secret.
    pub fn parse(spec: &str) -> Result<Self, McpError> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(invalid("the TOTP secret is empty"));
        }
        if spec.len() >= 8 && spec[..8].eq_ignore_ascii_case("otpauth:") {
            Self::parse_uri(spec)
        } else {
            Ok(Self {
                secret: decode_base32(spec)?,
                algorithm: Algorithm::Sha1,
                digits: 6,
                period: 30,
            })
        }
    }

    fn parse_uri(spec: &str) -> Result<Self, McpError> {
        let url = url::Url::parse(spec)
            .map_err(|error| invalid(format!("the otpauth URI could not be parsed: {error}")))?;
        if !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("totp"))
        {
            return Err(invalid(
                "only otpauth://totp/... URIs are supported (a counter-based otpauth://hotp/... \
                 URI cannot produce a time-based code)",
            ));
        }

        let mut secret = None;
        let mut algorithm = Algorithm::Sha1;
        let mut digits = 6;
        let mut period = 30;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "secret" => secret = Some(decode_base32(value.as_ref())?),
                "algorithm" => {
                    algorithm = match value.to_ascii_uppercase().as_str() {
                        "SHA1" => Algorithm::Sha1,
                        "SHA256" => Algorithm::Sha256,
                        "SHA512" => Algorithm::Sha512,
                        other => {
                            return Err(invalid(format!(
                                "unsupported otpauth algorithm `{other}`; expected SHA1, SHA256, or SHA512"
                            )));
                        }
                    }
                }
                "digits" => {
                    digits = value
                        .parse::<u32>()
                        .ok()
                        .filter(|d| DIGIT_RANGE.contains(d))
                        .ok_or_else(|| {
                            invalid(format!(
                                "otpauth `digits` must be between {} and {}, got `{value}`",
                                DIGIT_RANGE.start(),
                                DIGIT_RANGE.end()
                            ))
                        })?;
                }
                "period" => {
                    period = value.parse::<u64>().ok().filter(|p| *p > 0).ok_or_else(|| {
                        invalid(format!("otpauth `period` must be a positive number of seconds, got `{value}`"))
                    })?;
                }
                // issuer, image, and vendor-specific parameters are metadata.
                _ => {}
            }
        }

        Ok(Self {
            secret: secret.ok_or_else(|| invalid("the otpauth URI has no `secret` parameter"))?,
            algorithm,
            digits,
            period,
        })
    }

    /// Code for the current wall-clock time.
    ///
    /// Wall clock, not a monotonic instant: TOTP is defined against Unix time,
    /// and the verifying server uses its own clock. A machine whose clock has
    /// drifted more than the server's tolerance (typically one step either
    /// way) will produce rejected codes — that is inherent to TOTP, not
    /// something this can compensate for.
    pub fn current_code(&self) -> Result<String, McpError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                unexpected(
                    "the system clock is set before 1970, so no TOTP code can be derived",
                    None,
                )
            })?
            .as_secs();
        self.code_at(now)
    }

    /// Code for an explicit Unix timestamp. Separated from
    /// [`current_code`](Self::current_code) so the RFC 6238 vectors are
    /// directly testable.
    pub fn code_at(&self, unix_seconds: u64) -> Result<String, McpError> {
        let counter = unix_seconds / self.period;
        let tag = self.sign(counter)?;

        // RFC 4226 dynamic truncation: the low nibble of the last byte selects
        // a 4-byte window, whose high bit is masked off.
        let offset = (tag[tag.len() - 1] & 0x0f) as usize;
        let binary = u64::from(u32::from_be_bytes([
            tag[offset] & 0x7f,
            tag[offset + 1],
            tag[offset + 2],
            tag[offset + 3],
        ]));

        let modulus = 10_u64.pow(self.digits);
        let width = self.digits as usize;
        Ok(format!("{:0width$}", binary % modulus, width = width))
    }

    fn sign(&self, counter: u64) -> Result<Vec<u8>, McpError> {
        let message = counter.to_be_bytes();
        // A local macro rather than a generic function: expressing the bound
        // over `Hmac<D>` would mean taking a direct dependency on `digest`
        // purely for a trait name, for three four-line branches.
        macro_rules! hmac_of {
            ($hash:ty) => {{
                let mut mac = Hmac::<$hash>::new_from_slice(&self.secret)
                    .map_err(|_| invalid("the TOTP secret is empty after base32 decoding"))?;
                mac.update(&message);
                mac.finalize().into_bytes().to_vec()
            }};
        }
        Ok(match self.algorithm {
            Algorithm::Sha1 => hmac_of!(Sha1),
            Algorithm::Sha256 => hmac_of!(Sha256),
            Algorithm::Sha512 => hmac_of!(Sha512),
        })
    }
}

/// RFC 4648 base32, case-insensitive, tolerating the padding and the spacing
/// that authenticator apps show secrets with.
fn decode_base32(input: &str) -> Result<Vec<u8>, McpError> {
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(input.len() * 5 / 8);

    for character in input.chars() {
        if character == '=' || character.is_whitespace() || character == '-' {
            continue;
        }
        let value = match character.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            // Never echo the offending input: it is part of a seed.
            _ => return Err(invalid("the TOTP secret is not valid base32")),
        };
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).unwrap_or_default());
            buffer &= (1 << bits) - 1;
        }
    }

    if out.is_empty() {
        return Err(invalid("the TOTP secret decoded to no bytes"));
    }
    Ok(out)
}

/// Configuration errors surface as auth failures rather than internal errors:
/// the operator set a bad value and needs to fix it, exactly like a wrong
/// password.
fn invalid(detail: impl fmt::Display) -> McpError {
    auth_invalid(format!(
        "NINJAONE_TOTP_SECRET is not usable: {detail}. Expected an otpauth://totp/... URI or a \
         base32 secret."
    ))
}
