//! Wire behaviour of CPython 3.14 `http.server.BaseHTTPRequestHandler` with
//! `protocol_version = 'HTTP/1.0'`, `server_version = 'LinuxNotes'`,
//! `sys_version = ''`: one request per connection, request-line and header
//! parsing (`http.client.parse_headers` + `email` compat32 rules), status
//! lines, `Server`/`Date` headers and the HTML error page. Pure Rust.
use std::io::{self, BufRead, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::pytables::{DEFAULT_ERROR_MESSAGE, HTTP_STATUS};

pub const SERVER_VERSION: &str = "LinuxNotes";
pub const PROTOCOL_VERSION: &str = "HTTP/1.0";
const MAX_LINE: usize = 65536;
const MAX_HEADERS: usize = 100;

/// Request headers with `email.message.Message` lookup semantics.
#[derive(Debug, Clone, Default)]
pub struct Headers {
    pub items: Vec<(String, String)>,
    /// HTTP/0.9 requests have `self.headers = {}` (a dict, not a Message).
    pub is_plain_dict: bool,
}

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.items.iter().find(|(k, _)| k.eq_ignore_ascii_case(name) || k.to_lowercase() == name.to_lowercase()).map(|(_, v)| v.as_str())
    }
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.items.iter().filter(|(k, _)| k.to_lowercase() == name.to_lowercase()).map(|(_, v)| v.as_str()).collect()
    }
    /// `Message.get_content_type()`.
    pub fn content_type(&self) -> String {
        match self.get("content-type") {
            None => "text/plain".into(),
            Some(value) => {
                let ctype = value.split(';').next().unwrap_or("").trim().to_lowercase();
                if ctype.matches('/').count() != 1 {
                    "text/plain".into()
                } else {
                    ctype
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub command: String,
    pub path: String,
    pub request_version: String,
    pub headers: Headers,
}

/// A `send_error` call made while reading the request.
#[derive(Debug, Clone)]
pub struct ErrorReply {
    pub code: u16,
    pub message: Option<String>,
    pub explain: Option<String>,
    pub command: Option<String>,
    pub request_version: String,
}

pub enum Incoming {
    Request(Request),
    /// Nothing to answer (empty line / EOF / blank request line).
    Close,
    Error(ErrorReply),
}

pub fn is_timeout(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

fn read_line_limited<R: BufRead>(r: &mut R, limit: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(line);
        }
        let room = limit - line.len();
        let window = &buf[..buf.len().min(room)];
        if let Some(pos) = window.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&window[..=pos]);
            r.consume(pos + 1);
            return Ok(line);
        }
        let n = window.len();
        line.extend_from_slice(window);
        r.consume(n);
        if line.len() >= limit {
            return Ok(line);
        }
    }
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Python `str.isspace()` restricted to Latin-1.
fn py_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c' | '\x1d' | '\x1e' | '\x1f' | ' ' | '\u{85}' | '\u{a0}')
}

/// Python `repr(str)` for Latin-1 text.
pub fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c)
            }
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) || c as u32 == 0xad => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c if (c as u32) > 0xff && !(c.is_alphanumeric() || c.is_ascii_punctuation()) => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `email.feedparser` header block parsing (compat32), lines already split.
fn parse_header_block(text: &str) -> Vec<(String, String)> {
    // NLCRE_crack: split keeping \r\n, \r, \n as line ends.
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        current.push(c);
        if c == '\r' {
            if i + 1 < chars.len() && chars[i + 1] == '\n' {
                current.push('\n');
                i += 1;
            }
            lines.push(std::mem::take(&mut current));
        } else if c == '\n' {
            lines.push(std::mem::take(&mut current));
        }
        i += 1;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    let header_re = |line: &str| -> bool {
        if line.starts_with("From ") || line.starts_with('\t') || line.starts_with(' ') {
            return true;
        }
        for c in line.chars() {
            if c == ':' {
                return true;
            }
            let v = c as u32;
            if !((0x21..=0x39).contains(&v) || (0x3b..=0x7e).contains(&v)) {
                return false;
            }
        }
        false
    };
    let mut header_lines: Vec<String> = Vec::new();
    for line in lines {
        if !header_re(&line) {
            break; // blank separator or the start of a body
        }
        header_lines.push(line);
    }
    let mut out = Vec::new();
    let mut last: Option<(String, Vec<String>)> = None;
    let finish = |last: &mut Option<(String, Vec<String>)>, out: &mut Vec<(String, String)>| {
        if let Some((_, source)) = last.take() {
            let (name, value) = source[0].split_once(':').unwrap();
            let mut value = value.trim_start_matches([' ', '\t']).to_string();
            for extra in &source[1..] {
                value.push_str(extra);
            }
            out.push((name.to_string(), value.trim_end_matches(['\r', '\n']).to_string()));
        }
    };
    let count = header_lines.len();
    for (lineno, line) in header_lines.into_iter().enumerate() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, source)) = last.as_mut() {
                source.push(line);
            }
            continue;
        }
        finish(&mut last, &mut out);
        if line.starts_with("From ") {
            let _ = (lineno, count);
            continue; // unix-from / misplaced envelope: not a header
        }
        match line.find(':') {
            Some(0) | None => continue,
            Some(i) => last = Some((line[..i].to_string(), vec![line])),
        }
    }
    finish(&mut last, &mut out);
    out
}

