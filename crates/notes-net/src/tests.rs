//! Offline tests against local fakes only (127.0.0.1): a plain TCP server
//! for wire bytes and a TLS server with a throwaway CA made at test time.
//! With `NOTES_REFERENCE_ORACLE=1`, `python3` and the frozen historical
//! reference (reference/python/tools/notes_cloud.py), the same scenarios also
//! run through CPython (urllib / send_once / gzip) and the results must be
//! byte-identical; otherwise those oracle comparisons are skipped.
use crate::client::Client;
use crate::send_once::{self, SendError};
use crate::{gzip, Family, HttpRequest, NetError};
use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::ssl::{NameType, SslAcceptor, SslMethod, SslVersion};
use openssl::x509::extension::{
    AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName, SubjectKeyIdentifier,
};
use openssl::x509::{X509Name, X509};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- helpers

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join("notes-net-tests");
    std::fs::create_dir_all(&base).unwrap();
    let dir = base.join(format!("{tag}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn client() -> Client {
    Client::new(Duration::from_secs(5)).unwrap()
}

trait Conn: Read + Write {}
impl<T: Read + Write> Conn for T {}

type Handler = Arc<dyn Fn(&mut dyn Conn) + Send + Sync>;

/// Reads one HTTP request (head + Content-Length or chunked body).
fn read_request(s: &mut dyn Conn) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end;
    loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            head_end = i + 4;
            break;
        }
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return data,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
    }
    let head = String::from_utf8_lossy(&data[..head_end]).to_ascii_lowercase();
    let length = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok());
    let chunked = head.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"));
    loop {
        let body = &data[head_end..];
        let done = match (length, chunked) {
            (Some(n), _) => body.len() >= n,
            (None, true) => body.ends_with(b"0\r\n\r\n"),
            _ => true,
        };
        if done {
            return data;
        }
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return data,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
    }
}

struct Fake {
    port: u16,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    sni: Arc<Mutex<Vec<Option<String>>>>,
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

impl Fake {
    fn url(&self, scheme: &str, host: &str, path: &str) -> String {
        format!("{scheme}://{host}:{}{path}", self.port)
    }
    fn requests(&self) -> Vec<Vec<u8>> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(acceptor: Option<SslAcceptor>, handler: Handler) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fake = Fake {
        port,
        requests: Arc::new(Mutex::new(Vec::new())),
        sni: Arc::new(Mutex::new(Vec::new())),
        connections: Arc::new(AtomicUsize::new(0)),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let (requests, sni, connections, stop) =
        (fake.requests.clone(), fake.sni.clone(), fake.connections.clone(), fake.stop.clone());
    let acceptor = acceptor.map(Arc::new);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            connections.fetch_add(1, Ordering::SeqCst);
            let (requests, sni, handler, acceptor) = (requests.clone(), sni.clone(), handler.clone(), acceptor.clone());
            std::thread::spawn(move || {
                conn.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let handle = |s: &mut dyn Conn| {
                    let request = read_request(s);
                    requests.lock().unwrap().push(request);
                    handler(s);
                    let _ = s.flush();
                };
                match acceptor {
                    Some(acceptor) => {
                        if let Ok(mut tls) = acceptor.accept(conn) {
                            sni.lock().unwrap().push(tls.ssl().servername(NameType::HOST_NAME).map(str::to_string));
                            handle(&mut tls);
                        }
                    }
                    None => {
                        let mut conn = conn;
                        handle(&mut conn);
                    }
                }
            });
        }
    });
    fake
}

fn respond(bytes: Vec<u8>) -> Handler {
    Arc::new(move |s: &mut dyn Conn| {
        let _ = s.write_all(&bytes);
    })
}

fn plain(bytes: Vec<u8>) -> Fake {
    serve(None, respond(bytes))
}

fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn http(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\n").into_bytes();
    for (k, v) in headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

fn chunked(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(format!("{:x};ext=1\r\n", p.len()).as_bytes());
        out.extend_from_slice(p);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\nX-Trailer: 1\r\n\r\n");
    out
}

// ---------------------------------------------------------------- TLS fixtures

fn ec_key() -> PKey<Private> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
}

fn serial() -> openssl::asn1::Asn1Integer {
    let mut bn = BigNum::new().unwrap();
    bn.rand(127, MsbOption::MAYBE_ZERO, false).unwrap();
    bn.to_asn1_integer().unwrap()
}

fn name(cn: &str) -> X509Name {
    let mut n = X509Name::builder().unwrap();
    n.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
    n.build()
}

struct Ca {
    cert: X509,
    key: PKey<Private>,
}

fn make_ca(cn: &str) -> Ca {
    let key = ec_key();
    let mut b = X509::builder().unwrap();
    b.set_version(2).unwrap();
    b.set_serial_number(&serial()).unwrap();
    b.set_subject_name(&name(cn)).unwrap();
    b.set_issuer_name(&name(cn)).unwrap();
    b.set_pubkey(&key).unwrap();
    b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::days_from_now(2).unwrap()).unwrap();
    b.append_extension(BasicConstraints::new().critical().ca().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap()).unwrap();
    let ski = SubjectKeyIdentifier::new().build(&b.x509v3_context(None, None)).unwrap();
    b.append_extension(ski).unwrap();
    b.sign(&key, MessageDigest::sha256()).unwrap();
    Ca { cert: b.build(), key }
}

fn make_leaf(ca: &Ca, dns: &[&str], ips: &[&str]) -> (X509, PKey<Private>) {
    let key = ec_key();
    let mut b = X509::builder().unwrap();
    b.set_version(2).unwrap();
    b.set_serial_number(&serial()).unwrap();
    b.set_subject_name(&name("notes-net test server")).unwrap();
    b.set_issuer_name(ca.cert.subject_name()).unwrap();
    b.set_pubkey(&key).unwrap();
    b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::days_from_now(2).unwrap()).unwrap();
    b.append_extension(BasicConstraints::new().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().digital_signature().build().unwrap()).unwrap();
    b.append_extension(ExtendedKeyUsage::new().server_auth().build().unwrap()).unwrap();
    let mut san = SubjectAlternativeName::new();
    for d in dns {
        san.dns(d);
    }
    for i in ips {
        san.ip(i);
    }
    let san = san.build(&b.x509v3_context(Some(&ca.cert), None)).unwrap();
    b.append_extension(san).unwrap();
    let ski = SubjectKeyIdentifier::new().build(&b.x509v3_context(Some(&ca.cert), None)).unwrap();
    b.append_extension(ski).unwrap();
    let aki = AuthorityKeyIdentifier::new().keyid(true).build(&b.x509v3_context(Some(&ca.cert), None)).unwrap();
    b.append_extension(aki).unwrap();
    b.sign(&ca.key, MessageDigest::sha256()).unwrap();
    (b.build(), key)
}

