//! Offline RFC 7636 PKCE S256 primitives, independent of Samsung's servers.
//!
//! Each future authorization transaction needs its own verifier. This module
//! does not bind transactions, validate callbacks, make requests, or prove that
//! Samsung accepts this method or an independent client.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};
use std::{error::Error, fmt};
use zeroize::Zeroizing;

/// Explicit method identifier; there is no fallback to `plain`.
pub const S256_METHOD: &str = "S256";

/// A validated secret verifier. Debug is redacted; Display is not implemented.
///
/// Owned storage is zeroized on drop, but caller-owned copies, OS memory copies
/// and intermediate cryptographic state are outside this guarantee.
///
/// ```compile_fail
/// use notes_protocol::pkce::CodeVerifier;
/// let verifier = CodeVerifier::generate().unwrap();
/// println!("{}", verifier); // Explicit exposure is required.
/// ```
pub struct CodeVerifier(Zeroizing<String>);

impl CodeVerifier {
    /// Generate a verifier using 32 bytes from the operating system's CSPRNG.
    /// Random-source failures are returned without a weak fallback.
    pub fn generate() -> Result<Self, PkceError> {
        let mut entropy = Zeroizing::new([0_u8; 32]);
        getrandom::fill(&mut entropy[..]).map_err(|_| PkceError::RandomUnavailable)?;
        Ok(Self(Zeroizing::new(URL_SAFE_NO_PAD.encode(&entropy[..]))))
    }

    /// Copy a supplied verifier after validating RFC 7636 syntax exactly.
    ///
    /// Accepts 43..=128 ASCII unreserved characters, without trimming or Unicode
    /// normalization. Syntax validation cannot establish entropy: use
    /// [`Self::generate`] for a new transaction. The caller owns the input copy.
    pub fn from_secret(value: &str) -> Result<Self, PkceError> {
        if !(43..=128).contains(&value.len())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
        {
            return Err(PkceError::InvalidVerifier);
        }
        Ok(Self(Zeroizing::new(value.to_owned())))
    }

    /// Explicitly borrow the secret for a future token request. Never log it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Compute the public, unpadded base64url SHA-256 challenge.
    pub fn s256_challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.0.as_bytes()))
    }
}

impl fmt::Debug for CodeVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CodeVerifier([REDACTED])")
    }
}

/// Errors intentionally carry no verifier or underlying random-source data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkceError {
    InvalidVerifier,
    RandomUnavailable,
}

impl fmt::Display for PkceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidVerifier => "invalid PKCE verifier syntax",
            Self::RandomUnavailable => "secure random source unavailable",
        })
    }
}

impl Error for PkceError {}
