use notes_protocol::entrypoint::{
    EntryPointError, EntryPointMetadata, EntryPointRequest, ENTRYPOINT_URL, MAX_ENTRYPOINT_BYTES,
};

const FIXTURE: &str = include_str!("fixtures/entrypoint.json");

#[test]
fn prepares_observed_basic_empty_password_recipe_without_account_credentials() {
    let request = EntryPointRequest::prepare("Example123", "TR").unwrap();
    assert_eq!(request.method(), "GET");
    assert_eq!(request.url(), format!("{ENTRYPOINT_URL}?countryCode=TR"));
    assert_eq!(
        request.headers(),
        [
            ("x-osp-appId", "Example123"),
            ("Authorization", "Basic RXhhbXBsZTEyMzo="),
            ("Accept", "application/json"),
            ("User-Agent", "SamsungNotesLinuxResearch/0.1"),
        ]
    );
    assert!(!format!("{request:?}").contains("Example123"));
    assert!(!format!("{request:?}").contains("RXhhbXBsZTEyMzo="));
}

#[test]
fn request_rejects_header_query_injection_and_out_of_policy_identifiers() {
    for app in [
        "",
        "a:b",
        "abc\r\nInjected: yes",
        "a&x=y",
        "é",
        &"a".repeat(129),
    ] {
        assert_eq!(
            EntryPointRequest::prepare(app, "TR").unwrap_err(),
            EntryPointError::InvalidRequest
        );
    }
    for country in ["", "tr", "TUR", "T&", "TR&x=y", "İ"] {
        assert_eq!(
            EntryPointRequest::prepare("Example123", country).unwrap_err(),
            EntryPointError::InvalidRequest
        );
    }
}

#[test]
fn parses_observed_string_fields_without_claiming_rsa_validation() {
    let metadata = EntryPointMetadata::parse(FIXTURE.as_bytes()).unwrap();
    assert_eq!(metadata.iterations(), 200);
    assert_eq!(metadata.check_number(), "synthetic-check-value");
    assert_eq!(
        metadata.unvalidated_sign_in_uri(),
        "https://example.invalid/signInGate"
    );
    assert_eq!(metadata.unvalidated_public_key_bytes(), b"fake-key-not-DER");
    assert!(metadata.channel_present());
    assert_eq!(
        format!("{metadata:?}"),
        "EntryPointMetadata { fields: [REDACTED] }"
    );
}

#[test]
fn rejects_missing_duplicate_null_and_wrong_type_required_fields() {
    for field in ["signInURI", "chkDoNum", "pkiPublicKey", "pbeKySpcIters"] {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert_eq!(
            EntryPointMetadata::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            EntryPointError::InvalidJson
        );
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!(200),
            serde_json::json!(true),
        ] {
            value[field] = invalid;
            assert_eq!(
                EntryPointMetadata::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
                EntryPointError::InvalidJson
            );
        }
        let duplicate = FIXTURE.replacen('{', &format!("{{\"{field}\":\"duplicate\","), 1);
        assert_eq!(
            EntryPointMetadata::parse(duplicate.as_bytes()).unwrap_err(),
            EntryPointError::InvalidJson
        );
    }
}

#[test]
fn bounds_kdf_work_and_rejects_non_decimal_iterations() {
    for iterations in [
        "",
        "0",
        "-1",
        "+200",
        " 200",
        "2e2",
        "200.0",
        "1000001",
        "999999999999",
    ] {
        let input = FIXTURE.replace("\"200\"", &format!("\"{iterations}\""));
        assert_eq!(
            EntryPointMetadata::parse(input.as_bytes()).unwrap_err(),
            EntryPointError::InvalidMetadata
        );
    }
}

#[test]
fn rejects_bad_base64_empty_fields_controls_and_oversized_data_without_echo() {
    for (field, invalid) in [
        ("pkiPublicKey", "%%%sensitive%%".to_owned()),
        ("pkiPublicKey", String::new()),
        ("pkiPublicKey", "A".repeat(16 * 1024 + 1)),
        ("chkDoNum", String::new()),
        ("chkDoNum", "sensitive\n".to_owned()),
        ("chkDoNum", "a".repeat(1025)),
        ("signInURI", String::new()),
        ("signInURI", "a".repeat(4097)),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        value[field] = invalid.into();
        let error = EntryPointMetadata::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(error, EntryPointError::InvalidMetadata);
        assert!(!error.to_string().contains("sensitive"));
    }
    assert_eq!(
        EntryPointMetadata::parse(&vec![b' '; MAX_ENTRYPOINT_BYTES + 1]).unwrap_err(),
        EntryPointError::TooLarge
    );
}

#[test]
fn optional_channel_and_unknown_metadata_do_not_imply_full_schema_support() {
    let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    value.as_object_mut().unwrap().remove("acsChnl");
    value["futureServerField"] = serde_json::json!({"nested": [1, 2, 3]});
    let metadata = EntryPointMetadata::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!metadata.channel_present());
}

#[test]
fn malformed_json_and_trailing_documents_fail_with_static_error() {
    for input in [
        b"[]".as_slice(),
        b"{sensitive}",
        b"\xff",
        format!("{FIXTURE}{{}}").as_bytes(),
    ] {
        let error = EntryPointMetadata::parse(input).unwrap_err();
        assert_eq!(error, EntryPointError::InvalidJson);
        assert_eq!(error.to_string(), "invalid entrypoint JSON structure");
    }
}