fn acceptor(cert: &X509, key: &PKey<Private>, max: Option<SslVersion>) -> SslAcceptor {
    let mut a = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    a.set_private_key(key).unwrap();
    a.set_certificate(cert).unwrap();
    a.check_private_key().unwrap();
    if let Some(max) = max {
        a.set_min_proto_version(Some(SslVersion::TLS1)).unwrap();
        a.set_max_proto_version(Some(max)).unwrap();
        a.set_cipher_list("DEFAULT:@SECLEVEL=0").unwrap();
    }
    a.build()
}

fn tls_fake(ca: &Ca, dns: &[&str], ips: &[&str], handler: Handler) -> Fake {
    let (cert, key) = make_leaf(ca, dns, ips);
    serve(Some(acceptor(&cert, &key, None)), handler)
}

// ---------------------------------------------------------------- python oracle

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

/// `python3` with the repository's tools importable, or None (oracle tests skip).
fn python() -> Option<Command> {
    let root = repo_root().join("reference/python");
    if std::env::var_os("NOTES_REFERENCE_ORACLE").is_none_or(|v| v != "1") {
        eprintln!("python oracle skipped: NOTES_REFERENCE_ORACLE=1 not set");
        return None;
    }
    if !root.join("tools/notes_cloud.py").is_file() {
        eprintln!("python oracle skipped: reference/python/tools/notes_cloud.py not found");
        return None;
    }
    if Command::new("python3").arg("-c").arg("pass").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("python oracle skipped: python3 not available");
        return None;
    }
    let mut cmd = Command::new("python3");
    cmd.current_dir(&root)
        .env_remove("NOTES_IMPL")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONPATH", format!("{}:{}", root.join("tools").display(), root.display()))
        .env("XDG_STATE_HOME", scratch("xdg-state"))
        .env_remove("http_proxy").env_remove("https_proxy").env_remove("HTTP_PROXY").env_remove("HTTPS_PROXY")
        .env_remove("all_proxy").env_remove("ALL_PROXY").env_remove("SSLKEYLOGFILE");
    Some(cmd)
}

fn run_python(script: &str, args: &[&str]) -> Option<String> {
    let mut cmd = python()?;
    let dir = scratch("py");
    let path = dir.join("oracle.py");
    std::fs::write(&path, script).unwrap();
    let output = cmd.arg(&path).args(args).output().unwrap();
    assert!(output.status.success(), "python failed: {}", String::from_utf8_lossy(&output.stderr));
    Some(String::from_utf8(output.stdout).unwrap())
}

fn b64(data: &[u8]) -> String {
    openssl::base64::encode_block(data)
}

fn unb64(text: &str) -> Vec<u8> {
    openssl::base64::decode_block(text).unwrap()
}

/// Minimal JSON reading for the oracle's replies (flat values only): the raw
/// text of a number/null, or the decoded value of a string.
fn json_field(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\": ");
    let start = json.find(&pattern)? + pattern.len();
    let rest = &json[start..];
    if !rest.starts_with('"') {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        return Some(rest[..end].trim().to_string());
    }
    let mut out = String::new();
    let mut chars = rest[1..].chars();
    let mut pending_high: Option<u32> = None;
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => {
                let e = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let code: String = chars.by_ref().take(4).collect();
                        let unit = u32::from_str_radix(&code, 16).ok()?;
                        if (0xd800..0xdc00).contains(&unit) {
                            pending_high = Some(unit);
                            continue;
                        }
                        let scalar = match pending_high.take() {
                            Some(high) => 0x10000 + ((high - 0xd800) << 10) + (unit - 0xdc00),
                            None => unit,
                        };
                        out.push(char::from_u32(scalar)?);
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
    }
    None
}

// ---------------------------------------------------------------- wire bytes

const WIRE_ORACLE: &str = r#"
import base64, json, sys, urllib.request
spec = json.load(open(sys.argv[1]))
opener = urllib.request.build_opener()
for r in spec:
    data = None if r['data'] is None else base64.b64decode(r['data'])
    req = urllib.request.Request(r['url'], data=data, headers=dict(r['headers']), method=r['method'])
    opener.open(req, timeout=5).read()
print('done')
"#;

#[derive(Clone)]
struct Spec {
    method: Option<&'static str>,
    path: &'static str,
    headers: Vec<(String, String)>,
    data: Option<Vec<u8>>,
}

fn json_escape(s: &str) -> String {
    let mut out = String::new();
    crate::py::json_str(&mut out, s);
    out
}

fn specs_json(specs: &[Spec], base: &str) -> String {
    let items: Vec<String> = specs.iter().map(|s| {
        let headers: Vec<String> = s.headers.iter().map(|(k, v)| format!("[{}, {}]", json_escape(k), json_escape(v))).collect();
        format!("{{\"url\": {}, \"method\": {}, \"headers\": [{}], \"data\": {}}}",
            json_escape(&format!("{base}{}", s.path)),
            s.method.map_or("null".to_string(), json_escape),
            headers.join(", "),
            s.data.as_ref().map_or("null".to_string(), |d| json_escape(&b64(d))))
    }).collect();
    format!("[{}]", items.join(", "))
}

