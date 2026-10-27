//! Anonymous WINSDK metadata contract observed on 2026-10-25.
//!
//! This module prepares a request but performs no I/O. Parsing metadata neither
//! authenticates an account nor validates a sign-in destination or RSA key.
//! Bounds are local resource policies, not claims about all Samsung versions.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use std::{error::Error, fmt};

pub const MAX_ENTRYPOINT_BYTES: usize = 128 * 1024;
pub const ENTRYPOINT_URL: &str = "https://account.samsung.com/accounts/WINSDK/getEntryPoint";

/// No cookie, account token, or device identity is attached. A future transport
/// must verify TLS, disable redirects, and bound response bytes and duration.
pub struct EntryPointRequest {
    url: String,
    app_id: String,
    authorization: String,
}

impl EntryPointRequest {
    /// `app_id` is supplied explicitly; no vendor identifier is embedded here.
    /// Acceptance of metadata does not establish permission for other APIs.
    pub fn prepare(app_id: &str, country: &str) -> Result<Self, EntryPointError> {
        if app_id.is_empty()
            || app_id.len() > 128
            || !app_id.bytes().all(|b| b.is_ascii_alphanumeric())
            || country.len() != 2
            || !country.bytes().all(|b| b.is_ascii_uppercase())
        {
            return Err(EntryPointError::InvalidRequest);
        }
        // Validated alphabets need no query/header escaping. The trailing colon
        // encodes the observed empty second input, not an account password.
        Ok(Self {
            url: format!("{ENTRYPOINT_URL}?countryCode={country}"),
            app_id: app_id.to_owned(),
            authorization: format!("Basic {}", STANDARD.encode(format!("{app_id}:"))),
        })
    }

    pub fn method(&self) -> &'static str {
        "GET"
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Explicit access for a transport; avoid logging these values.
    pub fn headers(&self) -> [(&'static str, &str); 4] {
        [
            ("x-osp-appId", &self.app_id),
            ("Authorization", &self.authorization),
            ("Accept", "application/json"),
            ("User-Agent", "SamsungNotesLinuxResearch/0.1"),
        ]
    }
}

impl fmt::Debug for EntryPointRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EntryPointRequest { method: GET, parameters: [REDACTED] }")
    }
}

// Unknown fields are deliberately ignored. Required known-field duplicates,
// missing fields, nulls and wrong types are rejected by serde's struct visitor.
#[derive(Deserialize)]
struct WireMetadata {
    #[serde(rename = "signInURI")]
    sign_in_uri: String,
    #[serde(rename = "chkDoNum")]
    check_number: String,
    #[serde(rename = "pkiPublicKey")]
    public_key: String,
    #[serde(rename = "pbeKySpcIters")]
    iterations: String,
    #[serde(rename = "acsChnl")]
    channel: Option<String>,
}

pub struct EntryPointMetadata {
    sign_in_uri: String,
    check_number: String,
    public_key_bytes: Vec<u8>,
    iterations: u32,
    channel: Option<String>,
}

impl EntryPointMetadata {
    /// Parse the observed subset of JSON; does not follow any returned URI.
    /// In particular, Base64 decoding does NOT validate DER, RSA, or key trust.
    pub fn parse(input: &[u8]) -> Result<Self, EntryPointError> {
        if input.len() > MAX_ENTRYPOINT_BYTES {
            return Err(EntryPointError::TooLarge);
        }
        let wire: WireMetadata =
            serde_json::from_slice(input).map_err(|_| EntryPointError::InvalidJson)?;
        if !bounded_text(&wire.sign_in_uri, 4096)
            || !bounded_text(&wire.check_number, 1024)
            || wire.channel.as_ref().is_some_and(|s| !bounded_text(s, 128))
            || wire.public_key.len() > 16 * 1024
            || wire.iterations.is_empty()
            || wire.iterations.len() > 7
            || !wire.iterations.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(EntryPointError::InvalidMetadata);
        }
        let iterations: u32 = wire
            .iterations
            .parse()
            .map_err(|_| EntryPointError::InvalidMetadata)?;
        if !(1..=1_000_000).contains(&iterations) {
            return Err(EntryPointError::InvalidMetadata);
        }
        let public_key_bytes = STANDARD
            .decode(wire.public_key)
            .map_err(|_| EntryPointError::InvalidMetadata)?;
        if public_key_bytes.is_empty() {
            return Err(EntryPointError::InvalidMetadata);
        }
        Ok(Self {
            sign_in_uri: wire.sign_in_uri,
            check_number: wire.check_number,
            public_key_bytes,
            iterations,
            channel: wire.channel,
        })
    }

    /// Opaque input, NOT approved for navigation or HTTP requests.
    pub fn unvalidated_sign_in_uri(&self) -> &str {
        &self.sign_in_uri
    }

    /// Preserve exactly; its freshness/nonce semantics are not established.
    pub fn check_number(&self) -> &str {
        &self.check_number
    }

    pub fn unvalidated_public_key_bytes(&self) -> &[u8] {
        &self.public_key_bytes
    }

    pub fn iterations(&self) -> u32 {
        self.iterations
    }

    pub fn channel_present(&self) -> bool {
        self.channel.is_some()
    }
}

fn bounded_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

impl fmt::Debug for EntryPointMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EntryPointMetadata { fields: [REDACTED] }")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EntryPointError {
    InvalidRequest,
    TooLarge,
    InvalidJson,
    InvalidMetadata,
}

impl fmt::Display for EntryPointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "invalid entrypoint request parameters",
            Self::TooLarge => "entrypoint response exceeds the input size limit",
            Self::InvalidJson => "invalid entrypoint JSON structure",
            Self::InvalidMetadata => "entrypoint metadata violates local validation rules",
        })
    }
}

impl Error for EntryPointError {}
