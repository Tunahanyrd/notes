//! `opener.open(request, timeout=...)` with `NoRedirect` and the HTTPError
//! response treated as a normal response: one connection, one request, the
//! response parsed exactly like `http.client.HTTPResponse`.
use crate::error::NetError;
use crate::message::Message;
use crate::py;
use crate::request::{self, HttpRequest, Prepared};
use crate::tls;
use crate::url::{self, RequestUrl};
use openssl::ssl::{SslContext, SslStream};
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// http.client `_MAXLINE`
const MAXLINE: usize = 65536;
/// http.client `_MAXHEADERS`
const MAXHEADERS: usize = 100;
/// http.client `_MAXINTERIMRESPONSES`
const MAXINTERIM: usize = 100;
/// `socket.makefile('rb')` buffer (`io.DEFAULT_BUFFER_SIZE`).
const RECV_SIZE: usize = 128 * 1024;

/// A TCP socket with Python's timeout model: every operation gets its own
/// `timeout` (connect per address, each recv; a `sendall`/TLS handshake is
/// bounded as a whole).
pub(crate) struct Socket {
    tcp: TcpStream,
    timeout: Option<Duration>,
    deadline: Cell<Option<Instant>>,
}

impl Socket {
    fn arm(&self) {
        self.deadline.set(self.timeout.map(|t| Instant::now() + t));
    }
    fn remaining(&self) -> io::Result<Option<Duration>> {
        match self.deadline.get() {
            None => Ok(None),
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
                } else {
                    Ok(Some(left))
                }
            }
        }
    }
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(left) = self.remaining()? {
            self.tcp.set_read_timeout(Some(left))?;
        }
        self.tcp.read(buf)
    }
}

impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(left) = self.remaining()? {
            self.tcp.set_write_timeout(Some(left))?;
        }
        self.tcp.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) enum Stream {
    Plain(Socket),
    Tls(Box<SslStream<Socket>>),
    /// In-memory response bytes for the fuzz harness (`fuzzing` feature only;
    /// never compiled into the product).
    #[cfg(feature = "fuzzing")]
    Mem(MemStream),
}

/// A canned response delivered in `chunk`-sized recvs (`fuzzing` only).
#[cfg(feature = "fuzzing")]
pub(crate) struct MemStream {
    data: Vec<u8>,
    pos: usize,
    chunk: usize,
}

impl Stream {
    fn arm(&self) {
        match self {
            Stream::Plain(s) => s.arm(),
            Stream::Tls(s) => s.get_ref().arm(),
            #[cfg(feature = "fuzzing")]
            Stream::Mem(_) => {}
        }
    }
    /// One `recv_into`: 0 at EOF.
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        self.arm();
        match self {
            Stream::Plain(s) => loop {
                match s.read(buf) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(NetError::from_io(&e)),
                }
            },
            Stream::Tls(s) => tls::read(s, buf),
            #[cfg(feature = "fuzzing")]
            Stream::Mem(m) => {
                let n = buf.len().min(m.chunk).min(m.data.len() - m.pos);
                buf[..n].copy_from_slice(&m.data[m.pos..m.pos + n]);
                m.pos += n;
                Ok(n)
            }
        }
    }
    /// `sendall(data)`
    fn send_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        self.arm();
        match self {
            Stream::Plain(s) => s.write_all(data).map_err(|e| NetError::from_io(&e)),
            Stream::Tls(s) => tls::write_all(s, data),
            #[cfg(feature = "fuzzing")]
            Stream::Mem(_) => Ok(()),
        }
    }
}

