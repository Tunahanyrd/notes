//! Port of tests/test_app_server.py against the pure server (raw sockets).
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;

use common::*;
use notes_engine::server::AppServer;
use notes_engine::engine::Engine;
use std::sync::Arc;

struct Fixture {
    _t: TempDir,
    server: AppServer,
    engine: Arc<Engine>,
    _accept: std::thread::JoinHandle<()>,
}

impl Fixture {
    fn new() -> Fixture {
        let t = TempDir::new("app");
        let engine = offline_engine(t.path());
        let server = AppServer::bind(0, engine.clone(), "synthetic-local-secret", t.path(), Some(t.path().join("fonts"))).unwrap();
        let accept = server.spawn();
        Fixture { _t: t, server, engine, _accept: accept }
    }

    /// Returns (status, content-type, body).
    fn request(&self, method: &str, path: &str, body: Option<&str>, headers: &[(&str, &str)], auth: bool) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.server.port)).unwrap();
        let mut all: Vec<(String, String)> = vec![("Host".into(), format!("127.0.0.1:{}", self.server.port)), ("Content-Type".into(), "application/json".into())];
        if auth {
            all.push(("Authorization".into(), "Bearer synthetic-local-secret".into()));
        }
        for (k, v) in headers {
            if let Some(slot) = all.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
                slot.1 = v.to_string();
            } else {
                all.push((k.to_string(), v.to_string()));
            }
        }
        let payload = body.unwrap_or("");
        let mut text = format!("{} {} HTTP/1.1\r\n", method, path);
        for (k, v) in &all {
            text.push_str(&format!("{}: {}\r\n", k, v));
        }
        if body.is_some() {
            text.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
        text.push_str("Connection: close\r\n\r\n");
        text.push_str(payload);
        stream.write_all(text.as_bytes()).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
        let content_type = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Type: ").map(str::to_string))
            .unwrap_or_default();
        (status, content_type, raw[split + 4..].to_vec())
    }

    fn json(&self, method: &str, path: &str, body: Option<&str>) -> (u16, notes_host::Val) {
        let (status, _, raw) = self.request(method, path, body, &[], true);
        (status, rs_store::val::loads(&String::from_utf8(raw).unwrap()).unwrap())
    }
}

