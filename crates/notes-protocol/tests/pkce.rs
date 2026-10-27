use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use notes_protocol::pkce::{CodeVerifier, PkceError, S256_METHOD};

#[test]
fn rfc_7636_appendix_b_s256_vector() {
    // Public test values from https://www.rfc-editor.org/rfc/rfc7636#appendix-B
    let verifier = CodeVerifier::from_secret("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
        .expect("public RFC verifier is valid");
    assert_eq!(S256_METHOD, "S256");
    assert_eq!(
        verifier.s256_challenge(),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn accepts_both_length_bounds_and_all_unreserved_ascii() {
    for value in [
        "a".repeat(43),
        "Z".repeat(128),
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~".to_owned(),
    ] {
        let verifier = CodeVerifier::from_secret(&value).expect("valid syntax");
        assert_eq!(verifier.expose_secret(), value);
    }
}

#[test]
fn rejects_lengths_outside_rfc_range() {
    for length in [0, 1, 42, 129, 4096] {
        assert_eq!(
            CodeVerifier::from_secret(&"a".repeat(length)).unwrap_err(),
            PkceError::InvalidVerifier
        );
    }
}

#[test]
fn rejects_reserved_control_and_non_ascii_characters_without_normalizing() {
    for character in [
        '+', '/', '=', '%', ' ', '\t', '\n', '\r', '\0', '\u{7f}', 'é', 'ı', '📝',
    ] {
        let value = format!("{}{}", "a".repeat(43), character);
        assert_eq!(
            CodeVerifier::from_secret(&value).unwrap_err(),
            PkceError::InvalidVerifier
        );
    }
}

#[test]
fn generated_verifiers_encode_32_bytes_without_padding() {
    // This checks shape and OS RNG availability, not statistical randomness.
    for _ in 0..8 {
        let verifier = CodeVerifier::generate().expect("OS random source available");
        let value = verifier.expose_secret();
        assert_eq!(value.len(), 43);
        assert!(value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_eq!(URL_SAFE_NO_PAD.decode(value).expect("base64url").len(), 32);
        assert_eq!(verifier.s256_challenge().len(), 43);
    }
}

#[test]
fn debug_and_errors_never_include_the_verifier() {
    let secret = "private-test-value-".repeat(3);
    let verifier = CodeVerifier::from_secret(&secret).expect("valid syntax");
    assert_eq!(format!("{verifier:?}"), "CodeVerifier([REDACTED])");
    assert_eq!(format!("{verifier:#?}"), "CodeVerifier([REDACTED])");
    let invalid = format!("{secret}!");
    let error = CodeVerifier::from_secret(&invalid).unwrap_err();
    assert_eq!(error.to_string(), "invalid PKCE verifier syntax");
    assert_eq!(format!("{error:?}"), "InvalidVerifier");
    assert_eq!(
        PkceError::RandomUnavailable.to_string(),
        "secure random source unavailable"
    );
}