#[test]
fn request_bytes_identical_to_urllib() {
    let specs = vec![
        Spec { method: None, path: "/a?b=1#frag", headers: vec![], data: None },
        Spec { method: None, path: "", headers: vec![], data: None },
        Spec { method: Some("GET"), path: "?q=%41", headers: h(&[("x-sn-sdk-ver", "1"), ("Authorization", "Bearer t"),
            ("X-SN-Transaction-ID", "ND-abc"), ("Accept-Encoding", "gzip"), ("User-Agent", "SamsungNotesLinuxResearch/0.1"),
            ("x-sc-e2ee-state", "0")]), data: None },
        Spec { method: Some("POST"), path: "/identity/v2/user/register?deviceCurrentTime=1", headers: h(&[
            ("x-sc-app-id", "app"), ("Content-Type", "application/json"), ("Accept", "application/json"),
            ("User-Agent", "SamsungNotesLinux/0.1")]), data: Some(br#"{"app":{},"device":{}}"#.to_vec()) },
        Spec { method: None, path: "/form", headers: vec![], data: Some(b"a=1&b=2".to_vec()) },
        Spec { method: Some("POST"), path: "/nobody", headers: vec![], data: None },
        Spec { method: Some("PATCH"), path: "/patch", headers: vec![], data: None },
        Spec { method: Some("DELETE"), path: "/del", headers: vec![], data: None },
        Spec { method: Some("PUT"), path: "/empty", headers: h(&[("content-type", "application/octet-stream")]), data: Some(Vec::new()) },
        Spec { method: Some("GET"), path: "/latin", headers: h(&[("X-Name", "caf\u{e9}"), ("connection", "keep-alive"),
            ("HOST", "override.example"), ("accept-encoding", "br")]), data: None },
        Spec { method: Some("POST"), path: "/chunked", headers: h(&[("Transfer-Encoding", "chunked")]), data: Some(b"abc".to_vec()) },
        Spec { method: Some("PUT"), path: "/upload", headers: h(&[("Content-Range", "bytes 0-2/3"), ("Content-Length", "3")]),
            data: Some(b"xyz".to_vec()) },
    ];
    let fake = plain(http("200 OK", &[("Content-Length", "2")], b"ok"));
    let base = format!("http://127.0.0.1:{}", fake.port);
    let dir = scratch("wire");
    let spec_path = dir.join("spec.json");
    std::fs::write(&spec_path, specs_json(&specs, &base)).unwrap();
    let Some(out) = run_python(WIRE_ORACLE, &[spec_path.to_str().unwrap()]) else { return };
    assert_eq!(out.trim(), "done");
    let python_requests = fake.requests();
    assert_eq!(python_requests.len(), specs.len());
    let c = client();
    for s in &specs {
        let request = HttpRequest::new(s.method, &format!("{base}{}", s.path), &s.headers, s.data.clone());
        let mut response = c.open(&request).unwrap();
        assert_eq!(response.read(100).unwrap(), b"ok");
    }
    let all = fake.requests();
    for (i, s) in specs.iter().enumerate() {
        assert_eq!(String::from_utf8_lossy(&all[specs.len() + i]), String::from_utf8_lossy(&python_requests[i]),
            "request {} ({})", i, s.path);
        assert_eq!(all[specs.len() + i], python_requests[i]);
    }
}

// ---------------------------------------------------------------- send_once vs Python

const SEND_ONCE_ORACLE: &str = r#"
import base64, contextlib, io, json, sys, urllib.request
from pathlib import Path
import notes_cloud
spec = json.load(open(sys.argv[1]))
data = None if spec['data'] is None else base64.b64decode(spec['data'])
req = urllib.request.Request(spec['url'], data=data, headers=dict(spec['headers']), method=spec['method'])
buf = io.StringIO(); exc = None; status = payload = None
with contextlib.redirect_stdout(buf):
    try:
        status, payload = notes_cloud.send_once(Path(spec['dir']), spec['name'], req, spec['max_bytes'], spec['quiet'])
    except BaseException as e:
        exc = type(e).__name__
json.dump({'status': status, 'payload': None if payload is None else base64.b64encode(payload).decode(),
           'exception': exc, 'printed': buf.getvalue()}, sys.stdout)
"#;

fn mask_started(text: &str) -> String {
    let key = "\"startedUnixSeconds\": ";
    match text.find(key) {
        Some(i) => {
            let start = i + key.len();
            let end = start + text[start..].find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len() - start);
            format!("{}N{}", &text[..start], &text[end..])
        }
        None => text.to_string(),
    }
}

fn dir_snapshot(dir: &Path) -> Vec<(String, String, u32)> {
    let mut files: Vec<(String, String, u32)> = std::fs::read_dir(dir).unwrap().map(|e| {
        let e = e.unwrap();
        let bytes = std::fs::read(e.path()).unwrap();
        let mode = e.metadata().unwrap().permissions().mode() & 0o777;
        (e.file_name().to_string_lossy().to_string(), mask_started(&String::from_utf8_lossy(&bytes)), mode)
    }).collect();
    files.sort();
    files
}

fn python_class(e: &NetError) -> String {
    match e.python_class() {
        "zlib.error" => "error".to_string(),
        other => other.to_string(),
    }
}

struct Scenario {
    name: &'static str,
    response: Option<Vec<u8>>,
    method: &'static str,
    headers: Vec<(String, String)>,
    data: Option<Vec<u8>>,
    max_bytes: usize,
    quiet: bool,
}

fn scenario(name: &'static str, response: Vec<u8>) -> Scenario {
    Scenario { name, response: Some(response), method: "GET",
        headers: h(&[("Accept-Encoding", "gzip"), ("User-Agent", "SamsungNotesLinuxResearch/0.1"), ("x-sn-sdk-ver", "4")]),
        data: None, max_bytes: 4096, quiet: false }
}

fn run_send_once_pair(s: &Scenario) -> Option<()> {
    let (fake, url) = match &s.response {
        Some(bytes) => {
            let fake = plain(bytes.clone());
            let url = format!("http://127.0.0.1:{}/samsungnotes/v3/notes/changes?lastChangePoint=x", fake.port);
            (Some(fake), url)
        }
        None => (None, format!("http://127.0.0.1:{}/x", closed_port())),
    };
    let py_dir = scratch(&format!("py-{}", s.name));
    let rs_dir = scratch(&format!("rs-{}", s.name));
    let spec = format!(
        "{{\"url\": {}, \"method\": {}, \"headers\": [{}], \"data\": {}, \"dir\": {}, \"name\": {}, \"max_bytes\": {}, \"quiet\": {}}}",
        json_escape(&url), json_escape(s.method),
        s.headers.iter().map(|(k, v)| format!("[{}, {}]", json_escape(k), json_escape(v))).collect::<Vec<_>>().join(", "),
        s.data.as_ref().map_or("null".to_string(), |d| json_escape(&b64(d))),
        json_escape(py_dir.to_str().unwrap()), json_escape(s.name), s.max_bytes, s.quiet);
    let spec_path = scratch("spec").join("spec.json");
    std::fs::write(&spec_path, spec).unwrap();
    let reply = run_python(SEND_ONCE_ORACLE, &[spec_path.to_str().unwrap()])?;

    let request = HttpRequest::new(Some(s.method), &url, &s.headers, s.data.clone());
    let mut printed = Vec::new();
    let result = send_once::run(&rs_dir, s.name, &request, s.max_bytes, s.quiet, || Ok(client()), Some(&mut printed));

    let py_exception = json_field(&reply, "exception").filter(|v| v != "null");
    match (&result, py_exception) {
        (Ok((status, payload)), None) => {
            let py_status = json_field(&reply, "status").unwrap();
            assert_eq!(status.map_or("null".to_string(), |v| v.to_string()), py_status, "{}: status", s.name);
            let py_payload = json_field(&reply, "payload").filter(|v| v != "null").map(|v| unb64(&v));
            assert_eq!(payload, &py_payload, "{}: payload", s.name);
        }
        (Err(SendError::Escaped(e)), Some(class)) => assert_eq!(python_class(e), *class, "{}: escaped class", s.name),
        (other, py) => panic!("{}: rust {:?} vs python exception {:?}", s.name, other.as_ref().map(|r| r.0), py),
    }
    // stdout line (json.dumps of the receipt)
    let py_printed = json_field(&reply, "printed").unwrap_or_default();
    let rs_printed = String::from_utf8(printed).unwrap();
    assert_eq!(mask_started(&rs_printed), mask_started(&py_printed), "{}: printed receipt", s.name);
    // files on disk (names, bytes, modes)
    let py_files = dir_snapshot(&py_dir);
    let rs_files = dir_snapshot(&rs_dir);
    assert_eq!(rs_files, py_files, "{}: files", s.name);
    // the same bytes reached the server from both implementations
    if let Some(fake) = fake {
        let requests = fake.requests();
        assert_eq!(requests.len(), 2, "{}: requests", s.name);
        assert_eq!(requests[0], requests[1], "{}: request bytes", s.name);
    }
    Some(())
}

#[test]
fn send_once_files_identical_to_python() {
    let json = br#"{"changes":[],"has_next":false}"#.to_vec();
    let big = vec![b'z'; 5000];
    let mut long_type = "application/json; x=".to_string();
    long_type.push_str(&"a".repeat(8200));
    let gz_json = gz(&json);
    let mut multi = gz(b"first-");
    multi.extend_from_slice(&gz(b"second"));
    multi.extend_from_slice(&[0, 0, 0]);
    let mut trailing = gz(&json);
    trailing.extend_from_slice(b"junk");
    let mut corrupt = gz(&json)[..10].to_vec();
    corrupt.extend_from_slice(&[0xff; 16]);
    let many_headers: Vec<(String, String)> = (0..100).map(|i| (format!("X-H{i}"), "v".to_string())).collect();
    let many: Vec<(&str, &str)> = many_headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let long_header = "v".repeat(70000);

    let mut scenarios = vec![
        scenario("ok", http("200 OK", &[("Content-Type", "application/json; charset=utf-8"), ("Content-Length", &json.len().to_string())], &json)),
        Scenario { quiet: true, ..scenario("quiet", http("200 OK", &[("Content-Type", "application/json"), ("Content-Length", &json.len().to_string())], &json)) },
        scenario("gzip", http("200 OK", &[("Content-Type", "application/json"), ("Content-Encoding", "gzip"), ("Content-Length", &gz_json.len().to_string())], &gz_json)),
        scenario("gzip-chunked", http("200 OK", &[("Content-Type", "application/json"), ("Content-Encoding", "GZIP"), ("Transfer-Encoding", "Chunked")],
            &chunked(&[&gz_json[..7], &gz_json[7..]]))),
        Scenario { method: "POST", data: Some(br#"{"app":{"version":"1"},"device":{}}"#.to_vec()),
            headers: h(&[("x-sc-app-id", "a"), ("Content-Type", "application/json"), ("Accept", "application/json"), ("User-Agent", "SamsungNotesLinuxResearch/0.1")]),
            ..scenario("post-201", http("201 Created", &[("Content-Type", "application/json"), ("Content-Length", "2")], b"{}")) },
        Scenario { quiet: true, ..scenario("not-found", http("404 Not Found", &[("Content-Type", "text/html"), ("Content-Length", "9")], b"not found")) },
        scenario("redirect", http("302 Found", &[("Location", "https://elsewhere.example/x"), ("Content-Length", "3")], b"moved")),
        scenario("bad-location", http("302 Found", &[("Location", "http://[bad/x"), ("Content-Length", "0")], b"")),
        scenario("no-type", http("200 OK", &[("Content-Length", "2")], b"ok")),
        scenario("latin1-type", [&b"HTTP/1.1 200 OK\r\nContent-Type:   Text/HTML ; q=\xe9\"x  \r\nContent-Length: 2\r\n\r\nok"[..]].concat()),
        scenario("invalid-type", http("200 OK", &[("Content-Type", "foo"), ("Content-Length", "2")], b"ok")),
        scenario("long-type", http("200 OK", &[("Content-Type", &long_type), ("Content-Length", "2")], b"ok")),
        Scenario { max_bytes: 1000, ..scenario("wire-limit", http("200 OK", &[("Content-Type", "text/plain"), ("Content-Length", "5000")], &big)) },
        Scenario { max_bytes: 1000, ..scenario("decoded-limit", http("200 OK", &[("Content-Encoding", "gzip"), ("Content-Length", &gz(&big).len().to_string())], &gz(&big))) },
        scenario("unsupported-encoding", http("200 OK", &[("Content-Encoding", "br"), ("Content-Length", "2")], b"ok")),
        scenario("gzip-truncated", http("200 OK", &[("Content-Encoding", "gzip"), ("Content-Length", "12")], &gz_json[..12])),
        scenario("gzip-bad-magic", http("200 OK", &[("Content-Encoding", "gzip"), ("Content-Length", "2")], b"ok")),
        scenario("gzip-multi-member", http("200 OK", &[("Content-Encoding", "gzip"), ("Content-Length", &multi.len().to_string())], &multi)),
        scenario("gzip-trailing-junk", http("200 OK", &[("Content-Encoding", "gzip"), ("Content-Length", &trailing.len().to_string())], &trailing)),
        scenario("gzip-corrupt-deflate", http("200 OK", &[("Content-Encoding", "gzip"), ("Content-Length", &corrupt.len().to_string())], &corrupt)),
        scenario("bad-status", b"garbage\r\n\r\n".to_vec()),
        scenario("remote-disconnect", Vec::new()),
        scenario("until-close", b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nread until close".to_vec()),
        scenario("short-content", http("200 OK", &[("Content-Length", "100")], b"only ten!!")),
        scenario("chunked-incomplete", http("200 OK", &[("Transfer-Encoding", "chunked")], b"a\r\n01234")),
        scenario("chunked-bad-size", http("200 OK", &[("Transfer-Encoding", "chunked")], b"zz\r\n")),
        Scenario { method: "HEAD", ..scenario("head", http("200 OK", &[("Content-Length", "5")], b"")) },
        scenario("interim-100", [&b"HTTP/1.1 100 Continue\r\nX: 1\r\n\r\n"[..], &http("200 OK", &[("Content-Length", "2")], b"ok")].concat()),
        scenario("too-many-headers", http("200 OK", &many, b"")),
        scenario("long-header", http("200 OK", &[("X-Long", &long_header)], b"")),
        scenario("http10", b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nold".to_vec()),
        scenario("http2", b"HTTP/2 200\r\n\r\n".to_vec()),
        scenario("no-content", http("204 No Content", &[("Content-Length", "10")], b"")),
        scenario("weird-length", http("200 OK", &[("Content-Length", " +1_0 ")], b"0123456789extra")),
        scenario("chunk-hex-prefix", http("200 OK", &[("Transfer-Encoding", "chunked")], b"0x3\r\nabc\r\n0\r\n\r\n")),
        scenario("status-999", http("999 Odd", &[("Content-Length", "1")], b"x")),
        scenario("continuation", b"HTTP/1.1 200 OK\r\nContent-Type: text/\r\n plain\r\nContent-Length: 1\r\n\r\nx".to_vec()),
        Scenario { response: None, ..scenario("refused", Vec::new()) },
        Scenario { max_bytes: 0, ..scenario("size-bound", Vec::new()) },
    ];
    // exactly the bound / one past it
    scenarios.push(Scenario { max_bytes: 5000, ..scenario("exact-bound", http("200 OK", &[("Content-Length", "5000")], &big)) });
    for s in &scenarios {
        if s.name == "size-bound" {
            // Raised before any I/O in both implementations.
            let request = HttpRequest::new(Some("GET"), "http://127.0.0.1:1/", &[], None);
            let dir = scratch("size");
            assert!(matches!(send_once::run(&dir, "x", &request, 0, false, || Ok(client()), Some(&mut Vec::new())), Err(SendError::SizeBound)));
            assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
            continue;
        }
        if run_send_once_pair(s).is_none() {
            return; // python unavailable: skipped (reported on stderr)
        }
    }
}

// ---------------------------------------------------------------- send_once contract (Rust only)

#[test]
fn claim_exists_before_the_connection_is_opened() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = scratch("claim");
    let claim = dir.join(".probe-claim");
    let seen = Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    let claim2 = claim.clone();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        *seen2.lock().unwrap() = Some(std::fs::read(&claim2).ok());
        read_request(&mut conn);
        conn.write_all(&http("200 OK", &[("Content-Length", "2")], b"ok")).unwrap();
    });
    let request = HttpRequest::new(None, &format!("http://127.0.0.1:{port}/"), &[], None);
    let (status, body) = send_once::run(&dir, "probe", &request, 100, true, || Ok(client()), Some(&mut Vec::new())).unwrap();
    assert_eq!((status, body.as_deref()), (Some(200), Some(&b"ok"[..])));
    assert_eq!(seen.lock().unwrap().clone(), Some(Some(b"claimed-before-transport\n".to_vec())));
    let mode = std::fs::metadata(&claim).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn claim_written_even_when_connect_fails_and_reuse_is_refused() {
    let dir = scratch("refused");
    let request = HttpRequest::new(None, &format!("http://127.0.0.1:{}/", closed_port()), &[], None);
    let mut out = Vec::new();
    let (status, body) = send_once::run(&dir, "r", &request, 100, false, || Ok(client()), Some(&mut out)).unwrap();
    assert_eq!((status, body), (None, None));
    assert!(dir.join(".r-claim").is_file());
    assert!(!dir.join("r-response-meta.json").exists());
    let receipt = std::fs::read_to_string(dir.join("r-receipt.json")).unwrap();
    assert!(receipt.contains("\"result\": \"transport_or_response_error\""));
    assert!(String::from_utf8(out).unwrap().contains("\"result\": \"transport_or_response_error\""));
    // A second exchange under the same name never reaches the network.
    let fake = plain(http("200 OK", &[("Content-Length", "2")], b"ok"));
    let again = HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/"), &[], None);
    let result = send_once::run(&dir, "r", &again, 100, false, || Ok(client()), Some(&mut Vec::new()));
    assert!(matches!(result, Err(SendError::Claim(ref e)) if e.kind() == std::io::ErrorKind::AlreadyExists));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fake.connections.load(Ordering::SeqCst), 0);
    // An URL urllib.request.Request() rejects is refused before the claim.
    let bad = HttpRequest::new(None, "no-scheme/path", &[], None);
    assert!(matches!(send_once::run(&dir, "bad", &bad, 100, false, || Ok(client()), Some(&mut Vec::new())),
        Err(SendError::InvalidRequest(_))));
    assert!(!dir.join(".bad-claim").exists());
}

// ---------------------------------------------------------------- TLS

#[test]
fn tls_unknown_ca_is_rejected_by_the_production_client() {
    let ca = make_ca("notes-net throwaway CA");
    let fake = tls_fake(&ca, &["localhost"], &["127.0.0.1"], respond(http("200 OK", &[("Content-Length", "2")], b"ok")));
    let request = HttpRequest::new(None, &fake.url("https", "127.0.0.1", "/"), &[], None);
    let error = client().open(&request).err().expect("must fail");
    assert_eq!(error.python_class(), "SSLCertVerificationError", "{error}");
    assert!(error.is_os());
    assert!(fake.requests().is_empty());
}

#[test]
fn tls_with_injected_ca_succeeds_and_sends_the_same_bytes() {
    let ca = make_ca("notes-net throwaway CA");
    let fake = tls_fake(&ca, &["localhost"], &["127.0.0.1"], respond(http("200 OK", &[("Content-Length", "2")], b"ok")));
    let c = Client::with_extra_roots(Duration::from_secs(5), std::slice::from_ref(&ca.cert)).unwrap();
    for host in ["127.0.0.1", "localhost"] {
        let headers = h(&[("x-sc-app-id", "a"), ("Content-Type", "application/json")]);
        let request = HttpRequest::new(Some("POST"), &fake.url("https", host, "/p?x=1"), &headers, Some(b"{}".to_vec()));
        let mut response = c.open(&request).unwrap_or_else(|e| panic!("{host}: {e}"));
        assert_eq!(response.status(), 200);
        assert_eq!(response.read(10).unwrap(), b"ok");
        // Identical to the plain-HTTP request bytes for the same Request.
        let parsed = crate::url::request_url(&request.url).unwrap();
        let prepared = crate::request::prepare(&request, &parsed).unwrap();
        let mut expected = prepared.head.clone();
        expected.extend(prepared.body.concat());
        assert_eq!(fake.requests().last().unwrap(), &expected);
    }
    // SNI only for host names, like CPython.
    assert_eq!(fake.sni.lock().unwrap().clone(), vec![None, Some("localhost".to_string())]);
}

#[test]
fn tls_hostname_mismatch_is_rejected() {
    let ca = make_ca("notes-net throwaway CA");
    let fake = tls_fake(&ca, &["other.example"], &[], respond(http("200 OK", &[("Content-Length", "2")], b"ok")));
    let c = Client::with_extra_roots(Duration::from_secs(5), std::slice::from_ref(&ca.cert)).unwrap();
    let e = c.open(&HttpRequest::new(None, &fake.url("https", "127.0.0.1", "/"), &[], None)).err().unwrap();
    assert!(e.to_string().contains("IP address mismatch"), "{e}");
    let e = c.open(&HttpRequest::new(None, &fake.url("https", "localhost", "/"), &[], None)).err().unwrap();
    assert!(e.to_string().contains("Hostname mismatch"), "{e}");
    assert_eq!(e.python_class(), "SSLCertVerificationError");
    assert!(fake.requests().is_empty());
}

#[test]
fn tls_below_1_2_is_refused() {
    let ca = make_ca("notes-net throwaway CA");
    let (cert, key) = make_leaf(&ca, &["localhost"], &["127.0.0.1"]);
    let fake = serve(Some(acceptor(&cert, &key, Some(SslVersion::TLS1_1))), respond(b"HTTP/1.1 200 OK\r\n\r\n".to_vec()));
    let c = Client::with_extra_roots(Duration::from_secs(5), std::slice::from_ref(&ca.cert)).unwrap();
    let e = c.open(&HttpRequest::new(None, &fake.url("https", "127.0.0.1", "/"), &[], None)).err().unwrap();
    assert!(e.is_os(), "{e}");
    assert!(fake.requests().is_empty());
}

#[test]
fn tls_context_matches_python_default_context() {
    assert_eq!(crate::python_options().bits(), 0x8252_0050);
    assert_eq!(crate::python_verify_flags().bits(), 0x88020);
    let mut b = crate::tls::context_builder(&[]).unwrap();
    assert_eq!(b.options().bits(), 0x8252_0050);
    assert_eq!(b.min_proto_version(), Some(SslVersion::TLS1_2));
    assert_eq!(b.max_proto_version(), None);
    assert_eq!(b.verify_param_mut().flags().bits(), 0x88020);
}

const TLS_ORACLE: &str = r#"
import ssl, sys, urllib.request, urllib.error
ctx = ssl.create_default_context(cafile=sys.argv[1])
for url in sys.argv[2:]:
    try:
        opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx))
        r = opener.open(url, timeout=5); print(url, 'ok', r.status, r.read().decode())
    except Exception as e:
        print(url, 'error', type(getattr(e, 'reason', e)).__name__)
