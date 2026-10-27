//! Pure-Rust loopback API: request validation, routing, body limits, static
//! files and fonts, mirroring `samsung_notes/server.py` (`Handler`).
//!
//! Engine work is delegated through [`Backend`]; the PyO3 adapter in
//! `server.rs` implements it on top of the Python `Engine`, and a future
//! in-process (Tauri) host implements it on the Rust engine directly.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::mime;
use crate::pyhttp::{self, Incoming, Request};
use crate::pyjson::{self, JStr, JValue};
use crate::ui_assets;
use crate::urlparse::{self, qs_get};

pub const MAX_REQUEST: usize = 2 * 1024 * 1024;
pub const PREVIEW_WAIT_SECONDS: f64 = 8.0;
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(15);
// 'wasm-unsafe-eval' lets the page compile the bundled live pen renderer
// (ink-live.wasm, same origin); it does not allow eval() of strings.
pub const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'";
pub const INTERNAL_MESSAGE: &str = "İşlem tamamlanamadı. Kaydedilmiş yerel notlar korunuyor.";
const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// How a request ended without a normal reply (server.py `dispatch`).
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// StoreError / APIError / DocumentError mapped to (code, message, status).
    Api { code: String, message: String, status: u16 },
    /// BrokenPipeError / ConnectionResetError / TimeoutError: close silently.
    Disconnect,
    /// Any other exception: 500 internal_error.
    Internal,
}

impl Failure {
    pub fn store(code: &str, message: &str, status: u16) -> Failure {
        Failure::Api { code: code.into(), message: message.into(), status }
    }
}

/// Which engine object a call goes to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Engine,
    Store,
    Previews,
    Thumbnails,
    Sessions,
    /// `preferences.get_preferences(store)` / `set_preferences(store, body)`.
    Preferences,
    /// `shortcuts.get_shortcuts(store)` / `set_shortcuts(store, body)`.
    Shortcuts,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Json(JValue),
    Str(String),
    Int(i64),
    None,
    Bytes(Vec<u8>),
    /// Keyword float argument (`wait=8`).
    KwFloat(&'static str, f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub target: Target,
    pub method: &'static str,
    pub args: Vec<Arg>,
}

pub fn call(target: Target, method: &'static str, args: Vec<Arg>) -> Call {
    Call { target, method, args }
}

/// Engine side of the API. Every method returns the Python-equivalent result
/// or the `Failure` the Python handler would have produced.
pub trait Backend: Send + Sync {
    /// Run a call and return `json.dumps(result)` bytes.
    fn json(&self, call: Call) -> Result<Vec<u8>, Failure>;
    /// A call returning `bytes | None` (preview PNG).
    fn bytes_or_none(&self, call: Call) -> Result<Option<Vec<u8>>, Failure>;
    /// `thumbnails.request` -> `(bytes | None, complete)`.
    fn bytes_and_flag(&self, call: Call) -> Result<(Option<Vec<u8>>, bool), Failure>;
    /// `engine.get(id)` -> (`note['title']`, `note['body']`), both `str`.
    fn note_text(&self, call: Call) -> Result<(String, String), Failure>;
    /// `engine.MAX_PDF_BYTES`.
    fn max_pdf_bytes(&self) -> Result<u64, Failure>;
    /// Python `re` `\w` for one character (Unicode `str.isalnum() or '_'`).
    fn is_word_char(&self, c: char) -> bool {
        c == '_' || c.is_alphanumeric()
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub token: String,
    pub port: u16,
    /// Already resolved (`Path(static_dir).resolve()`).
    pub static_dir: PathBuf,
    pub font_dir: PathBuf,
}

impl Config {
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    pub fn expected_host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

/// One accepted connection: buffered reader plus the raw writer.
pub struct Conn<R: BufRead, W: Write> {
    pub reader: R,
    pub writer: W,
    pub req: Request,
}

fn io_failure(error: io::Error) -> Failure {
    let _ = error;
    Failure::Disconnect
}

impl<R: BufRead, W: Write> Conn<R, W> {
    /// `Handler.send`
    pub fn send(&mut self, status: u16, body: &[u8], content_type: &str, extra: &[(&str, String)]) -> Result<(), Failure> {
        let v = self.req.request_version.clone();
        let mut out = pyhttp::response_head(&v, status, None);
        for (k, val) in [
            ("Content-Type", content_type.to_string()),
            ("Content-Length", body.len().to_string()),
            ("Cache-Control", "no-store".into()),
            ("X-Content-Type-Options", "nosniff".into()),
            ("Referrer-Policy", "no-referrer".into()),
            ("Cross-Origin-Resource-Policy", "same-origin".into()),
            ("Cross-Origin-Opener-Policy", "same-origin".into()),
            ("Content-Security-Policy", CSP.into()),
        ] {
            out.extend(pyhttp::header_line(&v, k, &val));
        }
        for (k, val) in extra {
            out.extend(pyhttp::header_line(&v, k, val));
        }
        out.extend_from_slice(pyhttp::end_headers(&v));
        if self.req.command != "HEAD" {
            out.extend_from_slice(body);
        }
        pyhttp::write_all(&mut self.writer, &out).map_err(io_failure)
    }

