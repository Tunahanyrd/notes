//! The loopback HTTP API exactly as a connection is served, minus the socket:
//! `pyhttp::read_request` (request line, header block) and
//! `server_core::dispatch` (Host/Origin/token validation, routing, path
//! parameters, query strings, bounded JSON and binary bodies, static files).
//! The backend is a stub that converts JSON arguments like the engine and
//! runs the pure validators; nothing touches storage or the network.
//! Input: one selector byte, then the request bytes. With bit 0 set the
//! harness prepends a request line and valid Host/Authorization headers so
//! the fuzzer reaches the routes quickly.
#![no_main]
use libfuzzer_sys::fuzz_target;
use rs_engine::pyhttp::{self, Incoming};
use rs_engine::server_core::{self, Arg, Backend, Call, Config, Conn, Failure};
use std::io::{BufReader, Cursor};
use std::sync::OnceLock;

const TOKEN: &str = "fuzz-token-0123456789";
const PORT: u16 = 8123;

struct Stub;

fn arg_val(arg: &Arg) -> notes_host::Val {
    match arg {
        Arg::Json(v) => notes_engine::util::from_json(v),
        Arg::Str(s) => notes_host::Val::s(s.clone()),
        Arg::Int(i) => notes_host::Val::Int(*i),
        Arg::Bytes(b) => notes_host::Val::Bytes(b.clone()),
        Arg::KwFloat(_, f) => notes_host::Val::Float(*f),
        Arg::None => notes_host::Val::Null,
    }
}

impl Backend for Stub {
    fn json(&self, call: Call) -> Result<Vec<u8>, Failure> {
        let args: Vec<notes_host::Val> = call.args.iter().map(arg_val).collect();
        let first = args.first().cloned().unwrap_or(notes_host::Val::Null);
        match call.method {
            "set_preferences" => {
                let _ = rs_store::preferences::validate_preferences(&first);
            }
            "set_shortcuts" => {
                let _ = rs_store::shortcuts::prepare(&first, || Ok(rs_store::shortcuts::default_shortcuts()));
            }
            "create_editor" | "save_editor" => {
                for a in &args {
                    let _ = notes_host::areas::normalize_model(a);
                }
            }
            "save_ink" | "save_ink_draft" => {
                let _ = notes_host::areas::samples_from_points(args.get(1).unwrap_or(&notes_host::Val::Null));
            }
            _ => {}
        }
        notes_engine::util::dumps(&notes_host::Val::List(args)).map_err(|_| Failure::Internal)
    }
    fn bytes_or_none(&self, _call: Call) -> Result<Option<Vec<u8>>, Failure> {
        Ok(None)
    }
    fn bytes_and_flag(&self, _call: Call) -> Result<(Option<Vec<u8>>, bool), Failure> {
        Ok((Some(b"\x89PNG".to_vec()), true))
    }
    fn note_text(&self, _call: Call) -> Result<(String, String), Failure> {
        Ok(("Başlık".into(), "Gövde".into()))
    }
    fn max_pdf_bytes(&self) -> Result<u64, Failure> {
        Ok(64 * 1024)
    }
}

fn config() -> &'static Config {
    static CFG: OnceLock<Config> = OnceLock::new();
    CFG.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("notes-fuzz-static-{}", std::process::id()));
        let static_dir = base.join("dist");
        let font_dir = base.join("fonts");
        let _ = std::fs::create_dir_all(static_dir.join("assets"));
        let _ = std::fs::create_dir_all(&font_dir);
        let _ = std::fs::write(static_dir.join("index.html"), b"<!doctype html><title>Notes</title>");
        let _ = std::fs::write(static_dir.join("assets/app.js"), b"console.log(1)");
        let static_dir = server_core::realpath(&static_dir);
        Config { token: TOKEN.into(), port: PORT, static_dir, font_dir }
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((sel, rest)) = notes_fuzz::selector(data) else { return };
    let mut raw = Vec::with_capacity(rest.len() + 128);
    if sel & 1 == 1 {
        // `<METHOD> <path>` comes from the input up to the first newline.
        let (line, tail) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &[][..]),
        };
        raw.extend_from_slice(line);
        raw.extend_from_slice(b" HTTP/1.1\r\nHost: 127.0.0.1:8123\r\nAuthorization: Bearer ");
        raw.extend_from_slice(TOKEN.as_bytes());
        raw.extend_from_slice(b"\r\n");
        raw.extend_from_slice(tail);
    } else {
        raw.extend_from_slice(rest);
    }
    let mut reader = BufReader::with_capacity(8192, Cursor::new(raw));
    match pyhttp::read_request(&mut reader) {
        Err(_) | Ok(Incoming::Close) => {}
        Ok(Incoming::Error(reply)) => {
            let _ = pyhttp::error_response(&reply);
        }
        Ok(Incoming::Request(req)) => {
            let mut conn = Conn { reader, writer: Vec::new(), req };
            server_core::dispatch(config(), &Stub, &mut conn);
        }
    }
});
