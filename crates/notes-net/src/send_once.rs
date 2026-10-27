//! `tools/notes_cloud.send_once`: one claimed, receipted HTTP exchange.
use crate::client::Client;
use crate::error::NetError;
use crate::gzip;
use crate::py::{self, J};
use crate::request::HttpRequest;
use crate::url;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `notes_cloud.MAX_BYTES`
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// `opener.open(request, timeout=25)`
pub const SEND_ONCE_TIMEOUT: Duration = Duration::from_secs(25);

/// What escapes `send_once` (everything else becomes a
/// `transport_or_response_error` receipt).
#[derive(Debug)]
pub enum SendError {
    /// `ValueError('unsupported response size bound')`, before the claim.
    SizeBound,
    /// The ValueError `urllib.request.Request(url)` raises for this URL (no
    /// scheme, malformed bracketed host): the Python caller fails while
    /// building the request, so nothing is claimed.
    InvalidRequest(NetError),
    /// `private_write` of `.<name>-claim` failed (FileExistsError when the
    /// name was used before): nothing was sent.
    Claim(io::Error),
    /// An exception outside OSError/ValueError/EOFError (http.client
    /// HTTPException such as BadStatusLine/IncompleteRead/LineTooLong/
    /// InvalidURL, or zlib.error): Python propagates it, so no receipt is
    /// written; the claim (and possibly `<name>-response-meta.json`) stays.
    Escaped(NetError),
    /// Writing `<name>-receipt.json` or printing it failed (OSError).
    Receipt(io::Error),
    /// The TLS context could not be built (no request was sent).
    Setup(NetError),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::SizeBound => f.write_str("ValueError: unsupported response size bound"),
            SendError::InvalidRequest(e) | SendError::Escaped(e) | SendError::Setup(e) => write!(f, "{e}"),
            SendError::Claim(e) | SendError::Receipt(e) => write!(f, "OSError: {e}"),
        }
    }
}

impl std::error::Error for SendError {}

/// `notes_login.private_write`: O_CREAT|O_EXCL|O_WRONLY 0600, write, fsync.
pub fn private_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    file.write_all(data)?;
    file.flush()?;
    file.sync_all()
}

/// `notes_login.save_json` for a flat object.
fn save_json(path: &Path, obj: &[(&str, J)]) -> io::Result<()> {
    let mut text = py::dumps_indent2(obj);
    text.push('\n');
    private_write(path, text.as_bytes())
}

/// The ValueError `urllib.request.Request(url)` would raise, if any.
pub fn request_url_error(url: &str) -> Option<NetError> {
    url::request_url(url).err()
}

struct Receipt {
    fields: Vec<(&'static str, J)>,
}

impl Receipt {
    fn set(&mut self, key: &'static str, value: J) {
        match self.fields.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.fields.push((key, value)),
        }
    }
    fn status(&self) -> Option<u16> {
        self.fields.iter().find(|(k, _)| *k == "httpStatus").and_then(|(_, v)| match v {
            J::Int(i) => u16::try_from(*i).ok(),
            _ => None,
        })
    }
}

/// `send_once(directory, name, request, max_bytes, quiet)` with the
/// production client (system trust store, 25 s socket timeout), built after
/// the claim like Python's `build_opener(..., create_default_context())`.
pub fn send_once(directory: &Path, name: &str, request: &HttpRequest, max_bytes: usize, quiet: bool)
    -> Result<(Option<u16>, Option<Vec<u8>>), SendError> {
    run(directory, name, request, max_bytes, quiet, || Client::new(SEND_ONCE_TIMEOUT), None)
}

