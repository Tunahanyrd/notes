//! `urllib.request.Request` and the bytes `http.client` writes for it.
use crate::error::NetError;
use crate::py;
use crate::url::{self, RequestUrl};

/// What urllib sends as `User-Agent` when the request has none
/// (`OpenerDirector.addheaders`, `"Python-urllib/%d.%d" % sys.version_info[:2]`).
///
/// Kept byte-identical to the CPython 3.14 traffic the Python engine sends
/// today. The Cloud/registration/changes builders always set their own
/// `User-Agent`; this default only reaches the wire for requests built
/// without one (the signed file URL request of previews/PDF backgrounds).
pub const DEFAULT_USER_AGENT: &str = "Python-urllib/3.14";

/// `urllib.request.Request(url, data=..., headers=..., method=...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    /// `request.get_method()`
    pub method: String,
    /// `request.full_url` as given to the constructor.
    pub url: String,
    /// `request.header_items()` of the user headers: names already
    /// `str.capitalize()`d (`Content-type`, `X-sc-app-id`), insertion order,
    /// unique. [`HttpRequest::new`] produces this from raw names.
    pub headers: Vec<(String, String)>,
    /// `request.data`
    pub data: Option<Vec<u8>>,
}

impl HttpRequest {
    /// Like `Request(url, data=data, headers=headers, method=method)`:
    /// every name goes through `str.capitalize()` and a repeated name keeps
    /// its first position with the last value (`self.headers[k.capitalize()] = v`).
    /// `method: None` is `get_method()`'s default (`POST` with data, else `GET`).
    pub fn new(method: Option<&str>, url: &str, headers: &[(String, String)], data: Option<Vec<u8>>) -> HttpRequest {
        let mut normalized: Vec<(String, String)> = Vec::new();
        for (k, v) in headers {
            set(&mut normalized, &py::capitalize(k), v);
        }
        let method = match method {
            Some(m) if !m.is_empty() => m.to_string(),
            _ => if data.is_some() { "POST" } else { "GET" }.to_string(),
        };
        HttpRequest { method, url: url.to_string(), headers: normalized, data }
    }

    /// `request.has_header(name)` over the user headers (exact capitalised name).
    fn has(&self, name: &str) -> bool {
        self.headers.iter().any(|(k, _)| k == name)
    }
}

fn set(headers: &mut Vec<(String, String)>, key: &str, value: &str) {
    match headers.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value.to_string(),
        None => headers.push((key.to_string(), value.to_string())),
    }
}

/// The request after urllib's `do_request_`/`do_open` and http.client's
/// `putrequest`/`putheader` validation: ready to put on the wire.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub scheme: String,
    /// `HTTPConnection.host` (brackets stripped)
    pub host: String,
    /// `HTTPConnection.port` (Python int; range checked at connect)
    pub port: i128,
    /// Request line + headers + blank line (first `sendall`).
    pub head: Vec<u8>,
    /// Body `sendall`s that follow (already chunk-encoded when applicable).
    pub body: Vec<Vec<u8>>,
    pub method: String,
}

const METHODS_EXPECTING_BODY: [&str; 3] = ["PATCH", "POST", "PUT"];

