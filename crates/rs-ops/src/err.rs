//! Error model mirroring the Python exception classes these modules raise and
//! catch. `ext` carries an opaque foreign exception (e.g. a Python error from a
//! not-yet-ported dependency) so it can be re-raised unchanged.
use std::any::Any;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ink,
    Doc,
    PageOps,
    /// `page_shapes.ShapeError` (DocumentError subclass).
    Shape,
    Payload,
    Parse,
    Value,
    Type,
    Overflow,
    Key,
    Unicode,
    Stop,
    Attribute,
    ZeroDiv,
    Other,
}

pub struct Error {
    pub kind: Kind,
    pub msg: String,
    pub ext: Option<Box<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}({})", self.kind, self.msg)
    }
}

impl Error {
    pub fn new(kind: Kind, msg: impl Into<String>) -> Self {
        Error { kind, msg: msg.into(), ext: None }
    }

    /// `isinstance` against the Python class hierarchy:
    /// InkError/DocumentError/PayloadError/UnicodeError < ValueError,
    /// PageOpsError < DocumentError.
    pub fn is(&self, kind: Kind) -> bool {
        match kind {
            Kind::Value => matches!(
                self.kind,
                Kind::Value | Kind::Ink | Kind::Doc | Kind::PageOps | Kind::Shape | Kind::Payload | Kind::Unicode
            ),
            Kind::Doc => matches!(self.kind, Kind::Doc | Kind::PageOps | Kind::Shape),
            k => self.kind == k,
        }
    }

    pub fn any(&self, kinds: &[Kind]) -> bool {
        kinds.iter().any(|k| self.is(*k))
    }
}

/// Map the pure document parser error exactly as the native host adapter.
pub fn from_doc(e: _doc::Error) -> Error {
    use _doc::Error as D;
    match e {
        D::Payload(m) => Error::new(Kind::Payload, m),
        D::Document(m) => Error::new(Kind::Doc, m.to_string()),
        D::Value(m) => Error::new(Kind::Value, m.to_string()),
        D::Type(m) => Error::new(Kind::Type, m.to_string()),
        D::XmlParse(m) => Error::new(Kind::Parse, m),
        D::Io(e) => Error::new(Kind::Other, e.to_string()),
        D::External(_) => Error::new(Kind::Other, "external error"),
    }
}

pub type R<T> = Result<T, Error>;

pub fn ink<T>(msg: &str) -> R<T> {
    Err(Error::new(Kind::Ink, msg))
}
pub fn doc<T>(msg: &str) -> R<T> {
    Err(Error::new(Kind::Doc, msg))
}
pub fn pageops<T>(msg: &str) -> R<T> {
    Err(Error::new(Kind::PageOps, msg))
}
pub fn value<T>(msg: &str) -> R<T> {
    Err(Error::new(Kind::Value, msg))
}
pub fn type_err<T>(msg: &str) -> R<T> {
    Err(Error::new(Kind::Type, msg))
}
pub fn e_ink(msg: &str) -> Error {
    Error::new(Kind::Ink, msg)
}
pub fn e_doc(msg: &str) -> Error {
    Error::new(Kind::Doc, msg)
}
pub fn e_pageops(msg: &str) -> Error {
    Error::new(Kind::PageOps, msg)
}
pub fn e_parse(msg: &str) -> Error {
    Error::new(Kind::Parse, msg)
}

/// Re-map one error kind to another with a fixed message, like
/// `except X: raise Y(msg) from None`.
pub trait Remap<T> {
    fn remap(self, from: &[Kind], to: Kind, msg: &str) -> R<T>;
}

impl<T> Remap<T> for R<T> {
    fn remap(self, from: &[Kind], to: Kind, msg: &str) -> R<T> {
        self.map_err(|e| if e.any(from) { Error::new(to, msg) } else { e })
    }
}
