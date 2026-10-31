//! Engine outbox -> pure notes-cloud CloudApi -> synthetic in-memory server
//! (no network): a local create and an edit are published once, read back
//! and completed; an unknown publish outcome is never retried.
mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::TempDir;
use notes_cloud::api::decode_parts;
use notes_cloud::etree::{self, Element};
use notes_cloud::transport::{Request, Response, Transport};
use notes_cloud::{Fail, Kind, Options};
use notes_engine::cloud_impl::NativeCloud;
use notes_engine::cloud::CloudFactory;
use notes_engine::engine::{Engine, Options as EngineOptions};
use notes_engine::sessions::NoLogin;
use notes_engine::stub_previews::NoPreviewFactory;
use notes_host::Val;

const NOW: f64 = 1_700_000_000.0;

#[derive(Default)]
struct State {
    note: Option<Element>,
    commit: String,
    publishes: usize,
    fail_publish: bool,
}

#[derive(Clone, Default)]
struct Server(Arc<Mutex<State>>);

impl Transport for Server {
    fn send(&self, _dir: &Path, _name: &str, request: &Request, _limit: usize) -> Result<Response, Fail> {
        let mut s = self.0.lock().unwrap();
        let path = request.path().to_string();
        if path.ends_with("/compareForDownsync") {
            let mut root = Element::new("CompareNoteView");
            root.children.push(s.note.clone().ok_or_else(|| Fail::new(Kind::Other, "no note"))?);
            let mut commit = Element::new("commitId");
            commit.text = Some(s.commit.clone());
            root.children.push(commit);
            return Ok(Response::new(200, etree::tostring_ascii(&root), "application/xml"));
        }
        if path.ends_with("/notes") {
            s.publishes += 1;
            if s.fail_publish {
                return Err(Fail::os("synthetic disconnected transport"));
            }
            let parts = decode_parts(request.data.as_deref().unwrap(), request.header("Content-type").unwrap()).unwrap();
            s.note = Some(etree::parse_bytes(&parts[0].3, false).unwrap());
            s.commit = format!("commit-{}", s.publishes);
            let body = format!("<DefaultView><resourceId>synthetic-server</resourceId><commitId>{}</commitId></DefaultView>", s.commit);
            return Ok(Response::new(201, body.into_bytes(), "application/xml"));
        }
        Err(Fail::new(Kind::Other, format!("unexpected synthetic request {}", path)))
    }
}