"#;

#[test]
fn tls_outcomes_match_python() {
    let ca = make_ca("notes-net throwaway CA");
    let good = tls_fake(&ca, &["localhost"], &["127.0.0.1"], respond(http("200 OK", &[("Content-Length", "2")], b"ok")));
    let wrong = tls_fake(&ca, &["other.example"], &[], respond(http("200 OK", &[("Content-Length", "2")], b"ok")));
    let pem_dir = scratch("ca");
    let pem = pem_dir.join("ca.pem");
    std::fs::write(&pem, ca.cert.to_pem().unwrap()).unwrap();
    let urls = [good.url("https", "127.0.0.1", "/"), good.url("https", "localhost", "/"),
        wrong.url("https", "127.0.0.1", "/"), wrong.url("https", "localhost", "/")];
    let mut args = vec![pem.to_str().unwrap()];
    args.extend(urls.iter().map(String::as_str));
    let Some(out) = run_python(TLS_ORACLE, &args) else { return };
    let c = Client::with_extra_roots(Duration::from_secs(5), std::slice::from_ref(&ca.cert)).unwrap();
    for (line, url) in out.lines().zip(urls.iter()) {
        let rust = match c.open(&HttpRequest::new(None, url, &[], None)) {
            Ok(mut r) => format!("{url} ok {} {}", r.status(), String::from_utf8(r.read(10).unwrap()).unwrap()),
            Err(e) => format!("{url} error {}", e.python_class()),
        };
        assert_eq!(rust, line);
    }
    // Same request bytes over TLS from both implementations.
    let requests = good.requests();
    assert_eq!(requests[0], requests[2]);
    assert_eq!(requests[1], requests[3]);
}