/// `handle_one_request` up to the `do_<METHOD>` dispatch.
pub fn read_request<R: BufRead>(r: &mut R) -> io::Result<Incoming> {
    let raw = read_line_limited(r, MAX_LINE + 1)?;
    if raw.len() > MAX_LINE {
        return Ok(Incoming::Error(ErrorReply { code: 414, message: None, explain: None, command: Some(String::new()), request_version: String::new() }));
    }
    if raw.is_empty() {
        return Ok(Incoming::Close);
    }
    let requestline = latin1(&raw).trim_end_matches(['\r', '\n']).to_string();
    let words: Vec<&str> = requestline.split(py_space).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Ok(Incoming::Close);
    }
    let err = |code, message: String, version: &str| {
        Ok(Incoming::Error(ErrorReply { code, message: Some(message), explain: None, command: None, request_version: version.to_string() }))
    };
    let mut request_version = "HTTP/0.9".to_string();
    if words.len() >= 3 {
        let version = words[words.len() - 1];
        let parsed = (|| {
            let base = version.strip_prefix("HTTP/")?;
            let parts: Vec<&str> = base.split('.').collect();
            if parts.len() != 2 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) || p.len() > 10) {
                return None;
            }
            Some((parts[0].parse::<u64>().ok()?, parts[1].parse::<u64>().ok()?, base.to_string()))
        })();
        let Some((major, _minor, base)) = parsed else {
            return err(400, format!("Bad request version ({})", py_repr(version)), "");
        };
        request_version = version.to_string();
        if major >= 2 {
            return err(505, format!("Invalid HTTP version ({})", base), &request_version);
        }
    }
    if !(2..=3).contains(&words.len()) {
        return err(400, format!("Bad request syntax ({})", py_repr(&requestline)), "");
    }
    let (command, mut path) = (words[0].to_string(), words[1].to_string());
    if words.len() == 2 {
        if command != "GET" {
            return err(400, format!("Bad HTTP/0.9 request type ({})", py_repr(&command)), "");
        }
        if path.starts_with("//") {
            path = format!("/{}", path.trim_start_matches('/'));
        }
        return Ok(Incoming::Request(Request { command, path, request_version, headers: Headers { items: vec![], is_plain_dict: true } }));
    }
    if path.starts_with("//") {
        path = format!("/{}", path.trim_start_matches('/'));
    }
    // http.client._read_headers
    let mut block = Vec::new();
    let mut count = 0;
    loop {
        let line = read_line_limited(r, MAX_LINE + 1)?;
        if line.len() > MAX_LINE {
            return Ok(Incoming::Error(ErrorReply {
                code: 431,
                message: Some("Line too long".into()),
                explain: Some(format!("got more than {} bytes when reading header line", MAX_LINE)),
                command: Some(command),
                request_version,
            }));
        }
        count += 1;
        if count > MAX_HEADERS {
            return Ok(Incoming::Error(ErrorReply {
                code: 431,
                message: Some("Too many headers".into()),
                explain: Some(format!("got more than {} headers", MAX_HEADERS)),
                command: Some(command),
                request_version,
            }));
        }
        let end = line.is_empty() || line == b"\r\n" || line == b"\n";
        block.extend_from_slice(&line);
        if end {
            break;
        }
    }
    let headers = Headers { items: parse_header_block(&latin1(&block)), is_plain_dict: false };
    Ok(Incoming::Request(Request { command, path, request_version, headers }))
}

