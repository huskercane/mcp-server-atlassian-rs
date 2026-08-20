//! Registry of every secret the config can hold, and how it maps to a
//! keychain slot.
//!
//! One table drives three things that used to drift apart: runtime resolution
//! (`"keychain"` sentinel expansion), `creds migrate`, and the `creds set`
//! kind/vendor validation. Adding a vendor secret means adding a row here —
//! not touching three call sites.
//!
//! ## Principals
//!
//! A keychain entry is addressed by `(kind, vendor, principal)`, but most
//! vendor tokens have no account attached: there is no "who" for
//! `SLACK_TOKEN`. Those rows carry `principal_key: None` and fall back to
//! **the config key name as the principal**, which is self-describing in the
//! OS keychain UI and stays unique for vendors holding several tokens (e.g.
//! `NinjaOne`'s access token, session key, and session cookie).
//!
//! Rows that do have a natural account — an Atlassian email, a Zoom client id,
//! a WRDS or `NinjaOne` login — name it, so rotating the account moves the slot.

use super::keychain::SecretKind;
use crate::config::{
    VENDOR_BITBUCKET, VENDOR_CIRCLECI, VENDOR_CONFLUENCE, VENDOR_EDX, VENDOR_GRAFANA, VENDOR_JIRA,
    VENDOR_NEWRELIC, VENDOR_NINJAONE, VENDOR_POSTMAN, VENDOR_SLACK, VENDOR_SONARQUBE,
    VENDOR_SPLUNK, VENDOR_WRDS, VENDOR_ZOOM,
};

/// One secret-bearing config key, and the keychain slot it maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorSecret {
    /// Canonical vendor name; also the keychain service suffix.
    pub vendor: &'static str,
    /// The config key holding the secret (or the `"keychain"` sentinel).
    pub secret_key: &'static str,
    /// Config key holding the account this secret belongs to, when the vendor
    /// has one. `None` means the principal is [`Self::secret_key`] itself.
    pub principal_key: Option<&'static str>,
    pub kind: SecretKind,
}

impl VendorSecret {
    /// The keychain account for this secret: the configured principal where
    /// the vendor has one, otherwise the config key name.
    pub fn principal<'a>(&self, configured: Option<&'a str>) -> &'a str
    where
        'static: 'a,
    {
        match (self.principal_key, configured) {
            (Some(_), Some(principal)) if !principal.is_empty() => principal,
            _ => self.secret_key,
        }
    }
}

/// Shorthand for a row whose principal is the key name itself.
const fn token(vendor: &'static str, secret_key: &'static str) -> VendorSecret {
    VendorSecret {
        vendor,
        secret_key,
        principal_key: None,
        kind: SecretKind::Token,
    }
}

/// Shorthand for a row with a real account behind it.
const fn owned(
    vendor: &'static str,
    secret_key: &'static str,
    principal_key: &'static str,
    kind: SecretKind,
) -> VendorSecret {
    VendorSecret {
        vendor,
        secret_key,
        principal_key: Some(principal_key),
        kind,
    }
}