    pub fn json_raw(&mut self, status: u16, body: &[u8]) -> Result<(), Failure> {
        self.send(status, body, "application/json; charset=utf-8", &[])
    }

    pub fn fail(&mut self, code: &str, message: &str, status: u16) -> Result<(), Failure> {
        let body = pyjson::dumps(&JValue::object(vec![("error", JValue::object(vec![("code", JValue::str(code)), ("message", JValue::str(message))]))]))
            .map_err(|_| Failure::Internal)?;
        self.json_raw(status, &body)
    }

    fn lengths(&self, digits: usize) -> Result<usize, Failure> {
        if self.req.headers.get("Transfer-Encoding").map(|v| !v.is_empty()).unwrap_or(false) {
            return Err(Failure::store("invalid_body", "Desteklenmeyen istek gövdesi.", 400));
        }
        let lengths = self.req.headers.get_all("Content-Length");
        if lengths.len() != 1 || lengths[0].is_empty() || lengths[0].len() > digits || !lengths[0].bytes().all(|b| b.is_ascii_digit()) {
            return Err(Failure::store("invalid_length", "İstek boyutu geçersiz.", 411));
        }
        Ok(lengths[0].parse().unwrap_or(usize::MAX))
    }

    fn read_exact_body(&mut self, length: usize) -> Result<Vec<u8>, Failure> {
        let raw = pyhttp::read_body(&mut self.reader, length).map_err(io_failure)?;
        if raw.len() != length {
            return Err(Failure::store("incomplete_request", "İstek tamamlanmadı.", 400));
        }
        Ok(raw)
    }

    /// `Handler.binary_body`
    pub fn binary_body(&mut self, content_type: &str, limit: u64) -> Result<Vec<u8>, Failure> {
        let length = self.lengths(9)?;
        if !(0 < length && length as u64 <= limit) {
            return Err(Failure::store("request_too_large", "Dosya boyutu desteklenmiyor.", 413));
        }
        if self.req.headers.content_type() != content_type {
            return Err(Failure::store("invalid_content_type", "Dosya türü desteklenmiyor.", 415));
        }
        self.read_exact_body(length)
    }

