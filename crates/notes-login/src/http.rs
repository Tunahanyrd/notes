//! urllib `Request` model, the network seam, and faithful ports of the two
//! single-request transports: `notes_auth_bridge.perform_once` and
//! `notes_cloud.send_once` (claim file before transport, receipt after,
//! identical bounds and result names). No retries, ever.
use crate::error::{Error, Result};
use crate::files::{private_write, save_json};
use crate::json::{self, Value};
use crate::runtime::{unix_seconds, Runtime};
use std::path::Path;
use std::time::Duration;

/// A `urllib.request.Request`: header names are `str.capitalize()`d and kept
/// in insertion order (a repeated name keeps its first position, last value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub data: Option<Vec<u8>>,
}

/// Python `str.capitalize()` for an ASCII header name.
pub fn capitalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

impl HttpRequest {
    /// `urllib.request.Request(url, data=data, headers=headers, method=method)`
    pub fn new(url: &str, data: Option<Vec<u8>>, headers: &[(&str, &str)], method: Option<&str>) -> HttpRequest {
        let mut list: Vec<(String, String)> = Vec::new();
        for (k, v) in headers {
            let key = capitalize(k);
            match list.iter_mut().find(|(n, _)| *n == key) {
                Some(slot) => slot.1 = v.to_string(),
                None => list.push((key, v.to_string())),
            }
        }
        let method = match method {
            Some(m) => m.to_string(),
            None if data.is_some() => "POST".into(),
            None => "GET".into(),
        };
        HttpRequest { method, url: crate::urls::request_unwrap(url), headers: list, data }
    }

