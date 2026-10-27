use notes_protocol::callback::{
    decrypt_callback, validate_auth_host, verify_callback_state, CallbackError, EncryptedCallback,
    MAX_CIPHERTEXT_BYTES, MAX_STATE_BYTES,
};

// Independent Java 21 Cipher AES/CBC/PKCS5Padding vectors, generated offline.
// Source: artifacts/analysis/windows-account-region-v7/CallbackFixture.java.
// All values are synthetic; these are not a captured Samsung login response.
const STATE: &str = "0123456789abcdef-synthetic-callback-state-123";
const IV: &str = "000102030405060708090a0b0c0d0e0f";
const CODE: &str = "e3b4057e6e8b2471e9960714ed86e26de1bf84512a452ee2edc0596f5981d832";
const AUTH: &str = "707ce501d7de5cc505ce32ab09571640366e2c9663d4b7c19ddbb4a15dbbb653";
const API: &str = "348a5a3456ad3b25c776ebcf95c87667b3f5d4b126b22f1dd84b42cdf0a0226e";

fn fields(code: &str) -> EncryptedCallback<'_> {
    EncryptedCallback {
        code,
        auth_server_url: AUTH,
        api_server_url: API,
        ret_value: None,
    }
}

#[test]
fn independent_java_vector_decrypts_all_fields() {
    let result = decrypt_callback(fields(CODE), STATE, IV).unwrap();
    assert_eq!(result.code(), "synthetic-code-for-tests-only");
    assert_eq!(result.auth_server_url(), "eu-auth2.samsungosp.com");
    assert_eq!(result.api_server_url(), "eu-api.samsungosp.com");
    assert_eq!(result.login_id(), None);
    validate_auth_host(result.auth_server_url()).unwrap();
}

#[test]
fn hex_case_is_not_significant() {
    let code = CODE.to_uppercase();
    let result = decrypt_callback(fields(&code), STATE, &IV.to_uppercase()).unwrap();
    assert_eq!(result.code(), "synthetic-code-for-tests-only");
}

#[test]
fn key_is_first_sixteen_utf8_bytes_even_when_boundary_splits_a_character() {
    let ciphertext = "4b99529a5e1476eb2ad2d8f948f59abc98c51c5b4a57befb8459e325b7d1747f";
    let input = EncryptedCallback {
        code: ciphertext,
        auth_server_url: ciphertext,
        api_server_url: ciphertext,
        ret_value: None,
    };
    let result = decrypt_callback(input, "0123456789abcdeé-rest-of-state", IV).unwrap();
    assert_eq!(result.code(), "synthetic-unicode-key");
}

#[test]
fn remaining_state_bytes_are_not_part_of_the_encryption_key() {
    let result = decrypt_callback(fields(CODE), "0123456789abcdef-other-suffix", IV).unwrap();
    assert_eq!(result.code(), "synthetic-code-for-tests-only");
    // Exact full-state validation belongs to the pending-login caller, before decrypt.
}

#[test]
fn malformed_and_oversized_fields_are_rejected() {
    for invalid in [
        "",
        "0",
        "gg",
        "é",
        "0x00",
        "00",
        "00112233445566778899aabbccddeezz",
    ] {
        assert_eq!(
            decrypt_callback(fields(invalid), STATE, IV).unwrap_err(),
            CallbackError::InvalidCiphertext
        );
    }
    let large = "00".repeat(MAX_CIPHERTEXT_BYTES + 1);
    assert_eq!(
        decrypt_callback(fields(&large), STATE, IV).unwrap_err(),
        CallbackError::InvalidCiphertext
    );
    let bad_auth = EncryptedCallback {
        code: CODE,
        auth_server_url: "xx",
        api_server_url: API,
        ret_value: None,
    };
    assert_eq!(
        decrypt_callback(bad_auth, STATE, IV).unwrap_err(),
        CallbackError::InvalidCiphertext
    );
    let bad_api = EncryptedCallback {
        code: CODE,
        auth_server_url: AUTH,
        api_server_url: "xx",
        ret_value: None,
    };
    assert_eq!(
        decrypt_callback(bad_api, STATE, IV).unwrap_err(),
        CallbackError::InvalidCiphertext
    );
}

#[test]
fn invalid_iv_and_state_lengths_are_rejected() {
    for state in [
        "".to_owned(),
        "a".repeat(15),
        "a".repeat(MAX_STATE_BYTES + 1),
    ] {
        assert_eq!(
            decrypt_callback(fields(CODE), &state, IV).unwrap_err(),
            CallbackError::InvalidStateLength
        );
    }
    for iv in [
        "",
        "00",
        "000102030405060708090a0b0c0d0e0g",
        "000102030405060708090a0b0c0d0e0f00",
    ] {
        assert_eq!(
            decrypt_callback(fields(CODE), STATE, iv).unwrap_err(),
            CallbackError::InvalidIv
        );
    }
}

