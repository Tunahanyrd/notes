//! Hostile response bytes (status line, headers, chunked framing,
//! Content-Length, gzip) must end in Ok or Err, never a panic or an
//! unbounded read. Deterministic mutations; local 127.0.0.1 fakes only.
use crate::client::Client;
use crate::{gzip, HttpRequest};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::time::Duration;

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
    "0", "-1", "-5", "ffffffffffffffffffffffffffffffff", "-7fffffffffffffffffffffffffffffff", "0x10", "1_0",
    "99999999999999999999999999999999999999", "+3", " ", "", "\r\n", "\n", "HTTP/1.1", "HTTP/2", "chunked", "gzip",
    "Content-Length: -1\r\n", "Transfer-Encoding: chunked\r\n", ";ext=1", "100", "199", "204", "1000",
];

fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut d = src.to_vec();
    for _ in 0..1 + rng.below(3) {
        if d.is_empty() {
            d.push(b'H');
        }
        match rng.below(5) {
            0 => {
                let i = rng.below(d.len());
                d[i] ^= 1 << rng.below(8);
            }
            1 => {
                let i = rng.below(d.len());
                let t = TOKENS[rng.below(TOKENS.len())].as_bytes().to_vec();
                let end = (i + rng.below(8)).min(d.len());
                d.splice(i..end, t);
            }
            2 => {
                let n = rng.below(d.len());
                d.truncate(n);
            }
            3 => {
                let a = rng.below(d.len());
                let b = (a + rng.below(40)).min(d.len());
                let seg = d[a..b].to_vec();
                d.splice(b..b, seg);
            }
            _ => {
                let a = rng.below(d.len());
                let b = (a + rng.below(16)).min(d.len());
                d.drain(a..b);
            }
        }
    }
    d
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn seeds() -> Vec<Vec<u8>> {
    let body = gz(b"{\"ok\": true, \"items\": [1, 2, 3]}");
    let mut gz_response = format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    gz_response.extend(&body);
    vec![
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"a\": true}".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;x=y\r\n world\r\n0\r\nTrailer: 1\r\n\r\n".to_vec(),
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.0 404 Not Found\r\nContent-Length: 3\r\n\r\nno!".to_vec(),
        b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nuntil eof".to_vec(),
        gz_response,
    ]
}

/// Serve each input once on a loopback listener; the client reads with a
/// bound like `send_once` does.
#[test]
fn hostile_responses_never_panic() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for input in rx {
            let Ok((mut s, _)) = listener.accept() else { return };
            let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(&input);
        }
    });
    let client = Client::new(Duration::from_secs(2)).unwrap();
    let url = format!("http://127.0.0.1:{port}/x");
    let seeds = seeds();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let iters = std::env::var("NOTES_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    let mut panics = Vec::new();
    for i in 0..iters {
        let pick = rng.below(seeds.len());
        let input = mutate(&mut rng, &seeds[pick]);
        tx.send(input.clone()).unwrap();
        let ok = catch_unwind(AssertUnwindSafe(|| {
            let request = HttpRequest::new(Some("GET"), &url, &[], None);
            if let Ok(mut response) = client.open(&request) {
                let encoding = response.header("Content-Encoding").unwrap_or("").to_ascii_lowercase();
                if let Ok(raw) = response.read(64 * 1024 + 1) {
                    assert!(raw.len() <= 64 * 1024 + 1, "read past its bound");
                    if encoding == "gzip" {
                        let _ = gzip::read_bounded(&raw, 64 * 1024 + 1);
                    }
                }
            }
        }))
        .is_ok();
        if !ok {
            panics.push((i, String::from_utf8_lossy(&input).into_owned()));
        }
    }
    assert!(panics.is_empty(), "panics: {panics:?}");
}

#[test]
fn hostile_gzip_never_panics_and_stays_bounded() {
    let seeds = [gz(b"hello hello hello hello"), gz(&vec![0u8; 1 << 20]), gz(b"")];
    let mut rng = Rng(0x0dd_ba11_cafe_f00d);
    for _ in 0..3000 {
        let pick = rng.below(seeds.len());
        let input = mutate(&mut rng, &seeds[pick]);
        let out = catch_unwind(AssertUnwindSafe(|| gzip::read_bounded(&input, 4097)));
        let out = out.expect("gzip decoder panicked");
        if let Ok(data) = out {
            assert!(data.len() <= 4097 + 8192, "decoded far past the bound");
        }
    }
    // A bomb (64 MiB of zeros) is cut at the bound.
    let bomb = gz(&vec![0u8; 64 << 20]);
    assert!(gzip::read_bounded(&bomb, 1 << 20).unwrap().len() <= (1 << 20) + 8192);
}

/// A negative chunk size made `read(amt)` fall back to reading the whole
/// stream to EOF, ignoring the caller's bound (memory exhaustion from one
/// response). It is now a framing error answered without draining the
/// connection.
#[test]
fn negative_chunk_size_is_rejected_not_read_to_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n-1\r\n");
        // Keep streaming: an unbounded reader would block here until EOF.
        for _ in 0..30 {
            if s.write_all(&vec![b'x'; 64 * 1024]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let client = Client::new(Duration::from_secs(5)).unwrap();
    let request = HttpRequest::new(Some("GET"), &format!("http://127.0.0.1:{port}/"), &[], None);
    let mut response = client.open(&request).unwrap();
    let started = std::time::Instant::now();
    let result = response.read(1024);
    assert!(result.is_err(), "negative chunk size must be a framing error");
    assert!(started.elapsed() < Duration::from_secs(2), "the body was drained to EOF");
}