/// The exchange part of send_once; `Err` is the raised exception. `payload`
/// mirrors the Python local of the same name (it survives a failed body write).
fn exchange(client: &Client, directory: &Path, name: &str, request: &HttpRequest, max_bytes: usize,
            receipt: &mut Receipt, payload: &mut Option<Vec<u8>>) -> Result<(), NetError> {
    let mut response = client.open(request)?;
    receipt.set("httpStatus", J::Int(response.status() as i64));
    receipt.set("contentType", J::Str(response.headers().content_type()));
    let full_type = response.header("Content-Type").unwrap_or("").to_string();
    if full_type.chars().count() > 8192 {
        return Err(NetError::value("response content type too large"));
    }
    save_json(&directory.join(format!("{name}-response-meta.json")), &[("contentType", J::Str(full_type))])?;
    let encoding = py::lower(response.header("Content-Encoding").unwrap_or(""));
    let raw = response.read(max_bytes + 1)?;
    drop(response);
    if raw.len() > max_bytes {
        return Err(NetError::value("response wire limit exceeded"));
    }
    let decoded = if encoding == "gzip" {
        gzip::read_bounded(&raw, max_bytes + 1)?
    } else if encoding.is_empty() || encoding == "identity" {
        raw.clone()
    } else {
        return Err(NetError::value("unsupported response encoding"));
    };
    if decoded.len() > max_bytes {
        return Err(NetError::value("response decoded limit exceeded"));
    }
    let decoded_len = decoded.len();
    *payload = Some(decoded);
    private_write(&directory.join(format!("{name}-response.body")), payload.as_deref().unwrap_or_default())?;
    receipt.set("wireBytes", J::Int(raw.len() as i64));
    receipt.set("decodedBytes", J::Int(decoded_len as i64));
    receipt.set("result", J::Str("response_saved".into()));
    Ok(())
}

pub(crate) fn run(directory: &Path, name: &str, request: &HttpRequest, max_bytes: usize, quiet: bool,
                  client: impl FnOnce() -> Result<Client, NetError>, out: Option<&mut dyn Write>)
    -> Result<(Option<u16>, Option<Vec<u8>>), SendError> {
    if !(1..=32 * 1024 * 1024).contains(&max_bytes) {
        return Err(SendError::SizeBound);
    }
    if let Some(error) = request_url_error(&request.url) {
        return Err(SendError::InvalidRequest(error));
    }
    private_write(&directory.join(format!(".{name}-claim")), b"claimed-before-transport\n").map_err(SendError::Claim)?;
    let started = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut receipt = Receipt { fields: vec![
        ("schemaVersion", J::Int(1)),
        ("startedUnixSeconds", J::Int(started)),
        ("method", J::Str(request.method.clone())),
        ("requestCount", J::Int(1)),
        ("automaticRetry", J::Bool(false)),
        ("redirectsFollowed", J::Bool(false)),
        ("tlsVerified", J::Bool(true)),
        ("cookiesAttached", J::Bool(false)),
        ("result", J::Str("transport_outcome_unknown".into())),
    ] };
    let client = client().map_err(SendError::Setup)?;
    let mut payload = None;
    match exchange(&client, directory, name, request, max_bytes, &mut receipt, &mut payload) {
        Ok(()) => {}
        Err(error) if error.caught_by_send_once() => {
            receipt.set("result", J::Str("transport_or_response_error".into()));
        }
        Err(error) => return Err(SendError::Escaped(error)),
    }
    save_json(&directory.join(format!("{name}-receipt.json")), &receipt.fields).map_err(SendError::Receipt)?;
    let status = receipt.status();
    if !quiet || status != Some(200) {
        let line = format!("{}\n", py::dumps_compact(&receipt.fields));
        let written = match out {
            Some(out) => out.write_all(line.as_bytes()).and_then(|()| out.flush()),
            None => {
                let mut stdout = io::stdout().lock();
                stdout.write_all(line.as_bytes()).and_then(|()| stdout.flush())
            }
        };
        written.map_err(SendError::Receipt)?;
    }
    Ok((status, payload))
}

impl Client {
    /// [`send_once`] on this client (tests inject a client with a local CA
    /// or a short timeout).
    pub fn send_once(&self, directory: &Path, name: &str, request: &HttpRequest, max_bytes: usize, quiet: bool)
        -> Result<(Option<u16>, Option<Vec<u8>>), SendError> {
        run(directory, name, request, max_bytes, quiet, || Ok(self.clone()), None)
    }
}
