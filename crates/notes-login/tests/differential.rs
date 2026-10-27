//! Differential tests: the historical Python tools (frozen under
//! reference/python: tools/*.py, samsung_notes/session.py) and this crate run on identical synthetic inputs at the *same absolute
//! path* (sequentially), with the same fixed clock, the same deterministic
//! `secrets.token_bytes` sequence and the same canned HTTP responses. Every
//! produced file (name, mode, bytes), every request (URL, method, header
//! list, body, timeout) and every step result must match.
//!
//! Offline: Python's `urllib.request.build_opener` is replaced by a recorder
//! (reference/python/oracles/notes-login/harness.py); HOME/XDG_*/PATH point into a temp sandbox with fake
//! xdg-mime/xdg-open helpers. Opt-in since Python left the product
//! (2026-10-27): they run only with `NOTES_REFERENCE_ORACLE=1`, `python3` on
//! PATH and reference/python present; otherwise they are skipped.
// macOS skips the XDG desktop-handler cases (see notes_platform::url_scheme).
#![cfg_attr(target_os = "macos", allow(dead_code, unused_imports))]
mod common;
use common::*;
use notes_login::bridge::{authenticate, authorize_notes, exchange_notes_code};
use notes_login::callback::capture;
use notes_login::cloud::register_session;
use notes_login::files::{private_write, save_json};
use notes_login::handler::{install_handler, HandlerExec};
use notes_login::json::{self, Value};
use notes_login::urls::urlencode;
use notes_login::Error;
use std::os::unix::fs::PermissionsExt;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

const NOW: f64 = 1001.5;

fn python() -> Option<PathBuf> {
    if std::env::var_os("NOTES_REFERENCE_ORACLE").is_none_or(|v| v != "1")
        || !repo().join("reference/python/tools/notes_login.py").is_file() {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("python3")).find(|p| p.is_file())
}

fn canned_json(items: &[Canned]) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .map(|c| match c {
                Canned::TransportError => json!({"transportError": true}),
                Canned::Response { status, headers, body } => json!({
                    "status": status,
                    "headers": headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                    "bodyHex": body.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                }),
            })
            .collect(),
    )
}

fn requests_json(net: &FakeNet) -> serde_json::Value {
    serde_json::Value::Array(
        net.requests()
            .iter()
            .map(|r| {
                json!({
                    "url": r.request.url,
                    "method": r.request.method,
                    "headers": r.request.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                    "data": r.request.data.as_ref().map(|d| d.iter().map(|b| format!("{b:02x}")).collect::<String>()),
                    "timeout": r.timeout.as_secs(),
                })
            })
            .collect(),
    )
}

fn run_python(work: &Path, spec: serde_json::Value) -> serde_json::Value {
    let sb = Sandbox::at(work);
    let spec_path = work.join("..").join(format!("spec-{}.json", std::process::id()));
    let out_path = work.join("..").join(format!("out-{}.json", std::process::id()));
    let mut spec = spec;
    spec["now"] = json!(NOW);
    spec["out"] = json!(out_path.to_str().unwrap());
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let mut cmd = Command::new(python().unwrap());
    cmd.env_clear()
        .current_dir(repo().join("reference/python"))
        .env("PYTHONPATH", "tools:.")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("LANG", "C.UTF-8");
    for (k, v) in sb.env() {
        cmd.env(k, v);
    }
    let output = cmd.arg(repo().join("reference/python/oracles/notes-login/harness.py")).arg(&spec_path).output().unwrap();
    assert!(output.status.success(), "harness failed: {}", String::from_utf8_lossy(&output.stderr));
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(&out_path).unwrap()).unwrap();
    std::fs::remove_file(&spec_path).unwrap();
    std::fs::remove_file(&out_path).unwrap();
    result
}

fn class(e: &Error) -> &'static str {
    match e {
        Error::Value(_) => "ValueError",
        Error::Key(_) => "KeyError",
        Error::Type(_) => "TypeError",
        Error::Io(io) if io.kind() == std::io::ErrorKind::AlreadyExists => "FileExistsError",
        Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => "FileNotFoundError",
        Error::Io(_) => "OSError",
        Error::Callback(_) => "CallbackError",
        Error::Subprocess(_) => "CalledProcessError",
    }
}

