//! The preview transports: `samsung_notes.previews._http` and the default
//! download of `samsung_notes.pdf_background.fetch_pdf`.
use crate::client::Client;
use crate::error::NetError;
use crate::ip::is_global;
use crate::request::HttpRequest;
use crate::url;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

/// `previews._http` timeout.
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(20);
/// `fetch_pdf` download timeout.
pub const PDF_TIMEOUT: Duration = Duration::from_secs(25);

/// `socket.getaddrinfo(urlsplit(url).hostname, 443, type=SOCK_STREAM)` and
/// the `is_global` check of every answer. The answers are returned so the
/// connection is made to exactly the vetted addresses (Python re-resolves in
/// `create_connection`; using the vetted list closes that DNS-rebinding gap
/// and is otherwise identical).
fn vetted_addresses(url: &str) -> Result<(String, Vec<SocketAddr>), NetError> {
    let host = url::hostname(url).map_err(NetError::value)?;
    // getaddrinfo(None, 443) answers loopback addresses: never global.
    let host = host.ok_or(NetError::NotGlobal)?;
    if !host.is_ascii() {
        return Err(NetError::value_class("UnicodeError", "non-ASCII host names are not supported"));
    }
    let addresses: Vec<SocketAddr> =
        (host.as_str(), 443).to_socket_addrs().map_err(|e| NetError::os("gaierror", e.to_string()))?.collect();
    let mut unique: Vec<SocketAddr> = Vec::new();
    for a in addresses {
        if !unique.iter().any(|u| u.ip() == a.ip()) {
            unique.push(a);
        }
    }
    if unique.is_empty() || unique.iter().any(|a| !is_global(a.ip())) {
        return Err(NetError::NotGlobal);
    }
    Ok((host, unique))
}

fn open_get(client: &Client, url: &str, headers: &[(String, String)], vetted: Option<&(String, Vec<SocketAddr>)>)
    -> Result<crate::Response, NetError> {
    let request = HttpRequest::new(Some("GET"), url, headers, None);
    let parsed = url::request_url(url)?;
    // Use the vetted answers only when the connection host is the host that
    // was checked; anything else (e.g. userinfo in the netloc) resolves the
    // way Python's create_connection would.
    let same_host = |checked: &str| {
        let host = parsed.host.as_deref().unwrap_or("");
        let default = if parsed.scheme == "http" { 80 } else { 443 };
        url::host_port(host, default).is_ok_and(|(h, _)| crate::py::lower(&h) == checked)
    };
    let addresses = vetted.filter(|(checked, _)| same_host(checked)).map(|(_, a)| a.as_slice());
    client.open_parsed(&request, &parsed, addresses)
}

impl Client {
    /// `previews._http(url, headers)` -> `(status, body, Location)`.
    ///
    /// Header names are raw (capitalised here like `Request(headers=...)`).
    /// The body is read only for 200 and bounded by `max_bytes`
    /// ([`NetError::TooLarge`] beyond it); other statuses return an empty
    /// body without reading it. Error mapping for the caller:
    /// `NotGlobal` -> `PreviewError()`, `TooLarge` -> `PreviewError('Bu sayfa
    /// önizleme boyutu sınırını aşıyor.')`, OS/Value family ->
    /// `PreviewError(DOWNLOAD_FAILED)`, Eof/Escape -> raised as is.
    pub fn get(&self, url: &str, headers: &[(String, String)], max_bytes: usize)
        -> Result<(u16, Vec<u8>, Option<String>), NetError> {
        self.get_checked(url, headers, max_bytes, true)
    }

    pub(crate) fn get_checked(&self, url: &str, headers: &[(String, String)], max_bytes: usize, check_global: bool)
        -> Result<(u16, Vec<u8>, Option<String>), NetError> {
        let vetted = if check_global { Some(vetted_addresses(url)?) } else { None };
        let mut response = open_get(self, url, headers, vetted.as_ref())?;
        let status = response.status();
        let location = response.header("Location").map(str::to_string);
        if status != 200 {
            return Ok((status, Vec::new(), location));
        }
        let raw = response.read(max_bytes + 1)?;
        if raw.len() > max_bytes {
            return Err(NetError::TooLarge);
        }
        Ok((status, raw, location))
    }