#[test]
fn real_offline_create_save_list_export_and_revision_conflict() {
    let f = Fixture::new();
    let (status, result) = f.json("POST", "/api/notes", Some(r#"{"title": "Türkçe", "body": "İlk metin 🚀"}"#));
    assert_eq!(status, 201);
    let note = result.get("note").unwrap();
    let path = format!("/api/notes/{}", note.get("id").unwrap().as_str().unwrap());
    let (status, result) = f.json("PUT", &path, Some(r#"{"title": "Türkçe", "body": "Düzenlendi", "expectedRevision": 1}"#));
    assert_eq!(status, 200);
    assert_eq!(result.get("note").unwrap().get("syncState").unwrap().as_str(), Some("pending"));
    let (status, _) = f.json("PUT", &path, Some(r#"{"title": "", "body": "stale", "expectedRevision": 1}"#));
    assert_eq!(status, 409);
    let (_, list) = f.json("GET", "/api/notes", None);
    let notes = match list.get("notes").unwrap() {
        notes_host::Val::List(n) => n.clone(),
        _ => panic!(),
    };
    assert_eq!(notes[0].get("preview").unwrap().as_str(), Some("Düzenlendi"));
    let (status, _, exported) = f.request("GET", &format!("{}/export", path), None, &[], true);
    assert_eq!(status, 200);
    assert!(String::from_utf8(exported).unwrap().contains("Düzenlendi"));
}

#[test]
fn token_host_and_origin_required() {
    let f = Fixture::new();
    assert_eq!(f.request("GET", "/api/status", None, &[], false).0, 401);
    assert_eq!(f.request("GET", "/api/status", None, &[("Origin", "https://example.invalid")], true).0, 403);
    assert_eq!(f.request("GET", "/api/status", None, &[("Host", "example.invalid")], true).0, 403);
    let (status, value) = f.json("GET", "/api/status", None);
    assert_eq!(status, 200);
    assert_eq!(value.get("connected"), Some(&notes_host::Val::Bool(false)));
}

#[test]
fn startup_status_is_available_before_engine_and_requires_authorization() {
    let t = TempDir::new("starting");
    let startup = Arc::new(notes_engine::server::StartupBackend::new());
    startup.progress(7, 50);
    let server = AppServer::bind_starting(0, startup.clone(), "synthetic-local-secret", t.path(), None).unwrap();
    let accept = server.spawn();
    let request = |authorization: bool, path: &str| {
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        let auth = if authorization { "Authorization: Bearer synthetic-local-secret\r\n" } else { "" };
        write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{auth}Connection: close\r\n\r\n", server.port).unwrap();
        let mut raw = String::new(); stream.read_to_string(&mut raw).unwrap(); raw
    };
    assert!(request(false,"/api/status").starts_with("HTTP/1.0 401"));
    let status = request(true,"/api/status");
    assert!(status.starts_with("HTTP/1.0 200"));
    assert!(status.contains("\"completed\":7,\"total\":50"));
    assert!(request(true,"/api/notes").starts_with("HTTP/1.0 503"));
    startup.fail();
    assert!(request(true,"/api/status").contains("\"state\":\"failed\""));
    startup.ready(offline_engine(t.path()));
    assert!(request(true,"/api/status").contains("\"connected\""));
    server.close(); let _ = accept.join();
}

#[test]
fn json_content_type_and_private_file_traversal_rejected() {
    let f = Fixture::new();
    assert_eq!(f.request("POST", "/api/notes", Some("{}"), &[("Content-Type", "text/plain")], true).0, 415);
    assert_eq!(f.request("GET", "/%2e%2e/library.sqlite3", None, &[], true).0, 404);
    assert_eq!(f.request("DELETE", "/api/notes", None, &[], true).0, 404);
}

#[test]
fn invalid_unicode_returns_client_error_not_lost_existing_note() {
    let f = Fixture::new();
    let (status, _, _) = f.request("POST", "/api/notes", Some(r#"{"title": "bad", "body": "\ud800"}"#), &[], true);
    assert_eq!(status, 400);
    let (_, list) = f.json("GET", "/api/notes", None);
    assert_eq!(list.get("notes"), Some(&notes_host::Val::List(vec![])));
}

#[test]
fn offline_login_and_sync_requests_match_python_contract() {
    let f = Fixture::new();
    let (status, value) = f.json("POST", "/api/auth/login", Some("{}"));
    assert_eq!(status, 409);
    assert_eq!(value.get("error").unwrap().get("code").unwrap().as_str(), Some("offline_mode"));
    let (status, value) = f.json("POST", "/api/sync", Some("{}"));
    assert_eq!(status, 202);
    assert_eq!(value.get("started"), Some(&notes_host::Val::Bool(false)));
    let (status, value) = f.json("POST", "/api/auth/disconnect", Some("{}"));
    assert_eq!(status, 200);
    assert_eq!(value.get("connected"), Some(&notes_host::Val::Bool(false)));
}

#[test]
fn ink_draft_remaining_after_batch_survives_engine_restart() {
    let t = TempDir::new("ink-draft-restart");
    let engine = offline_engine(t.path());
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes-host/tests/fixtures/ink_objects_base.json")).unwrap();
    let doc = rs_store::val::loads(&text).unwrap();
    let id = engine.store.apply_document(&engine.host, &notes_host::Val::s("synthetic-restart"), &doc, None).unwrap();
    let projection = engine.ink(&id).unwrap();
    let revision = projection.get("revision").unwrap().clone();
    let notes_host::Val::List(pages) = projection.get("pages").unwrap() else { panic!() };
    let page = &pages[0];
    let notes_host::Val::List(sources) = page.get("strokes").unwrap() else { panic!() };
    let source = &sources[0];
    let point = |x| notes_host::Val::dict(vec![("x", notes_host::Val::Int(x)), ("y", notes_host::Val::Int(40)), ("pressure", notes_host::Val::Float(0.5)), ("timeMs", notes_host::Val::Int(0))]);
    let draft = |id: &str, x| notes_host::Val::dict(vec![
        ("id", notes_host::Val::s(id)), ("pageId", page.get("id").unwrap().clone()), ("sourceStrokeId", source.get("id").unwrap().clone()),
        ("kind", source.get("kind").unwrap().clone()), ("colorARGB", source.get("colorARGB").unwrap().clone()),
        ("size", source.get("size").unwrap().clone()), ("points", notes_host::Val::List(vec![point(x)])),
    ]);
    let first = draft("first", 40); let second = draft("second", 60); let third = draft("third", 80);
    engine.save_ink_draft(&id, &revision, &notes_host::Val::List(vec![first.clone()])).unwrap();
    engine.append_ink_draft(&id, &revision, &notes_host::Val::List(vec![second.clone()])).unwrap();
    let line = notes_host::Val::dict(vec![("pageId", first.get("pageId").unwrap().clone()), ("sourceStrokeId", first.get("sourceStrokeId").unwrap().clone()), ("points", first.get("points").unwrap().clone())]);
    engine.save_ink_from_draft(&id, &notes_host::Val::List(vec![line]), &revision, &notes_host::Val::List(vec![notes_host::Val::s("first")])).unwrap();
    // A later line is persisted while the immutable batch is still pending.
    engine.append_ink_draft(&id, &revision, &notes_host::Val::List(vec![third.clone()])).unwrap();
    engine.close();drop(engine);
    let restarted = offline_engine(t.path());
    assert_eq!(restarted.ink_draft(&id).unwrap().get("strokes"), Some(&notes_host::Val::List(vec![second, third])));
    assert_eq!(restarted.get(&id).unwrap().get("syncState"), Some(&notes_host::Val::s("pending")));
    restarted.close();
}


#[test]
fn ink_draft_append_and_batch_transfer_use_the_http_contract() {
    use notes_host::Val;
    let f=Fixture::new();
    let text=std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes-host/tests/fixtures/ink_objects_base.json")).unwrap();
    let doc=rs_store::val::loads(&text).unwrap();
    let id=f.engine.store.apply_document(&f.engine.host,&Val::s("synthetic-http-ink"),&doc,None).unwrap();
    let projection=f.engine.ink(&id).unwrap();let revision=projection.get("revision").unwrap().clone();
    let Val::List(pages)=projection.get("pages").unwrap() else {panic!()};let page=&pages[0];
    let Val::List(sources)=page.get("strokes").unwrap() else {panic!()};let source=&sources[0];
    let point=|x|Val::dict(vec![("x",Val::Int(x)),("y",Val::Int(40)),("pressure",Val::Float(0.5)),("timeMs",Val::Int(0))]);
    let draft=|id:&str,x|Val::dict(vec![("id",Val::s(id)),("pageId",page.get("id").unwrap().clone()),("sourceStrokeId",source.get("id").unwrap().clone()),("kind",source.get("kind").unwrap().clone()),("colorARGB",source.get("colorARGB").unwrap().clone()),("size",source.get("size").unwrap().clone()),("points",Val::List(vec![point(x)]))]);
    let first=draft("first",40);let second=draft("second",60);let path=format!("/api/notes/{id}/ink-draft");
    let body=|strokes:Vec<Val>|rs_store::val::dumps(&Val::dict(vec![("revision",revision.clone()),("strokes",Val::List(strokes))]),false,false).unwrap();
    assert_eq!(f.json("PUT",&path,Some(&body(vec![first.clone()]))).0,200);
    assert_eq!(f.json("POST",&path,Some(&body(vec![second.clone()]))).0,200);
    let stroke=Val::dict(vec![("pageId",first.get("pageId").unwrap().clone()),("sourceStrokeId",first.get("sourceStrokeId").unwrap().clone()),("points",first.get("points").unwrap().clone())]);
    let batch=rs_store::val::dumps(&Val::dict(vec![("expectedRevision",revision),("strokes",Val::List(vec![stroke])),("draftStrokeIds",Val::List(vec![Val::s("first")]))]),false,false).unwrap();
    assert_eq!(f.json("PUT",&format!("/api/notes/{id}/ink"),Some(&batch)).0,200);
    let (status,remaining)=f.json("GET",&path,None);assert_eq!(status,200);assert_eq!(remaining.get("strokes"),Some(&Val::List(vec![second])));
}
