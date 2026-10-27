//! Deterministic mutation fuzzing of the loopback API front end: request
//! line/header parsing, URL splitting, Host/Origin/bearer validation, body
//! limits, JSON bodies and routing (before and after authentication). Any
//! local process or web page can send these bytes; the answer must be a
//! reply or a closed connection, never a panic. The backend is a stub.
use std::io::{BufReader, Cursor};
use std::panic::{catch_unwind, AssertUnwindSafe};

use _engine::pyhttp::{self, Incoming};
use _engine::server_core::{self, Backend, Call, Config, Conn, Failure};

struct Stub;
impl Backend for Stub {
    fn json(&self, _: Call) -> Result<Vec<u8>, Failure> {
        Ok(b"{}".to_vec())
    }
    fn bytes_or_none(&self, _: Call) -> Result<Option<Vec<u8>>, Failure> {
        Ok(Some(vec![0x89, b'P', b'N', b'G']))
    }
    fn bytes_and_flag(&self, _: Call) -> Result<(Option<Vec<u8>>, bool), Failure> {
        Ok((None, true))
    }
    fn note_text(&self, _: Call) -> Result<(String, String), Failure> {
        Ok(("Başlık".into(), "Gövde\nikinci".into()))
    }
    fn max_pdf_bytes(&self) -> Result<u64, Failure> {
        Ok(1 << 20)
    }
}

const TOKEN: &str = "synthetic-test-token-0123456789abcdef";
const PORT: u16 = 41234;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

const TOKENS: &[&str] = &[
    "%", "%zz", "%ff", "%00", "%2e%2e/", "../", "//", "?", "#", "&", "=", ";", "\r\n", "\n", " ", "\t", "\u{0}",
    "Content-Length: 99999999\r\n", "Content-Length: -1\r\n", "Content-Length: 5\r\n", "Transfer-Encoding: chunked\r\n",
    "{", "}", "[", "]", "\"", "\\u", "\\ud800", "1e999", "-0", "99999999999999999999999", "null", "é", "\u{1f680}",
    "00000000-0000-4000-8000-000000000001", "/api/notes/", "/api/folders/", "HTTP/0.9", "HTTP/1.1", "GET", "PUT",
];

fn seeds() -> Vec<Vec<u8>> {
    let host = format!("Host: 127.0.0.1:{PORT}\r\nAuthorization: Bearer {TOKEN}\r\n");
    let note = "00000000-0000-4000-8000-000000000001";
    let mut out = vec![
        format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n\r\n"),
        format!("GET /assets/app.js HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n\r\n"),
        format!("GET /local-fonts/x.ttf HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n\r\n"),
        format!("GET /api/status HTTP/1.1\r\n{host}\r\n"),
        format!("GET /api/notes?q=a%20b&folder=x&view=all HTTP/1.1\r\n{host}\r\n"),
        format!("GET /api/templates?note={note} HTTP/1.1\r\n{host}\r\n"),
        format!("GET /api/notes/{note}/preview?page=0&revision=1 HTTP/1.1\r\n{host}\r\n"),
        format!("GET /api/notes/{note}/thumbnail?revision=1 HTTP/1.1\r\n{host}\r\n"),
        format!("GET /api/notes/{note}/export HTTP/1.1\r\n{host}\r\n"),
        format!("PATCH /api/folders/a%2Fb HTTP/1.1\r\n{host}Content-Type: application/json\r\nContent-Length: 14\r\n\r\n{{\"name\": \"x\"}}"),
        format!("PUT /api/preferences HTTP/1.1\r\n{host}Content-Type: application/json\r\nContent-Length: 17\r\n\r\n{{\"zoom\": [1, 2]}}"),
        format!("POST /api/notes/editor HTTP/1.1\r\n{host}Content-Type: application/json\r\nContent-Length: 30\r\n\r\n{{\"title\": \"t\", \"model\": null}}"),
        format!("PUT /api/notes/{note}/ink HTTP/1.1\r\n{host}Content-Type: application/json\r\nContent-Length: 27\r\n\r\n{{\"expectedRevision\": 1e400}}"),
        format!("POST /api/notes/{note}/pdf HTTP/1.1\r\n{host}Content-Type: application/pdf\r\nContent-Length: 8\r\n\r\n%PDF-1.4"),
        format!("OPTIONS /api/status HTTP/1.1\r\n{host}Origin: http://evil.example\r\n\r\n"),
    ];
    out.push("GET /\r\n".into());
    out.into_iter().map(String::into_bytes).collect()
}

fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut d = src.to_vec();
    for _ in 0..1 + rng.below(3) {
        if d.is_empty() {
            d.push(b'G');
        }
        match rng.below(5) {
            0 => {
                let i = rng.below(d.len());
                d[i] ^= 1 << rng.below(8);
            }
            1 | 2 => {
                let i = rng.below(d.len());
                let end = (i + rng.below(6)).min(d.len());
                d.splice(i..end, TOKENS[rng.below(TOKENS.len())].bytes());
            }
            3 => {
                let n = rng.below(d.len());
                d.truncate(n);
            }
            _ => {
                let a = rng.below(d.len());
                let b = (a + rng.below(32)).min(d.len());
                let seg = d[a..b].to_vec();
                d.splice(b..b, seg);
            }
        }
    }
    d
}

fn handle(cfg: &Config, raw: &[u8]) {
    let mut reader = BufReader::new(Cursor::new(raw.to_vec()));
    if let Ok(Incoming::Request(req)) = pyhttp::read_request(&mut reader) {
        let mut conn = Conn { reader, writer: Vec::new(), req };
        server_core::dispatch(cfg, &Stub, &mut conn);
    }
}

#[test]
fn hostile_requests_never_panic() {
    let root = std::env::temp_dir().join(format!("rs-engine-hostile-{}", std::process::id()));
    let static_dir = root.join("static");
    std::fs::create_dir_all(static_dir.join("assets")).unwrap();
    std::fs::write(static_dir.join("index.html"), "<!doctype html>").unwrap();
    std::fs::write(static_dir.join("assets/app.js"), "1").unwrap();
    let cfg = Config { token: TOKEN.into(), port: PORT, static_dir: server_core::realpath(&static_dir), font_dir: root.join("fonts") };
    let seeds = seeds();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let iters = std::env::var("NOTES_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
    let mut panics: Vec<String> = Vec::new();
    for _ in 0..iters {
        let pick = rng.below(seeds.len());
        let input = mutate(&mut rng, &seeds[pick]);
        if catch_unwind(AssertUnwindSafe(|| handle(&cfg, &input))).is_err() {
            panics.push(String::from_utf8_lossy(&input).into_owned());
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(panics.is_empty(), "{} panics, first: {:?}", panics.len(), panics.first());
}
