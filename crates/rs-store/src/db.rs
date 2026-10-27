//! SQLite access with CPython `sqlite3` (legacy transaction control) semantics:
//! DML statements implicitly open a deferred transaction, explicit
//! `BEGIN IMMEDIATE` works as written, and the owner commits or rolls back.
use crate::val::{JsonError, Val};
use parking_lot::ReentrantMutex;
use rusqlite::types::{Value as SqlVal, ValueRef};
use rusqlite::Connection;
use std::any::Any;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SqlErrKind {
    Operational,
    Integrity,
    Programming,
    Database,
    Data,
}

/// Core error. Mirrors the Python exceptions the original raised.
#[derive(Debug)]
pub enum Error {
    /// samsung_notes.storage.StoreError(code, message, status)
    Store { code: String, message: String, status: u16 },
    /// sqlite3.<kind>Error
    Sqlite(SqlErrKind, String),
    Value(String),
    Type(String),
    Key(String),
    Overflow(String),
    Attribute(String),
    /// json.JSONDecodeError
    Decode(String),
    /// OSError family
    Io(std::io::Error),
    /// An exception raised by a host/collaborator (e.g. a Python exception),
    /// propagated unchanged.
    External(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Store { code, message, .. } => write!(f, "{}: {}", code, message),
            Error::Sqlite(_, m) | Error::Value(m) | Error::Type(m) | Error::Key(m) | Error::Overflow(m) | Error::Attribute(m) | Error::Decode(m) => {
                f.write_str(m)
            }
            Error::External(e) => write!(f, "{}", e),
            Error::Io(e) => write!(f, "{}", e),
        }
    }
}
impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub fn store_err(code: &str, message: &str, status: u16) -> Error {
    Error::Store { code: code.into(), message: message.into(), status }
}
pub fn fail<T>(code: &str, message: &str, status: u16) -> Result<T> {
    Err(store_err(code, message, status))
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Runs blocking SQLite work. The Python layer installs a hook that releases
/// the GIL (CPython's sqlite3 does the same) so a busy wait never blocks the
/// thread that holds the competing transaction.
pub static BLOCKING: std::sync::OnceLock<fn(&mut dyn FnMut())> = std::sync::OnceLock::new();

pub fn blocking<R>(f: impl FnOnce() -> R) -> R {
    match BLOCKING.get() {
        Some(hook) => {
            let mut f = Some(f);
            let mut out = None;
            hook(&mut || out = Some((f.take().unwrap())()));
            out.unwrap()
        }
        None => f(),
    }
}

/// Positional SQL parameter conversion for values the core builds itself.
pub trait ToP {
    fn p(self) -> SqlVal;
}
impl ToP for &str {
    fn p(self) -> SqlVal {
        SqlVal::Text(self.to_string())
    }
}
impl ToP for &String {
    fn p(self) -> SqlVal {
        SqlVal::Text(self.clone())
    }
}
impl ToP for String {
    fn p(self) -> SqlVal {
        SqlVal::Text(self)
    }
}
impl ToP for i64 {
    fn p(self) -> SqlVal {
        SqlVal::Integer(self)
    }
}
impl ToP for bool {
    fn p(self) -> SqlVal {
        SqlVal::Integer(self as i64)
    }
}
impl ToP for Option<&str> {
    fn p(self) -> SqlVal {
        opt_text(self)
    }
}
impl ToP for SqlVal {
    fn p(self) -> SqlVal {
        self
    }
}
impl ToP for &SqlVal {
    fn p(self) -> SqlVal {
        self.clone()
    }
}

#[macro_export]
macro_rules! p {
    ($($e:expr),* $(,)?) => { vec![$($crate::db::ToP::p($e)),*] };
}

impl From<JsonError> for Error {
    fn from(e: JsonError) -> Self {
        match e {
            JsonError::Value(m) => Error::Value(m),
            JsonError::Type(m) => Error::Type(m),
            JsonError::Decode(m) => Error::Decode(m),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        use rusqlite::ffi::ErrorCode as C;
        match &e {
            rusqlite::Error::SqliteFailure(code, msg) => {
                let text = msg.clone().unwrap_or_else(|| code.to_string());
                let kind = match code.code {
                    C::ConstraintViolation => SqlErrKind::Integrity,
                    C::TooBig => SqlErrKind::Data,
                    C::ApiMisuse | C::ParameterOutOfRange => SqlErrKind::Programming,
                    C::DatabaseCorrupt | C::NotADatabase | C::InternalMalfunction => SqlErrKind::Database,
                    _ => SqlErrKind::Operational,
                };
                Error::Sqlite(kind, text)
            }
            rusqlite::Error::MultipleStatement => {
                Error::Sqlite(SqlErrKind::Programming, "You can only execute one statement at a time.".into())
            }
            rusqlite::Error::InvalidParameterCount(given, expected) => Error::Sqlite(
                SqlErrKind::Programming,
                format!(
                    "Incorrect number of bindings supplied. The current statement uses {}, and there are {} supplied.",
                    expected, given
                ),
            ),
            rusqlite::Error::Utf8Error(..) | rusqlite::Error::FromSqlConversionFailure(..) => {
                Error::Sqlite(SqlErrKind::Operational, format!("Could not decode to UTF-8 column: {}", e))
            }
            _ => Error::Sqlite(SqlErrKind::Operational, e.to_string()),
        }
    }
}

/// Fully fetched statement result.
#[derive(Debug, Default, Clone)]
pub struct Cursor {
    pub cols: Arc<Vec<String>>,
    pub rows: Vec<Vec<SqlVal>>,
    pub rowcount: i64,
    pub lastrowid: Option<i64>,
}

impl Cursor {
    pub fn one(&self) -> Option<Row<'_>> {
        self.rows.first().map(|vals| Row { cols: &self.cols, vals })
    }
    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(move |vals| Row { cols: &self.cols, vals })
    }
    /// fetchone()[0] for scalar queries.
    pub fn scalar(&self) -> Option<&SqlVal> {
        self.rows.first().and_then(|r| r.first())
    }
}

