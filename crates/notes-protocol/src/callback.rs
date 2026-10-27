//! Offline Samsung Account browser-callback decryption.
//!
//! Static evidence: SAClient 1.26.6021.0, RVAs 0x37020e0 and 0x3702350.
//! This is the browser CBC format, separate from the downstream IPC ECB format.
//! The caller must verify the callback state and callback URI against the pending
//! attempt before calling this module, and retain that attempt's sign-in IV.
//! CBC padding and UTF-8 checks do not authenticate a callback or detect every
//! ciphertext/IV change; exact full-state validation remains the caller's duty.

use openssl::symm::{Cipher, Crypter, Mode};
use std::{error::Error, fmt};
use zeroize::Zeroizing;

pub const MAX_CIPHERTEXT_BYTES: usize = 16 * 1024;
pub const MAX_STATE_BYTES: usize = 4096;

/// Borrowed, URL-decoded callback field values, still encoded as hex.
/// Debug deliberately excludes the values, even though they are ciphertext.
pub struct EncryptedCallback<'a> {
    pub code: &'a str,
    pub auth_server_url: &'a str,
    pub api_server_url: &'a str,
    /// Optional encrypted native `retValue`, decoded as the login identifier.
    pub ret_value: Option<&'a str>,
}
impl fmt::Debug for EncryptedCallback<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptedCallback([REDACTED])")
    }
}

/// Plaintexts are zeroized on drop and are available only through explicit accessors.
/// No Clone or serialization implementation is provided.
pub struct DecryptedCallback {
    code: Zeroizing<Vec<u8>>,
    auth_server_url: Zeroizing<Vec<u8>>,
    api_server_url: Zeroizing<Vec<u8>>,
    login_id: Option<Zeroizing<Vec<u8>>>,
}
impl DecryptedCallback {
    pub fn code(&self) -> &str {
        validated_text(&self.code)
    }
    pub fn auth_server_url(&self) -> &str {
        validated_text(&self.auth_server_url)
    }
    pub fn api_server_url(&self) -> &str {
        validated_text(&self.api_server_url)
    }
    pub fn login_id(&self) -> Option<&str> {
        self.login_id.as_ref().map(|value| validated_text(value))
    }
}
impl fmt::Debug for DecryptedCallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DecryptedCallback([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackError {
    InvalidStateLength,
    StateMismatch,
    InvalidIv,
    InvalidCiphertext,
    DecryptionFailed,
    InvalidPlaintext,
    InvalidAuthHost,
}
impl fmt::Display for CallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidStateLength => "callback state length is invalid",
            Self::StateMismatch => "callback state does not match the pending transaction",
            Self::InvalidIv => "sign-in IV must be exactly 16 hex-encoded bytes",
            Self::InvalidCiphertext => "callback ciphertext has invalid hex or length",
            Self::DecryptionFailed => "callback decryption failed",
            Self::InvalidPlaintext => "callback plaintext is invalid",
            Self::InvalidAuthHost => "callback authentication host is not permitted",
        })
    }
}
impl Error for CallbackError {}

/// Decrypt the three required browser-callback fields and optional `retValue`.
/// A missing login identifier remains `None`; UAT callers must require it.
///
/// Key = the first 16 **UTF-8 bytes** of the state. IV = the IV from the same
/// sign-in envelope. Each ciphertext uses AES-128-CBC with PKCS7 padding.
/// Host validation is deliberately separate: call [`validate_auth_host`] before
/// sending any authorization code to the decrypted authentication host.
pub fn decrypt_callback(
    fields: EncryptedCallback<'_>,
    state: &str,
    iv_hex: &str,
) -> Result<DecryptedCallback, CallbackError> {
    if !(16..=MAX_STATE_BYTES).contains(&state.len()) {
        return Err(CallbackError::InvalidStateLength);
    }
    if iv_hex.len() != 32 {
        return Err(CallbackError::InvalidIv);
    }
    let iv = decode_hex(iv_hex, 16).map_err(|_| CallbackError::InvalidIv)?;
    let mut key = Zeroizing::new([0u8; 16]);
    key.copy_from_slice(&state.as_bytes()[..16]);
    let code = decrypt_field(fields.code, &key[..], &iv)?;
    let auth_server_url = decrypt_field(fields.auth_server_url, &key[..], &iv)?;
    let api_server_url = decrypt_field(fields.api_server_url, &key[..], &iv)?;
    let login_id = fields
        .ret_value
        .map(|value| decrypt_field(value, &key[..], &iv))
        .transpose()?;
    Ok(DecryptedCallback {
        code,
        auth_server_url,
        api_server_url,
        login_id,
    })
}