/// Run `setup` + Python at `<temp>/<tag>/w` (spec strings may contain
/// `__WORK__`), move it to `py`; `setup` + Rust at the same path, move to
/// `rs`; compare both trees (contents of `masked` suffixes ignored).
fn compare<S, R>(tag: &str, setup: S, spec: serde_json::Value, rust: R, masked: &[&str]) -> (serde_json::Value, serde_json::Value)
where
    S: Fn(&Path),
    R: Fn(&Path) -> serde_json::Value,
{
    let root = temp_dir(tag);
    let work = root.join("w");
    let text = serde_json::to_string(&spec).unwrap().replace("__WORK__", work.to_str().unwrap());
    let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
    std::fs::create_dir(&work).unwrap();
    Sandbox::at(&work);
    setup(&work);
    let py = run_python(&work, spec);
    std::fs::rename(&work, root.join("py")).unwrap();
    std::fs::create_dir(&work).unwrap();
    Sandbox::at(&work);
    setup(&work);
    let rs = rust(&work);
    std::fs::rename(&work, root.join("rs")).unwrap();
    let mut a = tree(&root.join("py"));
    let mut b = tree(&root.join("rs"));
    for t in [&mut a, &mut b] {
        for entry in t.iter_mut() {
            if masked.iter().any(|m| entry.0.ends_with(m)) {
                entry.2 = b"<masked>".to_vec();
            }
        }
    }
    let names_a: Vec<_> = a.iter().map(|e| (&e.0, e.1)).collect();
    let names_b: Vec<_> = b.iter().map(|e| (&e.0, e.1)).collect();
    assert_eq!(names_a, names_b, "{tag}: file names/modes differ");
    for (x, y) in a.iter().zip(&b) {
        assert!(x.2 == y.2, "{tag}: {} differs:\n--- python\n{}\n--- rust\n{}", x.0, String::from_utf8_lossy(&x.2), String::from_utf8_lossy(&y.2));
    }
    assert_eq!(py["requests"], rs["requests"], "{tag}: requests differ");
    assert_eq!(py["randomCalls"], rs["randomCalls"], "{tag}: randomness consumption differs");
    let files = a.iter().filter(|e| !e.0.ends_with('/')).count();
    println!("{tag}: {files} files + {} requests identical", py["requests"].as_array().map_or(0, |r| r.len()));
    (py, rs)
}

fn rt_calls(rt: &FakeRuntime) -> u64 {
    rt.calls.load(std::sync::atomic::Ordering::SeqCst)
}

fn result_json<T: Into<serde_json::Value> + Clone>(r: &Result<T, Error>) -> serde_json::Value {
    match r {
        Ok(v) => json!(["ok", v.clone().into()]),
        Err(Error::Callback(m)) => json!(["callback", m]),
        Err(e) => json!(["error", class(e)]),
    }
}

macro_rules! need_python {
    () => {
        if python().is_none() {
            eprintln!("Python reference oracle not enabled (NOTES_REFERENCE_ORACLE=1, reference/python); differential test skipped");
            return;
        }
    };
}

// ---------------------------------------------------------------- shared inputs

fn login_profile(id: &str) -> Value {
    let mut p = json::loads(notes_login::login::DEFAULT_LOGIN_PROFILE).unwrap();
    p.set("devicePhysicalAddressText", Value::s(id));
    p.set("deviceUniqueID", Value::Str(format!("PCID:{id}")));
    p
}

