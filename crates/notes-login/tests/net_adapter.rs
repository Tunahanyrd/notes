//! `NotesNet` (the notes_net adapter) against throwaway 127.0.0.1 HTTP
//! servers: status/headers/body mapping, read bound, no redirect following,
//! transport errors and Python's uncaught (`escaped`) exceptions.
use notes_login::http::{HttpRequest, Network};
use notes_login::net::NotesNet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Serve `reply` to every connection; returns (base URL, connection counter,
/// last request bytes).
fn serve(reply: &'static [u8]) -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (c, s) = (Arc::clone(&count), Arc::clone(&seen));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            c.fetch_add(1, Ordering::SeqCst);
            stream.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            let mut buf = vec![0u8; 65536];
            let mut got = Vec::new();
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: ").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if got.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            *s.lock().unwrap() = got;
            let _ = stream.write_all(reply);
        }
    });
    (format!("http://{addr}"), count, seen)
}

#[test]
fn response_status_headers_and_bounded_body() {
    let (base, count, seen) = serve(b"HTTP/1.1 201 Created\r\nContent-Type: application/json; charset=utf-8\r\nX-Two: a\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"a\":123456}");
    let request = HttpRequest::new(&format!("{base}/p?q=1"), Some(b"k=v".to_vec()), &[("Content-Type", "application/x-www-form-urlencoded"), ("x-osp-clientmodel", "LinuxResearch"), ("User-Agent", "SamsungNotesLinuxResearch/0.1")], Some("POST"));
    let response = NotesNet.request(&request, Duration::from_secs(5), 5).unwrap();
    assert_eq!(response.status, 201);
    assert_eq!(response.header("content-type"), Some("application/json; charset=utf-8"));
    assert_eq!(response.content_type(), "application/json");
    assert_eq!(response.body, b"{\"a\":");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let wire = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
    assert!(wire.starts_with("POST /p?q=1 HTTP/1.1\r\n"), "{wire}");
    // urllib's do_open title-cases header names on the wire.
    assert!(wire.contains("\r\nContent-Type: application/x-www-form-urlencoded\r\n"));
    assert!(wire.contains("\r\nX-Osp-Clientmodel: LinuxResearch\r\n"));
    assert!(wire.contains("\r\nAccept-Encoding: identity\r\n"));
    assert!(wire.contains("\r\nContent-Length: 3\r\n"));
    assert!(wire.ends_with("\r\n\r\nk=v"));
}

#[test]
fn redirect_is_returned_not_followed() {
    let (base, count, _) = serve(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:9/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let response = NotesNet.request(&HttpRequest::new(&format!("{base}/x"), None, &[], None), Duration::from_secs(5), 100).unwrap();
    assert_eq!(response.status, 307);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn refused_connection_is_a_caught_transport_error() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = NotesNet.request(&HttpRequest::new(&format!("http://127.0.0.1:{port}/"), None, &[], None), Duration::from_secs(2), 10).unwrap_err();
    assert!(!err.escaped);
    assert!(err.head.is_none());
}

#[test]
fn short_body_is_returned_like_http_client_read_amt() {
    // HTTPResponse.read(amt) returns what arrived and does not raise.
    let (base, _, _) = serve(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort");
    let response = NotesNet.request(&HttpRequest::new(&format!("{base}/"), None, &[], None), Duration::from_secs(5), 1000).unwrap();
    assert_eq!(response.body, b"short");
}

#[test]
fn bad_status_line_is_escaped() {
    let (base, _, _) = serve(b"garbage\r\n\r\n");
    let err = NotesNet.request(&HttpRequest::new(&format!("{base}/"), None, &[], None), Duration::from_secs(5), 1000).unwrap_err();
    assert!(err.escaped, "BadStatusLine is not caught by the Python callers: {}", err.message);
}

#[test]
fn escaped_failure_keeps_claim_and_writes_no_receipt() {
    let (base, _, _) = serve(b"garbage\r\n\r\n");
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("net-escape-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let request = HttpRequest::new(&format!("{base}/"), Some(b"x".to_vec()), &[], Some("POST"));
    let rt = notes_login::SystemRuntime;
    assert!(notes_login::http::perform_once(&NotesNet, &rt, &dir, "uat", ".account-code-claim", &request).is_err());
    assert!(dir.join(".account-code-claim").exists());
    assert!(!dir.join("uat-receipt.json").exists());
}
