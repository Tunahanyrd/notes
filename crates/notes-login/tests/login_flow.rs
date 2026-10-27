//! End-to-end `login()` offline: fake network, temp HOME/XDG, fake desktop
//! helpers and a simulated browser that delivers the encrypted callback via
//! `dispatch` (exactly what the desktop handler runs).
mod common;
use common::*;
use notes_login::json::{self, Value};
use notes_login::login::{login, Closed, LoginContext, LoginError, LoginStore, LOGIN_FAILED};
use notes_login::urls::urlencode;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Store {
    id: Mutex<Option<String>>,
    fail: bool,
}

impl LoginStore for Store {
    fn client_device_id(&self) -> Result<Option<String>, LoginError> {
        if self.fail {
            return Err(LoginError::Store { code: "store_locked".into(), message: "Yerel arşiv kilitli.".into(), status: 409 });
        }
        Ok(self.id.lock().unwrap().clone())
    }
    fn set_client_device_id(&self, value: &str) -> Result<(), LoginError> {
        *self.id.lock().unwrap() = Some(value.to_string());
        Ok(())
    }
}

#[derive(Default)]
struct Flag(AtomicBool);
impl Closed for Flag {
    fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn wait(&self, _: Duration) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn enc(state: &str, iv_hex: &str, plain: &str) -> String {
    let key = &state.as_bytes()[..16];
    let iv: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&iv_hex[2 * i..2 * i + 2], 16).unwrap()).collect();
    let out = openssl::symm::encrypt(openssl::symm::Cipher::aes_128_cbc(), key, Some(&iv), plain.as_bytes()).unwrap();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// The browser + desktop handler: wait for the pending transaction, then