pub fn status_info(code: u16) -> Option<(&'static str, &'static str)> {
    HTTP_STATUS.iter().find(|(c, _, _)| *c == code).map(|(_, p, d)| (*p, *d))
}

/// `email.utils.formatdate(time.time(), usegmt=True)`.
pub fn http_date(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    // civil_from_days (H. Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(month - 1) as usize];
    format!("{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT", weekday, d, mon, year, h, m, s)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn latin1_bytes(s: &str) -> Vec<u8> {
    // The reason phrase is Latin-1 by construction (repr of a Latin-1 line).
    s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect()
}

/// The status line + `Server`/`Date` headers (`send_response`), or nothing for HTTP/0.9.
pub fn response_head(request_version: &str, code: u16, message: Option<&str>) -> Vec<u8> {
    if request_version == "HTTP/0.9" {
        return Vec::new();
    }
    let reason = message.map(str::to_string).unwrap_or_else(|| status_info(code).map(|x| x.0.to_string()).unwrap_or_default());
    let mut out = latin1_bytes(&format!("{} {} {}\r\n", PROTOCOL_VERSION, code, reason));
    out.extend_from_slice(format!("Server: {} \r\n", SERVER_VERSION).as_bytes());
    out.extend_from_slice(format!("Date: {}\r\n", http_date(SystemTime::now())).as_bytes());
    out
}

pub fn header_line(request_version: &str, name: &str, value: &str) -> Vec<u8> {
    if request_version == "HTTP/0.9" {
        return Vec::new();
    }
    latin1_bytes(&format!("{}: {}\r\n", name, value))
}

pub fn end_headers(request_version: &str) -> &'static [u8] {
    if request_version == "HTTP/0.9" {
        b""
    } else {
        b"\r\n"
    }
}

/// `send_error(code, message, explain)` bytes.
pub fn error_response(reply: &ErrorReply) -> Vec<u8> {
    let (short, long) = status_info(reply.code).unwrap_or(("???", "???"));
    let message = reply.message.clone().unwrap_or_else(|| short.to_string());
    let explain = reply.explain.clone().unwrap_or_else(|| long.to_string());
    let v = reply.request_version.as_str();
    let mut out = response_head(v, reply.code, Some(&message));
    out.extend(header_line(v, "Connection", "close"));
    let mut body = Vec::new();
    if reply.code >= 200 && ![204, 205, 304].contains(&reply.code) {
        body = DEFAULT_ERROR_MESSAGE
            .replacen("%(code)d", &reply.code.to_string(), 1)
            .replacen("%(message)s", &html_escape(&message), 1)
            .replacen("%(code)s", &reply.code.to_string(), 1)
            .replacen("%(explain)s", &html_escape(&explain), 1)
            .into_bytes();
        out.extend(header_line(v, "Content-Type", "text/html;charset=utf-8"));
        out.extend(header_line(v, "Content-Length", &body.len().to_string()));
    }
    out.extend_from_slice(end_headers(v));
    if reply.command.as_deref() != Some("HEAD") {
        out.extend(body);
    }
    out
}

/// Read exactly like `rfile.read(n)`: up to n bytes, fewer only at EOF.
pub fn read_body<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n.min(1 << 20));
    let mut limited = r.take(n as u64);
    limited.read_to_end(&mut out)?;
    Ok(out)
}

pub fn write_all<W: Write>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    w.write_all(bytes)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_like_cpython() {
        let raw = b"GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:1\r\nX-A:  v \r\n folded\r\nhost: second\r\n\r\nbody";
        let Incoming::Request(req) = read_request(&mut &raw[..]).unwrap() else { panic!() };
        assert_eq!(req.headers.get_all("Host"), vec!["127.0.0.1:1", "second"]);
        assert_eq!(req.headers.get("x-a"), Some("v \r\n folded"));
        assert_eq!(py_repr("a'b"), "\"a'b\"");
        assert_eq!(py_repr("\u{e9}\u{a0}"), "'\u{e9}\\xa0'");
        assert_eq!(http_date(UNIX_EPOCH + std::time::Duration::from_secs(1793033417)), "Mon, 26 Oct 2026 16:50:17 GMT");
        let Incoming::Error(e) = read_request(&mut &b"GET / HTTP/2.0\r\n\r\n"[..]).unwrap() else { panic!() };
        assert_eq!(e.code, 505);
    }
}