#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub cols: &'a [String],
    pub vals: &'a [SqlVal],
}

impl<'a> Row<'a> {
    /// sqlite3.Row lookup: case-insensitive column name.
    pub fn raw(&self, key: &str) -> &'a SqlVal {
        let at = self
            .cols
            .iter()
            .position(|c| c.eq_ignore_ascii_case(key))
            .unwrap_or_else(|| panic!("no such column {}", key));
        &self.vals[at]
    }
    pub fn try_raw(&self, key: &str) -> Option<&'a SqlVal> {
        self.cols.iter().position(|c| c.eq_ignore_ascii_case(key)).map(|at| &self.vals[at])
    }
    pub fn val(&self, key: &str) -> Val {
        sql_to_val(self.raw(key))
    }
    pub fn at(&self, i: usize) -> Val {
        sql_to_val(&self.vals[i])
    }
    pub fn text(&self, key: &str) -> Option<&'a str> {
        match self.raw(key) {
            SqlVal::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }
    pub fn int(&self, key: &str) -> i64 {
        match self.raw(key) {
            SqlVal::Integer(i) => *i,
            SqlVal::Real(f) => *f as i64,
            _ => 0,
        }
    }
    pub fn truthy(&self, key: &str) -> bool {
        sql_truthy(self.raw(key))
    }
    pub fn is_null(&self, key: &str) -> bool {
        matches!(self.raw(key), SqlVal::Null)
    }
    /// dict(row)
    pub fn to_dict(&self) -> Val {
        Val::Dict(self.cols.iter().zip(self.vals.iter()).map(|(c, v)| (c.clone(), sql_to_val(v))).collect())
    }
    pub fn to_tuple(&self) -> Vec<SqlVal> {
        self.vals.to_vec()
    }
}

pub fn sql_truthy(v: &SqlVal) -> bool {
    match v {
        SqlVal::Null => false,
        SqlVal::Integer(i) => *i != 0,
        SqlVal::Real(f) => *f != 0.0,
        SqlVal::Text(s) => !s.is_empty(),
        SqlVal::Blob(b) => !b.is_empty(),
    }
}

pub fn sql_to_val(v: &SqlVal) -> Val {
    match v {
        SqlVal::Null => Val::Null,
        SqlVal::Integer(i) => Val::Int(*i),
        SqlVal::Real(f) => Val::Float(*f),
        SqlVal::Text(s) => Val::Str(s.clone()),
        SqlVal::Blob(b) => Val::Bytes(b.clone()),
    }
}

/// Bind a value with CPython sqlite3 adaptation rules.
pub fn bind(v: &Val, index: usize) -> Result<SqlVal> {
    Ok(match v {
        Val::Null => SqlVal::Null,
        Val::Bool(b) => SqlVal::Integer(*b as i64),
        Val::Int(i) => SqlVal::Integer(*i),
        Val::BigInt(_) => return Err(Error::Overflow("Python int too large to convert to SQLite INTEGER".into())),
        Val::Float(f) => SqlVal::Real(*f),
        Val::Str(s) => SqlVal::Text(s.clone()),
        Val::Bytes(b) => SqlVal::Blob(b.clone()),
        other => {
            return Err(Error::Sqlite(
                SqlErrKind::Programming,
                format!("Error binding parameter {}: type '{}' is not supported", index, other.type_name()),
            ))
        }
    })
}

