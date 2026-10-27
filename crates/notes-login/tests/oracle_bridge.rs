//! Ports of tools/test_exchange_account_code.py, the lifecycle half of
//! tools/test_notes_login.py (against the live `authenticate` route, which
//! shares load_bound_inputs/prepare and the claim-before-transport rule) and
//! tools/test_notes_auth_bridge.py.
mod common;
use common::*;
use notes_login::bridge::{authenticate, authorize_notes, exchange_notes_code, prepare, uat_request};
use notes_login::files::save_json;
use notes_login::json::{self, Value};
use notes_login::urls::parse_qsl_strict;
use sha2::{Digest, Sha256};
use std::path::Path;

fn inputs() -> (Value, Value, Value) {
    (
        obj(vec![
            ("schemaVersion", Value::Int(3)),
            ("purpose", Value::s("experimental-signin-preparation")),
            ("createdUnixSeconds", Value::Int(1000)),
            ("expiresUnixSeconds", Value::Int(1600)),
            ("codeVerifier", Value::Str("A".repeat(43))),
        ]),
        obj(vec![
            ("clientId", Value::s("syntheticClient")),
            ("devicePhysicalAddressText", Value::s("synthetic-device")),
            ("deviceOSVersion", Value::s("Linux")),
            ("deviceModelID", Value::s("LinuxResearch")),
        ]),
        obj(vec![
            ("schemaVersion", Value::Int(2)),
            ("hostSource", Value::s("decrypted-state-bound-samsung-callback")),
            ("authServerUrl", Value::s("us-auth2.samsungosp.com")),
            ("code", Value::s("synthetic+code/&=")),
        ]),
    )
}

fn form(data: &[u8]) -> Vec<(String, String)> {
    parse_qsl_strict(std::str::from_utf8(data).unwrap(), 100).unwrap()
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    pairs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect()
}

