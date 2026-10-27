//! Port of the registration parts of tools/test_notes_cloud.py plus
//! send_once / accept_registration / discovery checks.
mod common;
use common::*;
use notes_login::cloud::{accept_registration, endpoint, register_session, registration_request};
use notes_login::http::{send_once, HttpRequest};
use notes_login::json::{self, Value};
use notes_login::urls::urlsplit;

fn endpoints() -> Value {
    obj(vec![("play", Value::s("https://play.samsungcloud.com")), ("default", Value::s("https://api.samsungcloud.com"))])
}

fn account() -> Value {
    obj(vec![
        ("schemaVersion", Value::Int(1)),
        ("credentialKind", Value::s("NotesServiceOAuth")),
        ("clientId", Value::s("synthetic")),
        ("receivedUnixSeconds", Value::Int(1000)),
        (
            "response",
            obj(vec![
                ("access_token", Value::s("synthetic-sa-token")),
                ("userId", Value::s("synthetic-user")),
                ("access_token_expires_in", Value::Int(1000)),
            ]),
        ),
    ])
}

fn profile() -> Value {
    obj(vec![
        ("appId", Value::s("synthetic")),
        ("clientDeviceId", Value::s("synthetic-device")),
        (
            "registrationBody",
            obj(vec![
                ("app", obj(vec![("version", Value::s("1")), ("sdkVersion", Value::s("1"))])),
                ("device", obj(vec![("osType", Value::s("linux")), ("alias", Value::s("Linux research"))])),
            ]),
        ),
    ])
}

