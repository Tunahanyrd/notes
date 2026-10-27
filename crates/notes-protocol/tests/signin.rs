use base64::{engine::general_purpose::STANDARD, Engine};
use notes_protocol::{
    entrypoint::EntryPointMetadata,
    signin::{PreparedSignIn, SignInError, MAX_PROFILE_BYTES, PROFILE_FIELDS},
};
use openssl::{
    encrypt::Decrypter,
    pkey::PKey,
    rsa::Padding,
    symm::{decrypt, Cipher},
};

const PROFILE: &[u8] = include_bytes!("fixtures/signin/profile.json");
const METADATA: &[u8] = include_bytes!("fixtures/signin/entrypoint.json");

fn percent_decode(s: &str) -> Vec<u8> {
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'%' {
            decoded.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            decoded.push(if s.as_bytes()[i] == b'+' {
                b' '
            } else {
                s.as_bytes()[i]
            });
            i += 1;
        }
    }
    decoded
}

fn decrypt_prepared(prepared: &PreparedSignIn) -> serde_json::Value {
    let encoded = prepared.request_url().split("&svcParam=").nth(1).unwrap();
    let once = String::from_utf8(percent_decode(encoded)).unwrap();
    let envelope: serde_json::Value =
        serde_json::from_slice(&STANDARD.decode(percent_decode(&once)).unwrap()).unwrap();
    assert_eq!(envelope.as_object().unwrap().len(), 4);
    assert!(envelope.get("salt").is_none());
    let private = PKey::private_key_from_pem(include_bytes!(
        "fixtures/signin/synthetic-private.pkcs8.pem"
    ))
    .unwrap();
    let mut decryptor = Decrypter::new(&private).unwrap();
    decryptor.set_rsa_padding(Padding::PKCS1).unwrap();
    let wrapped = STANDARD
        .decode(envelope["svcEncKY"].as_str().unwrap())
        .unwrap();
    let mut key_text = vec![0; decryptor.decrypt_len(&wrapped).unwrap()];
    let len = decryptor.decrypt(&wrapped, &mut key_text).unwrap();
    key_text.truncate(len);
    assert_eq!(key_text.len(), 24); // Base64 text, not the raw 16-byte key.
    let key = STANDARD.decode(key_text).unwrap();
    assert_eq!(key.len(), 16);
    let iv_hex = envelope["svcEncIV"].as_str().unwrap();
    assert_eq!(iv_hex.len(), 32);
    let iv: Vec<u8> = (0..32)
        .step_by(2)
        .map(|i| u8::from_str_radix(&iv_hex[i..i + 2], 16).unwrap())
        .collect();
    let ciphertext = STANDARD
        .decode(envelope["svcEncParam"].as_str().unwrap())
        .unwrap();
    let plaintext = decrypt(Cipher::aes_128_cbc(), &key, Some(&iv), &ciphertext).unwrap();
    serde_json::from_slice(&plaintext).unwrap()
}

#[test]
fn complete_envelope_recovers_34_fields_and_fresh_bound_pkce_state() {
    let metadata = EntryPointMetadata::parse(METADATA).unwrap();
    let prepared = PreparedSignIn::prepare(&metadata, PROFILE, "tr-TR").unwrap();
    let plaintext = decrypt_prepared(&prepared);
    assert_eq!(plaintext.as_object().unwrap().len(), 34);
    for key in PROFILE_FIELDS {
        assert!(plaintext.get(key).is_some());
    }
    assert_eq!(plaintext["clientId"], "synthetic-client");
    assert_eq!(plaintext["redirect_uri"], "samsungnotes-sa");
    assert!(plaintext["birthDate"].is_null());
    assert_eq!(plaintext["state"], prepared.state());
    assert_eq!(
        plaintext["code_challenge"],
        prepared.verifier().s256_challenge()
    );
    assert_eq!(plaintext["code_challenge_method"], "S256");
    assert!(plaintext.get("code_verifier").is_none());
    assert_eq!(prepared.state().len(), 43);
    assert!(!prepared
        .request_url()
        .contains(prepared.verifier().expose_secret()));
    assert!(!prepared.request_url().contains(prepared.state()));
    assert_eq!(
        format!("{prepared:?}"),
        "PreparedSignIn { transaction: [REDACTED] }"
    );
    let second = PreparedSignIn::prepare(&metadata, PROFILE, "tr-TR").unwrap();
    assert_ne!(prepared.state(), second.state());
    assert_ne!(
        prepared.verifier().expose_secret(),
        second.verifier().expose_secret()
    );
    assert_ne!(prepared.request_url(), second.request_url());
}

#[test]
fn profile_rejects_ambiguous_or_unsupported_structure() {
    let metadata = EntryPointMetadata::parse(METADATA).unwrap();
    for input in [
        b"{}".to_vec(),
        b"[]".to_vec(),
        vec![b' '; MAX_PROFILE_BYTES + 1],
        String::from_utf8(PROFILE.to_vec())
            .unwrap()
            .replacen('{', "{\"clientId\":\"secret-duplicate\",", 1)
            .into_bytes(),
    ] {
        assert_eq!(
            PreparedSignIn::prepare(&metadata, &input, "tr-TR").unwrap_err(),
            SignInError::InvalidProfile
        );
    }
    for (field, value) in [
        ("unknown", serde_json::json!("secret")),
        ("clientId", serde_json::Value::Null),
        ("redirect_uri", serde_json::json!("")),
        ("state", serde_json::json!(123)),
        ("deviceName", serde_json::json!("bad\nvalue")),
    ] {
        let mut profile: serde_json::Value = serde_json::from_slice(PROFILE).unwrap();
        profile[field] = value;
        let error =
            PreparedSignIn::prepare(&metadata, &serde_json::to_vec(&profile).unwrap(), "tr-TR")
                .unwrap_err();
        assert_eq!(error, SignInError::InvalidProfile);
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn refuses_untrusted_destinations_invalid_keys_and_query_injection() {
    for destination in [
        "http://account.samsung.com/accounts/WINSDK/signInGate",
        "https://example.invalid/",
        "https://account.samsung.com/accounts/WINSDK/signInGate?next=bad",
    ] {
        let mut raw: serde_json::Value = serde_json::from_slice(METADATA).unwrap();
        raw["signInURI"] = destination.into();
        let metadata = EntryPointMetadata::parse(&serde_json::to_vec(&raw).unwrap()).unwrap();
        assert_eq!(
            PreparedSignIn::prepare(&metadata, PROFILE, "tr-TR").unwrap_err(),
            SignInError::UntrustedDestination
        );
    }
    let metadata = EntryPointMetadata::parse(METADATA).unwrap();
    assert_eq!(
        PreparedSignIn::prepare(&metadata, PROFILE, "tr-TR&x=y").unwrap_err(),
        SignInError::InvalidProfile
    );
    let mut raw: serde_json::Value = serde_json::from_slice(METADATA).unwrap();
    raw["pkiPublicKey"] = "YWJj".into();
    let bad_key = EntryPointMetadata::parse(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(
        PreparedSignIn::prepare(&bad_key, PROFILE, "tr-TR").unwrap_err(),
        SignInError::InvalidPublicKey
    );
}