fn hexsha(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// A decrypted session ready for the bridge: transaction (compact, like
/// notes-cli), callback, profile and decoded-callback with real bindings.
fn bridge_session(dir: &Path) {
    let session = dir.join("session");
    std::fs::create_dir(&session).unwrap();
    std::fs::write(
        session.join("transaction.json"),
        br#"{"callbackIvHex":"000102030405060708090a0b0c0d0e0f","codeVerifier":"Zx9_Zx9-Zx9.Zx9~Zx9_Zx9-Zx9.Zx9~Zx9_Zx9-Zx9","createdUnixSeconds":1000,"expectedCallback":"ms-app://s-1-15-2-1/","expiresUnixSeconds":1600,"loginPerformed":false,"purpose":"experimental-signin-preparation","schemaVersion":3,"state":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
    )
    .unwrap();
    std::fs::write(session.join("callback.json"), br#"{"schemaVersion": 1, "outcome": "authorization_code_captured"}"#).unwrap();
    save_json(&session.join("profile.json"), &login_profile("0123456789abcdef0123456789abcdef")).unwrap();
    let b = |n: &str| Value::Str(hexsha(&std::fs::read(session.join(n)).unwrap()));
    let decoded = format!(
        r#"{{"apiServerUrl":"eu-api.samsungosp.com","authServerUrl":"eu-auth2.samsungosp.com","bindings":{{"callbackSha256":{},"profileSha256":{},"transactionSha256":{}}},"code":"browser+code/=&x~","hostSource":"decrypted-state-bound-samsung-callback","loginId":"synthetic+user@example.invalid","schemaVersion":2,"tokenExchangePerformed":false}}"#,
        json::dumps(&b("callback.json"), &json::COMPACT),
        json::dumps(&b("profile.json"), &json::COMPACT),
        json::dumps(&b("transaction.json"), &json::COMPACT)
    );
    private_write(&session.join("decoded-callback.json"), decoded.as_bytes()).unwrap();
}

// ---------------------------------------------------------------- bridge

fn uat_ok() -> Canned {
    json_response(200, r#"{"userauth_token":"synthetic-uat","userId":"synthetic-user","userauth_token_expires_in":3600.0,"extra":[1,{"a":null,"ü":"\u20ac"}]}"#)
}
fn authorize_ok() -> Canned {
    json_response(200, r#"{"code":"synthetic-app-code","code_expires_in":300,"requireTncAccepted":"N","privacyAccepted":false}"#)
}
fn token_ok() -> Canned {
    json_response(200, r#"{"access_token":"synthetic-notes-token","userId":"synthetic-user","access_token_expires_in":3600,"refresh_token":"r"}"#)
}

type Step = fn(&dyn notes_login::Network, &dyn notes_login::Runtime, &Path) -> Result<bool, Error>;

#[test]
fn bridge_steps_match_python() {
    need_python!();
    let cases: Vec<(&str, Vec<Canned>)> = vec![
        ("bridge-success", vec![uat_ok(), authorize_ok(), token_ok()]),
        ("bridge-uat-401", vec![json_response(401, r#"{"error":"invalid_grant","error_description":"x"}"#)]),
        ("bridge-uat-list", vec![json_response(200, "[1, 2]")]),
        ("bridge-uat-notjson", vec![Canned::Response { status: 200, headers: vec![], body: b"<html>".to_vec() }]),
        ("bridge-uat-big", vec![Canned::Response { status: 200, headers: vec![], body: vec![b' '; 128 * 1024 + 5] }]),
        ("bridge-uat-transport", vec![Canned::TransportError]),
        ("bridge-uat-307", vec![Canned::Response { status: 307, headers: vec![("Location".into(), "https://x.invalid/".into())], body: vec![] }]),
        ("bridge-uat-bad-expiry", vec![json_response(200, r#"{"userauth_token":"t","userId":"u","userauth_token_expires_in":true}"#)]),
        ("bridge-tnc", vec![uat_ok(), json_response(200, r#"{"code":"c","code_expires_in":300,"requireTncAccepted":"Y"}"#)]),
        ("bridge-nocode", vec![uat_ok(), json_response(200, r#"{"code_expires_in":300}"#)]),
        ("bridge-grant-life", vec![uat_ok(), json_response(200, r#"{"code":"c","code_expires_in":601}"#)]),
        ("bridge-mismatch", vec![uat_ok(), authorize_ok(), json_response(200, r#"{"access_token":"t","userId":"other"}"#)]),
        ("bridge-token-500", vec![uat_ok(), authorize_ok(), json_response(500, r#"{"error":"server"}"#)]),
    ];
    for (tag, responses) in cases {
        let spec = json!({"mode": "bridge", "responses": canned_json(&responses),
            "args": {"steps": ["authenticate", "authorize-notes", "exchange-notes-code"], "dir": "__WORK__/session"}});
        let (py, rs) = compare(tag, bridge_session, spec, |work| {
            let net = FakeNet::new(responses.clone());
            let rt = FakeRuntime::new(NOW);
            let mut out = Vec::new();
            let steps: [Step; 3] = [authenticate, authorize_notes, exchange_notes_code];
            for step in steps {
                let r = step(&net, &rt, &work.join("session"));
                out.push(result_json(&r));
                if r.is_err() {
                    break;
                }
            }
            json!({"steps": out, "requests": requests_json(&net), "randomCalls": rt_calls(&rt)})
        }, &[]);
        assert_eq!(py["steps"], rs["steps"], "{tag}: step results differ");
        println!("  {tag}: steps {}", py["steps"]);
        if tag == "bridge-success" {
            assert_eq!(py["steps"], json!([["ok", true], ["ok", true], ["ok", true]]));
            assert_eq!(py["requests"].as_array().unwrap().len(), 3);
        } else {
            assert!(!py["requests"].as_array().unwrap().is_empty(), "{tag}: scenario must reach the network");
        }
    }
}

// ---------------------------------------------------------------- registration

fn register_session_setup(dir: &Path) {
    let session = dir.join("session");
    std::fs::create_dir(&session).unwrap();
    std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        session.join("notes-account-token.json"),
        "{\n  \"schemaVersion\": 1,\n  \"credentialKind\": \"NotesServiceOAuth\",\n  \"clientId\": \"tcl0p60gnz\",\n  \"receivedUnixSeconds\": 1000,\n  \"authServerUrl\": \"eu-auth2.samsungosp.com\",\n  \"response\": {\n    \"access_token\": \"synthetic-notes-token\",\n    \"userId\": \"synthetic-user\",\n    \"access_token_expires_in\": 3600\n  },\n  \"cloudEligibilityVerified\": false\n}\n",
    )
    .unwrap();
}

fn discovery_ok() -> Canned {
    json_response(200, r#"{"play":"https://play.samsungcloud.com","default":"https://api.samsungcloud.com","nested":{"ü":[1.0,2.5e-7,1e22,12345678901234567890,-0.0,"😀"]},"dup":1,"dup":2}"#)
}

fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[test]
fn registration_matches_python() {
    need_python!();
    let reg_ok = r#"{"dvcId":"synthetic-dvc","tokens":[{"type":"device","accessToken":"d"},{"type":"user","accessToken":"synthetic-cloud","expiresIn":86400.0}]}"#;
    let cases: Vec<(&str, Vec<Canned>, &str)> = vec![
        ("reg-success", vec![discovery_ok(), json_response(200, reg_ok)], "ok"),
        (
            "reg-gzip",
            vec![
                discovery_ok(),
                Canned::Response {
                    status: 200,
                    headers: vec![("Content-Type".into(), "application/json; charset=UTF-8".into()), ("Content-Encoding".into(), "GZIP".into())],
                    body: gzip(reg_ok.as_bytes()),
                },
            ],
            "ok",
        ),
        ("reg-500", vec![discovery_ok(), json_response(500, r#"{"rcode":1}"#)], "error"),
        ("reg-br", vec![discovery_ok(), Canned::Response { status: 200, headers: vec![("Content-Encoding".into(), "br".into())], body: b"x".to_vec() }], "error"),
        ("reg-no-user", vec![discovery_ok(), json_response(200, r#"{"dvcId":"d","tokens":[{"type":"device"}]}"#)], "error"),
        ("reg-transport", vec![discovery_ok(), Canned::TransportError], "error"),
        ("reg-bad-endpoint", vec![json_response(200, r#"{"play":"https://play.samsungcloud.com.evil.test"}"#)], "error"),
        ("reg-discovery-404", vec![json_response(404, "{}")], "error"),
        ("reg-discovery-list", vec![json_response(200, "[]")], "error"),
    ];
    let profile = json!({"devicePhysicalAddressText": "0123456789abcdef0123456789abcdef", "other": 1});
    for (tag, responses, expected) in cases {
        let spec = json!({"mode": "register", "responses": canned_json(&responses), "args": {"dir": "__WORK__/session", "profile": profile}});
        let (py, rs) = compare(tag, register_session_setup, spec, |work| {
            let net = FakeNet::new(responses.clone());
            let rt = FakeRuntime::new(NOW);
            let login_profile = json::loads(profile.to_string().as_bytes()).unwrap();
            let outcome = match register_session(&net, &rt, &work.join("session"), &login_profile) {
                Ok(true) => "ok",
                _ => "error",
            };
            json!({"outcome": outcome, "requests": requests_json(&net), "randomCalls": rt_calls(&rt)})
        }, &[]);
        assert_eq!(py["outcome"], rs["outcome"], "{tag}");
        assert_eq!(py["outcome"], expected, "{tag}");
    }
}

// ---------------------------------------------------------------- capture (v2 literal state)

#[test]
fn capture_matches_python() {
    need_python!();
    let state = "A".repeat(43);
    let base = "ms-app://s-1-15-2-123/";
    let ok = format!("{base}?{}", urlencode(&[("code", "SYNTHETIC-only-code".into()), ("state", state.clone())]));
    let uris = vec![
        ok.replace("s-1-15-2-123", "s-1-15-2-999"),
        ok.replace("ms-app:", "https:"),
        ok.replace("s-1-", "user@s-1-"),
        format!("{ok}#code=other"),
        format!("{ok}&state={state}"),
        ok.replace("/?", "/wrong?"),
        format!("{ok}\r"),
        format!("{ok}&other=%ff"),
        format!("{ok}&other"),
        format!("{ok}&&x=1"),
        ok.replace("s-1-15-2-123", "S-1-15-2-123:"),
        ok.replace("s-1-15-2-123", "s-1-15-2-123:99999"),
        ok.replace("s-1-15-2-123", "s-1-15-2-123:x"),
        ok.replace("s-1-15-2-123", "[::1]"),
        ok.replace("s-1-15-2-123", "[v1.x]"),
        ok.replace("s-1-15-2-123", "s-1-15-2-12\u{ff13}"),
        ok.replace("s-1-15-2-123", "s-1-15-2-123\u{ff1a}"),
        format!("{base}?{}", (0..33).map(|i| format!("k{i}=v")).collect::<Vec<_>>().join("&")),
        format!("{base}?{}", urlencode(&[("error", "".into()), ("state", state.clone())])),
        format!("{base}?{}", urlencode(&[("error", "access_denied".into()), ("state", state.clone()), ("x", "é€".into())])),
        format!("  {ok}"),
        format!("{ok}&code2=%E2%82%AC+%41%zz"),
        ok.clone(),
        ok.clone(),
    ];
    let count = uris.len();
    let setup = move |dir: &Path| {
        for i in 0..count {
            let session = dir.join(format!("s{i:02}"));
            std::fs::create_dir(&session).unwrap();
            let txn = format!(
                r#"{{"schemaVersion": 2, "purpose": "experimental-signin-preparation", "createdUnixSeconds": 1000, "expiresUnixSeconds": 1600, "state": "{}", "expectedCallback": "ms-app://s-1-15-2-123/"}}"#,
                "A".repeat(43)
            );
            std::fs::write(session.join("transaction.json"), txn).unwrap();
        }
    };
    let pairs: Vec<_> = uris.iter().enumerate().map(|(i, u)| json!([format!("__WORK__/s{i:02}"), u])).collect();
    let spec = json!({"mode": "capture", "args": {"pairs": pairs, "captureNow": 1001.75}});
    let (py, rs) = compare("capture", setup, spec, |work| {
        let out: Vec<_> = uris.iter().enumerate().map(|(i, u)| result_json(&capture(&work.join(format!("s{i:02}")), u, 1001.75, None))).collect();
        json!({"results": out, "requests": [], "randomCalls": 0})
    }, &[]);
    assert_eq!(py["results"], rs["results"]);
    println!("capture results: {}", py["results"]);
}

// ---------------------------------------------------------------- dispatch (v3 encrypted state)

const V3_STATE: &str = "Qm9vdHN0cmFwLXN0YXRlLWZvci1kaWZmZXJlbnRpYWw";
const V3_IV: &str = "0f0e0d0c0b0a09080706050403020100";
const V3_CALLBACK: &str = "ms-app://s-1-15-2-4027708247-2189610-1983755848-2937435718-1578786913-2158692839-1974417358/";

fn enc(key_state: &str, plain: &str) -> String {
    let iv: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&V3_IV[2 * i..2 * i + 2], 16).unwrap()).collect();
    let out = openssl::symm::encrypt(openssl::symm::Cipher::aes_128_cbc(), &key_state.as_bytes()[..16], Some(&iv), plain.as_bytes()).unwrap();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

fn v3_setup(dir: &Path) {
    let sb = Sandbox::at(dir);
    let state = &sb.layout.state;
    let sessions = state.join("sessions");
    let session = sessions.join("19700101T001640Z-00112233");
    for d in [state.parent().unwrap().to_path_buf(), state.clone(), sessions.clone(), session.clone()] {
        std::fs::create_dir_all(&d).unwrap();
    }
    for d in [state, &sessions, &session] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let txn = format!(
        r#"{{"callbackIvHex":"{V3_IV}","codeVerifier":"{}","createdUnixSeconds":1000,"expectedCallback":"{V3_CALLBACK}","expiresUnixSeconds":1600,"loginPerformed":false,"purpose":"experimental-signin-preparation","schemaVersion":3,"state":"{V3_STATE}"}}"#,
        "v".repeat(43)
    );
    private_write(&session.join("transaction.json"), txn.as_bytes()).unwrap();
    save_json(&session.join("profile.json"), &login_profile("0123456789abcdef0123456789abcdef")).unwrap();
    save_json(&state.join("pending.json"), &Value::obj(vec![("transactionDirectory", Value::s(session.to_str().unwrap()))])).unwrap();
    sb.add_notify_send();
}

fn v3_uri(fields: &[(&str, String)]) -> String {
    format!("{V3_CALLBACK}?{}", urlencode(fields))
}

fn valid_v3() -> String {
    v3_uri(&[
        ("code", enc(V3_STATE, "synthetic-browser-code")),
        ("auth_server_url", enc(V3_STATE, "eu-auth2.samsungosp.com")),
        ("api_server_url", enc(V3_STATE, "eu-api.samsungosp.com")),
        ("retValue", enc(V3_STATE, "synthetic+user@example.invalid")),
        ("state", enc(V3_STATE, V3_STATE)),
    ])
}

#[test]
fn dispatch_matches_python() {
    need_python!();
    if !repo().join("target/debug/notes-cli").is_file() {
        eprintln!("target/debug/notes-cli missing (Python dispatch needs it); skipped");
        return;
    }
    let wrong_key = "Wrong-key-state-Qm9vdHN0cmFwLXN0YXRlLWZvc";
    let scenarios: Vec<(&str, Vec<String>)> = vec![
        (
            "dispatch-mixed",
            vec![
                valid_v3().replace("s-1-15-2-4027708247", "s-1-15-2-999"),
                v3_uri(&[("code", "x".into()), ("state", "not-hex".into())]),
                v3_uri(&[("code", "x".into()), ("state", enc(wrong_key, V3_STATE))]),
                valid_v3().replace("/?", "/wrong?"),
                valid_v3().replace("ms-app:", "https:"),
                format!("{}&state=00", valid_v3()),
                valid_v3(),
                valid_v3(),
            ],
        ),
        ("dispatch-error", vec![v3_uri(&[("error", "access_denied".into()), ("state", enc(V3_STATE, V3_STATE))]), valid_v3()]),
    ];
    for (tag, uris) in scenarios {
        let spec = json!({"mode": "dispatch", "args": {"uris": uris}});
        let (py, rs) = compare(tag, v3_setup, spec, |work| {
            let sb = Sandbox::at(work);
            let rt = FakeRuntime::new(NOW);
            let out: Vec<_> = uris.iter().map(|u| result_json(&notes_login::dispatch(&sb.layout, &rt, u))).collect();
            json!({"results": out, "requests": [], "randomCalls": rt_calls(&rt)})
        }, &[]);
        assert_eq!(py["results"], rs["results"], "{tag}");
        println!("{tag} results: {}", py["results"]);
    }
}

#[test]
fn decrypt_callback_matches_notes_cli_binary() {
    let cli = repo().join("target/debug/notes-cli");
    if !cli.is_file() {
        eprintln!("target/debug/notes-cli missing; skipped");
        return;
    }
    let root = temp_dir("decrypt");
    let work = root.join("w");
    std::fs::create_dir(&work).unwrap();
    v3_setup(&work);
    let sb = Sandbox::at(&work);
    let rt = FakeRuntime::new(NOW);
    assert_eq!(notes_login::dispatch(&sb.layout, &rt, &valid_v3()).unwrap(), "authorization_code_captured");
    let session = sb.layout.sessions().join("19700101T001640Z-00112233");
    let copy = root.join("cli-session");
    copy_tree(&session, &copy);
    let status = Command::new(&cli).arg("decrypt-callback").arg(&copy).env_clear().output().unwrap();
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    notes_login::decrypt::decrypt_saved_callback(&session).unwrap();
    let a = std::fs::read(copy.join("decoded-callback.json")).unwrap();
    let b = std::fs::read(session.join("decoded-callback.json")).unwrap();
    assert_eq!(a, b, "decoded-callback.json differs from notes-cli");
    assert_eq!(mode(&session.join("decoded-callback.json")), 0o600);
    println!("decrypt-callback: decoded-callback.json identical ({} bytes)", a.len());
}

// ---------------------------------------------------------------- install_handler / start

fn python_paths() -> (PathBuf, PathBuf) {
    let out = Command::new(python().unwrap()).env_clear().args(["-c", "import sys; print(sys.executable)"]).output().unwrap();
    let exe = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    (exe, repo().join("reference/python/tools/notes_login.py"))
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn install_handler_matches_python() {
    need_python!();
    let (interpreter, script) = python_paths();
    let exec = HandlerExec::Python { interpreter, script };
    let cases: Vec<(&str, fn(&Path))> = vec![
        ("install-fresh-mimeapps", |dir| {
            let sb = Sandbox::at(dir);
            std::fs::create_dir_all(&sb.layout.config_home).unwrap();
            std::fs::write(sb.layout.config_home.join("mimeapps.list"), "[Default Applications]\ntext/html=firefox.desktop\n").unwrap();
        }),
        ("install-fresh", |dir| {
            Sandbox::at(dir);
        }),
        ("install-foreign", |dir| {
            let sb = Sandbox::at(dir);
            std::fs::write(sb.home.join(".fake-xdg-mime-default"), "other.desktop\n").unwrap();
        }),
    ];
    for (tag, setup) in cases {
        let spec = json!({"mode": "install"});
        let (py, rs) = compare(tag, setup, spec, |work| {
            let sb = Sandbox::at(work);
            let rt = FakeRuntime::new(NOW);
            let outcome = match install_handler(&sb.layout, &exec, &rt) {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("error:{e}"),
            };
            json!({"outcome": outcome, "requests": [], "randomCalls": rt_calls(&rt)})
        }, &[]);
        let py_outcome = py["outcome"].as_str().unwrap().to_string() + &py["error"].as_str().map(|e| format!(":{}", e.split_once(':').unwrap().1)).unwrap_or_default();
        assert_eq!(py_outcome, rs["outcome"].as_str().unwrap(), "{tag}");
        assert_eq!(py["executable"].as_str().unwrap(), match &exec { HandlerExec::Python { interpreter, .. } => interpreter.to_str().unwrap(), _ => unreachable!() });
    }
}

fn start_setup(dir: &Path) {
    let sb = Sandbox::at(dir);
    let inputs = dir.join("app/login-000102030405");
    std::fs::create_dir_all(&inputs).unwrap();
    save_json(&inputs.join("profile.json"), &login_profile("0123456789abcdef0123456789abcdef")).unwrap();
    save_json(&inputs.join("metadata-client.json"), &Value::obj(vec![("value", Value::s("a4558shhl3"))])).unwrap();
    let _ = sb;
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn start_matches_python() {
    need_python!();
    if !repo().join("target/debug/notes-cli").is_file() {
        eprintln!("target/debug/notes-cli missing (Python start needs it); skipped");
        return;
    }
    let (interpreter, script) = python_paths();
    let exec = HandlerExec::Python { interpreter, script };
    let cases: Vec<(&str, Vec<Canned>, &str)> = vec![
        ("start-ok", vec![Canned::Response { status: 200, headers: vec![], body: ENTRYPOINT_FIXTURE.to_vec() }], "ok"),
        ("start-500", vec![json_response(500, "{}")], "error"),
        ("start-big", vec![Canned::Response { status: 200, headers: vec![], body: vec![b' '; 128 * 1024 + 1] }], "error"),
    ];
    for (tag, responses, expected) in cases {
        let spec = json!({"mode": "start", "responses": canned_json(&responses), "args": {
            "profile": "__WORK__/app/login-000102030405/profile.json",
            "appId": "__WORK__/app/login-000102030405/metadata-client.json", "open": false}});
        let (py, rs) = compare(tag, start_setup, spec, |work| {
            let sb = Sandbox::at(work);
            let net = FakeNet::new(responses.clone());
            let rt = FakeRuntime::new(NOW);
            let inputs = work.join("app/login-000102030405");
            let outcome = match notes_login::start(&sb.layout, &net, &rt, &inputs.join("profile.json"), &inputs.join("metadata-client.json"), false, &exec) {
                Ok(_) => "ok",
                Err(_) => "error",
            };
            json!({"outcome": outcome, "requests": requests_json(&net), "randomCalls": rt_calls(&rt)})
        }, &["transaction.json", "url.txt", "open-login.html"]);
        assert_eq!(py["outcome"], rs["outcome"], "{tag}: {:?}", py.get("error"));
        assert_eq!(py["outcome"], expected, "{tag}");
    }
}

// ---------------------------------------------------------------- SessionManager._login preamble

struct MemStore(std::sync::Mutex<Option<String>>);
impl notes_login::LoginStore for MemStore {
    fn client_device_id(&self) -> Result<Option<String>, notes_login::LoginError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn set_client_device_id(&self, value: &str) -> Result<(), notes_login::LoginError> {
        *self.0.lock().unwrap() = Some(value.to_string());
        Ok(())
    }
}
struct Never;
impl notes_login::Closed for Never {
    fn is_set(&self) -> bool {
        false
    }
}

/// Python's `run` is stopped at the first subprocess; the Rust side fails at
/// the (in-process) metadata request. Both must leave identical private
/// inputs, the same device identity and the same error text. The Rust side
/// also ran `start` up to the metadata GET; that sandbox state is removed
/// before comparing (Python never got that far).
#[test]
fn login_preamble_matches_session_manager() {
    need_python!();
    let cases: Vec<(&str, Option<&str>, bool)> = vec![
        ("login-new-identity", None, false),
        ("login-stored-identity", Some("feedfacefeedfacefeedfacefeedface"), false),
        ("login-existing-session", None, true),
    ];
    for (tag, stored, with_session) in cases {
        let setup = move |dir: &Path| {
            std::fs::create_dir_all(dir.join("app")).unwrap();
            if with_session {
                let old = dir.join("old-session");
                std::fs::create_dir(&old).unwrap();
                save_json(&old.join("profile.json"), &login_profile("abcdefabcdefabcdefabcdefabcdefab")).unwrap();
            }
        };
        let meta = match stored { Some(s) => json!({"client_device_id": s}), None => json!({}) };
        let spec = json!({"mode": "login-inputs", "args": {"store": "__WORK__/app", "meta": meta,
            "session": if with_session { json!("__WORK__/old-session") } else { json!(null) }}});
        let (py, rs) = compare(tag, setup, spec, |work| {
            let sb = Sandbox::at(work);
            let net = FakeNet::new(vec![Canned::TransportError]);
            let rt = FakeRuntime::new(NOW);
            let store = MemStore(std::sync::Mutex::new(stored.map(str::to_string)));
            let ctx = notes_login::LoginContext {
                store_directory: work.join("app"),
                session: if with_session { Some(work.join("old-session")) } else { None },
                store: &store,
                layout: sb.layout.clone(),
                default_profile: Some(repo().join("crates/notes-login/src/login_profile.json")),
                closed: &Never,
                net: &net,
                runtime: &rt,
                handler_executable: "/usr/bin/notes-engine".into(),
            };
            let error = notes_login::login(&ctx).unwrap_err().user_message().to_string();
            // Undo what only the in-process `start` reached.
            std::fs::remove_dir_all(sb.home.join(".local")).unwrap();
            for log in ["xdg-mime", "desktop-file-validate", "update-desktop-database"] {
                let _ = std::fs::remove_file(sb.home.join(format!(".fake-{log}.log")));
            }
            let _ = std::fs::remove_file(sb.home.join(".fake-xdg-mime-default"));
            // `start` drew exactly one token (the transaction name) before the
            // metadata GET failed; the rest is the preamble's own consumption.
            let calls = rt_calls(&rt) - 1;
            let id = store.0.lock().unwrap().clone();
            json!({"error": error, "id": id, "requests": [], "randomCalls": calls})
        }, &[]);
        assert_eq!(py["error"], rs["error"], "{tag}");
        assert_eq!(py["meta"].get("client_device_id").cloned().unwrap_or(json!(null)), rs["id"], "{tag}");
        assert_eq!(py["pending"], json!(false));
    }
}