/// `_is_legal_header_name = re.compile(rb'[^:\s][^:\r\n]*').fullmatch`
fn legal_header_name(name: &[u8]) -> bool {
    match name.split_first() {
        None => false,
        Some((first, rest)) => {
            !matches!(first, b':' | b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
                && !rest.iter().any(|b| matches!(b, b':' | b'\r' | b'\n'))
        }
    }
}

/// `_is_illegal_header_value = re.compile(rb'\n(?![ \t])|\r(?![ \t\n])').search`
fn illegal_header_value(value: &[u8]) -> bool {
    for (i, &b) in value.iter().enumerate() {
        let next = value.get(i + 1).copied();
        if b == b'\n' && !matches!(next, Some(b' ' | b'\t')) {
            return true;
        }
        if b == b'\r' && !matches!(next, Some(b' ' | b'\t' | b'\n')) {
            return true;
        }
    }
    false
}

fn bytes_repr(b: &[u8]) -> String {
    let mut s = String::from("b'");
    for &c in b {
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\'' => s.push_str("\\'"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s.push('\'');
    s
}

/// `HTTPConnection.putheader(name, value)`
fn putheader(lines: &mut Vec<Vec<u8>>, name: &str, value: &str) -> Result<(), NetError> {
    if !name.is_ascii() {
        return Err(NetError::value_class("UnicodeEncodeError", "'ascii' codec can't encode header name"));
    }
    if !legal_header_name(name.as_bytes()) {
        return Err(NetError::value(format!("Invalid header name {}", bytes_repr(name.as_bytes()))));
    }
    let value = py::encode_latin1(value)
        .ok_or_else(|| NetError::value_class("UnicodeEncodeError", "'latin-1' codec can't encode header value"))?;
    if illegal_header_value(&value) {
        return Err(NetError::value(format!("Invalid header value {}", bytes_repr(&value))));
    }
    let mut line = name.as_bytes().to_vec();
    line.extend_from_slice(b": ");
    line.extend_from_slice(&value);
    lines.push(line);
    Ok(())
}

/// Everything `opener.open(request)` does before the first byte is sent.
/// `parsed` is the request's URL as `Request()` parsed it.
pub(crate) fn prepare(request: &HttpRequest, parsed: &RequestUrl) -> Result<Prepared, NetError> {
    // OpenerDirector._open: only http/https handlers are provided here.
    let default_port = match parsed.scheme.as_str() {
        "http" => 80,
        "https" => 443,
        other => return Err(NetError::os("URLError", format!("<urlopen error unknown url type: {other}>"))),
    };
    let host = match parsed.host.as_deref() {
        Some(h) if !h.is_empty() => h,
        _ => return Err(NetError::os("URLError", "<urlopen error no host given>")),
    };

    // AbstractHTTPHandler.do_request_: unredirected headers.
    let mut unredirected: Vec<(String, String)> = Vec::new();
    if let Some(data) = &request.data {
        if !request.has("Content-type") {
            unredirected.push(("Content-type".into(), "application/x-www-form-urlencoded".into()));
        }
        if !request.has("Content-length") && !request.has("Transfer-encoding") {
            unredirected.push(("Content-length".into(), data.len().to_string()));
        }
    }
    if !request.has("Host") {
        unredirected.push(("Host".into(), host.to_string()));
    }
    if !request.has("User-agent") {
        unredirected.push(("User-agent".into(), DEFAULT_USER_AGENT.into()));
    }

    // do_open: HTTPConnection(host) -> _get_hostport + _validate_host.
    let (conn_host, port) = url::host_port(host, default_port)?;
    if let Some(c) = conn_host.chars().find(|&c| c <= ' ' || c == '\u{7f}') {
        return Err(NetError::escape("InvalidURL", format!("URL can't contain control characters. (found at least {c:?})")));
    }

    // headers = dict(unredirected); headers.update(req.headers);
    // headers["Connection"] = "close"; {name.title(): val}
    let mut headers = unredirected;
    for (k, v) in &request.headers {
        set(&mut headers, k, v);
    }
    set(&mut headers, "Connection", "close");
    let headers: Vec<(String, String)> = headers.into_iter().map(|(k, v)| (py::title(&k), v)).collect();
    let encode_chunked_requested = request.has("Transfer-encoding");

    // HTTPConnection._send_request
    let names: Vec<String> = headers.iter().map(|(k, _)| py::lower(k)).collect();
    let has = |n: &str| names.iter().any(|k| k == n);
    let method = &request.method;
    if let Some(c) = method.chars().find(|&c| c < ' ') {
        return Err(NetError::value(format!("method can't contain control characters. (found at least {c:?})")));
    }
    let selector = if parsed.selector.is_empty() { "/" } else { parsed.selector.as_str() };
    if let Some(c) = selector.chars().find(|&c| c <= ' ' || c == '\u{7f}') {
        return Err(NetError::escape("InvalidURL", format!("URL can't contain control characters. (found at least {c:?})")));
    }
    let request_line = format!("{method} {selector} HTTP/1.1");
    if !request_line.is_ascii() {
        return Err(NetError::value_class("UnicodeEncodeError", "'ascii' codec can't encode request line"));
    }
    let mut lines: Vec<Vec<u8>> = vec![request_line.into_bytes()];
    // putrequest(skip_host=True because urllib always sends Host)
    if !has("accept-encoding") {
        putheader(&mut lines, "Accept-Encoding", "identity")?;
    }
    let mut encode_chunked = encode_chunked_requested;
    if !has("content-length") {
        if !has("transfer-encoding") {
            encode_chunked = false;
            let length = match &request.data {
                None if METHODS_EXPECTING_BODY.contains(&py::upper(method).as_str()) => Some(0),
                None => None,
                Some(data) => Some(data.len()),
            };
            if let Some(length) = length {
                putheader(&mut lines, "Content-Length", &length.to_string())?;
            }
        }
    } else {
        encode_chunked = false;
    }
    for (k, v) in &headers {
        putheader(&mut lines, k, v)?;
    }
    lines.push(Vec::new());
    lines.push(Vec::new());
    let head = lines.join(&b"\r\n"[..]);

    // _send_output(message_body, encode_chunked)
    let mut body = Vec::new();
    if let Some(data) = &request.data {
        if !data.is_empty() {
            if encode_chunked {
                let mut chunk = format!("{:X}\r\n", data.len()).into_bytes();
                chunk.extend_from_slice(data);
                chunk.extend_from_slice(b"\r\n");
                body.push(chunk);
            } else {
                body.push(data.clone());
            }
        }
        if encode_chunked {
            body.push(b"0\r\n\r\n".to_vec());
        }
    }
    Ok(Prepared { scheme: parsed.scheme.clone(), host: conn_host, port, head, body, method: method.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(r: &HttpRequest) -> Vec<u8> {
        let parsed = url::request_url(&r.url).unwrap();
        let p = prepare(r, &parsed).unwrap();
        let mut out = p.head.clone();
        for b in p.body {
            out.extend(b);
        }
        out
    }

    #[test]
    fn header_order_matches_urllib_capture() {
        // Byte strings captured from CPython 3.14 urllib against a local socket.
        let r = HttpRequest::new(None, "http://127.0.0.1:59595/a?b=1#frag", &[], None);
        assert_eq!(wire(&r), b"GET /a?b=1 HTTP/1.1\r\nAccept-Encoding: identity\r\nHost: 127.0.0.1:59595\r\nUser-Agent: Python-urllib/3.14\r\nConnection: close\r\n\r\n");
        let h = |k: &str, v: &str| (k.to_string(), v.to_string());
        let r = HttpRequest::new(Some("POST"), "http://127.0.0.1:59595?q",
            &[h("x-sc-e2ee-state", "1"), h("Content-Type", "application/json"), h("User-Agent", "U/1")], Some(b"xyz".to_vec()));
        assert_eq!(wire(&r), &b"POST /?q HTTP/1.1\r\nAccept-Encoding: identity\r\nContent-Length: 3\r\nHost: 127.0.0.1:59595\r\nX-Sc-E2Ee-State: 1\r\nContent-Type: application/json\r\nUser-Agent: U/1\r\nConnection: close\r\n\r\nxyz"[..]);
        let r = HttpRequest::new(Some("POST"), "http://127.0.0.1:59595",
            &[h("Accept-Encoding", "gzip"), h("connection", "keep"), h("host", "h.example")], None);
        assert_eq!(wire(&r), b"POST / HTTP/1.1\r\nContent-Length: 0\r\nUser-Agent: Python-urllib/3.14\r\nAccept-Encoding: gzip\r\nConnection: close\r\nHost: h.example\r\n\r\n");
        let r = HttpRequest::new(Some("PUT"), "http://127.0.0.1:59595/p", &[], Some(Vec::new()));
        assert_eq!(wire(&r), b"PUT /p HTTP/1.1\r\nAccept-Encoding: identity\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 0\r\nHost: 127.0.0.1:59595\r\nUser-Agent: Python-urllib/3.14\r\nConnection: close\r\n\r\n");
    }

    #[test]
    fn validation_errors_match_http_client() {
        let h = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
        let prep = |r: &HttpRequest| prepare(r, &url::request_url(&r.url).unwrap());
        let e = prep(&HttpRequest::new(None, "https://h/", &h("X", "a\r\nInjected: 1"), None)).unwrap_err();
        assert_eq!(e.family(), Some(crate::Family::Value));
        let e = prep(&HttpRequest::new(None, "https://h/a b", &[], None)).unwrap_err();
        assert_eq!((e.family(), e.python_class()), (Some(crate::Family::Escape), "InvalidURL"));
        let e = prep(&HttpRequest::new(None, "https://h/\u{e9}", &[], None)).unwrap_err();
        assert_eq!(e.family(), Some(crate::Family::Value));
        let e = prep(&HttpRequest::new(None, "https://h/", &h("X", "\u{1f600}"), None)).unwrap_err();
        assert_eq!(e.python_class(), "UnicodeEncodeError");
        assert!(prep(&HttpRequest::new(None, "https://h/", &h("X", "\u{e9}"), None)).is_ok());
        let e = prep(&HttpRequest::new(None, "https://h:abc/", &[], None)).unwrap_err();
        assert_eq!(e.python_class(), "InvalidURL");
        let e = prep(&HttpRequest::new(None, "ftp://h/", &[], None)).unwrap_err();
        assert!(e.is_os());
        let e = prep(&HttpRequest::new(None, "https:///x", &[], None)).unwrap_err();
        assert!(e.is_os());
    }

    #[test]
    fn transfer_encoding_header_chunks_the_body() {
        let r = HttpRequest::new(Some("POST"), "http://h/", &[("Transfer-Encoding".into(), "chunked".into())], Some(b"abc".to_vec()));
        let w = wire(&r);
        assert!(w.ends_with(b"Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n0\r\n\r\n"));
        assert!(!w.windows(15).any(|x| x == b"Content-Length:"));
    }
}