// ---------------------------------------------------------------- response parsing / limits / timeouts

#[test]
fn redirects_are_returned_not_followed() {
    let fake = plain(http("302 Found", &[("Location", "/elsewhere"), ("Content-Length", "0")], b""));
    let request = HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/start"), &[], None);
    let mut response = client().open(&request).unwrap();
    assert_eq!(response.status(), 302);
    assert_eq!(response.header("location"), Some("/elsewhere"));
    assert_eq!(response.read(10).unwrap(), b"");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fake.connections.load(Ordering::SeqCst), 1);
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn chunked_and_gzip_bodies() {
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let compressed = gz(&payload);
    let parts: Vec<&[u8]> = compressed.chunks(1000).collect();
    let fake = plain(http("200 OK", &[("Transfer-Encoding", "chunked"), ("Content-Encoding", "gzip")], &chunked(&parts)));
    let dir = scratch("chunked-gzip");
    let request = HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/"), &[], None);
    let (status, body) = send_once::run(&dir, "c", &request, 300_000, true, || Ok(client()), Some(&mut Vec::new())).unwrap();
    assert_eq!(status, Some(200));
    assert_eq!(body.unwrap(), payload);
    let receipt = std::fs::read_to_string(dir.join("c-receipt.json")).unwrap();
    assert!(receipt.contains(&format!("\"wireBytes\": {}", compressed.len())));
    assert!(receipt.contains("\"decodedBytes\": 200000"));
}

