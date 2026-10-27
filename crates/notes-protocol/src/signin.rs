//! Experimental WINSDK2.25.109 sign-in preparation, with no network or login.
//!
//! Compatibility uses the recovered legacy CBC / RSA PKCS#1 v1.5 envelope.
//! It is not a new encryption design or a format for storing private notes.
//! JSON is compact and key-sorted; exact WinRT renderer bytes are unverified.

use crate::{entrypoint::EntryPointMetadata, pkce::CodeVerifier};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use openssl::{
    encrypt::Encrypter,
    hash::MessageDigest,
    pkcs5,
    pkey::{Id, PKey},
    rsa::Padding,
    symm::{encrypt, Cipher},
};
use serde::{
    de::{MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, fmt};
use zeroize::Zeroizing;

pub const MAX_PROFILE_BYTES: usize = 32 * 1024;
pub const SIGN_IN_URL: &str = "https://account.samsung.com/accounts/WINSDK/signInGate";
/// Field identity is from native PropertyDefinition -> getter -> field evidence.
pub const PROFILE_FIELDS: [&str; 34] = [
    "clientId",
    "originalClientId",
    "replaceableClientId",
    "state",
    "scope",
    "prompt",
    "deviceType",
    "deviceModelID",
    "deviceUniqueID",
    "devicePhysicalAddressText",
    "replaceableDevicePhysicalAddressText",
    "deviceOSVersion",
    "competitorDeviceYNFlag",
    "deviceModelCode",
    "deviceName",
    "deviceDisplayName",
    "deviceSerialNumberText",
    "svcIptLgnID",
    "birthDate",
    "firstName",
    "lastName",
    "redirect_uri",
    "countryCode",
    "responseEncryptionYNFlag",
    "responseEncryptionType",
    "code_challenge",
    "code_challenge_method",
    "replaceableClientConnectYN",
    "deviceInfo",
    "iosYNFlag",
    "iosType",
    "clientVer",
    "svcEncUat",
    "gActYN",
];

struct Profile(BTreeMap<String, Option<String>>);

impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ProfileVisitor;
        impl<'de> Visitor<'de> for ProfileVisitor {
            type Value = Profile;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a complete sign-in profile with string or null values")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Profile, M::Error> {
                let mut fields = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !PROFILE_FIELDS.contains(&key.as_str()) || fields.contains_key(&key) {
                        return Err(serde::de::Error::custom(
                            "unknown or duplicate profile field",
                        ));
                    }
                    let value = map.next_value::<Option<String>>()?;
                    if value
                        .as_ref()
                        .is_some_and(|v| v.len() > 4096 || v.chars().any(char::is_control))
                    {
                        return Err(serde::de::Error::custom("invalid profile field"));
                    }
                    fields.insert(key, value);
                }
                if fields.len() != PROFILE_FIELDS.len() {
                    return Err(serde::de::Error::custom("incomplete profile"));
                }
                Ok(Profile(fields))
            }
        }
        deserializer.deserialize_map(ProfileVisitor)
    }
}

/// A fresh transaction. Explicit accessors are intended for private storage and
/// a future browser flow. Nothing is submitted, and callbacks are not handled.
pub struct PreparedSignIn {
    url: Zeroizing<String>,
    state: Zeroizing<String>,
    verifier: CodeVerifier,
    callback: String,
    callback_iv: String,
}