    /// `request.get_header(name)` (name given capitalized).
    pub fn header(&self, name: &str) -> Option<&str> {
        let key = capitalize(name);
        self.headers.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    /// `dict(request.header_items())` as borrowed pairs.
    pub fn header_pairs(&self) -> Vec<(&str, &str)> {
        self.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }
}

/// One HTTP response with any status (urllib's `HTTPError` is a response too).
#[derive(Clone, Debug, Default)]
pub struct HttpResponse {
    pub status: u16,
    /// Response headers in wire order (names as received).
    pub headers: Vec<(String, String)>,
    /// At most `read_limit` bytes of the body (`response.read(read_limit)`).
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// `response.headers.get(name)` (case-insensitive, first occurrence).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// `email.message.Message.get_content_type()`
    pub fn content_type(&self) -> String {
        let Some(value) = self.header("Content-Type") else { return "text/plain".into() };
        let main = value.split_once(';').map(|(a, _)| a).unwrap_or(value);
        let ctype = crate::runtime::py_strip(main).to_lowercase();
        if ctype.matches('/').count() != 1 {
            return "text/plain".into();
        }
        ctype
    }
}

/// A transport failure (`OSError`/`URLError`/timeout/TLS); never a status.
/// `head` is set when the status line and headers arrived but reading the
/// body failed (Python then already recorded `httpStatus`/`contentType`).
/// `escaped` marks exceptions the Python callers do not catch
/// (`http.client.HTTPException` subclasses such as `IncompleteRead`,
/// `BadStatusLine`, `InvalidURL`): the claim stays, no receipt is written and
/// the step fails, exactly like the Python process dying with a traceback.
#[derive(Debug, Clone)]
pub struct NetError {
    pub message: String,
    pub head: Option<HttpResponse>,
    pub escaped: bool,
}

impl NetError {
    pub fn new(message: impl Into<String>) -> NetError {
        NetError { message: message.into(), head: None, escaped: false }
    }
}

fn escaped_error() -> Error {
    Error::Io(std::io::Error::other("uncaught transport exception; claim retained, no receipt"))
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The network seam. The contract is urllib's
/// `build_opener(NoRedirect(), HTTPSHandler(context=ssl.create_default_context()))`
/// followed by `opener.open(request, timeout)` and `response.read(read_limit)`:
/// exactly one request, redirects are NOT followed (a 3xx is returned as a
/// response), TLS verified with the system trust store, no cookie jar, no
/// retry, no proxy surprises. Any HTTP status is `Ok`; only transport
/// failures are `Err`.
pub trait Network: Send + Sync {
    fn request(&self, request: &HttpRequest, timeout: Duration, read_limit: usize) -> std::result::Result<HttpResponse, NetError>;
}

/// `http.client` refuses URLs containing these (`InvalidURL`, an
/// `HTTPException` raised inside `opener.open`, i.e. after the claim file and
/// not caught by the callers).
fn url_has_disallowed(url: &str) -> bool {
    url.chars().any(|c| (c as u32) <= 0x20 || c as u32 == 0x7f)
}

fn open(net: &dyn Network, request: &HttpRequest, timeout: u64, limit: usize) -> std::result::Result<HttpResponse, NetError> {
    if url_has_disallowed(&request.url) {
        return Err(NetError { escaped: true, ..NetError::new("URL can't contain control characters") });
    }
    net.request(request, Duration::from_secs(timeout), limit)
}

/// `with opener.open(request, timeout) as response: if response.status != 200:
/// raise ...; raw = response.read(limit + 1)` (non-2xx is an HTTPError).
pub fn get_ok(net: &dyn Network, request: &HttpRequest, timeout: u64, limit: usize, status_message: &str) -> Result<Vec<u8>> {
    let response = open(net, request, timeout, limit + 1).map_err(|e| Error::Io(std::io::Error::other(e.message)))?;
    if !(200..300).contains(&response.status) {
        return Err(Error::Io(std::io::Error::other("HTTP error status")));
    }
    if response.status != 200 {
        return Err(Error::value(status_message));
    }
    Ok(response.body)
}

/// `notes_auth_bridge.perform_once(directory, name, claim_name, request)`.
/// Returns `(receipt.get('httpStatus'), body)`; `body` is the parsed JSON
/// value (possibly not an object, exactly like Python).
pub fn perform_once(
    net: &dyn Network,
    rt: &dyn Runtime,
    directory: &Path,
    name: &str,
    claim_name: &str,
    request: &HttpRequest,
) -> Result<(Option<u16>, Option<Value>)> {
    private_write(&directory.join(claim_name), b"claimed-before-transport\n")?;
    let mut receipt = Value::obj(vec![
        ("schemaVersion", Value::Int(1)),
        ("startedUnixSeconds", Value::Int(unix_seconds(rt))),
        ("method", Value::s("POST")),
        ("requestCount", Value::Int(1)),
        ("automaticRetry", Value::Bool(false)),
        ("redirectsFollowed", Value::Bool(false)),
        ("cookiesAttached", Value::Bool(false)),
        ("tlsVerified", Value::Bool(true)),
        ("result", Value::s("transport_outcome_unknown")),
    ]);
    let mut body = None;
    let mut status = None;
    let mut escaped = false;
    let outcome = (|| -> std::result::Result<(), ()> {
        let response = match open(net, request, 20, 128 * 1024 + 1) {
            Ok(r) => r,
            Err(e) if e.escaped => {
                escaped = true;
                return Err(());
            }
            Err(e) => {
                if let Some(head) = e.head {
                    status = Some(head.status);
                    receipt.set("httpStatus", Value::Int(head.status as i64));
                }
                return Err(());
            }
        };
        status = Some(response.status);
        receipt.set("httpStatus", Value::Int(response.status as i64));
        let raw = response.body;
        receipt.set("responseBytes", Value::Int(raw.len() as i64));
        if raw.len() > 128 * 1024 {
            receipt.set("result", Value::s("response_limit_exceeded"));
            return Ok(());
        }
        private_write(&directory.join(format!("{name}-response.body")), &raw).map_err(|_| ())?;
        let parsed = json::loads(&raw).map_err(|_| ())?;
        let fields = match &parsed {
            Value::Object(items) => Some(items.iter().map(|(k, v)| (k.clone(), Value::s(v.type_name()))).collect::<Vec<_>>()),
            _ => None,
        };
        body = Some(parsed);
        let fields = fields.ok_or(())?;
        receipt.set("result", Value::s("json_response"));
        receipt.set("responseFieldTypes", Value::Object(fields));
        Ok(())
    })();
    if escaped {
        return Err(escaped_error());
    }
    if outcome.is_err() {
        receipt.set("result", Value::s("transport_or_response_error"));
    }
    save_json(&directory.join(format!("{name}-receipt.json")), &receipt)?;
    Ok((status, body))
}

pub const SEND_ONCE_MAX_BYTES: usize = 2 * 1024 * 1024;

/// `notes_cloud.send_once(directory, name, request, max_bytes, quiet)`.
/// Returns `(receipt.get('httpStatus'), payload)`.
///
/// Divergence: CPython lets a corrupt deflate stream (`zlib.error`) escape
/// without writing the receipt; here every decoding failure is recorded as
/// `transport_or_response_error` with a receipt.
pub fn send_once(
    net: &dyn Network,
    rt: &dyn Runtime,
    directory: &Path,
    name: &str,
    request: &HttpRequest,
    max_bytes: usize,
) -> Result<(Option<u16>, Option<Vec<u8>>)> {
    if !(1..=32 * 1024 * 1024).contains(&max_bytes) {
        return Err(Error::value("unsupported response size bound"));
    }
    private_write(&directory.join(format!(".{name}-claim")), b"claimed-before-transport\n")?;
    let mut receipt = Value::obj(vec![
        ("schemaVersion", Value::Int(1)),
        ("startedUnixSeconds", Value::Int(unix_seconds(rt))),
        ("method", Value::s(&request.method)),
        ("requestCount", Value::Int(1)),
        ("automaticRetry", Value::Bool(false)),
        ("redirectsFollowed", Value::Bool(false)),
        ("tlsVerified", Value::Bool(true)),
        ("cookiesAttached", Value::Bool(false)),
        ("result", Value::s("transport_outcome_unknown")),
    ]);
    let mut payload: Option<Vec<u8>> = None;
    let mut status = None;
    let mut escaped = false;
    let outcome = (|| -> std::result::Result<(), ()> {
        let (response, read_failed) = match open(net, request, 25, max_bytes + 1) {
            Ok(r) => (r, false),
            Err(NetError { escaped: true, .. }) => {
                escaped = true;
                return Err(());
            }
            Err(NetError { head: Some(head), .. }) => (head, true),
            Err(_) => return Err(()),
        };
        status = Some(response.status);
        receipt.set("httpStatus", Value::Int(response.status as i64));
        receipt.set("contentType", Value::s(&response.content_type()));
        let full_type = response.header("Content-Type").unwrap_or("").to_string();
        if full_type.chars().count() > 8192 {
            return Err(());
        }
        save_json(&directory.join(format!("{name}-response-meta.json")), &Value::obj(vec![("contentType", Value::Str(full_type))]))
            .map_err(|_| ())?;
        if read_failed {
            return Err(());
        }
        let encoding = response.header("Content-Encoding").unwrap_or("").to_lowercase();
        let raw = response.body;
        if raw.len() > max_bytes {
            return Err(());
        }
        let decoded = if encoding == "gzip" {
            gunzip_limited(&raw, max_bytes + 1)?
        } else if encoding.is_empty() || encoding == "identity" {
            raw.clone()
        } else {
            return Err(());
        };
        if decoded.len() > max_bytes {
            return Err(());
        }
        payload = Some(decoded);
        private_write(&directory.join(format!("{name}-response.body")), payload.as_ref().unwrap()).map_err(|_| ())?;
        receipt.set("wireBytes", Value::Int(raw.len() as i64));
        receipt.set("decodedBytes", Value::Int(payload.as_ref().unwrap().len() as i64));
        receipt.set("result", Value::s("response_saved"));
        Ok(())
    })();
    if escaped {
        return Err(escaped_error());
    }
    if outcome.is_err() {
        receipt.set("result", Value::s("transport_or_response_error"));
    }
    save_json(&directory.join(format!("{name}-receipt.json")), &receipt)?;
    Ok((status, payload))
}

/// `gzip.GzipFile(fileobj=BytesIO(raw)).read(limit)` (multi-member).
fn gunzip_limited(raw: &[u8], limit: usize) -> std::result::Result<Vec<u8>, ()> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(raw).take(limit as u64).read_to_end(&mut out).map_err(|_| ())?;
    Ok(out)
}