pub fn text(s: &str) -> SqlVal {
    SqlVal::Text(s.to_string())
}
pub fn int(i: i64) -> SqlVal {
    SqlVal::Integer(i)
}
pub fn null() -> SqlVal {
    SqlVal::Null
}
pub fn opt_text(s: Option<&str>) -> SqlVal {
    s.map(text).unwrap_or(SqlVal::Null)
}

/// Statement execution interface shared by native connections and foreign
/// (Python sqlite3) connections handed to `migrate`.
pub trait Sql: Sync {
    fn exec(&self, sql: &str, params: &[SqlVal]) -> Result<Cursor>;
    fn as_any(&self) -> &dyn Any;
    fn query(&self, sql: &str, params: &[SqlVal]) -> Result<Cursor> {
        self.exec(sql, params)
    }
    fn one(&self, sql: &str, params: &[SqlVal]) -> Result<Option<Vec<SqlVal>>> {
        Ok(self.exec(sql, params)?.rows.into_iter().next())
    }
}

/// CPython `_sqlite/statement.c` DML detection: leading keyword after
/// whitespace/comments is INSERT, UPDATE, DELETE or REPLACE.
pub fn is_dml(sql: &str) -> bool {
    let b = sql.as_bytes();
    let mut i = 0;
    loop {
        while i < b.len() && (b[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            match sql[i + 2..].find("*/") {
                Some(at) => {
                    i += 2 + at + 2;
                    continue;
                }
                None => return false,
            }
        }
        break;
    }
    let rest = &sql[i..];
    ["insert", "update", "delete", "replace"]
        .iter()
        .any(|kw| rest.len() >= kw.len() && rest[..kw.len()].eq_ignore_ascii_case(kw))
}

/// One SQLite connection (Store.connect() semantics).
pub struct Db {
    conn: ReentrantMutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = blocking(|| Connection::open(path))?;
        // sqlite3.connect(path, timeout=10)
        conn.busy_timeout(Duration::from_secs(10))?;
        let db = Db { conn: ReentrantMutex::new(conn) };
        db.exec("PRAGMA foreign_keys=ON", &[])?;
        db.exec("PRAGMA synchronous=FULL", &[])?;
        db.exec("PRAGMA busy_timeout=10000", &[])?;
        Ok(db)
    }
    pub fn in_transaction(&self) -> bool {
        !self.conn.lock().is_autocommit()
    }
    pub fn commit(&self) -> Result<()> {
        blocking(|| {
            let c = self.conn.lock();
            if !c.is_autocommit() {
                c.execute_batch("COMMIT")?;
            }
            Ok(())
        })
    }
    pub fn rollback(&self) -> Result<()> {
        blocking(|| {
            let c = self.conn.lock();
            if !c.is_autocommit() {
                c.execute_batch("ROLLBACK")?;
            }
            Ok(())
        })
    }
    /// sqlite3.Connection.executescript: commit first, then run as-is.
    pub fn executescript(&self, sql: &str) -> Result<()> {
        blocking(|| {
            let c = self.conn.lock();
            if !c.is_autocommit() {
                c.execute_batch("COMMIT")?;
            }
            c.execute_batch(sql)?;
            Ok(())
        })
    }
    pub fn total_changes(&self) -> i64 {
        self.conn.lock().total_changes() as i64
    }
    fn run(&self, sql: &str, params: &[SqlVal]) -> Result<Cursor> {
        blocking(|| self.run_locked(sql, params))
    }
    fn run_locked(&self, sql: &str, params: &[SqlVal]) -> Result<Cursor> {
        let c = self.conn.lock();
        let dml = is_dml(sql);
        if dml && c.is_autocommit() {
            c.execute_batch("BEGIN ")?;
        }
        let mut stmt = c.prepare(sql)?;
        let expected = stmt.parameter_count();
        if expected != params.len() {
            return Err(rusqlite::Error::InvalidParameterCount(params.len(), expected).into());
        }
        for (i, p) in params.iter().enumerate() {
            stmt.raw_bind_parameter(i + 1, p)?;
        }
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let n = cols.len();
        let mut out = Vec::new();
        {
            let mut rows = stmt.raw_query();
            while let Some(row) = rows.next()? {
                let mut vals = Vec::with_capacity(n);
                for i in 0..n {
                    vals.push(match row.get_ref(i)? {
                        ValueRef::Null => SqlVal::Null,
                        ValueRef::Integer(v) => SqlVal::Integer(v),
                        ValueRef::Real(v) => SqlVal::Real(v),
                        ValueRef::Text(t) => SqlVal::Text(
                            std::str::from_utf8(t)
                                .map_err(|_| {
                                    Error::Sqlite(
                                        SqlErrKind::Operational,
                                        format!("Could not decode to UTF-8 column '{}'", cols[i]),
                                    )
                                })?
                                .to_string(),
                        ),
                        ValueRef::Blob(b) => SqlVal::Blob(b.to_vec()),
                    });
                }
                out.push(vals);
            }
        }
        let rowcount = if dml { c.changes() as i64 } else { -1 };
        let lastrowid = Some(c.last_insert_rowid());
        Ok(Cursor { cols: Arc::new(cols), rows: out, rowcount, lastrowid })
    }
}

