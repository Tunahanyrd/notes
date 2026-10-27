//! Pure-Rust blocking HTTP/1.1 + TLS client for the Python-free engine.
//!
//! It reproduces what the Python engine puts on the wire and on disk today:
//! `urllib.request` + `http.client` of CPython 3.14 with a `NoRedirect`
//! handler and `ssl.create_default_context()` (system trust store, TLS >= 1.2,
//! hostname verification, SNI), one request per connection, no redirects,
//! cookies or proxies, and socket timeouts per operation.
//!
//! * [`send_once`]: `tools/notes_cloud.send_once` (claim file, receipt,
//!   meta, body; byte-identical files).
//! * [`get`] / [`Client::get`]: `samsung_notes.previews._http`.
//! * [`download`] / [`Client::download`]: `fetch_pdf`'s default download.
//! * [`Client::open`]: the bare exchange ([`Response::read`] mirrors
//!   `HTTPResponse.read(amt)`).
//!
//! No Python and no PyO3: openssl (system OpenSSL) for TLS, system zlib for
//! gzip.
//!
//! Wire decisions: header order/casing exactly as urllib+http.client
//! (`Accept-Encoding: identity` unless given, `Content-Length` for
//! POST/PUT/PATCH, `Host`, `Connection: close`, `.title()`d names), and the
//! default `User-Agent` is [`DEFAULT_USER_AGENT`] (`Python-urllib/3.14`),
//! byte-identical to today's traffic for requests that set none. The TLS
//! ClientHello equals CPython's (same OpenSSL, options, ciphers, verify flags;
//! no ALPN) — checked against python3 in the tests.
//!
//! Deliberate differences from the Python code path:
//! * proxy environment variables are ignored (urllib's ProxyHandler would
//!   honour `https_proxy`); `SSLKEYLOGFILE` is ignored;
//! * non-ASCII host names fail with a ValueError-family error instead of
//!   being IDNA-encoded; only `http`/`https` URLs are handled (others are a
//!   URLError);
//! * [`get`]/[`download`] connect only to the addresses that passed the
//!   `is_global` check (Python resolves a second time when connecting);
//! * timeouts: each recv/TLS read and each connect attempt gets the full
//!   timeout, a `sendall` and the TLS handshake are bounded as a whole.
mod client;
mod error;
mod fetch;
mod gzip;
mod ip;
mod message;
mod py;
mod request;
mod send_once;
mod tls;
mod url;

pub use client::{Client, Response};
#[cfg(feature = "fuzzing")]
pub use client::fuzz_exchange;
pub use error::{Family, NetError};
pub use fetch::{create_private, download, get, DownloadError, PDF_TIMEOUT, PREVIEW_TIMEOUT};
pub use ip::is_global;
pub use message::Message;
pub use py::capitalize;
pub use request::{HttpRequest, DEFAULT_USER_AGENT};
pub use send_once::{private_write, request_url_error, send_once, SendError, MAX_BYTES, SEND_ONCE_TIMEOUT};
pub use tls::{python_options, python_verify_flags, PYTHON_CIPHERS};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod hostile_tests;
