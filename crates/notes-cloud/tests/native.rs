//! Native Rust ports of key tests/test_cloud_api.py cases with a synthetic
//! in-memory server (no network, synthetic session directories).
use notes_cloud::api::{decode_parts, CloudApi, Options};
use notes_cloud::etree::{self, Element};
use notes_cloud::metadata::{parse_folder_changes, parse_note_list};
use notes_cloud::transport::{Request, Response, Transport};
use notes_cloud::{util, Fail, Kind, Val};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const NOW: f64 = 1_700_000_000.0;
const LOCAL_ID: &str = "00000000-0000-4000-8000-000000000001";

#[derive(Default)]
struct State {
    note: Option<Element>,
    commit: String,
    calls: Vec<Request>,
    publish_count: usize,
    fail_publish: bool,
    fail_readback: bool,
    fail_compare: bool,
    publish_status: i64,
}

#[derive(Clone)]
struct Server(Arc<Mutex<State>>);

impl Server {
    fn new() -> Server {
        let (_, xml) = rs_doc::doc::new_document("Başlık", "Gövde", Some(LOCAL_ID), rs_doc::doc::NowArg::Int((NOW as i128) * 1000 - 10000), 256 * 1024).unwrap();
        let note = etree::parse_bytes(&xml, false).unwrap();
        Server(Arc::new(Mutex::new(State { note: Some(note), commit: "commit-1".into(), publish_status: 201, ..Default::default() })))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
}

fn child<'a>(e: &'a Element, tag: &str) -> Option<&'a Element> {
    e.children.iter().find(|c| c.tag == tag)
}

