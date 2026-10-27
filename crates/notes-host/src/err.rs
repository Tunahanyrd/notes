//! Error classification shared by the pure engine crates.
//!
//! The Python engine decides behaviour with `except` clauses over exception
//! classes (`StoreError`, `APIError`, `ConflictError`, `DocumentError`,
//! `ValueError`, `OSError`, ...). [`Fail`] carries the class ([`Kind`]) plus
//! the attributes those clauses read (`code`, `message`, `status`,
//! `ambiguous`) so the same decisions can be made in Rust.
use std::fmt;

/// Python exception class of a failure. Subclass relations are answered by
/// the `is_*` predicates on [`Fail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `samsung_notes.storage.StoreError(code, message, status)` (ValueError subclass).
    Store,
    /// `cloud_api.APIError(code, message, ambiguous=)`.
    Api,
    /// `cloud_api.ConflictError` (APIError subclass, code `conflict`).
    Conflict,
    /// `cloud_api.UnsupportedDocumentError` (APIError subclass).
    Unsupported,
    /// `document.DocumentError` (ValueError subclass).
    Document,
    /// `page_ops.PageOpsError` (DocumentError subclass).
    PageOps,
    /// `tools.notes_payload.PayloadError` (ValueError subclass).
    Payload,
    /// `ink.InkError` (ValueError subclass).
    Ink,
    /// `pdf_background.PdfBackgroundError` (ValueError subclass, `.message`).
    PdfBackground,
    /// `previews.PreviewError` (plain Exception, `.message`).
    Preview,
    /// `ink_render.InkRenderError` (ValueError subclass).
    InkRender,
    /// `xml.etree.ElementTree.ParseError` (SyntaxError subclass).
    XmlParse,
    Value,
    /// `UnicodeError` family (ValueError subclass).
    Unicode,
    Type,
    Key,
    Index,
    Overflow,
    Attribute,
    /// `OSError` family (incl. `subprocess.TimeoutExpired`-free OS failures).
    Os,
    /// `TimeoutError` / `BrokenPipeError` / `ConnectionResetError` (OSError subclasses).
    Disconnect,
    /// `sqlite3.*Error`.
    Sqlite,
    /// Anything else (`Exception`).
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fail {
    pub kind: Kind,
    /// `.code` for Store/Api errors, empty otherwise.
    pub code: String,
    /// `.message` (Store/Api/PdfBackground/Preview) or `str(exception)`.
    pub message: String,
    /// `StoreError.status`.
    pub status: u16,
    /// `APIError.ambiguous`.
    pub ambiguous: bool,
}

impl Fail {
    pub fn new(kind: Kind, message: impl Into<String>) -> Fail {
        Fail { kind, code: String::new(), message: message.into(), status: 0, ambiguous: false }
    }
    /// `StoreError(code, message, status)`
    pub fn store(code: &str, message: &str, status: u16) -> Fail {
        Fail { kind: Kind::Store, code: code.into(), message: message.into(), status, ambiguous: false }
    }
    /// `StoreError(code, message)` with the default status 400.
    pub fn store400(code: &str, message: &str) -> Fail {
        Fail::store(code, message, 400)
    }
    /// `APIError(code, message)`
    pub fn api(code: &str, message: &str) -> Fail {
        Fail { kind: Kind::Api, code: code.into(), message: message.into(), status: 0, ambiguous: false }
    }
    /// `APIError(code, message, ambiguous=True)`
    pub fn ambiguous(code: &str, message: &str) -> Fail {
        Fail { kind: Kind::Api, code: code.into(), message: message.into(), status: 0, ambiguous: true }
    }
    /// `ConflictError(message)`
    pub fn conflict(message: &str) -> Fail {
        Fail { kind: Kind::Conflict, code: "conflict".into(), message: message.into(), status: 0, ambiguous: false }
    }
    pub fn document(message: impl Into<String>) -> Fail {
        Fail::new(Kind::Document, message)
    }
    pub fn value(message: impl Into<String>) -> Fail {
        Fail::new(Kind::Value, message)
    }
    pub fn type_(message: impl Into<String>) -> Fail {
        Fail::new(Kind::Type, message)
    }
    pub fn key(message: impl Into<String>) -> Fail {
        Fail::new(Kind::Key, message)
    }
    pub fn os(message: impl Into<String>) -> Fail {
        Fail::new(Kind::Os, message)
    }