#[test]
fn transport_records_effective_method_for_implicit_get() {
    let dir = temp_dir("send-get");
    let net = FakeNet::new(vec![Canned::Response {
        status: 200,
        headers: vec![("Content-Type".into(), "application/xml".into())],
        body: b"<synthetic/>".to_vec(),
    }]);
    let request = HttpRequest::new("https://api.samsungcloud.com/synthetic", None, &[], None);
    let rt = FakeRuntime::new(1001.0);
    let (status, body) = send_once(&net, &rt, &dir, "test", &request, 2 * 1024 * 1024).unwrap();
    assert_eq!((status, body.as_deref()), (Some(200), Some(&b"<synthetic/>"[..])));
    let receipt = json::loads(&std::fs::read(dir.join("test-receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt.get("method").unwrap().as_str(), Some("GET"));
    assert_eq!(receipt.get("result").unwrap().as_str(), Some("response_saved"));
    assert_eq!(receipt.get("contentType").unwrap().as_str(), Some("application/xml"));
    assert_eq!(net.count(), 1);
    assert!(dir.join(".test-claim").exists());
    assert_eq!(std::fs::read_to_string(dir.join("test-response-meta.json")).unwrap(), "{\n  \"contentType\": \"application/xml\"\n}\n");
}

#[test]
fn registration_uses_notes_account_token_and_separate_identity_host() {
    let request = registration_request(&account(), &endpoints(), &profile(), 1001.0).unwrap();
    let parts = urlsplit(&request.url).unwrap();
    assert_eq!(parts.hostname().as_deref(), Some("play.samsungcloud.com"));
    assert_eq!(parts.path, "/identity/v2/user/register");
    assert_eq!(parts.query, "deviceCurrentTime=1001000");
    let body = json::loads(request.data.as_ref().unwrap()).unwrap();
    assert_eq!(body.get("device").unwrap().get("alias").unwrap().as_str(), Some("Linux research"));
    assert_eq!(request.header("X-sc-access-token"), Some("synthetic-sa-token"));
    assert!(request.header("Authorization").is_none());
    let mut wrong = account();
    wrong.set("credentialKind", Value::s("SamsungUserAuth"));
    assert!(registration_request(&wrong, &endpoints(), &profile(), 1001.0).is_err());
    let mut other = profile();
    other.set("appId", Value::s("other"));
    assert!(registration_request(&account(), &endpoints(), &other, 1001.0).is_err());
    assert!(registration_request(&account(), &endpoints(), &profile(), 2000.0).is_err(), "expired");
    assert_eq!(
        std::str::from_utf8(request.data.as_ref().unwrap()).unwrap(),
        r#"{"app":{"version":"1","sdkVersion":"1"},"device":{"osType":"linux","alias":"Linux research"}}"#
    );
}

#[test]
fn discovery_cannot_redirect_credentials_to_arbitrary_origin_or_path() {
    for value in [
        "http://play.samsungcloud.com",
        "https://play.samsungcloud.com.evil.test",
        "https://user@play.samsungcloud.com",
        "https://play.samsungcloud.com:443",
        "https://play.samsungcloud.com/other",
        "https://play.samsungcloud.com?x=y",
    ] {
        assert!(endpoint(&obj(vec![("play", Value::s(value))]), "play").is_err(), "{value}");
    }
    assert_eq!(endpoint(&obj(vec![("play", Value::s("https://play.samsungcloud.com/"))]), "play").unwrap(), "https://play.samsungcloud.com");
}

#[test]
fn accept_registration_requires_unique_user_token() {
    let rt = FakeRuntime::new(1001.0);
    for bad in [
        r#"{"tokens":[]}"#,
        r#"{"dvcId":"d","tokens":[]}"#,
        r#"{"dvcId":"d","tokens":[{"type":"user","accessToken":"a","expiresIn":1},{"type":"user","accessToken":"b","expiresIn":1}]}"#,
        r#"{"dvcId":"d","tokens":[{"type":"user","accessToken":"a","expiresIn":0}]}"#,
        r#"[1]"#,
    ] {
        let dir = temp_dir("accept-bad");
        assert!(accept_registration(&rt, &dir, bad.as_bytes()).is_err(), "{bad}");
        assert!(!dir.join("cloud-token.json").exists());
    }
    let dir = temp_dir("accept-ok");
    assert!(accept_registration(&rt, &dir, br#"{"dvcId":"d","tokens":[{"type":"device"},{"type":"user","accessToken":"a","expiresIn":3600.0}]}"#).unwrap());
    assert_eq!(
        std::fs::read_to_string(dir.join("cloud-token.json")).unwrap(),
        "{\n  \"schemaVersion\": 1,\n  \"credentialKind\": \"CloudUser\",\n  \"receivedUnixSeconds\": 1001,\n  \"expiresIn\": 3600.0,\n  \"dvcId\": \"d\",\n  \"accessToken\": \"a\"\n}\n"
    );
}

#[test]
fn register_session_discovers_once_and_registers_once() {
    let dir = temp_dir("register");
    let session = dir.join("session");
    std::fs::create_dir(&session).unwrap();
    let mut acct = account();
    acct.set("clientId", Value::s("tcl0p60gnz"));
    notes_login::files::save_json(&session.join("notes-account-token.json"), &acct).unwrap();
    let net = FakeNet::new(vec![
        json_response(200, r#"{"play":"https://play.samsungcloud.com","default":"https://api.samsungcloud.com","ünï":1.5e300}"#),
        json_response(200, r#"{"dvcId":"device-1","tokens":[{"type":"user","accessToken":"cloud-token","expiresIn":86400}]}"#),
    ]);
    let rt = FakeRuntime::new(1001.0);
    let login_profile = obj(vec![("devicePhysicalAddressText", Value::s("abc123"))]);
    assert!(register_session(&net, &rt, &session, &login_profile).unwrap());
    let calls = net.requests();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].request.url, "https://ers.samsungcloud.com/ers/v1/endpoints");
    assert_eq!(calls[0].request.method, "GET");
    assert_eq!(calls[0].timeout.as_secs(), 20);
    assert_eq!(calls[0].read_limit, 128 * 1024 + 1);
    assert_eq!(calls[0].request.headers[0], ("X-sc-app-id".into(), "tcl0p60gnz".into()));
    assert_eq!(calls[1].request.method, "POST");
    assert_eq!(calls[1].timeout.as_secs(), 25);
    assert!(session.join("cloud/cloud-token.json").is_file());
    assert_eq!(mode(&session.join("cloud")), 0o700);
    let endpoints_text = std::fs::read_to_string(session.join("app-endpoints.json")).unwrap();
    assert!(endpoints_text.contains("\"\\u00fcn\\u00ef\": 1.5e+300"));
    let profile = std::fs::read_to_string(session.join("app-cloud-profile.json")).unwrap();
    assert!(profile.contains("\"initialCursor\": \"eyJ2ZXJzaW9uIjowLCJlbmNyeXB0ZWRHdWlkIjoiIiwicmVxdWVzdFRpbWVTdGFtcCI6MCwibW9kaWZpZWRUaW1lIjowLCJhcHBWZXJzaW9uIjoiMCIsInNka1ZlcnNpb24iOiIwIiwicHVycG9zZSI6IklOSVRJQUwiLCJ0eXBlIjoiTk9URSJ9\""));
    assert!(profile.contains("\"x-sn-model-name\": \"TGludXhSZXNlYXJjaA==\""));
}

#[test]
fn discovery_failure_stops_before_registration() {
    for canned in [json_response(500, "{}"), json_response(204, "{}"), Canned::TransportError, json_response(302, "")] {
        let dir = temp_dir("discovery-fail");
        let net = FakeNet::new(vec![canned]);
        let rt = FakeRuntime::new(1001.0);
        let login_profile = obj(vec![("devicePhysicalAddressText", Value::s("abc123"))]);
        assert!(register_session(&net, &rt, &dir, &login_profile).is_err());
        assert_eq!(net.count(), 1);
        assert!(!dir.join("app-endpoints.json").exists());
    }
}
