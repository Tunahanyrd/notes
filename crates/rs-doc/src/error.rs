//! Pure-Rust error type shared by the doc crate (no PyO3 types).
use std::borrow::Cow;
use std::fmt;

/// Errors raised by the pure-Rust core. The PyO3 layer maps each variant onto
/// the Python exception class the original module raised:
/// `Payload` -> `tools.notes_payload.PayloadError`,
/// `Document` -> `samsung_notes.document.DocumentError`, `Value` -> `ValueError`,
/// `XmlParse` -> `xml.etree.ElementTree.ParseError`, `Io` -> `OSError` family.
#[derive(Debug)]
pub enum Error {
    Payload(&'static str),
    Document(Cow<'static, str>),
    Value(Cow<'static, str>),
    Type(Cow<'static, str>),
    XmlParse(String),
    Io(std::io::Error),
    /// Opaque error produced by a caller-supplied hook (e.g. a Python exception).
    External(Box<dyn std::any::Any + Send>),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn document(msg: &'static str) -> Self {
        Error::Document(Cow::Borrowed(msg))
    }
    pub fn value(msg: &'static str) -> Self {
        Error::Value(Cow::Borrowed(msg))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Payload(m) => f.write_str(m),
            Error::Document(m) | Error::Value(m) | Error::Type(m) => f.write_str(m),
            Error::XmlParse(m) => f.write_str(m),
            Error::Io(e) => write!(f, "{e}"),
            Error::External(_) => f.write_str("external error"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