impl PreparedSignIn {
    /// Profile input contains the 34 observed fields. Every call replaces state
    /// and PKCE fields with new CSPRNG-bound values. No real device data is read.
    pub fn prepare(
        metadata: &EntryPointMetadata,
        profile_json: &[u8],
        locale: &str,
    ) -> Result<Self, SignInError> {
        if metadata.unvalidated_sign_in_uri() != SIGN_IN_URL {
            return Err(SignInError::UntrustedDestination);
        }
        if locale.is_empty()
            || locale.len() > 32
            || !locale
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(SignInError::InvalidProfile);
        }
        if profile_json.len() > MAX_PROFILE_BYTES {
            return Err(SignInError::InvalidProfile);
        }
        let mut profile: Profile =
            serde_json::from_slice(profile_json).map_err(|_| SignInError::InvalidProfile)?;
        for name in ["clientId", "redirect_uri"] {
            if !profile
                .0
                .get(name)
                .and_then(Option::as_ref)
                .is_some_and(|s| !s.is_empty() && s.len() <= 256)
            {
                return Err(SignInError::InvalidProfile);
            }
        }
        let Some(Some(callback)) = profile.0.get("redirect_uri").cloned() else {
            return Err(SignInError::InvalidProfile);
        };
        let verifier = CodeVerifier::generate().map_err(|_| SignInError::RandomUnavailable)?;
        let mut entropy = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut entropy[..]).map_err(|_| SignInError::RandomUnavailable)?;
        let state = Zeroizing::new(URL_SAFE_NO_PAD.encode(&entropy[..]));
        profile.0.insert("state".into(), Some(state.to_string()));
        profile
            .0
            .insert("code_challenge".into(), Some(verifier.s256_challenge()));
        profile
            .0
            .insert("code_challenge_method".into(), Some("S256".into()));
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&profile.0).map_err(|_| SignInError::InvalidProfile)?,
        );
        let envelope = encrypt_envelope(metadata, &plaintext)?;
        let callback_iv = envelope.iv.clone();
        let encoded =
            STANDARD.encode(serde_json::to_vec(&envelope).map_err(|_| SignInError::CryptoFailed)?);
        let inner = uri_escape(encoded.as_bytes());
        let url = Zeroizing::new(format!(
            "{SIGN_IN_URL}?type=popup&locale={}&svcParam={}",
            form_escape(locale.as_bytes()),
            form_escape(inner.as_bytes())
        ));
        Ok(Self {
            url,
            state,
            verifier,
            callback,
            callback_iv,
        })
    }

    pub fn request_url(&self) -> &str {
        &self.url
    }
    pub fn state(&self) -> &str {
        &self.state
    }
    pub fn verifier(&self) -> &CodeVerifier {
        &self.verifier
    }
    pub fn expected_callback(&self) -> &str {
        &self.callback
    }
    /// Same AES IV is used by the native application to decrypt callback fields.
    pub fn callback_iv_hex(&self) -> &str {
        &self.callback_iv
    }
}

impl fmt::Debug for PreparedSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedSignIn { transaction: [REDACTED] }")
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    #[serde(rename = "chkDoNum")]
    check_number: &'a str,
    #[serde(rename = "svcEncParam")]
    content: String,
    #[serde(rename = "svcEncKY")]
    key: String,
    #[serde(rename = "svcEncIV")]
    iv: String,
}

fn encrypt_envelope<'a>(
    metadata: &'a EntryPointMetadata,
    plaintext: &[u8],
) -> Result<Envelope<'a>, SignInError> {
    let public_der = metadata.unvalidated_public_key_bytes();
    let public_key =
        PKey::public_key_from_der(public_der).map_err(|_| SignInError::InvalidPublicKey)?;
    // Bound work, reject non-RSA and trailing/noncanonical encodings. The 1024
    // lower bound is solely for the observed legacy server, not a recommendation.
    if public_key.id() != Id::RSA
        || !(1024..=4096).contains(&public_key.bits())
        || public_key
            .public_key_to_der()
            .map_err(|_| SignInError::InvalidPublicKey)?
            != public_der
    {
        return Err(SignInError::InvalidPublicKey);
    }
    let mut salt = Zeroizing::new([0u8; 16]);
    let mut iv = [0u8; 16];
    getrandom::fill(&mut salt[..]).map_err(|_| SignInError::RandomUnavailable)?;
    getrandom::fill(&mut iv).map_err(|_| SignInError::RandomUnavailable)?;
    let key = derive_key(metadata.check_number(), &salt, metadata.iterations())?;
    let wrapped_plaintext = Zeroizing::new(STANDARD.encode(&key[..]));
    let mut encrypter = Encrypter::new(&public_key).map_err(|_| SignInError::CryptoFailed)?;
    encrypter
        .set_rsa_padding(Padding::PKCS1)
        .map_err(|_| SignInError::CryptoFailed)?;
    let mut wrapped = vec![
        0u8;
        encrypter
            .encrypt_len(wrapped_plaintext.as_bytes())
            .map_err(|_| SignInError::CryptoFailed)?
    ];
    let len = encrypter
        .encrypt(wrapped_plaintext.as_bytes(), &mut wrapped)
        .map_err(|_| SignInError::CryptoFailed)?;
    wrapped.truncate(len);
    let content = encrypt(Cipher::aes_128_cbc(), &key[..], Some(&iv), plaintext)
        .map_err(|_| SignInError::CryptoFailed)?;
    Ok(Envelope {
        check_number: metadata.check_number(),
        content: STANDARD.encode(content),
        key: STANDARD.encode(wrapped),
        // WinRT hex case is not a live golden; lowercase is a documented local
        // choice. Both cases represent the same IV bytes.
        iv: iv.iter().map(|b| format!("{b:02x}")).collect(),
    })
}