/// Every secret the server will expand from the keychain.
///
/// Deliberately absent: `NINJAONE_DB_ENVIRONMENTS`. It is a JSON document with
/// per-environment passwords inside, not a single secret, so it does not fit a
/// one-string slot — storing the whole blob would put hostnames and usernames
/// in the keychain too and make editing it a round-trip through `creds set`.
///
/// Absent for a different reason: the `password` and `totpSecret` fields of a
/// `NINJAONE_SERVERS` entry. Those *are* keychain-backed, each under its
/// entry's own `email`, but a row here names a config key and they are
/// addressed by a path into a nested document. The two places that need to
/// know handle them explicitly — [`crate::vendor::ninjaone`] resolves them at
/// login, and `cli::creds` migrates them.
pub const VENDOR_SECRETS: &[VendorSecret] = &[
    // Atlassian: one API token per product, plus Bitbucket's app password.
    owned(
        VENDOR_BITBUCKET,
        "ATLASSIAN_API_TOKEN",
        "ATLASSIAN_USER_EMAIL",
        SecretKind::ApiToken,
    ),
    owned(
        VENDOR_JIRA,
        "ATLASSIAN_API_TOKEN",
        "ATLASSIAN_USER_EMAIL",
        SecretKind::ApiToken,
    ),
    owned(
        VENDOR_CONFLUENCE,
        "ATLASSIAN_API_TOKEN",
        "ATLASSIAN_USER_EMAIL",
        SecretKind::ApiToken,
    ),
    owned(
        VENDOR_BITBUCKET,
        "ATLASSIAN_BITBUCKET_APP_PASSWORD",
        "ATLASSIAN_BITBUCKET_USERNAME",
        SecretKind::AppPassword,
    ),
    // Zoom's client secret belongs to the client id, not to a person.
    owned(
        VENDOR_ZOOM,
        "ZOOM_CLIENT_SECRET",
        "ZOOM_CLIENT_ID",
        SecretKind::Token,
    ),
    // Single-token vendors.
    token(VENDOR_SLACK, "SLACK_TOKEN"),
    token(VENDOR_CIRCLECI, "CIRCLECI_TOKEN"),
    token(VENDOR_POSTMAN, "POSTMAN_API_KEY"),
    token(VENDOR_NEWRELIC, "NEW_RELIC_API_KEY"),
    token(VENDOR_GRAFANA, "GRAFANA_TOKEN"),
    token(VENDOR_SONARQUBE, "SONARQUBE_TOKEN"),
    token(VENDOR_SPLUNK, "SPLUNK_TOKEN"),
    token(VENDOR_EDX, "EDX_ACCESS_TOKEN"),
    // WRDS logs in with a real account.
    owned(
        VENDOR_WRDS,
        "WRDS_PASSWORD",
        "WRDS_USERNAME",
        SecretKind::Password,
    ),
    // NinjaOne: console login (account-scoped) plus three carrier credentials
    // that have no account of their own.
    owned(
        VENDOR_NINJAONE,
        "NINJAONE_PASSWORD",
        "NINJAONE_EMAIL",
        SecretKind::Password,
    ),
    owned(
        VENDOR_NINJAONE,
        "NINJAONE_TOTP_SECRET",
        "NINJAONE_EMAIL",
        SecretKind::TotpSecret,
    ),
    token(VENDOR_NINJAONE, "NINJAONE_ACCESS_TOKEN"),
    token(VENDOR_NINJAONE, "NINJAONE_SESSION_KEY"),
    token(VENDOR_NINJAONE, "NINJAONE_SESSION_COOKIE"),
];

/// Look up the row for a `(vendor, secret_key)` pair.
pub fn lookup(vendor: &str, secret_key: &str) -> Option<&'static VendorSecret> {
    VENDOR_SECRETS
        .iter()
        .find(|secret| secret.vendor == vendor && secret.secret_key == secret_key)
}

/// Look up a row by config key alone. Used by the CLI so `--kind SLACK_TOKEN`
/// resolves; ambiguous only for `ATLASSIAN_API_TOKEN`, where every row shares
/// the same kind, so the first match is correct.
pub fn lookup_by_key(secret_key: &str) -> Option<&'static VendorSecret> {
    VENDOR_SECRETS
        .iter()
        .find(|secret| secret.secret_key == secret_key)
}

/// Every secret registered for a vendor, in declaration order.
pub fn for_vendor(vendor: &str) -> impl Iterator<Item = &'static VendorSecret> {
    VENDOR_SECRETS
        .iter()
        .filter(move |secret| secret.vendor == vendor)
}

/// Whether a `(kind, vendor)` pair addresses any real slot. Drives the CLI
/// guard that keeps operators from filing an entry nothing will ever read.
pub fn kind_supported_by(kind: SecretKind, vendor: &str) -> bool {
    VENDOR_SECRETS
        .iter()
        .any(|secret| secret.kind == kind && secret.vendor == vendor)
}

/// Canonical vendors that hold at least one registered secret, in declaration
/// order and without duplicates. Used for CLI help and validation.
pub fn vendors_with_secrets() -> Vec<&'static str> {
    let mut vendors: Vec<&'static str> = Vec::new();
    for secret in VENDOR_SECRETS {
        if !vendors.contains(&secret.vendor) {
            vendors.push(secret.vendor);
        }
    }
    vendors
}