/// deliver a callback URI built from the transaction's state and IV.
fn browser(layout: notes_login::Layout, rt: Arc<FakeRuntime>, error: bool) -> std::thread::JoinHandle<Option<String>> {
    std::thread::spawn(move || {
        for _ in 0..500 {
            let pending = layout.state.join("pending.json");
            if let Ok(raw) = std::fs::read(&pending) {
                let dir = PathBuf::from(json::loads(&raw).unwrap().get("transactionDirectory").unwrap().as_str().unwrap());
                if let Ok(txn) = std::fs::read(dir.join("transaction.json")) {
                    let txn = json::loads(&txn).unwrap();
                    let state = txn.get("state").unwrap().as_str().unwrap();
                    let iv = txn.get("callbackIvHex").unwrap().as_str().unwrap();
                    let expected = txn.get("expectedCallback").unwrap().as_str().unwrap();
                    let mut fields = vec![("state", enc(state, iv, state))];
                    if error {
                        fields.push(("error", "access_denied".into()));
                    } else {
                        fields.push(("code", enc(state, iv, "synthetic-browser-code")));
                        fields.push(("auth_server_url", enc(state, iv, "eu-auth2.samsungosp.com")));
                        fields.push(("api_server_url", enc(state, iv, "eu-api.samsungosp.com")));
                        fields.push(("retValue", enc(state, iv, "synthetic+user@example.invalid")));
                    }
                    let uri = format!("{expected}?{}", urlencode(&fields));
                    return notes_login::dispatch(&layout, rt.as_ref(), &uri).ok();
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    })
}

fn happy_responses() -> Vec<Canned> {
    vec![
        Canned::Response { status: 200, headers: vec![], body: ENTRYPOINT_FIXTURE.to_vec() },
        json_response(200, r#"{"userauth_token":"synthetic-uat","userId":"synthetic-user","userauth_token_expires_in":3600}"#),
        json_response(200, r#"{"code":"synthetic-app-code","code_expires_in":300}"#),
        json_response(200, r#"{"access_token":"synthetic-notes-token","userId":"synthetic-user","access_token_expires_in":3600}"#),
        json_response(200, r#"{"play":"https://play.samsungcloud.com","default":"https://api.samsungcloud.com"}"#),
        json_response(200, r#"{"dvcId":"synthetic-device","tokens":[{"type":"user","accessToken":"synthetic-cloud","expiresIn":86400}]}"#),
    ]
}

struct Run {
    sb: Sandbox,
    store_dir: PathBuf,
    net: FakeNet,
    rt: Arc<FakeRuntime>,
    store: Store,
    closed: Flag,
}

impl Run {
    fn new(tag: &str, responses: Vec<Canned>) -> Run {
        let sb = Sandbox::new(tag);
        let store_dir = sb.root.join("app");
        std::fs::create_dir(&store_dir).unwrap();
        Run { sb, store_dir, net: FakeNet::new(responses), rt: Arc::new(FakeRuntime::new(1_800_000_000.25)), store: Store::default(), closed: Flag::default() }
    }
    fn ctx(&self, session: Option<PathBuf>) -> LoginContext<'_> {
        LoginContext {
            store_directory: self.store_dir.clone(),
            session,
            store: &self.store,
            layout: self.sb.layout.clone(),
            default_profile: None,
            closed: &self.closed,
            net: &self.net,
            runtime: self.rt.as_ref(),
            handler_executable: PathBuf::from("/usr/lib/samsung-notes/notes-engine"),
        }
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    v.sort();
    v
}

#[test]
fn full_sign_in_produces_an_adoptable_session() {
    let run = Run::new("login-ok", happy_responses());
    let handle = browser(run.sb.layout.clone(), Arc::clone(&run.rt), false);
    let session = login(&run.ctx(None)).expect("login");
    assert_eq!(handle.join().unwrap().as_deref(), Some("authorization_code_captured"));
    assert_eq!(session.parent().unwrap(), run.sb.layout.sessions());
    assert_eq!(mode(&session), 0o700);
    for file in [
        "transaction.json", "url.txt", "profile.json", "open-login.html", "callback.json", ".callback-claim",
        "decoded-callback.json", ".account-code-claim", "uat-receipt.json", "uat-response.body", "uat-token.json",
        "notes-app-transaction.json", ".notes-authorize-claim", "notes-authorize-receipt.json", "notes-app-code.json",
        ".notes-code-claim", "notes-token-receipt.json", "notes-account-token.json", "app-endpoints.json",
        "app-cloud-profile.json", "cloud/endpoints.json", "cloud/profile.json", "cloud/.register-claim",
        "cloud/register-receipt.json", "cloud/register-response-meta.json", "cloud/register-response.body", "cloud/cloud-token.json",
    ] {
        let p = session.join(file);
        assert!(p.is_file(), "{file}");
        assert_eq!(mode(&p), 0o600, "{file}");
    }
    let calls = run.net.requests();
    let urls: Vec<&str> = calls.iter().map(|c| c.request.url.as_str()).collect();
    assert_eq!(urls[0], "https://account.samsung.com/accounts/WINSDK/getEntryPoint?countryCode=TR");
    assert_eq!(urls[1], "https://eu-auth2.samsungosp.com/auth/oauth2/authenticate");
    assert_eq!(urls[2], "https://eu-auth2.samsungosp.com/auth/oauth2/v2/authorize");
    assert_eq!(urls[3], "https://eu-auth2.samsungosp.com/auth/oauth2/token");
    assert_eq!(urls[4], "https://ers.samsungcloud.com/ers/v1/endpoints");
    assert!(urls[5].starts_with("https://play.samsungcloud.com/identity/v2/user/register?deviceCurrentTime=1800000000250"));
    assert_eq!(calls.len(), 6);
    assert_eq!(calls[0].request.header("Authorization"), Some("Basic YTQ1NThzaGhsMzo="));
    // Device identity persisted and placed into the profile (PCID: prefix).
    let id = run.store.id.lock().unwrap().clone().unwrap();
    assert_eq!(id.len(), 32);
    let profile = json::loads(&std::fs::read(session.join("profile.json")).unwrap()).unwrap();
    assert_eq!(profile.get("deviceUniqueID").unwrap().as_str(), Some(format!("PCID:{id}").as_str()));
    let cloud_profile = json::loads(&std::fs::read(session.join("app-cloud-profile.json")).unwrap()).unwrap();
    assert_eq!(cloud_profile.get("clientDeviceId").unwrap().as_str(), Some(id.as_str()));
    // Handler: engine binary, browser opened on the local landing page.
    let desktop = std::fs::read_to_string(run.sb.layout.data_home.join("applications/samsung-notes-research-callback.desktop")).unwrap();
    assert!(desktop.contains("Exec=/usr/lib/samsung-notes/notes-engine __notes-login-callback %u\n"));
    for _ in 0..200 {
        if !run.sb.log("xdg-open").is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(run.sb.log("xdg-open").contains("open-login.html"));
    // Login inputs directory is private and kept.
    let inputs: Vec<String> = names(&run.store_dir);
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].starts_with("login-"));
    assert_eq!(names(&run.store_dir.join(&inputs[0])), vec!["metadata-client.json", "profile.json"]);
    assert_eq!(
        std::fs::read_to_string(run.store_dir.join(&inputs[0]).join("metadata-client.json")).unwrap(),
        "{\n  \"value\": \"a4558shhl3\"\n}\n"
    );
    let decoded = json::loads(&std::fs::read(session.join("decoded-callback.json")).unwrap()).unwrap();
    assert_eq!(decoded.get("loginId").unwrap().as_str(), Some("synthetic+user@example.invalid"));
}

#[test]
fn closed_application_stops_waiting_without_any_credential_request() {
    let run = Run::new("login-closed", happy_responses());
    run.closed.0.store(true, Ordering::SeqCst);
    let err = login(&run.ctx(None)).unwrap_err();
    assert_eq!(err.user_message(), LOGIN_FAILED);
    assert_eq!(run.net.count(), 1, "only the anonymous entry point metadata");
}

#[test]
fn authorization_error_callback_sends_nothing_further() {
    let run = Run::new("login-denied", happy_responses());
    let handle = browser(run.sb.layout.clone(), Arc::clone(&run.rt), true);
    let err = login(&run.ctx(None)).unwrap_err();
    assert_eq!(handle.join().unwrap().as_deref(), Some("authorization_error"));
    assert_eq!(err.user_message(), LOGIN_FAILED);
    assert_eq!(run.net.count(), 1);
}

#[test]
fn ambiguous_uat_exchange_is_never_retried() {
    let mut responses = happy_responses();
    responses.truncate(1);
    responses.push(Canned::TransportError);
    let run = Run::new("login-uat-timeout", responses);
    let handle = browser(run.sb.layout.clone(), Arc::clone(&run.rt), false);
    let err = login(&run.ctx(None)).unwrap_err();
    handle.join().unwrap();
    assert_eq!(err.user_message(), LOGIN_FAILED);
    assert_eq!(run.net.count(), 2);
    let pending = json::loads(&std::fs::read(run.sb.layout.state.join("pending.json")).unwrap()).unwrap();
    let session = PathBuf::from(pending.get("transactionDirectory").unwrap().as_str().unwrap());
    assert!(session.join(".account-code-claim").exists());
    assert!(!session.join("uat-token.json").exists());
    // A second attempt on the same transaction is refused before transport.
    let rt = FakeRuntime::new(1_800_000_000.25);
    assert!(notes_login::bridge::authenticate(&run.net, &rt, &session).unwrap_err().is_file_exists());
    assert_eq!(run.net.count(), 2);
}

#[test]
fn store_error_message_is_shown_verbatim() {
    let mut run = Run::new("login-store", happy_responses());
    run.store.fail = true;
    let err = login(&run.ctx(None)).unwrap_err();
    assert_eq!(err.user_message(), "Yerel arşiv kilitli.");
    assert_eq!(run.net.count(), 0);
}

#[test]
fn existing_session_profile_is_reused_without_new_identity() {
    let run = Run::new("login-reuse", happy_responses());
    let old = run.sb.root.join("old-session");
    std::fs::create_dir(&old).unwrap();
    let mut profile = json::loads(notes_login::login::DEFAULT_LOGIN_PROFILE).unwrap();
    profile.set("devicePhysicalAddressText", Value::s("feedfacefeedfacefeedfacefeedface"));
    profile.set("deviceUniqueID", Value::s("PCID:feedfacefeedfacefeedfacefeedface"));
    notes_login::files::save_json(&old.join("profile.json"), &profile).unwrap();
    let handle = browser(run.sb.layout.clone(), Arc::clone(&run.rt), false);
    let session = login(&run.ctx(Some(old))).expect("login");
    handle.join().unwrap();
    assert!(run.store.id.lock().unwrap().is_none(), "no client_device_id written when a session exists");
    let cloud_profile = json::loads(&std::fs::read(session.join("app-cloud-profile.json")).unwrap()).unwrap();
    assert_eq!(cloud_profile.get("clientDeviceId").unwrap().as_str(), Some("feedfacefeedfacefeedfacefeedface"));
}

#[test]
fn live_pending_transaction_blocks_a_second_start() {
    let run = Run::new("login-live", happy_responses());
    run.closed.0.store(true, Ordering::SeqCst);
    let _ = login(&run.ctx(None));
    // The first transaction is still live and unclaimed: a new start fails
    // before any network request.
    let net = FakeNet::new(vec![]);
    let ctx = LoginContext { net: &net, ..run.ctx(None) };
    assert_eq!(login(&ctx).unwrap_err().user_message(), LOGIN_FAILED);
    assert_eq!(net.count(), 0);
}

#[test]
fn run_callback_ignores_normal_engine_arguments() {
    assert_eq!(notes_login::run_callback(&["notes-engine".into(), "--port".into(), "1".into()]), None);
    assert_eq!(notes_login::run_callback(&["notes-engine".into()]), None);
}

#[test]
fn callback_main_usage_and_failure_codes() {
    let sb = Sandbox::new("cbmain");
    let rt = FakeRuntime::new(1001.0);
    assert_eq!(notes_login::callback_main(&[], &sb.layout, &rt), 2);
    // No pending transaction → handled failure, exit 1.
    assert_eq!(notes_login::callback_main(&["ms-app://x/?state=y".into()], &sb.layout, &rt), 1);
}