    /// `isinstance(e, StoreError)`
    pub fn is_store(&self) -> bool {
        self.kind == Kind::Store
    }
    /// `isinstance(e, APIError)`
    pub fn is_api(&self) -> bool {
        matches!(self.kind, Kind::Api | Kind::Conflict | Kind::Unsupported)
    }
    /// `isinstance(e, ConflictError)`
    pub fn is_conflict(&self) -> bool {
        self.kind == Kind::Conflict
    }
    /// `isinstance(e, DocumentError)`
    pub fn is_document(&self) -> bool {
        matches!(self.kind, Kind::Document | Kind::PageOps)
    }
    /// `isinstance(e, ValueError)`
    pub fn is_value(&self) -> bool {
        matches!(
            self.kind,
            Kind::Store | Kind::Document | Kind::PageOps | Kind::Payload | Kind::Ink | Kind::PdfBackground | Kind::InkRender | Kind::Value | Kind::Unicode
        )
    }
    /// `isinstance(e, OSError)`
    pub fn is_os(&self) -> bool {
        matches!(self.kind, Kind::Os | Kind::Disconnect)
    }
    pub fn is_type(&self) -> bool {
        self.kind == Kind::Type
    }
    pub fn is_key(&self) -> bool {
        self.kind == Kind::Key
    }
    /// `except (OSError, ValueError, KeyError, TypeError)`
    pub fn is_os_value_key_type(&self) -> bool {
        self.is_os() || self.is_value() || self.is_key() || self.is_type()
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Fail {}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Fail {
        use std::io::ErrorKind as K;
        let kind = match e.kind() {
            K::TimedOut | K::BrokenPipe | K::ConnectionReset => Kind::Disconnect,
            _ => Kind::Os,
        };
        Fail::new(kind, e.to_string())
    }
}

impl From<rs_store::db::Error> for Fail {
    fn from(e: rs_store::db::Error) -> Fail {
        use rs_store::db::Error as E;
        match e {
            E::Store { code, message, status } => Fail { kind: Kind::Store, code, message, status, ambiguous: false },
            E::Sqlite(_, m) => Fail::new(Kind::Sqlite, m),
            E::Value(m) => Fail::new(Kind::Value, m),
            E::Type(m) => Fail::new(Kind::Type, m),
            E::Key(m) => Fail::new(Kind::Key, m),
            E::Overflow(m) => Fail::new(Kind::Overflow, m),
            E::Attribute(m) => Fail::new(Kind::Attribute, m),
            E::Decode(m) => Fail::new(Kind::Value, m),
            E::Io(e) => Fail::from(e),
            E::External(inner) => match inner.downcast::<Fail>() {
                Ok(f) => *f,
                Err(other) => Fail::new(Kind::Other, other.to_string()),
            },
        }
    }
}

impl From<Fail> for rs_store::db::Error {
    /// Wrap a collaborator failure so the store core propagates it unchanged;
    /// `Fail::from(db::Error)` unwraps it again.
    fn from(f: Fail) -> rs_store::db::Error {
        match f.kind {
            Kind::Store => rs_store::db::Error::Store { code: f.code, message: f.message, status: f.status },
            _ => rs_store::db::Error::External(Box::new(f)),
        }
    }
}

impl From<rs_doc::Error> for Fail {
    fn from(e: rs_doc::Error) -> Fail {
        use rs_doc::Error as E;
        match e {
            E::Payload(m) => Fail::new(Kind::Payload, m),
            E::Document(m) => Fail::new(Kind::Document, m.into_owned()),
            E::Value(m) => Fail::new(Kind::Value, m.into_owned()),
            E::Type(m) => Fail::new(Kind::Type, m.into_owned()),
            E::XmlParse(m) => Fail::new(Kind::XmlParse, m),
            E::Io(e) => Fail::from(e),
            E::External(inner) => match inner.downcast::<Fail>() {
                Ok(f) => *f,
                Err(_) => Fail::new(Kind::Other, "external error"),
            },
        }
    }
}

pub type R<T> = Result<T, Fail>;