#[test]
fn read_bound_is_max_plus_one() {
    let body = vec![b'a'; 10_000];
    let fake = plain(http("200 OK", &[("Content-Length", "10000")], &body));
    let mut response = client().open(&HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/"), &[], None)).unwrap();
    assert_eq!(response.read(4097).unwrap().len(), 4097);
    assert_eq!(response.read(100_000).unwrap().len(), 10_000 - 4097);
    assert_eq!(response.read(10).unwrap(), b"");
}

#[test]
fn header_limits_escape_like_http_client() {
    let long = format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "v".repeat(70_000)).into_bytes();
    let fake = plain(long);
    let e = client().open(&HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/"), &[], None)).err().unwrap();
    assert_eq!((e.family(), e.python_class()), (Some(Family::Escape), "LineTooLong"));
    let mut many = b"HTTP/1.1 200 OK\r\n".to_vec();
    for i in 0..100 {
        many.extend_from_slice(format!("X-{i}: v\r\n").as_bytes());
    }
    many.extend_from_slice(b"\r\n");
    let fake = plain(many);
    let e = client().open(&HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/"), &[], None)).err().unwrap();
    assert_eq!(e.python_class(), "HTTPException");
    // 99 headers + the blank line are accepted.
    let mut ok = b"HTTP/1.1 200 OK\r\n".to_vec();
    for i in 0..99 {
        ok.extend_from_slice(format!("X-{i}: v\r\n").as_bytes());
    }
    ok.extend_from_slice(b"\r\n");
    let fake = plain(ok);
    assert!(client().open(&HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/"), &[], None)).is_ok());
}

fn silent_server(after_accept: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            std::thread::spawn(move || {
                std::thread::sleep(after_accept);
                drop(conn);
            });
        }
    });
    port
}

#[test]
fn read_timeout_is_per_operation() {
    let port = silent_server(Duration::from_secs(5));
    let c = Client::new(Duration::from_millis(300)).unwrap();
    let started = Instant::now();
    let e = c.open(&HttpRequest::new(None, &format!("http://127.0.0.1:{port}/"), &[], None)).err().unwrap();
    assert_eq!(e.python_class(), "TimeoutError");
    assert!(e.is_os());
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    // A TLS handshake the server never answers is bounded by the same timeout.
    let started = Instant::now();
    let e = c.open(&HttpRequest::new(None, &format!("https://127.0.0.1:{port}/"), &[], None)).err().unwrap();
    assert_eq!(e.python_class(), "TimeoutError", "{e}");
    assert!(started.elapsed() < Duration::from_secs(2));
    // send_once records it as a transport error.
    let dir = scratch("timeout");
    let request = HttpRequest::new(None, &format!("http://127.0.0.1:{port}/"), &[], None);
    let (status, body) = send_once::run(&dir, "t", &request, 100, true, || Ok(c.clone()), Some(&mut Vec::new())).unwrap();
    assert_eq!((status, body), (None, None));
    assert!(std::fs::read_to_string(dir.join("t-receipt.json")).unwrap().contains("transport_or_response_error"));
}

#[test]
fn slow_body_times_out_between_reads() {
    let handler: Handler = Arc::new(|s: &mut dyn Conn| {
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc");
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(3));
    });
    let fake = serve(None, handler);
    let c = Client::new(Duration::from_millis(300)).unwrap();
    let mut response = c.open(&HttpRequest::new(None, &fake.url("http", "127.0.0.1", "/"), &[], None)).unwrap();
    let e = response.read(11).err().unwrap();
    assert_eq!(e.python_class(), "TimeoutError");
}