fn session(root: &Path) -> std::path::PathBuf {
    let session = root.join("session");
    std::fs::create_dir_all(session.join("cloud")).unwrap();
    let w = |n: &str, v: &str| std::fs::write(session.join(n), v).unwrap();
    w("cloud/profile.json", r#"{"appId": "synthetic-notes-client", "initialCursor": "initial", "registrationBody": {"app": {"version": "synthetic"}}, "notesHeaders": {"x-sn-sdk-ver": "4000", "x-sn-app-ver": "synthetic", "x-sn-xml-ver": "2", "x-sn-model-name": "U3ludGhldGlj"}}"#);
    w("cloud/endpoints.json", r#"{"default": "https://api.samsungcloud.com", "play": "https://play.samsungcloud.com"}"#);
    w("cloud/cloud-token.json", r#"{"schemaVersion": 1, "credentialKind": "CloudUser", "receivedUnixSeconds": 1699999995, "expiresIn": 3600, "accessToken": "synthetic-cloud", "dvcId": "synthetic-device"}"#);
    w("notes-account-token.json", r#"{"schemaVersion": 1, "credentialKind": "NotesServiceOAuth", "clientId": "synthetic-notes-client", "receivedUnixSeconds": 1699999995, "response": {"access_token": "synthetic-account", "userId": "synthetic-user", "expires_in": 3600, "refresh_token": "-1"}}"#);
    session
}

fn engine(root: &Path, server: &Server) -> Arc<Engine> {
    let server = server.clone();
    let cloud = NativeCloud { options: Arc::new(move || Options::default().with_transport(server.clone()).with_clock(|| NOW)) };
    let engine = Engine::new(
        &root.join("app"),
        EngineOptions {
            offline: true,
            session: None,
            auto_sync: false,
            interval: 3600.0,
            cloud: Arc::new(cloud),
            previews: Arc::new(NoPreviewFactory),
            login: Arc::new(NoLogin),
        },
    )
    .unwrap();
    engine.sessions.set_offline(false);
    engine.sessions.set_path(Some(session(root)));
    engine
}

#[test]
fn local_create_is_published_once_read_back_and_completed() {
    let t = TempDir::new("cloud-create");
    let server = Server::default();
    let engine = engine(t.path(), &server);
    let note = engine.create(&Val::s("Türkçe başlık"), &Val::s("Gövde 🚀")).unwrap();
    let id = note.get("id").unwrap().as_str().unwrap().to_string();
    let api = engine.apis.api().unwrap();
    engine.push(&api).unwrap();
    assert_eq!(server.0.lock().unwrap().publishes, 1);
    let synced = engine.store.get(&id).unwrap();
    assert_eq!(synced.get("syncState"), Some(&Val::s("synced")));
    assert_eq!(synced.get("source"), Some(&Val::s("cloud")));
    assert_eq!(synced.get("body"), Some(&Val::s("Gövde 🚀")));
    // A second push has nothing to publish.
    engine.push(&api).unwrap();
    assert_eq!(server.0.lock().unwrap().publishes, 1);
    // The immutable operation receipts are private.
    let ops = t.path().join("app/cloud/operations");
    let entries: Vec<_> = std::fs::read_dir(&ops).unwrap().collect();
    assert_eq!(entries.len(), 1);
    engine.close();
}

#[test]
fn unknown_publish_outcome_is_uncertain_and_never_retried() {
    let t = TempDir::new("cloud-unknown");
    let server = Server::default();
    server.0.lock().unwrap().fail_publish = true;
    let engine = engine(t.path(), &server);
    let note = engine.create(&Val::s("Başlık"), &Val::s("Gövde")).unwrap();
    let id = note.get("id").unwrap().as_str().unwrap().to_string();
    let api = engine.apis.api().unwrap();
    engine.push(&api).unwrap();
    engine.push(&api).unwrap();
    assert_eq!(server.0.lock().unwrap().publishes, 1);
    assert_eq!(engine.store.get(&id).unwrap().get("syncState"), Some(&Val::s("uncertain")));
    engine.close();
}

#[test]
fn container_backed_factory_publishes_replayable_receipts() {
    let t = TempDir::new("cloud-container-receipt");
    let server = Server::default();
    let app = t.path().join("app");
    let _store = rs_store::storage::StoreCore::open(&app).unwrap();
    let transport = server.clone();
    let factory = NativeCloud { options: Arc::new(move || Options::default().with_transport(transport.clone()).with_clock(|| NOW)) };
    let api = factory.open_for_library(&session(t.path()), &app.join("cloud"), Some(&app)).unwrap();
    let op = Val::s("11111111-1111-4111-8111-111111111111");
    let local = Val::s("22222222-2222-4222-8222-222222222222");
    let first = api.create(&Val::s("Synthetic"), &Val::s("Text"), &op, &local).unwrap();
    let receipt_path = app.join("cloud/operations/11111111-1111-4111-8111-111111111111/result.json");
    let receipt = rs_store::val::loads(&std::fs::read_to_string(receipt_path).unwrap()).unwrap();
    let snapshot = receipt.get("snapshot").unwrap();
    assert!(snapshot.get("container_sha256").is_some());
    assert!(snapshot.get("raw_body").is_none());
    let replay = api.create(&Val::s("Synthetic"), &Val::s("Text"), &op, &local).unwrap();
    assert!(rs_store::val::py_eq(&first, &replay));
    assert_eq!(server.0.lock().unwrap().publishes, 1);
}

#[test]
fn schema7_engine_completes_one_post_with_container_receipt() {
    let t = TempDir::new("cloud-v7-outbox");
    let app = t.path().join("app");
    rs_store::storage::private_directory(&app).unwrap();
    let _lock = notes_engine::launcher::lock_library(&app).ok().unwrap();
    notes_engine::prepare_schema7(&app).unwrap();
    let server = Server::default();
    let engine = engine(t.path(), &server);
    let note = engine.create(&Val::s("Synthetic"), &Val::s("Text")).unwrap();
    let id = note.get("id").unwrap().as_str().unwrap();
    let api = engine.apis.api().unwrap();
    engine.push(&api).unwrap();
    engine.push(&api).unwrap();
    assert_eq!(server.0.lock().unwrap().publishes, 1);
    use rs_store::db::Sql;
    let row = engine.store.with_db(|db| db.query(
        "SELECT n.base_document IS NULL AS base_null,n.container_metadata,j.state,j.snapshot,j.id FROM notes n JOIN jobs j ON j.note_id=n.id WHERE n.id=?",
        &rs_store::p![id],
    )).unwrap();
    let row = row.one().unwrap();
    assert!(row.truthy("base_null") && row.is_null("snapshot"));
    assert_eq!(row.text("state"), Some("done"));
    assert!(rs_store::val::loads(row.text("container_metadata").unwrap()).unwrap().get("container_sha256").is_some());
    let op = row.text("id").unwrap();
    let receipt = std::fs::read_to_string(app.join("cloud/operations").join(op).join("result.json")).unwrap();
    let receipt = rs_store::val::loads(&receipt).unwrap();
    assert!(receipt.get("snapshot").unwrap().get("container_sha256").is_some());
    assert!(receipt.get("snapshot").unwrap().get("raw_body").is_none());
    engine.close();
}