fn derive_key(
    check_number: &str,
    salt: &[u8; 16],
    iterations: u32,
) -> Result<Zeroizing<[u8; 16]>, SignInError> {
    let password = Zeroizing::new(STANDARD.encode(Sha256::digest(check_number.as_bytes())));
    let mut key = Zeroizing::new([0u8; 16]);
    pkcs5::pbkdf2_hmac(
        password.as_bytes(),
        salt,
        iterations as usize,
        MessageDigest::sha256(),
        &mut key[..],
    )
    .map_err(|_| SignInError::CryptoFailed)?;
    Ok(key)
}

fn escape(input: &[u8], form: bool) -> String {
    let alphabet = if form {
        b"0123456789abcdef"
    } else {
        b"0123456789ABCDEF"
    };
    let mut output = String::new();
    for &b in input {
        let safe = b.is_ascii_alphanumeric()
            || if form {
                b"!()*-._".contains(&b)
            } else {
                b"-._~".contains(&b)
            };
        if safe {
            output.push(b as char);
        } else if form && b == b' ' {
            output.push('+');
        } else {
            output.push('%');
            output.push(alphabet[(b >> 4) as usize] as char);
            output.push(alphabet[(b & 15) as usize] as char);
        }
    }
    output
}

fn uri_escape(input: &[u8]) -> String {
    escape(input, false)
}
fn form_escape(input: &[u8]) -> String {
    escape(input, true)
}

#[derive(Debug, PartialEq, Eq)]
pub enum SignInError {
    InvalidProfile,
    UntrustedDestination,
    InvalidPublicKey,
    RandomUnavailable,
    CryptoFailed,
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidProfile => "invalid or unsupported sign-in profile",
            Self::UntrustedDestination => "unsupported sign-in destination",
            Self::InvalidPublicKey => "invalid or unsupported sign-in public key",
            Self::RandomUnavailable => "secure random source unavailable",
            Self::CryptoFailed => "sign-in encryption failed",
        })
    }
}
impl Error for SignInError {}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::encrypt::Decrypter;

    #[test]
    fn kdf_and_aes_match_independent_java_unicode_vector() {
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/signin/vector.json")).unwrap();
        let salt: [u8; 16] = std::array::from_fn(|i| i as u8);
        let iv: [u8; 16] = std::array::from_fn(|i| 0xf0 + i as u8);
        let key = derive_key(vector["inputs"]["chk_do_num"].as_str().unwrap(), &salt, 200).unwrap();
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, vector["derived_key_hex"].as_str().unwrap());
        let plaintext = vector["inputs"]["serialized_json"]
            .as_str()
            .unwrap()
            .as_bytes();
        let ciphertext = encrypt(Cipher::aes_128_cbc(), &key[..], Some(&iv), plaintext).unwrap();
        assert_eq!(
            STANDARD.encode(ciphertext),
            vector["aes_ciphertext_base64"].as_str().unwrap()
        );
    }

    #[test]
    fn openssl_unwraps_independent_java_rsa_fixture_as_base64_key_text() {
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/signin/vector.json")).unwrap();
        let private = PKey::private_key_from_pem(include_bytes!(
            "../tests/fixtures/signin/synthetic-private.pkcs8.pem"
        ))
        .unwrap();
        let mut decryptor = Decrypter::new(&private).unwrap();
        decryptor.set_rsa_padding(Padding::PKCS1).unwrap();
        let ciphertext = STANDARD
            .decode(vector["rsa_ciphertext_base64"].as_str().unwrap())
            .unwrap();
        let mut plaintext = vec![0; decryptor.decrypt_len(&ciphertext).unwrap()];
        let len = decryptor.decrypt(&ciphertext, &mut plaintext).unwrap();
        assert_eq!(
            &plaintext[..len],
            vector["rsa_plaintext_ascii"].as_str().unwrap().as_bytes()
        );
    }

    #[test]
    fn recovered_two_stage_encoding_preserves_literal_percent_escapes() {
        let inner = uri_escape(b"A+/=");
        assert_eq!(inner, "A%2B%2F%3D");
        assert_eq!(form_escape(inner.as_bytes()), "A%252B%252F%253D");
        assert_eq!(form_escape(b"a b~!()*-._"), "a+b%7e!()*-._");
        assert_eq!(uri_escape("é".as_bytes()), "%C3%A9");
    }
}