// ---------------------------------------------------------------- previews / PDF transports

#[test]
fn get_reads_only_200_bodies_and_bounds_them() {
    let fake = plain(http("200 OK", &[("Content-Length", "5"), ("Location", "ignored")], b"image"));
    let c = client();
    let (status, body, location) = c.get_checked(&fake.url("http", "127.0.0.1", "/p"), &h(&[("x-sc-app-id", "a")]), 5, false).unwrap();
    assert_eq!((status, body.as_slice(), location.as_deref()), (200, &b"image"[..], Some("ignored")));
    assert!(String::from_utf8_lossy(&fake.requests()[0]).contains("\r\nX-Sc-App-Id: a\r\n"));
    let e = c.get_checked(&fake.url("http", "127.0.0.1", "/p"), &[], 4, false).err().unwrap();
    assert!(matches!(e, NetError::TooLarge));
    let fake = plain(http("302 Found", &[("Location", "https://files.example/signed"), ("Content-Length", "100000")], b""));
    let (status, body, location) = c.get_checked(&fake.url("http", "127.0.0.1", "/p"), &[], 5, false).unwrap();
    assert_eq!((status, body.len(), location.as_deref()), (302, 0, Some("https://files.example/signed")));
}

#[test]
fn get_and_download_refuse_non_global_addresses() {
    // Resolves locally (no DNS traffic for literals / localhost), never connects.
    for url in ["https://127.0.0.1/x", "https://localhost/x", "https://[::1]/x", "https://10.0.0.1/x"] {
        let e = crate::get(url, &[], 10, Duration::from_secs(1)).err().unwrap();
        assert!(matches!(e, NetError::NotGlobal), "{url}: {e}");
        let dir = scratch("nonglobal");
        let mut check = || Ok::<(), ()>(());
        let e = crate::download(url, &[], &dir.join("f"), 10, Duration::from_secs(1), &mut check).err().unwrap();
        assert!(matches!(e, crate::DownloadError::Net(NetError::NotGlobal)), "{url}");
        assert!(!dir.join("f").exists());
    }
}

#[test]
fn download_streams_with_checks_and_bounds() {
    let body: Vec<u8> = (0..200_000u32).map(|i| (i % 7) as u8).collect();
    let fake = plain(http("200 OK", &[("Content-Length", "200000")], &body));
    let dir = scratch("download");
    let dest = dir.join(".partial-x");
    let mut calls = 0;
    let mut check = || {
        calls += 1;
        Ok::<(), &'static str>(())
    };
    let result = client().download_checked(&fake.url("http", "127.0.0.1", "/f.pdf"), &h(&[("Accept", "application/pdf")]), 300_000,
        || crate::create_private(&dest), &mut check, false).unwrap();
    assert_eq!(result, (200, None));
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(calls, 5); // 4 reads of <= 64 KiB with data + the final empty read

    // Content-Length above the bound: refused before the file is created.
    let dest2 = dir.join(".partial-y");
    let mut check = || Ok::<(), ()>(());
    let e = client().download_checked(&fake.url("http", "127.0.0.1", "/f.pdf"), &[], 100_000, || crate::create_private(&dest2), &mut check, false).err().unwrap();
    assert!(matches!(e, crate::DownloadError::Net(NetError::TooLarge)));
    assert!(!dest2.exists());

    // Streaming bound (no Content-Length).
    let fake = plain([&b"HTTP/1.1 200 OK\r\n\r\n"[..], &body].concat());
    let dest3 = dir.join(".partial-z");
    let e = client().download_checked(&fake.url("http", "127.0.0.1", "/f.pdf"), &[], 100_000, || crate::create_private(&dest3), &mut check, false).err().unwrap();
    assert!(matches!(e, crate::DownloadError::Net(NetError::TooLarge)));

    // Non-200: no file, Location passed through.
    let fake = plain(http("302 Found", &[("Location", "https://files.example/x")], b""));
    let dest4 = dir.join(".partial-w");
    let r = client().download_checked(&fake.url("http", "127.0.0.1", "/f.pdf"), &[], 100, || crate::create_private(&dest4), &mut check, false).unwrap();
    assert_eq!(r, (302, Some("https://files.example/x".to_string())));
    assert!(!dest4.exists());

    // check() failures stop the download and are returned as such.
    let fake = plain(http("200 OK", &[("Content-Length", "200000")], &body));
    let dest5 = dir.join(".partial-v");
    let mut n = 0;
    let mut failing = || {
        n += 1;
        if n == 2 { Err("cancelled") } else { Ok(()) }
    };
    let e = client().download_checked(&fake.url("http", "127.0.0.1", "/"), &[], 300_000, || crate::create_private(&dest5), &mut failing, false).err().unwrap();
    assert!(matches!(e, crate::DownloadError::Check("cancelled")));
}

// ---------------------------------------------------------------- gzip vs CPython

const GZIP_ORACLE: &str = r#"
import base64, gzip, hashlib, io, json, random, struct, sys, zlib
random.seed(20261026)
cases = []
def gz(data, level=6): return gzip.compress(data, compresslevel=level, mtime=0)
small = b'hello world ' * 50
g = gz(small)
for n in [1, 10, len(small), len(small) + 1, 10000]: cases.append((g, n))
for cut in range(len(g) + 1): cases.append((g[:cut], len(small) + 1))
for i in range(len(g)):
    b = bytearray(g); b[i] ^= 0xff; cases.append((bytes(b), len(small) + 1))
for extra in [g, b'\0' * 10, b'\0' * 10 + g, b'garbage', b'\x1f', b'\x1f\x8b', b'\x1f\x8b\x08', b'\x1f\x8b\x07' + b'\0' * 7]:
    cases.append((g + extra, 4000))
raw = zlib.compressobj(6, zlib.DEFLATED, -15)
body = raw.compress(small) + raw.flush()
header = b'\x1f\x8b\x08' + bytes([2 | 4 | 8 | 16]) + b'\0' * 6 + b'\x03\x00abc' + b'name\0' + b'comment\0' + b'\xaa\xbb'
cases.append((header + body + struct.pack('<II', zlib.crc32(small), len(small)), 4000))
cases.append((header + body + struct.pack('<II', zlib.crc32(small), len(small) + 1), 4000))
big = random.randbytes(300000) + b'a' * 300000 + bytes(range(256)) * 400
gb = gz(big)
for n in [131071, 131072, 131073, 262144, 262145, len(big) - 1, len(big), len(big) + 1, 2 * 1024 * 1024 + 1]:
    cases.append((gb, n))
