use std::io::{BufReader, Cursor};
use std::sync::Mutex;
use _engine::pyhttp::{self, Incoming};
use _engine::server_core::{self, Arg, Backend, Call, Config, Conn, Failure};

struct Stub(Mutex<Vec<Call>>);
impl Backend for Stub {
    fn json(&self, _: Call) -> Result<Vec<u8>, Failure> { Ok(b"{}".to_vec()) }
    fn bytes_or_none(&self, call: Call) -> Result<Option<Vec<u8>>, Failure> { self.0.lock().unwrap().push(call); Ok(Some(b"png".to_vec())) }
    fn ink_layer(&self, call: Call) -> Result<Option<(Vec<u8>,String)>, Failure> {
        self.0.lock().unwrap().push(call);Ok(Some((b"png".to_vec(), "a".repeat(64))))
    }
    fn text_ink_layer(&self, call: Call) -> Result<Option<(Vec<u8>,String)>, Failure> {
        self.0.lock().unwrap().push(call);Ok(Some((b"png".to_vec(), "b".repeat(64))))
    }
    fn bytes_and_flag(&self, _: Call) -> Result<(Option<Vec<u8>>,bool),Failure> { Ok((None,true)) }
    fn note_text(&self, _: Call) -> Result<(String,String),Failure> { Ok((String::new(),String::new())) }
    fn max_pdf_bytes(&self) -> Result<u64,Failure> { Ok(1024) }
}

#[test]
fn text_composites_are_revision_bound_and_reject_tile_parameters() {
    let stub=Stub(Mutex::new(vec![]));
    for variant in ["notext", "fulltext"] {
        assert!(send(&stub,&format!("page=1&revision=3&variant={variant}")).starts_with("HTTP/1.0 200"));
        assert!(send(&stub,&format!("page=1&variant={variant}&scale=2")).starts_with("HTTP/1.0 400"));
    }
    let calls=stub.0.lock().unwrap();
    assert_eq!(calls.len(),2);
    assert_eq!(calls[0].method,"notext");
    assert_eq!(calls[1].method,"fulltext");
    assert_eq!(calls[0].args[1..], [Arg::Int(1),Arg::Int(3)]);
}

#[test]
fn text_ink_tiles_use_snapshot_and_validate_scale() {
    let stub=Stub(Mutex::new(vec![]));
    assert!(send(&stub,"page=0&revision=2&variant=notextbase").starts_with("HTTP/1.0 200"));
    assert!(send(&stub,&format!("page=0&revision=2&layer=textink&scale=1.5&tile=0,1&snapshot={}","b".repeat(64))).starts_with("HTTP/1.0 200"));
    assert!(send(&stub,"page=0&layer=textink&scale=1.25&tile=0,0").starts_with("HTTP/1.0 400"));
    let calls=stub.0.lock().unwrap();assert_eq!(calls.len(),2);assert_eq!(calls[0].method,"text_ink_layer");assert_eq!(calls[1].args[3..6],[Arg::Int(3),Arg::Int(0),Arg::Int(1)]);
}
fn send(backend:&Stub, query:&str) -> String {
    let port=41234;let token="synthetic-test-token-0123456789abcdef";
    let raw=format!("GET /api/notes/00000000-0000-4000-8000-000000000001/preview?{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\r\n");
    let mut reader=BufReader::new(Cursor::new(raw.into_bytes()));
    let Incoming::Request(req)=pyhttp::read_request(&mut reader).unwrap() else { panic!() };
    let mut conn=Conn{reader,writer:Vec::new(),req};
    let cfg=Config{token:token.into(),port,static_dir:std::env::temp_dir(),font_dir:std::env::temp_dir()};
    server_core::dispatch(&cfg,backend,&mut conn);
    String::from_utf8(conn.writer).unwrap()
}
#[test]
fn layer_requests_validate_geometry_and_preserve_snapshot_token() {
    let stub=Stub(Mutex::new(vec![]));
    let response=send(&stub,"page=0&revision=3&variant=noink");
    assert!(response.starts_with("HTTP/1.0 200"),"{response}");
    assert!(response.contains("X-Ink-Tiles: 1"));
    assert!(response.contains(&format!("X-Ink-Snapshot: {}", "a".repeat(64))));
    let response=send(&stub,&format!("page=0&revision=3&layer=ink&scale=1.5&tile=1,2&snapshot={}","b".repeat(64)));
    assert!(response.starts_with("HTTP/1.0 200"),"{response}");
    let calls=stub.0.lock().unwrap();
    assert_eq!(calls.len(),2);
    assert_eq!(calls[1].args,vec![Arg::Str("00000000-0000-4000-8000-000000000001".into()),Arg::Int(0),Arg::Int(3),Arg::Int(3),Arg::Int(1),Arg::Int(2),Arg::Str("b".repeat(64))]);
    drop(calls);
    assert!(send(&stub,"page=0&layer=ink&scale=1&tile=0,0").starts_with("HTTP/1.0 200"));
    assert_eq!(stub.0.lock().unwrap()[2].args[2],Arg::Int(-1));
    for query in ["page=0&revision=3&layer=ink&scale=2.5&tile=0,0", "page=0&revision=3&layer=ink&scale=1&tile=-1,0", "page=0&revision=3&layer=ink&scale=1&tile=0,0&tile=0,1", "page=0&revision=3&variant=noink&layer=ink", "page=0&revision=3&variant=noink&snapshot=bad"] {
        assert!(send(&stub,query).starts_with("HTTP/1.0 400"),"{query}");
    }
    assert_eq!(stub.0.lock().unwrap().len(),3);
}