    /// `fetch_pdf`'s default `download(url, headers)` -> `(status, Location)`.
    ///
    /// For 200: a numeric `Content-Length` above `max_bytes` (or a
    /// non-decimal one) is [`NetError::TooLarge`] before anything is
    /// created; then `open_sink()` creates the private partial file,
    /// `check()` runs before every `read(65536)`, the running total is
    /// bounded, and the file is flushed and fsynced. Other statuses return
    /// `(status, Location)` without opening the sink. The body is written as
    /// received (Content-Encoding is not decoded, like the Python code).
    pub fn download<E>(&self, url: &str, headers: &[(String, String)], max_bytes: u64,
                       open_sink: impl FnOnce() -> io::Result<File>,
                       check: &mut dyn FnMut() -> Result<(), E>) -> Result<(u16, Option<String>), DownloadError<E>> {
        self.download_checked(url, headers, max_bytes, open_sink, check, true)
    }

    pub(crate) fn download_checked<E>(&self, url: &str, headers: &[(String, String)], max_bytes: u64,
                                      open_sink: impl FnOnce() -> io::Result<File>,
                                      check: &mut dyn FnMut() -> Result<(), E>, check_global: bool)
        -> Result<(u16, Option<String>), DownloadError<E>> {
        let vetted = if check_global { Some(vetted_addresses(url)?) } else { None };
        let mut response = open_get(self, url, headers, vetted.as_ref())?;
        if response.status() != 200 {
            return Ok((response.status(), response.header("Location").map(str::to_string)));
        }
        if let Some(length) = response.header("Content-Length") {
            if !length.is_empty() {
                let decimal = length.chars().all(|c| c.is_ascii_digit());
                let too_big = !decimal || length.trim_start_matches('0').len() > 20
                    || length.parse::<u128>().map_or(true, |n| n > max_bytes as u128);
                if too_big {
                    return Err(NetError::TooLarge.into());
                }
            }
        }
        let mut file = open_sink().map_err(NetError::from)?;
        let mut total: u64 = 0;
        loop {
            check().map_err(DownloadError::Check)?;
            let chunk = response.read(65536)?;
            if chunk.is_empty() {
                break;
            }
            total += chunk.len() as u64;
            if total > max_bytes {
                return Err(NetError::TooLarge.into());
            }
            file.write_all(&chunk).map_err(NetError::from)?;
        }
        file.flush().map_err(NetError::from)?;
        file.sync_all().map_err(NetError::from)?;
        Ok((200, None))
    }

    /// [`Client::download`] into `dest` created like `fetch_pdf`'s temporary
    /// (`O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW`, 0600) — only once a 200
    /// response passed the Content-Length check.
    pub fn download_to_path<E>(&self, url: &str, headers: &[(String, String)], dest: &Path, max_bytes: u64,
                               check: &mut dyn FnMut() -> Result<(), E>) -> Result<(u16, Option<String>), DownloadError<E>> {
        self.download(url, headers, max_bytes, || create_private(dest), check)
    }
}

/// `os.open(path, O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW, 0o600)`
pub fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)
}

/// A download failure: the transport's, or the caller's `check()`.
#[derive(Debug)]
pub enum DownloadError<E> {
    Net(NetError),
    Check(E),
}

impl<E> From<NetError> for DownloadError<E> {
    fn from(e: NetError) -> Self {
        DownloadError::Net(e)
    }
}

/// `previews._http(url, headers)` with a fresh production client.
pub fn get(url: &str, headers: &[(String, String)], max_bytes: usize, timeout: Duration)
    -> Result<(u16, Vec<u8>, Option<String>), NetError> {
    Client::new(timeout)?.get(url, headers, max_bytes)
}

/// `fetch_pdf`'s default download with a fresh production client.
pub fn download<E>(url: &str, headers: &[(String, String)], dest: &Path, max_bytes: u64, timeout: Duration,
                   check: &mut dyn FnMut() -> Result<(), E>) -> Result<(u16, Option<String>), DownloadError<E>> {
    let client = Client::new(timeout).map_err(DownloadError::Net)?;
    client.download_to_path(url, headers, dest, max_bytes, check)
}
