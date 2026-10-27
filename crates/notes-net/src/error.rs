//! Errors, classified the way the Python callers' `except` clauses see them.
use std::fmt;
use std::io;

/// Which Python exception family an error belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// `OSError` and subclasses: `urllib.error.URLError`, `TimeoutError`,
    /// `ConnectionError`s (incl. `http.client.RemoteDisconnected`),
    /// `socket.gaierror`, `ssl.SSLError`, `gzip.BadGzipFile`, file I/O.
    Os,
    /// `ValueError` (UnicodeEncodeError of the request line or a header,
    /// invalid header name/value, `urlsplit` errors, ...).
    Value,
    /// `EOFError` (truncated gzip stream).
    Eof,
    /// Anything outside OSError/ValueError/EOFError, which the Python
    /// callers do not catch: `http.client.HTTPException` subclasses other
    /// than RemoteDisconnected (BadStatusLine, LineTooLong, IncompleteRead,
    /// UnknownProtocol, InvalidURL, "got more than 100 headers") and
    /// `zlib.error`.
    Escape,
}

/// A transport failure. `class` names the CPython exception class the
/// urllib code path would have raised (informational; branch on [`Family`]).
#[derive(Debug)]
pub enum NetError {
    Raised { family: Family, class: &'static str, message: String },
    /// `get`/`download` only: a getaddrinfo answer was not `is_global`
    /// (or there was no answer) — previews: `PreviewError()`, fetch_pdf:
    /// `PdfBackgroundError('PDF sunucusu doğrulanamadı.')`.
    NotGlobal,
    /// `get`/`download` only: the body is larger than the bound — previews:
    /// `PreviewError('Bu sayfa önizleme boyutu sınırını aşıyor.')`,
    /// fetch_pdf: `PdfBackgroundError('PDF önizleme boyutu sınırını aşıyor.')`.
    TooLarge,
}

impl NetError {
    pub(crate) fn os(class: &'static str, message: impl Into<String>) -> NetError {
        NetError::Raised { family: Family::Os, class, message: message.into() }
    }
    pub(crate) fn value(message: impl Into<String>) -> NetError {
        NetError::Raised { family: Family::Value, class: "ValueError", message: message.into() }
    }
    pub(crate) fn value_class(class: &'static str, message: impl Into<String>) -> NetError {
        NetError::Raised { family: Family::Value, class, message: message.into() }
    }
    pub(crate) fn eof(message: impl Into<String>) -> NetError {
        NetError::Raised { family: Family::Eof, class: "EOFError", message: message.into() }
    }
    pub(crate) fn escape(class: &'static str, message: impl Into<String>) -> NetError {
        NetError::Raised { family: Family::Escape, class, message: message.into() }
    }
    pub(crate) fn timeout() -> NetError {
        NetError::os("TimeoutError", "timed out")
    }

    /// The exception family, `None` for [`NetError::NotGlobal`]/[`NetError::TooLarge`].
    pub fn family(&self) -> Option<Family> {
        match self {
            NetError::Raised { family, .. } => Some(*family),
            _ => None,
        }
    }
    /// CPython exception class name.
    pub fn python_class(&self) -> &'static str {
        match self {
            NetError::Raised { class, .. } => class,
            NetError::NotGlobal => "NotGlobal",
            NetError::TooLarge => "TooLarge",
        }
    }
    pub fn is_os(&self) -> bool {
        self.family() == Some(Family::Os)
    }
    /// Caught by `except (OSError, ValueError, EOFError)` (tools/notes_cloud.send_once).
    pub fn caught_by_send_once(&self) -> bool {
        matches!(self.family(), Some(Family::Os | Family::Value | Family::Eof))
    }

    /// An `io::Error` from the socket layer as Python would classify it.
    pub(crate) fn from_io(error: &io::Error) -> NetError {
        use io::ErrorKind::*;
        let class = match error.kind() {
            WouldBlock | TimedOut => return NetError::timeout(),
            ConnectionRefused => "ConnectionRefusedError",
            ConnectionReset => "ConnectionResetError",
            ConnectionAborted => "ConnectionAbortedError",
            BrokenPipe => "BrokenPipeError",
            NotFound => "FileNotFoundError",
            AlreadyExists => "FileExistsError",
            PermissionDenied => "PermissionError",
            _ => "OSError",
        };
        NetError::os(class, error.to_string())
    }
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Raised { class, message, .. } => write!(f, "{class}: {message}"),
            NetError::NotGlobal => f.write_str("address is not global"),
            NetError::TooLarge => f.write_str("response size bound exceeded"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<io::Error> for NetError {
    fn from(error: io::Error) -> NetError {
        NetError::from_io(&error)
    }
}
