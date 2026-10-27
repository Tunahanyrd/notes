//! The network seam. `CloudAPI(..., transport=fake)` in the Python tests calls
//! `transport(directory, name, request, limit)` and expects
//! `(status, raw_or_None, content_type)`; without a transport the oracle calls
//! `tools/notes_cloud.send_once`, which writes its own receipts. [`Transport`]
//! models both (see [`Transport::writes_receipts`]).
use crate::util::capitalize;
use notes_host::Fail;
use std::path::Path;

/// A plain Python dict of header names (insertion ordered, exact keys).
pub type Headers = Vec<(String, String)>;

/// `headers[key] = value` on a Python dict.
pub fn set_header(headers: &mut Headers, key: &str, value: &str) {
    match headers.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value.to_string(),
        None => headers.push((key.to_string(), value.to_string())),
    }
}

/// `headers.get(key)` on a Python dict (exact key).
pub fn get_header<'a>(headers: &'a Headers, key: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `urllib.request.Request(url, data=..., method=..., headers=...)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// `request.get_method()`
    pub method: String,
    /// `request.full_url`
    pub url: String,
    /// `request.header_items()`: names normalised with `str.capitalize()`
    /// (`Content-type`, `X-sn-transaction-id`), first-insertion order.
    pub headers: Headers,
    /// `request.data`
    pub data: Option<Vec<u8>>,
}

impl Request {
    pub fn new(method: &str, url: impl Into<String>, headers: &Headers, data: Option<Vec<u8>>) -> Request {
        let mut normalized: Headers = Vec::new();
        for (k, v) in headers {
            set_header(&mut normalized, &capitalize(k), v);
        }
        Request { method: method.to_string(), url: url.into(), headers: normalized, data }
    }
    /// `request.get_header(name)`; the name is capitalised first for convenience.
    pub fn header(&self, name: &str) -> Option<&str> {
        get_header(&self.headers, &capitalize(name))
    }
    /// Path component of `full_url` (`urllib.parse.urlsplit(url).path`).
    pub fn path(&self) -> &str {
        let rest = self.url.split_once("://").map(|(_, r)| r).unwrap_or(&self.url);
        let start = rest.find('/').unwrap_or(rest.len());
        let tail = &rest[start..];
        let end = tail.find(['?', '#']).unwrap_or(tail.len());
        &tail[..end]
    }
    /// Query component of `full_url`.
    pub fn query(&self) -> &str {
        match self.url.split_once('?') {
            Some((_, q)) => q.split('#').next().unwrap_or(""),
            None => "",
        }
    }
}

/// `(status, raw, content_type)`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Response {
    /// HTTP status (None: no response, like a transport that returns `None`).
    pub status: Option<i64>,
    /// Decoded body; `None` mirrors Python's `raw is None`.
    pub body: Option<Vec<u8>>,
    /// Full `Content-Type` (None is written to receipts as `null`).
    pub content_type: Option<String>,
}

impl Response {
    pub fn new(status: i64, body: impl Into<Vec<u8>>, content_type: &str) -> Response {
        Response { status: Some(status), body: Some(body.into()), content_type: Some(content_type.to_string()) }
    }
}

/// One HTTP exchange, never retried by the caller.
///
/// * Injected-transport semantics (`writes_receipts() == false`, the default):
///   `CloudApi::call` validates the size bound and writes
///   `<name>-receipt.json`, `<name>-response.body` and
///   `<name>-response-meta.json` itself.
/// * `send_once` semantics (`writes_receipts() == true`): the transport
///   writes `.<name>-claim`, its own receipt, body and
///   `<name>-response-meta.json` (`{"contentType": ...}`); `call` then reads
///   the content type from that meta file and ignores `Response::content_type`.
///
/// Errors are returned as the exception the oracle would raise (an OSError
/// family `Fail` for a broken connection, `APIError` for policy failures).
pub trait Transport: Send + Sync {
    fn send(&self, directory: &Path, name: &str, request: &Request, limit: usize) -> Result<Response, Fail>;
    fn writes_receipts(&self) -> bool {
        false
    }
}

impl<F> Transport for F
where
    F: Fn(&Path, &str, &Request, usize) -> Result<Response, Fail> + Send + Sync,
{
    fn send(&self, directory: &Path, name: &str, request: &Request, limit: usize) -> Result<Response, Fail> {
        self(directory, name, request, limit)
    }
}

/// Default transport until the pure `send_once` (crates/rs-auth) is plugged
/// in: every request fails like an unreachable Cloud (`APIError
/// cloud_unavailable`), nothing leaves the machine.
pub struct NoNetwork;

impl Transport for NoNetwork {
    fn send(&self, _directory: &Path, _name: &str, _request: &Request, _limit: usize) -> Result<Response, Fail> {
        Err(Fail::api("cloud_unavailable", "Cloud yanıtı alınamadı; yerel veriler korunuyor."))
    }
}