/// Shared handle: lets a Python `Connection` object and the core use the same
/// transaction (needed when a Python override receives `db`).
#[derive(Clone)]
pub struct DbHandle(pub Arc<Db>);

impl Sql for DbHandle {
    fn exec(&self, sql: &str, params: &[SqlVal]) -> Result<Cursor> {
        self.0.run(sql, params)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Sql for Db {
    fn exec(&self, sql: &str, params: &[SqlVal]) -> Result<Cursor> {
        self.run(sql, params)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `with self.connect() as db:` — commit on success, rollback on error, close.
pub fn with_db<T>(path: &Path, f: impl FnOnce(&DbHandle) -> Result<T>) -> Result<T> {
    let db = DbHandle(Arc::new(Db::open(path)?));
    match f(&db) {
        Ok(v) => match db.0.commit() {
            Ok(()) => Ok(v),
            Err(e) => {
                let _ = db.0.rollback();
                Err(e)
            }
        },
        Err(e) => {
            let _ = db.0.rollback();
            Err(e)
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_nanos() / 1_000_000) as i64)
        .unwrap_or(0)
}

pub fn uuid4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// json.loads of a TEXT column.
pub fn loads_sql(v: &SqlVal) -> Result<Val> {
    match v {
        SqlVal::Text(s) => Ok(crate::val::loads(s)?),
        SqlVal::Blob(b) => Ok(crate::val::loads(&String::from_utf8_lossy(b))?),
        SqlVal::Null => Err(Error::Type("the JSON object must be str, bytes or bytearray, not NoneType".into())),
        SqlVal::Integer(_) => Err(Error::Type("the JSON object must be str, bytes or bytearray, not int".into())),
        SqlVal::Real(_) => Err(Error::Type("the JSON object must be str, bytes or bytearray, not float".into())),
    }
}

/// Python `==` between a SQL value and a JSON value.
pub fn sql_eq(a: &SqlVal, b: &Val) -> bool {
    crate::val::py_eq(&sql_to_val(a), b)
}

/// Python repr() of a str (quote choice and escapes as CPython).
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `value[key]` on a JSON-like value with Python's KeyError/TypeError.
pub fn key<'a>(v: &'a Val, k: &str) -> Result<&'a Val> {
    match v {
        Val::Dict(_) => v.get(k).ok_or_else(|| Error::Key(py_repr_str(k))),
        Val::Null => Err(Error::Type("'NoneType' object is not subscriptable".into())),
        Val::List(_) | Val::Str(_) => Err(Error::Type(format!("{} indices must be integers or slices, not str", v.type_name()))),
        other => Err(Error::Type(format!("'{}' object is not subscriptable", other.type_name()))),
    }
}

/// `value.get(key, default)` (AttributeError for non-dicts mirrored as TypeError).
pub fn get_or<'a>(v: &'a Val, k: &str, default: &'a Val) -> &'a Val {
    v.get(k).unwrap_or(default)
}

/// json.loads of an optional TEXT column (`json.loads(x) if x else None`).
pub fn loads_opt(v: &SqlVal) -> Result<Option<Val>> {
    if sql_truthy(v) {
        Ok(Some(loads_sql(v)?))
    } else {
        Ok(None)
    }
}

pub fn dumps_ascii(v: &Val) -> Result<String> {
    Ok(crate::val::dumps_ascii(v)?)
}
pub fn dumps_utf8(v: &Val) -> Result<String> {
    Ok(crate::val::dumps_utf8(v)?)
}

/// Bind a JSON value as a positional parameter (1-based index for messages).
pub fn b(v: &Val) -> Result<SqlVal> {
    bind(v, 1)
}