/// `sock.makefile('rb')`: a buffered reader over the connection.
pub(crate) struct Reader {
    stream: Stream,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl Reader {
    fn available(&self) -> &[u8] {
        &self.buf[self.pos..]
    }
    fn consume(&mut self, n: usize) -> Vec<u8> {
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        out
    }
    /// One more recv into the buffer; false at EOF.
    fn fill(&mut self) -> Result<bool, NetError> {
        if self.eof {
            return Ok(false);
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let old = self.buf.len();
        self.buf.resize(old + RECV_SIZE, 0);
        let result = self.stream.recv(&mut self.buf[old..]);
        let n = *result.as_ref().unwrap_or(&0);
        self.buf.truncate(old + n);
        result?;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        Ok(true)
    }
    /// `readline(limit)`: through the next `\n`, at most `limit` bytes, or to EOF.
    fn readline(&mut self, limit: usize) -> Result<Vec<u8>, NetError> {
        loop {
            let avail = self.available();
            if let Some(i) = avail.iter().take(limit).position(|&b| b == b'\n') {
                return Ok(self.consume(i + 1));
            }
            if avail.len() >= limit {
                return Ok(self.consume(limit));
            }
            if !self.fill()? {
                let n = self.available().len();
                return Ok(self.consume(n));
            }
        }
    }
    /// `read(n)` of a BufferedReader: `n` bytes, fewer only at EOF.
    fn read(&mut self, n: usize) -> Result<Vec<u8>, NetError> {
        while self.available().len() < n {
            if !self.fill()? {
                break;
            }
        }
        let take = n.min(self.available().len());
        Ok(self.consume(take))
    }
    /// `read()`: everything up to EOF.
    fn read_all(&mut self) -> Result<Vec<u8>, NetError> {
        while self.fill()? {}
        let n = self.available().len();
        Ok(self.consume(n))
    }
}

/// `http.client.HTTPResponse` as urllib hands it out (an `HTTPError` for
/// non-2xx statuses reads the same way).
pub struct Response {
    status: u16,
    headers: Message,
    reader: Option<Reader>,
    chunked: bool,
    chunk_left: Option<i128>,
    length: Option<i128>,
    head: bool,
}

fn line_too_long(what: &str) -> NetError {
    NetError::escape("LineTooLong", format!("got more than {MAXLINE} bytes when reading {what}"))
}

fn incomplete(partial: usize, expected: Option<i128>) -> NetError {
    match expected {
        Some(more) => NetError::escape("IncompleteRead", format!("IncompleteRead({partial} bytes read, {more} more expected)")),
        None => NetError::escape("IncompleteRead", format!("IncompleteRead({partial} bytes read)")),
    }
}

/// `str.split(None, maxsplit)`
fn split_ws(s: &str, mut maxsplit: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while maxsplit > 0 {
        rest = rest.trim_start_matches(py::is_space);
        if rest.is_empty() {
            return out;
        }
        let end = rest.find(py::is_space).unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = &rest[end..];
        maxsplit -= 1;
    }
    let rest = rest.trim_start_matches(py::is_space);
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

impl Response {
    /// `HTTPResponse._read_status`
    fn read_status(reader: &mut Reader) -> Result<(String, i128), NetError> {
        let raw = reader.readline(MAXLINE + 1)?;
        if raw.len() > MAXLINE {
            return Err(line_too_long("status line"));
        }
        let line = py::latin1(&raw);
        if line.is_empty() {
            return Err(NetError::os("RemoteDisconnected", "Remote end closed connection without response"));
        }
        let parts = split_ws(&line, 2);
        let (version, status) = if parts.len() == 3 {
            (parts[0], Some(parts[1]))
        } else {
            let parts = split_ws(&line, 1);
            if parts.len() == 2 { (parts[0], Some(parts[1])) } else { ("", None) }
        };
        if !version.starts_with("HTTP/") {
            return Err(NetError::escape("BadStatusLine", format!("{line:?}")));
        }
        match status.and_then(py::int10) {
            Some(code) if (100..=999).contains(&code) => Ok((version.to_string(), code)),
            _ => Err(NetError::escape("BadStatusLine", format!("{line:?}"))),
        }
    }

    /// `http.client._read_headers`
    fn read_headers(reader: &mut Reader) -> Result<Vec<Vec<u8>>, NetError> {
        let mut headers = Vec::new();
        loop {
            let line = reader.readline(MAXLINE + 1)?;
            if line.len() > MAXLINE {
                return Err(line_too_long("header line"));
            }
            let end = line.is_empty() || line == b"\r\n" || line == b"\n";
            headers.push(line);
            if headers.len() > MAXHEADERS {
                return Err(NetError::escape("HTTPException", format!("got more than {MAXHEADERS} headers")));
            }
            if end {
                return Ok(headers);
            }
        }
    }

    /// `HTTPResponse.begin`
    fn begin(mut reader: Reader, method: &str) -> Result<Response, NetError> {
        let mut found = None;
        for _ in 0..MAXINTERIM {
            let (version, status) = Self::read_status(&mut reader)?;
            if status != 100 {
                found = Some((version, status));
                break;
            }
            Self::read_headers(&mut reader)?;
        }
        let Some((version, status)) = found else {
            return Err(NetError::escape("HTTPException", format!("got more than {MAXINTERIM} interim responses")));
        };
        if !(version == "HTTP/1.0" || version == "HTTP/0.9" || version.starts_with("HTTP/1.")) {
            return Err(NetError::escape("UnknownProtocol", version));
        }
        let headers = Message::parse(&Self::read_headers(&mut reader)?);
        let chunked = headers.get("transfer-encoding").is_some_and(|t| py::lower(t) == "chunked");
        let mut length = None;
        if let Some(text) = headers.get("content-length") {
            if !text.is_empty() && !chunked {
                length = py::int10(text).filter(|&n| n >= 0);
            }
        }
        let head = method == "HEAD";
        if status == 204 || status == 304 || (100..200).contains(&status) || head {
            length = Some(0);
        }
        Ok(Response { status: status as u16, headers, reader: Some(reader), chunked, chunk_left: None, length, head })
    }

    /// `response.code`
    pub fn status(&self) -> u16 {
        self.status
    }
    /// `response.headers`
    pub fn headers(&self) -> &Message {
        &self.headers
    }
    /// `response.headers.get(name)`
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    fn close(&mut self) {
        self.reader = None;
    }

    /// `HTTPResponse.read(amt)`: at most `amt` bytes of the body; an empty
    /// result is the end. Content-Encoding is not decoded.
    pub fn read(&mut self, amt: usize) -> Result<Vec<u8>, NetError> {
        if self.reader.is_none() {
            return Ok(Vec::new());
        }
        if self.head {
            self.close();
            return Ok(Vec::new());
        }
        if self.chunked {
            return self.read_chunked(amt as i128);
        }
        let mut amt = amt as i128;
        if let Some(length) = self.length {
            amt = amt.min(length);
        }
        let Some(reader) = self.reader.as_mut() else { return Ok(Vec::new()) };
        let s = reader.read(amt as usize)?;
        if s.is_empty() && amt != 0 {
            self.close();
        } else if let Some(length) = self.length {
            let left = length - s.len() as i128;
            self.length = Some(left);
            if left == 0 {
                self.close();
            }
        }
        Ok(s)
    }

    /// `HTTPResponse._safe_read(amt)` (negative: everything to EOF).
    fn safe_read(&mut self, amt: i128) -> Result<Vec<u8>, NetError> {
        let reader = self.reader.as_mut().ok_or_else(|| incomplete(0, Some(amt.max(0))))?;
        if amt < 0 {
            return reader.read_all();
        }
        let data = reader.read(amt as usize)?;
        if (data.len() as i128) < amt {
            return Err(incomplete(data.len(), Some(amt - data.len() as i128)));
        }
        Ok(data)
    }

    /// `HTTPResponse._read_next_chunk_size`
    fn read_next_chunk_size(&mut self) -> Result<Result<i128, ()>, NetError> {
        let reader = self.reader.as_mut().ok_or_else(|| incomplete(0, None))?;
        let mut line = reader.readline(MAXLINE + 1)?;
        if line.len() > MAXLINE {
            return Err(line_too_long("chunk size"));
        }
        if let Some(i) = line.iter().position(|&b| b == b';') {
            line.truncate(i);
        }
        // CPython's int(line, 16) also accepts "-1"; `_safe_read` of a
        // negative size then reads to EOF and ignores the caller's bound.
        // A negative chunk size is a framing error here.
        match py::int16_bytes(&line) {
            Some(n) if n >= 0 => Ok(Ok(n)),
            _ => {
                self.close();
                Ok(Err(()))
            }
        }
    }

    /// `HTTPResponse._read_and_discard_trailer`
    fn read_and_discard_trailer(&mut self) -> Result<(), NetError> {
        let reader = self.reader.as_mut().ok_or_else(|| incomplete(0, None))?;
        let mut trailers = 0;
        loop {
            let line = reader.readline(MAXLINE + 1)?;
            if line.len() > MAXLINE {
                return Err(line_too_long("trailer line"));
            }
            if line.is_empty() || line == b"\r\n" || line == b"\n" {
                return Ok(());
            }
            trailers += 1;
            if trailers > MAXHEADERS {
                return Err(NetError::escape("HTTPException", format!("got more than {MAXHEADERS} trailers")));
            }
        }
    }

    /// `HTTPResponse._get_chunk_left`
    fn get_chunk_left(&mut self) -> Result<Option<i128>, NetError> {
        let mut chunk_left = self.chunk_left;
        if chunk_left.is_none_or(|n| n == 0) {
            if chunk_left.is_some() {
                self.safe_read(2)?;
            }
            chunk_left = match self.read_next_chunk_size()? {
                Ok(n) => Some(n),
                Err(()) => return Err(incomplete(0, None)),
            };
            if chunk_left == Some(0) {
                self.read_and_discard_trailer()?;
                self.close();
                chunk_left = None;
            }
            self.chunk_left = chunk_left;
        }
        Ok(chunk_left)
    }

    /// `HTTPResponse._read_chunked(amt)`
    fn read_chunked(&mut self, mut amt: i128) -> Result<Vec<u8>, NetError> {
        let mut value = Vec::new();
        let result = (|| -> Result<(), NetError> {
            while let Some(chunk_left) = self.get_chunk_left()? {
                if amt <= chunk_left {
                    value.extend(self.safe_read(amt)?);
                    self.chunk_left = Some(chunk_left - amt);
                    break;
                }
                value.extend(self.safe_read(chunk_left)?);
                amt = amt.saturating_sub(chunk_left);
                self.chunk_left = Some(0);
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(value),
            Err(NetError::Raised { class: "IncompleteRead", .. }) => Err(incomplete(value.len(), None)),
            Err(e) => Err(e),
        }
    }
}

/// Fuzz entry point (`fuzzing` feature only): parse `raw` as the response to
/// a `method` request exactly like [`Client::open`] + `send_once`'s exchange
/// (status line, interim responses, headers, Content-Length / chunked body,
/// `read(max_bytes + 1)`, gzip Content-Encoding bounded by `max_bytes`).
/// `chunk` is the recv size the canned stream delivers per read.
#[cfg(feature = "fuzzing")]
pub fn fuzz_exchange(raw: &[u8], method: &str, chunk: usize, max_bytes: usize)
    -> Result<(u16, Vec<(String, String)>, Vec<u8>), NetError> {
    let stream = Stream::Mem(MemStream { data: raw.to_vec(), pos: 0, chunk: chunk.max(1) });
    let reader = Reader { stream, buf: Vec::new(), pos: 0, eof: false };
    let mut response = Response::begin(reader, method)?;
    if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        let target = response.headers.get("location").or_else(|| response.headers.get("uri"));
        if let Some(target) = target {
            url::urlsplit_check(target).map_err(NetError::value)?;
        }
    }
    let _ = response.headers().content_type();
    let encoding = py::lower(response.header("Content-Encoding").unwrap_or(""));
    let raw = response.read(max_bytes + 1)?;
    if raw.len() > max_bytes {
        return Err(NetError::value("response wire limit exceeded"));
    }
    let body = if encoding == "gzip" { crate::gzip::read_bounded(&raw, max_bytes + 1)? } else { raw };
    Ok((response.status(), response.headers().items().to_vec(), body))
}

/// A configured client (Python builds one opener + default SSL context per
/// call; this reuses the context, which has no observable effect because
/// client-side session caching stays off).
#[derive(Clone)]
pub struct Client {
    timeout: Option<Duration>,
    tls: SslContext,
}

impl Client {
    /// Production client: system trust store, `timeout` per socket operation.
    pub fn new(timeout: Duration) -> Result<Client, NetError> {
        Ok(Client { timeout: Some(timeout), tls: tls::default_context(&[])? })
    }

    /// Test-only: the system store plus `extra_roots` (a throwaway CA for a
    /// local TLS fake). Never used by production code paths.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_extra_roots(timeout: Duration, extra_roots: &[openssl::x509::X509]) -> Result<Client, NetError> {
        Ok(Client { timeout: Some(timeout), tls: tls::default_context(extra_roots)? })
    }

    /// `opener.open(request, timeout)` with NoRedirect; a non-2xx status is
    /// returned as a normal response (as `except HTTPError as error:
    /// response = error` does). `request.url` must already be accepted by
    /// `Request()` (see [`crate::request_url_error`]).
    pub fn open(&self, request: &HttpRequest) -> Result<Response, NetError> {
        let parsed = url::request_url(&request.url)?;
        self.open_parsed(request, &parsed, None)
    }

    pub(crate) fn open_parsed(&self, request: &HttpRequest, parsed: &RequestUrl, vetted: Option<&[SocketAddr]>)
        -> Result<Response, NetError> {
        let prepared = request::prepare(request, parsed)?;
        let mut stream = self.connect(&prepared, vetted)?;
        stream.send_all(&prepared.head)?;
        for part in &prepared.body {
            stream.send_all(part)?;
        }
        let reader = Reader { stream, buf: Vec::new(), pos: 0, eof: false };
        let response = Response::begin(reader, &prepared.method)?;
        // HTTPRedirectHandler.http_error_30x runs urlparse(Location) before
        // NoRedirect.redirect_request declines: a malformed value is a ValueError.
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let target = response.headers.get("location").or_else(|| response.headers.get("uri"));
            if let Some(target) = target {
                url::urlsplit_check(target).map_err(NetError::value)?;
            }
        }
        Ok(response)
    }

    /// `socket.create_connection((host, port), timeout)` (+ TCP_NODELAY and
    /// the TLS wrap for https). With `vetted`, only those addresses are tried.
    fn connect(&self, prepared: &Prepared, vetted: Option<&[SocketAddr]>) -> Result<Stream, NetError> {
        let port: u16 = if (0..=65535).contains(&prepared.port) {
            prepared.port as u16
        } else if prepared.port > 65535 && prepared.port <= u64::MAX as i128 {
            // glibc getaddrinfo: numeric service truncated by htons().
            (prepared.port & 0xffff) as u16
        } else {
            return Err(NetError::os("gaierror", "[Errno -8] Servname not supported for ai_socktype"));
        };
        let addresses: Vec<SocketAddr> = match vetted {
            Some(list) => list.iter().map(|a| SocketAddr::new(a.ip(), port)).collect(),
            None => {
                if !prepared.host.is_ascii() {
                    return Err(NetError::value_class("UnicodeError", "non-ASCII host names are not supported"));
                }
                (prepared.host.as_str(), port).to_socket_addrs()
                    .map_err(|e| NetError::os("gaierror", e.to_string()))?
                    .collect()
            }
        };
        let mut last = None;
        let mut tcp = None;
        for address in &addresses {
            let attempt = match self.timeout {
                Some(t) => TcpStream::connect_timeout(address, t),
                None => TcpStream::connect(address),
            };
            match attempt {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let tcp = match tcp {
            Some(t) => t,
            None => {
                return Err(match last {
                    Some(e) => NetError::from_io(&e),
                    None => NetError::os("OSError", "getaddrinfo returns an empty list"),
                })
            }
        };
        let _ = tcp.set_nodelay(true);
        let socket = Socket { tcp, timeout: self.timeout, deadline: Cell::new(None) };
        if prepared.scheme == "https" {
            socket.arm();
            Ok(Stream::Tls(Box::new(tls::wrap(&self.tls, socket, &prepared.host)?)))
        } else {
            Ok(Stream::Plain(socket))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_ws;

    #[test]
    fn python_split() {
        assert_eq!(split_ws("HTTP/1.1 200 OK\r\n", 2), vec!["HTTP/1.1", "200", "OK\r\n"]);
        assert_eq!(split_ws("HTTP/1.1 200\r\n", 2), vec!["HTTP/1.1", "200"]);
        assert_eq!(split_ws("HTTP/1.1 200\r\n", 1), vec!["HTTP/1.1", "200\r\n"]);
        assert_eq!(split_ws("  a   b  c d ", 2), vec!["a", "b", "c d "]);
        assert!(split_ws(" \r\n", 2).is_empty());
    }
}