fn hexsha(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn write_inputs(directory: &Path, login_id: bool) {
    let (transaction, mut profile, mut decoded) = inputs();
    profile.set("originalClientId", Value::s("syntheticNotes"));
    for (name, value) in [("transaction", &transaction), ("profile", &profile), ("callback", &obj(vec![("synthetic", Value::Bool(true))]))] {
        save_json(&directory.join(format!("{name}.json")), value).unwrap();
    }
    let bindings: Vec<(&str, Value)> = ["transaction", "callback", "profile"]
        .iter()
        .map(|n| (match *n { "transaction" => "transactionSha256", "callback" => "callbackSha256", _ => "profileSha256" }, Value::Str(hexsha(&std::fs::read(directory.join(format!("{n}.json"))).unwrap()))))
        .collect();
    decoded.set("bindings", obj(bindings));
    if login_id {
        decoded.set("loginId", Value::s("synthetic+user@example.invalid"));
    }
    save_json(&directory.join("decoded-callback.json"), &decoded).unwrap();
}

// ---------------------------------------------------------------- exchange_account_code

#[test]
fn recovered_form_contract_and_no_entrypoint_auth() {
    let (t, p, d) = inputs();
    let request = prepare(&t, &p, &d, 1001.0).unwrap();
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://us-auth2.samsungosp.com/auth/oauth2/token");
    let values = form(request.data.as_ref().unwrap());
    assert_eq!(field(&values, "code"), vec!["synthetic+code/&="]);
    let mut keys: Vec<&str> = values.iter().map(|(k, _)| k.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["client_id", "code", "code_verifier", "grant_type", "physical_address_text"]);
    assert!(request.header("Authorization").is_none());
    assert!(request.header("X-osp-appid").is_none());
    assert_eq!(request.header("Content-Type"), Some("application/x-www-form-urlencoded"));
    assert_eq!(request.headers[0].0, "Content-type");
}

#[test]
fn host_redirect_injection_and_metadata_injection_rejected() {
    for host in [
        "us-auth2.samsungosp.com.evil.test",
        "https://us-auth2.samsungosp.com",
        "us-auth2.samsungosp.com:443",
        "us-auth2.samsungosp.com/x",
        "user@us-auth2.samsungosp.com",
        "account.samsung.com",
    ] {
        let (t, p, mut d) = inputs();
        d.set("authServerUrl", Value::s(host));
        assert!(prepare(&t, &p, &d, 1001.0).is_err(), "{host}");
    }
    let (t, mut p, d) = inputs();
    p.set("deviceModelID", Value::s("x\r\nAuthorization: bad"));
    assert!(prepare(&t, &p, &d, 1001.0).is_err());
    let (t, p, mut d) = inputs();
    d.set("authServerUrl", Value::s("auth2.samsungosp.com"));
    assert!(prepare(&t, &p, &d, 1001.0).is_ok());
}

#[test]
fn expiry_verifier_and_schema_are_enforced() {
    for now in [999.0, 1600.0] {
        let (t, p, d) = inputs();
        assert!(prepare(&t, &p, &d, now).is_err());
    }
    for (key, value) in [("schemaVersion", Value::Int(2)), ("codeVerifier", Value::s("short"))] {
        let (mut t, p, d) = inputs();
        t.set(key, value);
        assert!(prepare(&t, &p, &d, 1001.0).is_err());
    }
    let (mut t, p, d) = inputs();
    t.set("schemaVersion", Value::Float(3.0));
    assert!(prepare(&t, &p, &d, 1001.0).is_ok(), "3.0 == 3 like Python");
    let (mut t, p, d) = inputs();
    t.set("createdUnixSeconds", Value::Bool(true));
    assert!(prepare(&t, &p, &d, 1001.0).is_err(), "type(True) is not int");
}

// ---------------------------------------------------------------- lifecycle (claim before transport)

fn run_auth(directory: &Path, net: &FakeNet) -> Result<bool, notes_login::Error> {
    let rt = FakeRuntime::new(1001.0);
    authenticate(net, &rt, directory)
}

#[test]
fn success_saves_private_token_and_cannot_retry() {
    let dir = temp_dir("life-ok");
    write_inputs(&dir, true);
    let net = FakeNet::new(vec![json_response(200, r#"{"userauth_token":"synthetic-uat","userId":"synthetic-user","userauth_token_expires_in":3600}"#)]);
    assert!(run_auth(&dir, &net).unwrap());
    assert_eq!(net.count(), 1);
    assert_eq!(mode(&dir.join("uat-token.json")), 0o600);
    assert!(dir.join(".account-code-claim").exists());
    let err = run_auth(&dir, &net).unwrap_err();
    assert!(err.is_file_exists());
    assert_eq!(net.count(), 1);
}

#[test]
fn timeout_claim_survives_and_cannot_retry() {
    let dir = temp_dir("life-timeout");
    write_inputs(&dir, true);
    let net = FakeNet::new(vec![Canned::TransportError]);
    assert!(!run_auth(&dir, &net).unwrap());
    assert!(dir.join(".account-code-claim").exists());
    let receipt = json::loads(&std::fs::read(dir.join("uat-receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt.get("result").unwrap().as_str(), Some("transport_or_response_error"));
    assert!(receipt.get("httpStatus").is_none());
    assert!(run_auth(&dir, &net).unwrap_err().is_file_exists());
    assert_eq!(net.count(), 1);
}

#[test]
fn oversized_response_has_receipt_but_no_token_or_raw_body() {
    let dir = temp_dir("life-big");
    write_inputs(&dir, true);
    let net = FakeNet::new(vec![Canned::Response { status: 200, headers: vec![], body: vec![b'x'; 128 * 1024 + 2] }]);
    assert!(!run_auth(&dir, &net).unwrap());
    let receipt = json::loads(&std::fs::read(dir.join("uat-receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt.get("result").unwrap().as_str(), Some("response_limit_exceeded"));
    assert!(!dir.join("uat-token.json").exists());
    assert!(!dir.join("uat-response.body").exists());
}

#[test]
fn redirect_is_not_followed_and_not_resubmitted() {
    let dir = temp_dir("life-307");
    write_inputs(&dir, true);
    let net = FakeNet::new(vec![Canned::Response {
        status: 307,
        headers: vec![("Location".into(), "https://elsewhere.invalid/".into())],
        body: b"synthetic non-JSON redirect response".to_vec(),
    }]);
    assert!(!run_auth(&dir, &net).unwrap());
    assert_eq!(net.count(), 1);
    let receipt = json::loads(&std::fs::read(dir.join("uat-receipt.json")).unwrap()).unwrap();
    assert!(json::eq_int(receipt.get("httpStatus"), 307));
    assert!(!dir.join("uat-token.json").exists());
}

#[test]
fn each_replaced_input_is_rejected_before_claim_or_transport() {
    for name in ["transaction", "callback", "profile"] {
        let dir = temp_dir("life-replaced");
        write_inputs(&dir, true);
        let path = dir.join(format!("{name}.json"));
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(b' ');
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let net = FakeNet::new(vec![]);
        assert!(matches!(run_auth(&dir, &net), Err(notes_login::Error::Value(_))), "{name}");
        assert!(!dir.join(".account-code-claim").exists());
        assert_eq!(net.count(), 0);
    }
}

#[test]
fn missing_binding_and_old_schema_fail_before_claim() {
    for change in ["missing", "old"] {
        let dir = temp_dir("life-schema");
        write_inputs(&dir, true);
        let path = dir.join("decoded-callback.json");
        let mut decoded = json::loads(&std::fs::read(&path).unwrap()).unwrap();
        if change == "missing" {
            if let Value::Object(items) = &mut decoded {
                items.retain(|(k, _)| k != "bindings");
            }
        } else {
            decoded.set("schemaVersion", Value::Int(1));
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, json::dumps(&decoded, &json::DEFAULT)).unwrap();
        let net = FakeNet::new(vec![]);
        assert!(matches!(run_auth(&dir, &net), Err(notes_login::Error::Value(_))));
        assert!(!dir.join(".account-code-claim").exists());
        assert_eq!(net.count(), 0);
    }
}

#[test]
fn invalid_host_is_rejected_before_claim_or_transport() {
    let dir = temp_dir("life-host");
    write_inputs(&dir, true);
    let path = dir.join("decoded-callback.json");
    let mut decoded = json::loads(&std::fs::read(&path).unwrap()).unwrap();
    decoded.set("authServerUrl", Value::s("attacker.invalid"));
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, json::dumps(&decoded, &json::DEFAULT)).unwrap();
    let net = FakeNet::new(vec![]);
    assert!(run_auth(&dir, &net).is_err());
    assert!(!dir.join(".account-code-claim").exists());
    assert_eq!(net.count(), 0);
}

// ---------------------------------------------------------------- notes_auth_bridge

#[test]
fn browser_code_goes_to_authenticate_with_decrypted_identity() {
    let (t, mut p, mut d) = inputs();
    p.set("clientId", Value::s("syntheticAccount"));
    d.set("code", Value::s("synthetic-code"));
    d.set("loginId", Value::s("synthetic+user@example.invalid"));
    let request = uat_request(&t, &p, &d, 1001.0).unwrap();
    assert!(request.url.ends_with("/auth/oauth2/authenticate"));
    let body = form(request.data.as_ref().unwrap());
    assert_eq!(field(&body, "username"), vec!["synthetic+user@example.invalid"]);
    assert_eq!(field(&body, "serviceType"), vec!["M"]);
    assert_eq!(field(&body, "client_id"), vec!["syntheticAccount"]);
    assert!(request.header("Authorization").is_none());
    assert_eq!(body.len(), 7);
    d.set("loginId", Value::Null);
    assert!(uat_request(&t, &p, &d, 1001.0).is_err());
}

#[test]
fn app_code_uses_distinct_notes_pkce_then_matching_verifier_for_token() {
    let dir = temp_dir("bridge-pkce");
    write_inputs(&dir, true);
    let decoded = json::loads(&std::fs::read(dir.join("decoded-callback.json")).unwrap()).unwrap();
    save_json(
        &dir.join("uat-token.json"),
        &obj(vec![
            ("credentialKind", Value::s("SamsungUserAuth")),
            ("sourceBindings", decoded.get("bindings").unwrap().clone()),
            ("receivedUnixSeconds", Value::Int(1000)),
            ("expiresIn", Value::Int(1000)),
            ("response", obj(vec![("userauth_token", Value::s("synthetic-uat")), ("userId", Value::s("same-synthetic-user"))])),
        ]),
    )
    .unwrap();
    let net = FakeNet::new(vec![
        json_response(200, r#"{"code":"app-only-code","code_expires_in":300}"#),
        json_response(200, r#"{"access_token":"synthetic-notes-token","userId":"same-synthetic-user"}"#),
    ]);
    let rt = FakeRuntime::new(1001.0);
    assert!(authorize_notes(&net, &rt, &dir).unwrap());
    let app = json::loads(&std::fs::read(dir.join("notes-app-transaction.json")).unwrap()).unwrap();
    let verifier = app.get("codeVerifier").unwrap().as_str().unwrap().to_string();
    assert_ne!(verifier, "A".repeat(43));
    assert!(exchange_notes_code(&net, &rt, &dir).unwrap());
    let calls = net.requests();
    let first = form(calls[0].request.data.as_ref().unwrap());
    let second = form(calls[1].request.data.as_ref().unwrap());
    assert_eq!(field(&first, "client_id"), vec!["syntheticNotes"]);
    use base64::Engine;
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(field(&first, "code_challenge"), vec![expected.as_str()]);
    assert_eq!(field(&second, "code_verifier"), vec![verifier.as_str()]);
    assert_eq!(field(&second, "code"), vec!["app-only-code"]);
    assert!(calls.iter().all(|c| !String::from_utf8_lossy(c.request.data.as_ref().unwrap()).contains("synthetic%2Bcode")));
    assert!(calls[0].request.url.ends_with("/auth/oauth2/v2/authorize"));
    assert_eq!(calls[0].timeout.as_secs(), 20);
    let token = json::loads(&std::fs::read(dir.join("notes-account-token.json")).unwrap()).unwrap();
    assert_eq!(token.get("credentialKind").unwrap().as_str(), Some("NotesServiceOAuth"));
}

#[test]
fn additional_user_steps_are_never_accepted_automatically() {
    for flag in [r#""requireTncAccepted":"Y""#, r#""privacyAccepted":true"#, r#""requireChildAccountGraduated":1"#] {
        let dir = temp_dir("bridge-tnc");
        write_inputs(&dir, true);
        let decoded = json::loads(&std::fs::read(dir.join("decoded-callback.json")).unwrap()).unwrap();
        save_json(
            &dir.join("uat-token.json"),
            &obj(vec![
                ("credentialKind", Value::s("SamsungUserAuth")),
                ("sourceBindings", decoded.get("bindings").unwrap().clone()),
                ("receivedUnixSeconds", Value::Int(1000)),
                ("expiresIn", Value::Int(1000)),
                ("response", obj(vec![("userauth_token", Value::s("synthetic-uat")), ("userId", Value::s("u"))])),
            ]),
        )
        .unwrap();
        let net = FakeNet::new(vec![json_response(200, &format!(r#"{{"code":"c",{flag}}}"#))]);
        let rt = FakeRuntime::new(1001.0);
        assert!(!authorize_notes(&net, &rt, &dir).unwrap());
        assert!(!dir.join("notes-app-code.json").exists());
    }
    // 0 == False and "false"/"N" are accepted exactly like Python's `in`.
    let dir = temp_dir("bridge-tnc-ok");
    write_inputs(&dir, true);
    let decoded = json::loads(&std::fs::read(dir.join("decoded-callback.json")).unwrap()).unwrap();
    save_json(
        &dir.join("uat-token.json"),
        &obj(vec![
            ("credentialKind", Value::s("SamsungUserAuth")),
            ("sourceBindings", decoded.get("bindings").unwrap().clone()),
            ("receivedUnixSeconds", Value::Int(1000)),
            ("expiresIn", Value::Int(1000)),
            ("response", obj(vec![("userauth_token", Value::s("synthetic-uat")), ("userId", Value::s("u"))])),
        ]),
    )
    .unwrap();
    let net = FakeNet::new(vec![json_response(200, r#"{"code":"c","requireTncAccepted":0,"privacyAccepted":"false","requireChildAccountGraduated":"N"}"#)]);
    assert!(authorize_notes(&net, &FakeRuntime::new(1001.0), &dir).unwrap());
}