    /// `Handler.body`: bounded, duplicate-free JSON object.
    pub fn body(&mut self) -> Result<JValue, Failure> {
        let length = self.lengths(8)?;
        if !(0 < length && length <= MAX_REQUEST) {
            return Err(Failure::store("request_too_large", "İstek boyutu desteklenmiyor.", 413));
        }
        if self.req.headers.content_type() != "application/json" {
            return Err(Failure::store("invalid_content_type", "JSON isteği gerekli.", 415));
        }
        let raw = self.read_exact_body(length)?;
        let value = pyjson::loads(&raw).map_err(|_| Failure::store("invalid_json", "İstek içeriği geçersiz.", 400))?;
        if !matches!(value, JValue::Object(_)) {
            return Err(Failure::store("invalid_json", "İstek bir JSON nesnesi olmalı.", 400));
        }
        Ok(value)
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8 | (a.len() != b.len()) as u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Python `str.encode()` of a Latin-1 header value.
fn utf8_of(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn validate<R: BufRead, W: Write>(cfg: &Config, conn: &Conn<R, W>, api: bool) -> Result<(), Failure> {
    let headers = &conn.req.headers;
    if headers.is_plain_dict {
        return Err(Failure::Internal); // HTTP/0.9: dict has no get_all
    }
    let hosts = headers.get_all("Host");
    if hosts.len() != 1 || hosts[0] != cfg.expected_host() {
        return Err(Failure::store("invalid_host", "Geçersiz yerel uygulama adresi.", 403));
    }
    if let Some(origin) = headers.get("Origin") {
        if origin != cfg.origin() {
            return Err(Failure::store("invalid_origin", "Bu istek başka bir sayfadan geldi.", 403));
        }
    }
    if headers.get("Sec-Fetch-Site") == Some("cross-site") {
        return Err(Failure::store("invalid_origin", "Bu istek başka bir sayfadan geldi.", 403));
    }
    if api {
        let auth = headers.get_all("Authorization");
        let expected = format!("Bearer {}", cfg.token);
        if auth.len() != 1 || !ct_eq(&utf8_of(auth[0]), expected.as_bytes()) {
            return Err(Failure::store("unauthorized", "Yerel uygulama oturumu geçersiz. Notlar uygulamasını yeniden aç.", 401));
        }
    }
    Ok(())
}

fn is_revision(s: &str) -> bool {
    // [1-9][0-9]{0,15}
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= 16 && (b'1'..=b'9').contains(&b[0]) && b[1..].iter().all(|c| c.is_ascii_digit())
}

fn is_uuidish(s: &str) -> bool {
    // [0-9a-f-]{36}
    s.chars().count() == 36 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f' | '-'))
}

fn j(value: &JValue, key: &str) -> Arg {
    Arg::Json(value.get(key).cloned().unwrap_or(JValue::Null))
}

fn json_get_or(value: &JValue, key: &str, default: JValue) -> Arg {
    Arg::Json(value.get(key).cloned().unwrap_or(default))
}

fn wrap(key: &str, inner: Vec<u8>) -> Vec<u8> {
    let mut out = format!("{{\"{}\": ", key).into_bytes();
    out.extend(inner);
    out.push(b'}');
    out
}

/// `folder_id()` in server.py.
fn folder_id(value: &str) -> Result<String, Failure> {
    let bad = || Failure::store("invalid_folder", "Klasör kimliği geçersiz.", 400);
    let value = urlparse::unquote(value, true).map_err(|_| bad())?;
    let n = value.chars().count();
    if !(0 < n && n <= 1024) || value.chars().any(|c| (c as u32) < 32) {
        return Err(bad());
    }
    Ok(value)
}

/// `re.fullmatch(r'/api/folders/([^/]{1,12288})(suffix)', path)`
fn folder_route<'a>(path: &'a str, suffix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix("/api/folders/")?;
    let id = rest.strip_suffix(suffix)?;
    let n = id.chars().count();
    if (1..=12288).contains(&n) && !id.contains('/') {
        Some(id)
    } else {
        None
    }
}

pub const NOTE_ACTIONS: &[&str] = &[
    "resolve", "export", "pages", "preview", "title", "editor", "thumbnail", "ink", "ink-draft", "page-style", "pdf",
    "template", "ink-geometry", "ink-erase", "ink-move", "page-ops", "shapes", "export-file", "ink-pens",
];

/// `re.fullmatch(r'/api/notes/([0-9a-f-]{36})(?:/(actions))?', path)`
fn note_route(path: &str) -> Option<(String, Option<&'static str>)> {
    let rest = path.strip_prefix("/api/notes/")?;
    // 36 ASCII chars from the class (byte length == char count here).
    let id: String = rest.chars().take(36).collect();
    if !is_uuidish(&id) {
        return None;
    }
    let tail = &rest[id.len()..];
    if tail.is_empty() {
        return Some((id, None));
    }
    let action = tail.strip_prefix('/')?;
    NOTE_ACTIONS.iter().find(|a| **a == action).map(|a| (id, Some(*a)))
}

fn set_keys_minus(params: &[(String, Vec<String>)], allowed: &[&str]) -> bool {
    params.iter().any(|(k, _)| !allowed.contains(&k.as_str()))
}

fn set_keys_eq(params: &[(String, Vec<String>)], names: &[&str]) -> bool {
    params.len() == names.len() && names.iter().all(|n| params.iter().any(|(k, _)| k == n))
}

/// `Handler.handle_request` (without the `dispatch` exception mapping).
pub fn handle_request<B: Backend + ?Sized, R: BufRead, W: Write>(cfg: &Config, backend: &B, conn: &mut Conn<R, W>) -> Result<(), Failure> {
    let parsed = urlparse::urlsplit(&conn.req.path).map_err(|_| Failure::Internal)?;
    let path = parsed.path.clone();
    let api = path.starts_with("/api/");
    validate(cfg, conn, api)?;
    if !api {
        if conn.req.command != "GET" && conn.req.command != "HEAD" {
            return Err(Failure::store("method_not_allowed", "Bu işlem desteklenmiyor.", 405));
        }
        if let Some(name) = path.strip_prefix("/local-fonts/") {
            let font_dir = cfg.font_dir.clone();
            let raw = ui_assets::local_font(&font_dir, name).ok_or_else(|| Failure::store("font_unavailable", "Yerel yazı tipi bulunamadı.", 404))?;
            return conn.send(200, &raw, "font/ttf", &[]);
        }
        return static_file(cfg, conn, &path);
    }
    let method = conn.req.command.clone();
    let m = method.as_str();
    use Target::*;
    if m == "GET" && path == "/api/status" {
        let body = backend.json(call(Engine, "status", vec![]))?;
        return conn.json_raw(200, &body);
    }
    if m == "GET" && path == "/api/templates" {
        let params = urlparse::parse_qs(&parsed.query, 1).map_err(|_| Failure::Internal)?;
        let notes = qs_get(&params, "note").cloned().unwrap_or_default();
        if set_keys_minus(&params, &["note"]) || notes.len() > 1 || notes.first().map(|n| !is_uuidish(n)).unwrap_or(false) {
            return Err(Failure::store("invalid_change", "Geçersiz not.", 400));
        }
        let note = notes.first().map(|n| Arg::Str(n.clone())).unwrap_or(Arg::None);
        let body = backend.json(call(Engine, "templates", vec![note]))?;
        return conn.json_raw(200, &body);
    }
    for (route, target, getter, setter) in [
        ("/api/preferences", Preferences, "get_preferences", "set_preferences"),
        ("/api/shortcuts", Shortcuts, "get_shortcuts", "set_shortcuts"),
    ] {
        if path == route {
            if m == "GET" {
                let body = backend.json(call(target, getter, vec![]))?;
                return conn.json_raw(200, &body);
            }
            if m == "PUT" {
                let data = conn.body()?;
                let body = backend.json(call(target, setter, vec![Arg::Json(data)]))?;
                return conn.json_raw(200, &body);
            }
        }
    }
    if m == "GET" && path == "/api/folders" {
        let body = backend.json(call(Store, "folders", vec![]))?;
        return conn.json_raw(200, &body);
    }
    if m == "POST" && path == "/api/folders" {
        let data = conn.body()?;
        let body = backend.json(call(Engine, "folder_change", vec![Arg::None, Arg::Json(data)]))?;
        return conn.json_raw(201, &wrap("folder", body));
    }
    if let Some(id) = folder_route(&path, "") {
        if m == "PATCH" {
            let id = folder_id(id)?;
            let data = conn.body()?;
            let body = backend.json(call(Engine, "folder_change", vec![Arg::Str(id), Arg::Json(data)]))?;
            return conn.json_raw(200, &wrap("folder", body));
        }
    }
    if let Some(id) = folder_route(&path, "/resolve") {
        if m == "POST" {
            let id = folder_id(id)?;
            let data = conn.body()?;
            let body = backend.json(call(Engine, "resolve_folder", vec![Arg::Str(id), j(&data, "strategy")]))?;
            return conn.json_raw(200, &wrap("folder", body));
        }
    }
    if path == "/api/notes/editor" && m == "POST" {
        let data = conn.body()?;
        if !data.keys_equal(&["title", "model"]) {
            return Err(Failure::store("invalid_change", "Başlık ve düzenlenebilir içerik gerekli.", 400));
        }
        let body = backend.json(call(Engine, "create_editor", vec![j(&data, "title"), j(&data, "model")]))?;
        return conn.json_raw(201, &wrap("note", body));
    }
    if path == "/api/notes" {
        if m == "GET" {
            let params = urlparse::parse_qs(&parsed.query, 10).map_err(|_| Failure::Internal)?;
            let query = qs_get(&params, "q").map(|v| v[0].clone()).unwrap_or_default();
            if query.chars().count() > 1024 {
                return Err(Failure::store("query_too_long", "Arama metni çok uzun.", 400));
            }
            let filter = qs_get(&params, "filter").map(|v| v[0].clone()).unwrap_or_else(|| "all".into());
            let folder = qs_get(&params, "folder").map(|v| Arg::Str(v[0].clone())).unwrap_or(Arg::None);
            let body = backend.json(call(Store, "list", vec![Arg::Str(query), Arg::Str(filter), folder]))?;
            return conn.json_raw(200, &wrap("notes", body));
        }
        if m == "POST" {
            let data = conn.body()?;
            let body = backend.json(call(Engine, "create", vec![json_get_or(&data, "title", JValue::str("")), json_get_or(&data, "body", JValue::str(""))]))?;
            return conn.json_raw(201, &wrap("note", body));
        }
    }
    if let Some((identity, action)) = note_route(&path) {
        let id = || Arg::Str(identity.clone());
        match (m, action) {
            ("GET", Some("ink")) => {
                let body = backend.json(call(Engine, "ink", vec![id()]))?;
                return conn.json_raw(200, &body);
            }
            ("POST", Some("template")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["templateId", "expectedRevision"]) {
                    return Err(Failure::store("invalid_change", "Şablon ve not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "apply_template", vec![id(), j(&data, "templateId"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("POST", Some("pdf")) => {
                let params = urlparse::parse_qs(&parsed.query, 2).map_err(|_| Failure::Internal)?;
                let revisions = qs_get(&params, "revision").cloned().unwrap_or_default();
                let names = qs_get(&params, "name").cloned().unwrap_or_default();
                if set_keys_minus(&params, &["revision", "name"]) || revisions.len() != 1 || names.len() > 1 || !is_revision(&revisions[0]) {
                    return Err(Failure::store("revision_required", "Güncel not sürümü gerekli.", 400));
                }
                let limit = backend.max_pdf_bytes()?;
                let pdf = conn.binary_body("application/pdf", limit)?;
                let name = names.first().cloned().unwrap_or_default();
                let body = backend.json(call(Engine, "add_pdf", vec![id(), Arg::Bytes(pdf), Arg::Str(name), Arg::Int(revisions[0].parse().unwrap())]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("GET", Some("ink-geometry")) => {
                let params = urlparse::parse_qs(&parsed.query, 2).map_err(|_| Failure::Internal)?;
                let pages = qs_get(&params, "page").cloned().unwrap_or_default();
                let revisions = qs_get(&params, "revision").cloned().unwrap_or_default();
                if !set_keys_eq(&params, &["page", "revision"]) || pages.len() != 1 || revisions.len() != 1 || !is_uuidish(&pages[0]) || !is_revision(&revisions[0]) {
                    return Err(Failure::store("invalid_ink", "Sayfa ve not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "ink_geometry", vec![id(), Arg::Str(pages[0].clone()), Arg::Int(revisions[0].parse().unwrap())]))?;
                return conn.json_raw(200, &body);
            }
            ("GET", Some("ink-pens")) => {
                // Saved pens of template strokes for the live pen preview.
                let params = urlparse::parse_qs(&parsed.query, 2).map_err(|_| Failure::Internal)?;
                let revisions = qs_get(&params, "revision").cloned().unwrap_or_default();
                if !set_keys_eq(&params, &["revision"]) || revisions.len() != 1 || !is_revision(&revisions[0]) {
                    return Err(Failure::store("invalid_ink", "Not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "ink_pens", vec![id(), Arg::Int(revisions[0].parse().unwrap())]))?;
                return conn.json_raw(200, &body);
            }
            ("PUT", Some("ink-erase")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["erase", "expectedRevision"]) {
                    return Err(Failure::store("invalid_erase", "Silinecek çizgiler ve not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "save_ink_erase", vec![id(), j(&data, "erase"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("PUT", Some("ink-move")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["move", "expectedRevision"]) {
                    return Err(Failure::store("invalid_move", "Taşınacak çizgiler ve not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "save_ink_move", vec![id(), j(&data, "move"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("GET", Some("shapes")) => {
                let body = backend.json(call(Engine, "shapes", vec![id()]))?;
                return conn.json_raw(200, &body);
            }
            ("GET", Some("page-ops")) => {
                let body = backend.json(call(Engine, "page_ops", vec![id()]))?;
                return conn.json_raw(200, &body);
            }
            ("POST", Some("page-ops")) => {
                let data = conn.body()?;
                let op = data.get("op");
                if matches!(op, Some(JValue::Array(_)) | Some(JValue::Object(_))) {
                    return Err(Failure::Internal); // unhashable `op in allowed`
                }
                let allowed: Option<&[&str]> = match op {
                    Some(JValue::Str(JStr::Utf8(s))) if s == "add" => Some(&["op", "afterIndex", "expectedRevision"]),
                    Some(JValue::Str(JStr::Utf8(s))) if s == "delete" || s == "reorder" => Some(&["op", "pageIds", "expectedRevision"]),
                    Some(JValue::Str(JStr::Utf8(s))) if s == "shape" => Some(&["op", "pageId", "shape", "expectedRevision"]),
                    _ => None,
                };
                if allowed.map(|a| !data.keys_equal(a)).unwrap_or(true) {
                    return Err(Failure::store("invalid_change", "Sayfa işlemi ve not sürümü gerekli.", 400));
                }
                let mut spec = Vec::new();
                for k in ["afterIndex", "pageIds", "pageId", "shape"] {
                    if let Some(v) = data.get(k) {
                        spec.push((JStr::Utf8(k.into()), v.clone()));
                    }
                }
                let body = backend.json(call(Engine, "save_page_op", vec![id(), j(&data, "op"), Arg::Json(JValue::Object(spec)), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("GET", Some("page-style")) => {
                let body = backend.json(call(Engine, "page_style", vec![id()]))?;
                return conn.json_raw(200, &body);
            }
            ("PUT", Some("page-style")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["backgroundArgb", "expectedRevision"]) {
                    return Err(Failure::store("invalid_change", "Sayfa rengi ve not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "save_page_background", vec![id(), j(&data, "backgroundArgb"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("GET", Some("ink-draft")) => {
                let body = backend.json(call(Engine, "ink_draft", vec![id()]))?;
                return conn.json_raw(200, &body);
            }
            ("PUT", Some("ink-draft")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["revision", "strokes"]) {
                    return Err(Failure::store("invalid_ink", "Çizim taslağı geçersiz.", 400));
                }
                let body = backend.json(call(Engine, "save_ink_draft", vec![id(), j(&data, "revision"), j(&data, "strokes")]))?;
                return conn.json_raw(200, &body);
            }
            ("PUT", Some("ink")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["expectedRevision", "strokes"]) {
                    return Err(Failure::store("invalid_ink", "Çizim ve not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "save_ink", vec![id(), j(&data, "strokes"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("GET", Some("thumbnail")) => {
                let params = urlparse::parse_qs(&parsed.query, 2).map_err(|_| Failure::Internal)?;
                let revisions = qs_get(&params, "revision").cloned().unwrap_or_default();
                if !set_keys_eq(&params, &["revision"]) || revisions.len() != 1 || !is_revision(&revisions[0]) {
                    return Err(Failure::store("revision_required", "Güncel not sürümü gerekli.", 400));
                }
                let (raw, complete) = backend.bytes_and_flag(call(Thumbnails, "request", vec![id(), Arg::Int(revisions[0].parse().unwrap())]))?;
                let flag = ("X-Notes-Preview-Complete", if complete { "true" } else { "false" }.to_string());
                return match raw {
                    None => conn.send(202, b"{\"state\":\"pending\"}", "application/json; charset=utf-8", &[flag, ("Retry-After", "2".into())]),
                    Some(raw) => conn.send(200, &raw, "image/png", &[flag]),
                };
            }
            ("GET", Some("editor")) => {
                let body = backend.json(call(Engine, "editor", vec![id()]))?;
                return conn.json_raw(200, &body);
            }
            ("PUT", Some("editor")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["title", "model", "expectedRevision"]) {
                    return Err(Failure::store("invalid_change", "Başlık, düzenlenebilir içerik ve not sürümü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "save_editor", vec![id(), j(&data, "title"), j(&data, "model"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("PATCH", Some("title")) => {
                let data = conn.body()?;
                if !data.keys_subset(&["title", "expectedRevision"]) {
                    return Err(Failure::store("invalid_change", "Başlık işleminde not içeriği gönderilemez.", 400));
                }
                let body = backend.json(call(Engine, "save_title", vec![id(), j(&data, "title"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("GET", Some("pages")) => {
                let body = backend.json(call(Previews, "manifest", vec![id()]))?;
                return conn.json_raw(200, &body);
            }
            ("GET", Some("preview")) => {
                let params = urlparse::parse_qs(&parsed.query, 2).map_err(|_| Failure::Internal)?;
                let page = qs_get(&params, "page").cloned().unwrap_or_else(|| vec!["0".into()]);
                if page.len() != 1 || page[0].is_empty() || page[0].len() > 4 || !page[0].bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Failure::store("invalid_page", "Sayfa numarası geçersiz.", 400));
                }
                let raw = backend.bytes_or_none(call(Previews, "request", vec![id(), Arg::Int(page[0].parse().unwrap()), Arg::KwFloat("wait", PREVIEW_WAIT_SECONDS)]))?;
                return match raw {
                    None => conn.json_raw(202, b"{\"state\": \"pending\"}"),
                    Some(raw) => conn.send(200, &raw, "image/png", &[]),
                };
            }
            ("POST", Some("export-file")) => {
                let data = conn.body()?;
                if !data.keys_equal(&["format"]) {
                    return Err(Failure::store("invalid_change", "Dosya türü gerekli.", 400));
                }
                let body = backend.json(call(Engine, "export_file", vec![id(), j(&data, "format")]))?;
                return conn.json_raw(200, &body);
            }
            ("GET", Some("export")) => {
                let (title, body) = backend.note_text(call(Engine, "get", vec![id()]))?;
                let text = format!("{}\n\n{}", title, body).into_bytes();
                let name: String = title.chars().map(|c| if backend.is_word_char(c) || " .()-".contains(c) { c } else { '_' }).take(80).collect();
                let name = if name.is_empty() { "Not".to_string() } else { name };
                let disposition = format!("attachment; filename*=UTF-8''{}", urlparse::quote(&format!("{}.txt", name)));
                return conn.send(200, &text, "text/plain; charset=utf-8", &[("Content-Disposition", disposition)]);
            }
            ("GET", None) => {
                let body = backend.json(call(Engine, "get", vec![id()]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("PUT", None) => {
                let data = conn.body()?;
                let body = backend.json(call(Engine, "save", vec![id(), j(&data, "title"), j(&data, "body"), j(&data, "expectedRevision")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("PATCH", None) => {
                let data = conn.body()?;
                let body = backend.json(call(Engine, "organize", vec![id(), Arg::Json(data)]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            ("POST", Some("resolve")) => {
                let data = conn.body()?;
                let body = backend.json(call(Engine, "resolve", vec![id(), j(&data, "strategy")]))?;
                return conn.json_raw(200, &wrap("note", body));
            }
            _ => {}
        }
    }
    if m == "POST" && path == "/api/sync" {
        conn.body()?;
        let body = backend.json(call(Engine, "request_sync", vec![]))?;
        return conn.json_raw(202, &wrap("started", body));
    }
    if m == "POST" && path == "/api/auth/login" {
        conn.body()?;
        let body = backend.json(call(Sessions, "start_login", vec![]))?;
        return conn.json_raw(202, &wrap("started", body));
    }
    if m == "POST" && path == "/api/auth/disconnect" {
        conn.body()?;
        let body = backend.json(call(Sessions, "disconnect", vec![]))?;
        return conn.json_raw(200, &body);
    }
    Err(Failure::store("not_found", "Bu işlem bulunamadı.", 404))
}

/// `os.path.realpath(path, strict=False)` (what `Path.resolve()` uses).
pub fn realpath(path: &Path) -> PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut resolved = if path.is_absolute() { PathBuf::from("/") } else { std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")) };
    for c in path.components().rev() {
        match c {
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir => {}
            Component::ParentDir => rest.push("..".into()),
            Component::Normal(n) => rest.push(n.to_os_string()),
        }
    }
    let mut hops = 0;
    while let Some(name) = rest.pop() {
        if name == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&name);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                hops += 1;
                if hops > 40 {
                    // Symlink loop: return what is resolved plus the rest unchanged.
                    let mut out = candidate;
                    while let Some(n) = rest.pop() {
                        out.push(n);
                    }
                    return out;
                }
                match std::fs::read_link(&candidate) {
                    Ok(target) => {
                        if target.is_absolute() {
                            resolved = PathBuf::from("/");
                        }
                        for c in target.components().rev() {
                            match c {
                                Component::Normal(n) => rest.push(n.to_os_string()),
                                Component::ParentDir => rest.push("..".into()),
                                _ => {}
                            }
                        }
                    }
                    Err(_) => resolved = candidate,
                }
            }
            _ => resolved = candidate,
        }
    }
    resolved
}

/// `PurePath(relative).name`
fn pure_name(relative: &str) -> &str {
    relative.split('/').filter(|p| !p.is_empty() && *p != ".").last().unwrap_or("")
}

/// `Handler.static`
fn static_file<R: BufRead, W: Write>(cfg: &Config, conn: &mut Conn<R, W>, request_path: &str) -> Result<(), Failure> {
    let unquoted = urlparse::unquote(request_path, false).unwrap_or_default();
    let relative = unquoted.trim_start_matches('/');
    if relative.contains('\0') {
        return Err(Failure::store("not_found", "Dosya bulunamadı.", 404));
    }
    let joined = cfg.static_dir.join(if relative.is_empty() { "index.html" } else { relative });
    let mut path = realpath(&joined);
    if !path.starts_with(&cfg.static_dir) {
        return Err(Failure::store("not_found", "Dosya bulunamadı.", 404));
    }
    if !path.is_file() {
        if !pure_name(relative).contains('.') {
            path = cfg.static_dir.join("index.html");
        }
        if !path.is_file() {
            return Err(Failure::store("frontend_missing", "Arayüz derlenmemiş. desktop dizininde npm run build çalıştır.", 503));
        }
    }
    let name = path.file_name().map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned()).unwrap_or_default();
    let mut content_type = mime::guess_type(&name).unwrap_or_else(|| "application/octet-stream".into());
    if content_type.starts_with("text/") || content_type == "application/javascript" || content_type == "application/json" {
        content_type.push_str("; charset=utf-8");
    }
    let raw = std::fs::read(&path).map_err(|_| Failure::Internal)?;
    conn.send(200, &raw, &content_type, &[])
}

/// One connection, like `socketserver` + `BaseHTTPRequestHandler.handle()`
/// with `protocol_version = 'HTTP/1.0'` (exactly one request).
pub fn serve_connection<B: Backend + ?Sized>(cfg: &Config, backend: &B, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::with_capacity(8192, read_half);
    let mut writer = stream;
    match pyhttp::read_request(&mut reader) {
        Err(_) | Ok(Incoming::Close) => {}
        Ok(Incoming::Error(reply)) => {
            let _ = pyhttp::write_all(&mut writer, &pyhttp::error_response(&reply));
        }
        Ok(Incoming::Request(req)) => {
            if !METHODS.contains(&req.command.as_str()) {
                let reply = pyhttp::ErrorReply {
                    code: 501,
                    message: Some(format!("Unsupported method ({})", pyhttp::py_repr(&req.command))),
                    explain: None,
                    command: Some(req.command.clone()),
                    request_version: req.request_version.clone(),
                };
                let _ = pyhttp::write_all(&mut writer, &pyhttp::error_response(&reply));
            } else {
                let mut conn = Conn { reader, writer, req };
                dispatch(cfg, backend, &mut conn);
                writer = conn.writer;
            }
        }
    }
    let _ = writer.flush();
    let _ = writer.shutdown(Shutdown::Write);
}

/// `Handler.dispatch`
pub fn dispatch<B: Backend + ?Sized, R: BufRead, W: Write>(cfg: &Config, backend: &B, conn: &mut Conn<R, W>) {
    match handle_request(cfg, backend, conn) {
        Ok(()) | Err(Failure::Disconnect) => {}
        Err(Failure::Api { code, message, status }) => {
            let _ = conn.fail(&code, &message, status);
        }
        Err(Failure::Internal) => {
            let _ = conn.fail("internal_error", INTERNAL_MESSAGE, 500);
        }
    }
}

/// Helper for embedders: read the whole body of a reader (unused by the server).
pub fn read_to_end<Rd: Read>(r: &mut Rd) -> io::Result<Vec<u8>> {
    let mut v = Vec::new();
    r.read_to_end(&mut v)?;
    Ok(v)
}