impl Transport for Server {
    fn send(&self, _dir: &Path, _name: &str, request: &Request, _limit: usize) -> Result<Response, Fail> {
        let mut s = self.state();
        s.calls.push(request.clone());
        let path = request.path().to_string();
        if path.ends_with("/compareForDownsync") {
            if s.fail_readback && s.publish_count > 0 {
                return Ok(Response::new(503, b"failed".to_vec(), "text/plain"));
            }
            let mut root = Element::new("CompareNoteView");
            root.children.push(s.note.clone().unwrap());
            let mut commit = Element::new("commitId");
            commit.text = Some(s.commit.clone());
            root.children.push(commit);
            return Ok(Response::new(200, etree::tostring_ascii(&root), "application/xml"));
        }
        if path.ends_with("/compareForUpsync") {
            if s.fail_compare {
                return Ok(Response::new(503, b"failed".to_vec(), "text/plain"));
            }
            let tree = etree::parse_bytes(&util::gunzip_limited(request.data.as_deref().unwrap(), 1 << 24).unwrap(), false).unwrap();
            let mut result = Element::new("CompareForUpsyncView");
            let mut commit = Element::new("commitId");
            commit.text = Some(s.commit.clone());
            result.children.push(commit);
            let server_h = child(child(s.note.as_ref().unwrap(), "noteTree").unwrap(), "bodyText").unwrap().get("h").map(str::to_string);
            let body = child(&tree, "bodyText").unwrap();
            if body.get("h").map(str::to_string) != server_h {
                result.children.push(body.clone());
            }
            return Ok(Response::new(200, etree::tostring_ascii(&result), "application/xml"));
        }
        if path.ends_with("/notes") || path.ends_with("/update") {
            s.publish_count += 1;
            if s.fail_publish {
                return Err(Fail::os("synthetic disconnected transport"));
            }
            if s.publish_status != 201 {
                return Ok(Response::new(s.publish_status, b"<failure/>".to_vec(), "application/xml"));
            }
            let parts = decode_parts(request.data.as_deref().unwrap(), request.header("Content-type").unwrap()).unwrap();
            let upload = etree::parse_bytes(&parts[0].3, false).unwrap();
            if path.ends_with("/notes") {
                s.note = Some(upload);
            } else {
                let note = s.note.as_mut().unwrap();
                note.attrib = upload.attrib.clone();
                for incoming in upload.children {
                    note.children.retain(|c| c.tag != incoming.tag);
                    note.children.push(incoming);
                }
            }
            s.commit = format!("commit-{}", s.publish_count + 1);
            let body = format!("<DefaultView><resourceId>synthetic-server</resourceId><commitId>{}</commitId></DefaultView>", s.commit);
            return Ok(Response::new(201, body.into_bytes(), "application/xml"));
        }
        Err(Fail::new(Kind::Other, format!("unexpected synthetic request {path}")))
    }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    session: PathBuf,
    server: Server,
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp() -> TempDir {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("native-{}", notes_cloud::OsRandom.token_hex_pub()));
    std::fs::create_dir_all(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    TempDir(base)
}

trait HexPub {
    fn token_hex_pub(&self) -> String;
}
impl HexPub for notes_cloud::OsRandom {
    fn token_hex_pub(&self) -> String {
        use notes_cloud::Random;
        self.token_hex(8)
    }
}

fn write(session: &Path, name: &str, value: &str) {
    std::fs::write(session.join(name), value).unwrap();
}

fn fixture() -> Fixture {
    let t = temp();
    let root = t.0.clone();
    let session = root.join("session");
    std::fs::create_dir_all(session.join("cloud")).unwrap();
    write(&session, "cloud/profile.json", r#"{"appId": "synthetic-notes-client", "initialCursor": "initial", "registrationBody": {"app": {"version": "synthetic"}}, "notesHeaders": {"x-sn-sdk-ver": "4000", "x-sn-app-ver": "synthetic", "x-sn-xml-ver": "2", "x-sn-model-name": "U3ludGhldGlj"}}"#);
    write(&session, "cloud/endpoints.json", r#"{"default": "https://api.samsungcloud.com", "play": "https://play.samsungcloud.com"}"#);
    write(&session, "cloud/cloud-token.json", r#"{"schemaVersion": 1, "credentialKind": "CloudUser", "receivedUnixSeconds": 1699999995, "expiresIn": 3600, "accessToken": "synthetic-cloud", "dvcId": "synthetic-device"}"#);
    write(&session, "notes-account-token.json", r#"{"schemaVersion": 1, "credentialKind": "NotesServiceOAuth", "clientId": "synthetic-notes-client", "receivedUnixSeconds": 1699999995, "response": {"access_token": "synthetic-account", "userId": "synthetic-user", "expires_in": 3600, "refresh_token": "-1"}}"#);
    Fixture { _temp: t, root, session, server: Server::new() }
}

impl Fixture {
    fn api(&self) -> CloudApi {
        CloudApi::new(&self.session, self.root.join("work"), Options::default().with_transport(self.server.clone()).with_clock(|| NOW)).unwrap()
    }
}

#[test]
fn create_verify_and_restart_replay_does_not_publish_again() {
    let f = fixture();
    let api = f.api();
    let op = "11111111-1111-4111-8111-111111111111";
    let result = api.create("Yeni", "", op, Some(LOCAL_ID)).unwrap();
    assert_eq!(result.get("verified"), Some(&Val::Bool(true)));
    assert_eq!(result.get("snapshot").unwrap().get("body"), Some(&Val::s("")));
    assert_eq!(api.create("Yeni", "", op, Some(LOCAL_ID)).unwrap(), result);
    assert_eq!(f.api().create("Yeni", "", op, Some(LOCAL_ID)).unwrap(), result);
    assert_eq!(f.server.state().publish_count, 1);
    let err = api.create("Different", "", op, Some(LOCAL_ID)).unwrap_err();
    assert_eq!(err.code, "operation_mismatch");
    // Receipts are private and the claim precedes the single POST.
    let dir = f.root.join("work/operations").join(op);
    for name in ["intent.json", "expected.json", "draft.xml", "request.multipart.gz", ".mutation-claim", "published.json", "result.json"] {
        let mode = std::fs::metadata(dir.join(name)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{name}");
    }
}

#[test]
fn update_body_and_title_reads_back_and_conflict_blocks_write() {
    let f = fixture();
    let api = f.api();
    let doc = api.get_note("synthetic-server").unwrap();
    let result = api.update("synthetic-server", &doc, "New title", "New body", None, "22222222-2222-4222-8222-222222222222").unwrap();
    let snap = result.get("snapshot").unwrap();
    assert_eq!((snap.get("title"), snap.get("body")), (Some(&Val::s("New title")), Some(&Val::s("New body"))));
    let doc = api.get_note("synthetic-server").unwrap();
    f.server.state().commit = "other-device-edit".into();
    let err = api.update("synthetic-server", &doc, &doc.title, "new", None, "33333333-3333-4333-8333-333333333333").unwrap_err();
    assert!(err.is_conflict());
    assert_eq!(f.server.state().publish_count, 1);
}

#[test]
fn unknown_publish_outcome_never_retries_and_rejection_is_not_ambiguous() {
    let f = fixture();
    let api = f.api();
    f.server.state().fail_publish = true;
    let op = "44444444-4444-4444-8444-444444444444";
    for _ in 0..2 {
        let err = api.create("New", "Body", op, Some(LOCAL_ID)).unwrap_err();
        assert!(err.ambiguous);
        assert_eq!(err.code, "write_uncertain");
    }
    assert_eq!(f.server.state().publish_count, 1);
    let g = fixture();
    g.server.state().publish_status = 400;
    let err = g.api().create("New", "Body", op, Some(LOCAL_ID)).unwrap_err();
    assert_eq!((err.code.as_str(), err.ambiguous), ("write_rejected", false));
    assert_eq!(err.message, "Sunucu kaydı kabul etmedi; yerel metin korunuyor.");
}

#[test]
fn published_then_failed_readback_reconciles_without_second_write() {
    let f = fixture();
    let api = f.api();
    f.server.state().fail_readback = true;
    let op = "55555555-5555-4555-8555-555555555555";
    assert!(api.create("New", "Body", op, Some(LOCAL_ID)).unwrap_err().ambiguous);
    assert!(f.root.join("work/operations").join(op).join("published.json").exists());
    f.server.state().fail_readback = false;
    assert_eq!(api.create("New", "Body", op, Some(LOCAL_ID)).unwrap().get("verified"), Some(&Val::Bool(true)));
    assert_eq!(f.server.state().publish_count, 1);
}

#[test]
fn failed_readonly_compare_resumes_same_operation() {
    let f = fixture();
    let api = f.api();
    let doc = api.get_note("synthetic-server").unwrap();
    let op = "66666666-6666-4666-8666-666666666666";
    f.server.state().fail_compare = true;
    let err = api.update("synthetic-server", &doc, &doc.title, "new", None, op).unwrap_err();
    assert!(!err.ambiguous);
    f.server.state().fail_compare = false;
    assert!(api.update("synthetic-server", &doc, &doc.title, "new", None, op).is_ok());
    assert_eq!(f.server.state().publish_count, 1);
}

#[test]
fn headers_auth_status_and_cached_token_of_other_account() {
    let f = fixture();
    let api = f.api();
    let headers = api.headers(true).unwrap();
    let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["X-sn-sdk-ver", "X-sn-app-ver", "X-sn-xml-ver", "X-sn-model-name", "Authorization", "Accept-encoding", "User-agent", "X-SN-Transaction-ID"]);
    assert!(headers.last().unwrap().1.starts_with("NU-"));
    let status = api.auth_status().unwrap();
    assert_eq!(status.get("valid"), Some(&Val::Bool(true)));
    assert_eq!(status.get("cloud_expires_at"), Some(&Val::Int(1_700_003_595)));
    std::fs::write(f.root.join("work/cloud-token.json"), r#"{"schemaVersion": 1, "credentialKind": "CloudUser", "receivedUnixSeconds": 1699999995, "expiresIn": 3600, "accessToken": "different-account-cloud", "dvcId": "d", "accountFingerprint": "not-this-account"}"#).unwrap();
    api.get_note("synthetic-server").unwrap();
    assert_eq!(f.server.state().calls.last().unwrap().header("Authorization"), Some("Bearer synthetic-cloud"));
}

#[test]
fn expired_notes_token_demands_login_without_exchange() {
    let f = fixture();
    write(&f.session, "notes-account-token.json", r#"{"schemaVersion": 1, "credentialKind": "NotesServiceOAuth", "clientId": "synthetic-notes-client", "receivedUnixSeconds": 1699999995, "response": {"access_token": "synthetic-account", "userId": "synthetic-user", "expires_in": 1}}"#);
    write(&f.session, "cloud/cloud-token.json", r#"{"schemaVersion": 1, "credentialKind": "CloudUser", "receivedUnixSeconds": 1699999995, "expiresIn": 1, "accessToken": "synthetic-cloud", "dvcId": "synthetic-device"}"#);
    let api = f.api();
    assert_eq!(api.auth_status().unwrap().get("needs_login"), Some(&Val::Bool(true)));
    assert_eq!(api.renew_cloud_token().unwrap_err().code, "reauth_required");
    assert!(f.server.state().calls.is_empty());
}

#[test]
fn identity_mismatch_untrusted_endpoint_and_unsafe_storage() {
    let f = fixture();
    write(&f.session, "notes-account-token.json", r#"{"schemaVersion": 1, "credentialKind": "NotesServiceOAuth", "clientId": "different", "receivedUnixSeconds": 1, "response": {"userId": "u"}}"#);
    let err = CloudApi::new(&f.session, f.root.join("other"), Options::default()).err().unwrap();
    assert_eq!(err.code, "identity_mismatch");
    let g = fixture();
    write(&g.session, "cloud/endpoints.json", r#"{"default": "https://evil.invalid", "play": "https://play.samsungcloud.com"}"#);
    let err = CloudApi::new(&g.session, g.root.join("third"), Options::default()).err().unwrap();
    assert!(err.is_value());
    let h = fixture();
    let open = h.root.join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(CloudApi::new(&h.session, &open, Options::default()).err().unwrap().code, "unsafe_storage");
}

#[test]
fn no_network_default_fails_like_an_unreachable_cloud() {
    let f = fixture();
    let api = CloudApi::new(&f.session, f.root.join("work"), Options::default().with_clock(|| NOW)).unwrap();
    let err = api.get_note("synthetic-server").unwrap_err();
    assert_eq!(err.code, "cloud_unavailable");
    let err = api.create("t", "b", "77777777-7777-4777-8777-777777777777", Some(LOCAL_ID)).unwrap_err();
    assert_eq!((err.code.as_str(), err.ambiguous), ("write_uncertain", true));
}

/// A `send_once`-style transport writes its own receipts; `call` reads the
/// content type from `<name>-response-meta.json`.
struct ReceiptWriter;
impl Transport for ReceiptWriter {
    fn send(&self, dir: &Path, name: &str, _request: &Request, _limit: usize) -> Result<Response, Fail> {
        std::fs::write(dir.join(format!("{name}-response-meta.json")), r#"{"contentType": "application/xml"}"#).unwrap();
        let body = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes-protocol/tests/fixtures/changes.xml")).unwrap();
        std::fs::write(dir.join(format!("{name}-response.body")), &body).unwrap();
        Ok(Response { status: Some(200), body: Some(body), content_type: None })
    }
    fn writes_receipts(&self) -> bool {
        true
    }
}

#[test]
fn default_change_feed_parser_matches_cli_contract() {
    let f = fixture();
    let api = CloudApi::new(&f.session, f.root.join("work"), Options::default().with_transport(ReceiptWriter).with_clock(|| NOW)).unwrap();
    let page = api.changes(None).unwrap();
    assert!(matches!(page.get("changes"), Some(Val::List(items)) if !items.is_empty()));
    assert!(page.get("has_next").is_some());
    assert_eq!(api.changes(Some("bad\ncursor")).unwrap_err().code, "invalid_cursor");
}

#[test]
fn folder_graph_and_note_list_parsing() {
    let graph = br#"<FolderChangesView><lastChangePoint>c</lastChangePoint><FolderResource><syncModifiedTime>5</syncModifiedTime><encrypted>false</encrypted><nodeList><node id="a" type="folder"><parentFolderNodeId>root</parentFolderNodeId><syncModifiedTime>4</syncModifiedTime><name>U2VudGV0aWs=</name><state>trashed</state></node></nodeList></FolderResource></FolderChangesView>"#;
    let parsed = parse_folder_changes(graph).unwrap();
    let node = &match parsed.get("nodes") { Some(Val::List(n)) => n.clone(), _ => panic!() }[0];
    assert_eq!(node.get("name"), Some(&Val::s("Sentetik")));
    assert_eq!(node.get("state"), Some(&Val::s("trashed")));
    assert_eq!(parse_folder_changes(b"<!DOCTYPE a><FolderChangesView/>").unwrap_err().code, "invalid_metadata");
    let list = br#"<NoteListView><noteList><note><serverId>s</serverId><resourceId>r</resourceId><syncModifiedTime>1</syncModifiedTime></note></noteList><notePageToken>t</notePageToken></NoteListView>"#;
    let parsed = parse_note_list(list).unwrap();
    assert_eq!(parsed.get("note_page_token"), Some(&Val::s("t")));
}
