//! Explicit preview retry is a state-changing, authenticated request.
use _engine::pyhttp::{self, Incoming};
use _engine::server_core::{self, Arg, Backend, Call, Config, Conn, Failure};
use std::io::{BufReader, Cursor};
use std::sync::Mutex;
struct Stub(Mutex<Vec<Call>>);
impl Backend for Stub {
    fn json(&self, call: Call) -> Result<Vec<u8>, Failure> {
        self.0.lock().unwrap().push(call);
        Ok(br#"{"pending":true,"pages":[],"providerAvailable":false}"#.to_vec())
    }
    fn bytes_or_none(&self, _: Call) -> Result<Option<Vec<u8>>, Failure> {
        Ok(None)
    }
    fn bytes_and_flag(&self, _: Call) -> Result<(Option<Vec<u8>>, bool), Failure> {
        Ok((None, true))
    }
    fn note_text(&self, _: Call) -> Result<(String, String), Failure> {
        Ok((String::new(), String::new()))
    }
    fn max_pdf_bytes(&self) -> Result<u64, Failure> {
        Ok(1024)
    }
}
fn send(backend: &Stub, body: &str, token: bool) -> String {
    let port = 41234;
    let auth = "synthetic-test-token-0123456789abcdef";
    let raw=format!("POST /api/notes/00000000-0000-4000-8000-000000000001/pages HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", if token { format!("Authorization: Bearer {auth}\r\n") } else { String::new() }, body.len());
    let mut reader = BufReader::new(Cursor::new(raw.into_bytes()));
    let Incoming::Request(req) = pyhttp::read_request(&mut reader).unwrap() else {
        panic!()
    };
    let mut conn = Conn {
        reader,
        writer: Vec::new(),
        req,
    };
    let cfg = Config {
        token: auth.into(),
        port,
        static_dir: std::env::temp_dir(),
        font_dir: std::env::temp_dir(),
    };
    server_core::dispatch(&cfg, backend, &mut conn);
    String::from_utf8(conn.writer).unwrap()
}
#[test]
fn retry_requires_auth_and_empty_object() {
    let stub = Stub(Mutex::new(vec![]));
    assert!(send(&stub, "{}", false).starts_with("HTTP/1.0 401"));
    assert!(send(&stub, r#"{"unexpected":true}"#, true).starts_with("HTTP/1.0 400"));
    assert!(stub.0.lock().unwrap().is_empty());
    assert!(send(&stub, "{}", true).starts_with("HTTP/1.0 200"));
    let calls = stub.0.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "retry_inventory");
    assert_eq!(
        calls[0].args,
        vec![Arg::Str("00000000-0000-4000-8000-000000000001".into())]
    );
}