/// Verify the encrypted browser state against the entire expected state.
///
/// The field is AES-CBC hex encrypted under the first 16 UTF-8 bytes of the
/// expected state and the original sign-in IV. Comparing the full decrypted
/// state prevents accepting a different state sharing the same key prefix.
/// This must succeed before processing the other callback fields.
pub fn verify_callback_state(
    encrypted_state: &str,
    expected_state: &str,
    iv_hex: &str,
) -> Result<(), CallbackError> {
    if !(16..=MAX_STATE_BYTES).contains(&expected_state.len()) {
        return Err(CallbackError::InvalidStateLength);
    }
    if iv_hex.len() != 32 {
        return Err(CallbackError::InvalidIv);
    }
    let iv = decode_hex(iv_hex, 16).map_err(|_| CallbackError::InvalidIv)?;
    let mut key = Zeroizing::new([0u8; 16]);
    key.copy_from_slice(&expected_state.as_bytes()[..16]);
    let actual = decrypt_field(encrypted_state, &key[..], &iv)?;
    if actual.len() != expected_state.len()
        || !openssl::memcmp::eq(&actual, expected_state.as_bytes())
    {
        return Err(CallbackError::StateMismatch);
    }
    Ok(())
}

fn decode_hex(value: &str, max_bytes: usize) -> Result<Zeroizing<Vec<u8>>, CallbackError> {
    if value.is_empty() || value.len() % 2 != 0 || value.len() > max_bytes * 2 {
        return Err(CallbackError::InvalidCiphertext);
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut bytes = Zeroizing::new(Vec::with_capacity(value.len() / 2));
    for pair in value.as_bytes().chunks_exact(2) {
        let hi = nibble(pair[0]).ok_or(CallbackError::InvalidCiphertext)?;
        let lo = nibble(pair[1]).ok_or(CallbackError::InvalidCiphertext)?;
        bytes.push(hi << 4 | lo);
    }
    Ok(bytes)
}

fn decrypt_field(hex: &str, key: &[u8], iv: &[u8]) -> Result<Zeroizing<Vec<u8>>, CallbackError> {
    let ciphertext = decode_hex(hex, MAX_CIPHERTEXT_BYTES)?;
    if ciphertext.len() % 16 != 0 {
        return Err(CallbackError::InvalidCiphertext);
    }
    let cipher = Cipher::aes_128_cbc();
    let mut crypter = Crypter::new(cipher, Mode::Decrypt, key, Some(iv))
        .map_err(|_| CallbackError::DecryptionFailed)?;
    // Keep the buffer under Zeroizing before update/finalize: failed padding may
    // leave partially decrypted data in it. Do not use symm::decrypt's plain Vec.
    let mut plaintext = Zeroizing::new(vec![0u8; ciphertext.len() + cipher.block_size()]);
    let written = crypter
        .update(&ciphertext, &mut plaintext)
        .map_err(|_| CallbackError::DecryptionFailed)?;
    let final_bytes = crypter
        .finalize(&mut plaintext[written..])
        .map_err(|_| CallbackError::DecryptionFailed)?;
    let size = written + final_bytes;
    plaintext[size..].fill(0);
    plaintext.truncate(size);
    let text = std::str::from_utf8(&plaintext).map_err(|_| CallbackError::InvalidPlaintext)?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(CallbackError::InvalidPlaintext);
    }
    Ok(plaintext)
}

fn validated_text(bytes: &[u8]) -> &str {
    // Every field was checked as UTF-8 when the callback was decrypted; an
    // impossible violation yields "" (rejected downstream), never a panic.
    std::str::from_utf8(bytes).unwrap_or_default()
}

/// Narrow client policy, not an exhaustive Samsung host registry.
///
/// Accept `auth2.samsungosp.com` and a lowercase two-letter regional prefix such
/// as `us-auth2.samsungosp.com`. Reject schemes, ports, paths, trailing dots,
/// nested names, whitespace, Unicode and IP addresses. No normalization occurs.
/// The API host is deliberately not accepted as a substitute for the auth host.
pub fn validate_auth_host(host: &str) -> Result<(), CallbackError> {
    if host == "auth2.samsungosp.com" {
        return Ok(());
    }
    if let Some(region) = host.strip_suffix("-auth2.samsungosp.com") {
        if region.len() == 2 && region.bytes().all(|c| c.is_ascii_lowercase()) {
            return Ok(());
        }
    }
    Err(CallbackError::InvalidAuthHost)
}
