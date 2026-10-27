//! One error type mirroring the Python exception classes the ported tools
//! raise. Messages never contain URIs, tokens, codes or response bodies.
use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    /// `ValueError(message)` (includes `json.JSONDecodeError`).
    Value(String),
    /// `KeyError(key)`
    Key(String),
    /// `TypeError` / `AttributeError` from an unexpected JSON shape.
    Type(String),
    /// `OSError` (file system, network transport).
    Io(io::Error),
    /// `capture_oauth_callback.CallbackError(message)`
    Callback(String),
    /// `subprocess.SubprocessError` / non-zero exit of a helper command.
    Subprocess(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn value(message: impl Into<String>) -> Error {
        Error::Value(message.into())
    }
    pub fn callback(message: impl Into<String>) -> Error {
        Error::Callback(message.into())
    }
    pub fn key(key: &str) -> Error {
        Error::Key(key.to_string())
    }
    pub fn type_(message: impl Into<String>) -> Error {
        Error::Type(message.into())
    }
    /// `str(error)` for a CallbackError (the rejected-callback diagnostic).
    pub fn callback_message(&self) -> Option<&str> {
        match self {
            Error::Callback(message) => Some(message),
            _ => None,
        }
    }
    pub fn is_file_exists(&self) -> bool {
        matches!(self, Error::Io(e) if e.kind() == io::ErrorKind::AlreadyExists)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Value(m) | Error::Type(m) | Error::Callback(m) | Error::Subprocess(m) => f.write_str(m),
            Error::Key(k) => write!(f, "missing key {k:?}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<crate::json::DecodeError> for Error {
    fn from(e: crate::json::DecodeError) -> Error {
        Error::Value(format!("{} (char {})", e.msg, e.pos))
    }
}