#[test]
fn padding_is_verified() {
    // CBC: XOR ff into last byte of first block. This changes the final padded
    // plaintext byte from 04 to fb, guaranteeing invalid padding in block two.
    let invalid = "e3b4057e6e8b2471e9960714ed86e292e1bf84512a452ee2edc0596f5981d832";
    assert_eq!(
        decrypt_callback(fields(invalid), STATE, IV).unwrap_err(),
        CallbackError::DecryptionFailed
    );
    assert!(decrypt_callback(fields(CODE), "fedcba9876543210-synthetic", IV).is_err());
}

#[test]
fn empty_invalid_utf8_and_control_plaintexts_are_rejected() {
    for invalid in [
        "cada65093bdfede24f6df5db60ae4e18", // padded empty string
        "5c477f64db2b959a94897aed24c99bf3", // ff fe: invalid UTF-8
        "3189435c78c9db8733b6a5991db0c6df", // embedded LF
    ] {
        assert_eq!(
            decrypt_callback(fields(invalid), STATE, IV).unwrap_err(),
            CallbackError::InvalidPlaintext
        );
    }
}

#[test]
fn debug_output_does_not_disclose_ciphertext_or_plaintext() {
    assert_eq!(
        format!("{:?}", fields(CODE)),
        "EncryptedCallback([REDACTED])"
    );
    let result = decrypt_callback(fields(CODE), STATE, IV).unwrap();
    assert_eq!(format!("{result:?}"), "DecryptedCallback([REDACTED])");
}

#[test]
fn host_policy_accepts_only_exact_narrow_auth_names() {
    for valid in [
        "auth2.samsungosp.com",
        "us-auth2.samsungosp.com",
        "eu-auth2.samsungosp.com",
    ] {
        validate_auth_host(valid).unwrap();
    }
    for invalid in [
        "",
        "https://us-auth2.samsungosp.com",
        "us-auth2.samsungosp.com:443",
        "us-auth2.samsungosp.com/path",
        "us-auth2.samsungosp.com.",
        "us-auth2.samsungosp.com\n",
        " us-auth2.samsungosp.com",
        "US-auth2.samsungosp.com",
        "us-auth2.Samsungosp.com",
        "foo.us-auth2.samsungosp.com",
        "us-auth2.samsungosp.com.attacker.invalid",
        "attacker.invalid@us-auth2.samsungosp.com",
        "us-api.samsungosp.com",
        "localhost",
        "127.0.0.1",
        "u-auth2.samsungosp.com",
        "usa-auth2.samsungosp.com",
        "12-auth2.samsungosp.com",
        "é-auth2.samsungosp.com",
        "cn-auth2.samsungosp.com.cn",
    ] {
        assert_eq!(
            validate_auth_host(invalid),
            Err(CallbackError::InvalidAuthHost),
            "{invalid:?}"
        );
    }
}

#[test]
fn optional_ret_value_is_decrypted_as_private_login_id() {
    let mut input = fields(CODE);
    input.ret_value = Some("0d627626c3db6b0a56de6fe581e18fc7eec2eac59921b28e0e448116fbcf0899");
    let result = decrypt_callback(input, STATE, IV).unwrap();
    assert_eq!(result.login_id(), Some("synthetic-user@example.invalid"));
    assert_eq!(format!("{result:?}"), "DecryptedCallback([REDACTED])");
}

#[test]
fn present_ret_value_is_not_silently_ignored() {
    for (value, expected) in [
        ("", CallbackError::InvalidCiphertext),
        ("not-hex", CallbackError::InvalidCiphertext),
        (
            "cada65093bdfede24f6df5db60ae4e18",
            CallbackError::InvalidPlaintext,
        ),
        (
            "5c477f64db2b959a94897aed24c99bf3",
            CallbackError::InvalidPlaintext,
        ),
    ] {
        let mut input = fields(CODE);
        input.ret_value = Some(value);
        assert_eq!(decrypt_callback(input, STATE, IV).unwrap_err(), expected);
    }
}

#[test]
fn encrypted_state_echo_must_match_entire_expected_state() {
    // Independent Java AES/CBC fixture encrypting the complete expected state.
    let encrypted = "59b24fcf01976acae963981ba6573d330e7d55755b2cfbacd7577c52b2ff7248492f165b6c7fbda725971ca0ab438db4";
    verify_callback_state(encrypted, STATE, IV).unwrap();
    verify_callback_state(&encrypted.to_uppercase(), STATE, IV).unwrap();
    assert_eq!(
        verify_callback_state(
            encrypted,
            "0123456789abcdef-synthetic-callback-state-999",
            IV
        ),
        Err(CallbackError::StateMismatch)
    );
    assert_eq!(
        verify_callback_state(encrypted, "0123456789abcdef-shorter", IV),
        Err(CallbackError::StateMismatch)
    );
    // No plaintext-state fallback; malformed ciphertext also fails closed.
    assert_eq!(
        verify_callback_state(STATE, STATE, IV),
        Err(CallbackError::InvalidCiphertext)
    );
    assert_eq!(
        verify_callback_state("not-hex", STATE, IV),
        Err(CallbackError::InvalidCiphertext)
    );
    assert!(verify_callback_state(encrypted, STATE, "100102030405060708090a0b0c0d0e0f").is_err());
}
