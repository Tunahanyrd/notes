//! Port of tests/test_app_server.py against the pure server (raw sockets).
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;

use common::*;
use notes_engine::server::AppServer;

struct Fixture {
    _t: TempDir,
    server: AppServer,
    _accept: std::thread::JoinHandle<()>,
}

impl Fixture {
    fn new() -> Fixture {
        let t = TempDir::new("app");
        let engine = offline_engine(t.path());
        let server = AppServer::bind(0, engine, "synthetic-local-secret", t.path(), Some(t.path().join("fonts"))).unwrap();
        let accept = server.spawn();
        Fixture { _t: t, server, _accept: accept }
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