for pos in [len(gb) // 3, len(gb) // 2, len(gb) - 300, len(gb) - 20, len(gb) - 5]:
    b = bytearray(gb); b[pos] ^= 0x55
    for n in [1000, 131073, 300001, 600001, len(big) + 1]:
        cases.append((bytes(b), n))
for cut in [10, 1000, len(gb) // 2, len(gb) - 8, len(gb) - 1]:
    for n in [131073, len(big) + 1]:
        cases.append((gb[:cut], n))
zeros = gz(b'\0' * 1000000, 9)
for n in [999999, 1000000, 1000001, 131072 * 3, 131072 * 3 + 1]:
    cases.append((zeros, n))
for _ in range(300):
    b = bytearray(gz(random.randbytes(random.randint(0, 3000)) * random.randint(1, 5)))
    for _ in range(random.randint(0, 3)):
        if b: b[random.randrange(len(b))] = random.randrange(256)
    cases.append((bytes(b), random.choice([1, 100, 2000, 20000])))
out = []
for raw, n in cases:
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(raw)) as f:
            d = f.read(n)
        res = {'sha': hashlib.sha256(d).hexdigest(), 'len': len(d)}
    except Exception as e:
        res = {'err': type(e).__name__}
    out.append(dict(raw=base64.b64encode(raw).decode(), n=n, **res))
json.dump(out, open(sys.argv[1], 'w'))
"#;

#[test]
fn gzip_reader_matches_cpython() {
    let dir = scratch("gzip");
    let path = dir.join("cases.json");
    if run_python(GZIP_ORACLE, &[path.to_str().unwrap()]).is_none() {
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let mut count = 0;
    for item in text.split("{\"raw\": ").skip(1) {
        let item = format!("{{\"raw\": {item}");
        let raw = unb64(&json_field(&item, "raw").unwrap());
        let n: usize = json_field(&item, "n").unwrap().parse().unwrap();
        let rust = match gzip::read_bounded(&raw, n) {
            Ok(d) => format!("ok {} {}", hex(&openssl::sha::sha256(&d)), d.len()),
            Err(e) => format!("err {}", python_class(&e)),
        };
        let python = match json_field(&item, "err") {
            Some(class) => format!("err {class}"),
            None => format!("ok {} {}", json_field(&item, "sha").unwrap(), json_field(&item, "len").unwrap()),
        };
        assert_eq!(rust, python, "case {count} (n={n}, {} bytes)", raw.len());
        count += 1;
    }
    assert!(count > 400, "{count}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- TLS ClientHello vs CPython

/// Accepts connections and keeps the first TLS record of each (the ClientHello).
fn hello_catcher() -> (u16, Arc<Mutex<Vec<Vec<u8>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hellos = Arc::new(Mutex::new(Vec::new()));
    let sink = hellos.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            conn.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut header = [0u8; 5];
            if conn.read_exact(&mut header).is_err() {
                continue;
            }
            let len = u16::from_be_bytes([header[3], header[4]]) as usize;
            let mut body = vec![0u8; len];
            if conn.read_exact(&mut body).is_ok() {
                sink.lock().unwrap().push(body);
            }
        }
    });
    (port, hellos)
}

/// The ClientHello without its per-connection random parts: legacy version,
/// cipher suites, compression methods and every extension (key_share
/// reduced to its group list).
fn hello_fingerprint(record: &[u8]) -> Vec<String> {
    let mut p = 4; // handshake type + length
    let mut out = vec![format!("version {:02x}{:02x}", record[p], record[p + 1])];
    p += 2 + 32; // version + random
    let sid = record[p] as usize;
    p += 1 + sid;
    let suites = u16::from_be_bytes([record[p], record[p + 1]]) as usize;
    out.push(format!("suites {}", hex(&record[p + 2..p + 2 + suites])));
    p += 2 + suites;
    let comp = record[p] as usize;
    out.push(format!("compression {}", hex(&record[p + 1..p + 1 + comp])));
    p += 1 + comp;
    let total = u16::from_be_bytes([record[p], record[p + 1]]) as usize;
    p += 2;
    let end = p + total;
    while p + 4 <= end {
        let kind = u16::from_be_bytes([record[p], record[p + 1]]);
        let len = u16::from_be_bytes([record[p + 2], record[p + 3]]) as usize;
        let data = &record[p + 4..p + 4 + len];
        let shown = if kind == 51 {
            // key_share: group ids only
            let mut groups = Vec::new();
            let mut q = 2;
            while q + 4 <= data.len() {
                groups.push(hex(&data[q..q + 2]));
                q += 4 + u16::from_be_bytes([data[q + 2], data[q + 3]]) as usize;
            }
            groups.join(",")
        } else {
            hex(data)
        };
        out.push(format!("ext {kind} {shown}"));
        p += 4 + len;
    }
    out
}

const HELLO_ORACLE: &str = r#"
import ssl, sys, urllib.request
for url in sys.argv[1:]:
    try:
        opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ssl.create_default_context()))
        opener.open(url, timeout=3)
    except Exception:
        pass
print('done')
"#;

#[test]
fn tls_client_hello_matches_cpython() {
    let (port, hellos) = hello_catcher();
    let urls = [format!("https://localhost:{port}/"), format!("https://127.0.0.1:{port}/")];
    let Some(_) = run_python(HELLO_ORACLE, &[&urls[0], &urls[1]]) else { return };
    let c = Client::new(Duration::from_secs(3)).unwrap();
    for url in &urls {
        let _ = c.open(&HttpRequest::new(None, url, &[], None));
    }
    std::thread::sleep(Duration::from_millis(200));
    let hellos = hellos.lock().unwrap().clone();
    assert_eq!(hellos.len(), 4, "ClientHellos captured");
    // python: [localhost, ip], rust: [localhost, ip]
    assert_eq!(hello_fingerprint(&hellos[2]), hello_fingerprint(&hellos[0]), "SNI host ClientHello");
    assert_eq!(hello_fingerprint(&hellos[3]), hello_fingerprint(&hellos[1]), "IP ClientHello");
    assert_eq!(hellos[2].len(), hellos[0].len());
    let sni = hello_fingerprint(&hellos[2]);
    assert!(sni.iter().any(|e| e.starts_with("ext 0 ")), "{sni:?}");
    assert!(sni.iter().filter(|e| e.starts_with("ext ")).count() >= 8, "{sni:?}");
    assert!(!hello_fingerprint(&hellos[3]).iter().any(|e| e.starts_with("ext 0 ")));
    assert!(!sni.iter().any(|e| e.starts_with("ext 16 ")), "no ALPN, like urllib with an explicit context");
}
