//! Python boundary: value conversion, exceptions (StoreError + sqlite3
//! classes), the sqlite3-compatible Connection/Cursor/Row objects returned by
//! `Store.connect()`, and duck-typed access to foreign Python connections.
use crate::db::{bind, sql_to_val, Cursor, Db, DbHandle, Error, Sql, SqlErrKind};
use crate::val::Val;
use parking_lot::Mutex;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyAttributeError, PyKeyError, PyOverflowError, PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
#[cfg(feature = "python")]
use pyo3::PyTypeInfo;
use rusqlite::types::Value as SqlVal;
use std::any::Any;
use std::sync::Arc;

// ---------------------------------------------------------------- values

#[cfg(feature = "python")]
fn py_str(s: &Bound<'_, PyString>) -> PyResult<String> {
    match s.to_str() {
        Ok(v) => Ok(v.to_string()),
        Err(_) => {
            // Lone surrogates: keep everything else, map each surrogate to
            // U+FFFF (rejected by text validation like the surrogate itself).
            let bytes = s.call_method1("encode", ("utf-16-le", "surrogatepass"))?;
            let bytes: Vec<u8> = bytes.extract()?;
            let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(char::decode_utf16(units.into_iter()).map(|r| r.unwrap_or('\u{FFFF}')).collect())
        }
    }
}

#[cfg(feature = "python")]
fn dict_key(k: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = k.cast::<PyString>() {
        return py_str(s);
    }
    if k.is_none() {
        return Ok("null".into());
    }
    if let Ok(b) = k.cast::<PyBool>() {
        return Ok(if b.is_true() { "true" } else { "false" }.into());
    }
    if k.is_instance_of::<PyInt>() {
        return Ok(k.str()?.to_string());
    }
    if let Ok(f) = k.cast::<PyFloat>() {
        return Ok(crate::val::float_repr(f.value()));
    }
    Err(PyTypeError::new_err(format!(
        "keys must be str, int, float, bool or None, not {}",
        k.get_type().name()?
    )))
}

#[cfg(feature = "python")]
/// Python object -> JSON-like value (json.dumps-compatible types).
pub fn to_val(obj: &Bound<'_, PyAny>) -> PyResult<Val> {
    if obj.is_none() {
        return Ok(Val::Null);
    }
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(Val::Bool(b.is_true()));
    }
    if obj.is_instance_of::<PyInt>() {
        return Ok(match obj.extract::<i64>() {
            Ok(i) => Val::Int(i),
            Err(_) => Val::BigInt(obj.call_method0("__index__")?.str()?.to_string()),
        });
    }
    if let Ok(f) = obj.cast::<PyFloat>() {
        return Ok(Val::Float(f.value()));
    }
    if let Ok(s) = obj.cast::<PyString>() {
        return Ok(Val::Str(py_str(s)?));
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        let mut items: Vec<(String, Val)> = Vec::with_capacity(d.len());
        for (k, v) in d.iter() {
            let k = dict_key(&k)?;
            let v = to_val(&v)?;
            if let Some(slot) = items.iter_mut().find(|(e, _)| *e == k) {
                slot.1 = v;
            } else {
                items.push((k, v));
            }
        }
        return Ok(Val::Dict(items));
    }
    if let Ok(l) = obj.cast::<PyList>() {
        return Ok(Val::List(l.iter().map(|v| to_val(&v)).collect::<PyResult<_>>()?));
    }
    if let Ok(t) = obj.cast::<PyTuple>() {
        return Ok(Val::List(t.iter().map(|v| to_val(&v)).collect::<PyResult<_>>()?));
    }
    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok(Val::Bytes(b.as_bytes().to_vec()));
    }
    Err(PyTypeError::new_err(format!("Object of type {} is not JSON serializable", obj.get_type().name()?)))
}

#[cfg(feature = "python")]
pub fn to_py(py: Python<'_>, v: &Val) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Val::Null => py.None(),
        Val::Bool(b) => PyBool::new(py, *b).to_owned().into_any().unbind(),
        Val::Int(i) => i.into_pyobject(py)?.into_any().unbind(),
        Val::BigInt(s) => py.get_type::<PyInt>().call1((s.as_str(),))?.unbind(),
        Val::Float(f) => PyFloat::new(py, *f).into_any().unbind(),
        Val::Str(s) => PyString::new(py, s).into_any().unbind(),
        Val::Bytes(b) => PyBytes::new(py, b).into_any().unbind(),
        Val::List(items) => {
            let l = PyList::empty(py);
            for item in items {
                l.append(to_py(py, item)?)?;
            }
            l.into_any().unbind()
        }
        Val::Dict(items) => {
            let d = PyDict::new(py);
            for (k, item) in items {
                d.set_item(k, to_py(py, item)?)?;
            }
            d.into_any().unbind()
        }
    })
}

#[cfg(feature = "python")]
pub fn sql_to_py(py: Python<'_>, v: &SqlVal) -> PyResult<Py<PyAny>> {
    to_py(py, &sql_to_val(v))
}

#[cfg(feature = "python")]
fn py_to_sql(obj: &Bound<'_, PyAny>, index: usize) -> PyResult<SqlVal> {
    let v = if let Ok(b) = obj.cast::<pyo3::types::PyByteArray>() {
        Val::Bytes(b.to_vec())
    } else {
        match to_val(obj) {
            Ok(v) => v,
            Err(_) => {
                return Err(sqlite_exc(
                    obj.py(),
                    SqlErrKind::Programming,
                    &format!("Error binding parameter {}: type '{}' is not supported", index, obj.get_type().name()?),
                ))
            }
        }
    };
    bind(&v, index).map_err(|e| err_to_py(obj.py(), e))
}

#[cfg(feature = "python")]
fn params_from_py(params: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<SqlVal>> {
    let Some(params) = params else { return Ok(Vec::new()) };
    if params.is_instance_of::<PyDict>() {
        return Err(sqlite_exc(params.py(), SqlErrKind::Programming, "named parameters are not supported by this connection"));
    }
    if params.is_instance_of::<PyString>() {
        return Err(sqlite_exc(params.py(), SqlErrKind::Programming, "parameters are of unsupported type"));
    }
    let mut out = Vec::new();
    for (i, item) in params.try_iter()?.enumerate() {
        out.push(py_to_sql(&item?, i + 1)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------- errors

#[cfg(feature = "python")]
#[pyclass(extends = PyValueError, subclass, module = "samsung_notes.storage", name = "StoreError")]
pub struct StoreError {
    #[pyo3(get, set)]
    code: Py<PyAny>,
    #[pyo3(get, set)]
    message: Py<PyAny>,
    #[pyo3(get, set)]
    status: Py<PyAny>,
}

#[cfg(feature = "python")]
#[pymethods]
impl StoreError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (code, message, status = None))]
    fn new(py: Python<'_>, code: Py<PyAny>, message: Py<PyAny>, status: Option<Py<PyAny>>) -> Self {
        let status = status.unwrap_or_else(|| 400i64.into_pyobject(py).unwrap().into_any().unbind());
        StoreError { code, message, status }
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (code, message, status = None))]
    fn __init__(slf: &Bound<'_, Self>, code: Py<PyAny>, message: Py<PyAny>, status: Option<Py<PyAny>>) -> PyResult<()> {
        let py = slf.py();
        PyValueError::type_object(py).getattr("__init__")?.call1((slf, message.clone_ref(py)))?;
        let mut this = slf.borrow_mut();
        this.code = code;
        this.message = message;
        this.status = status.unwrap_or_else(|| 400i64.into_pyobject(py).unwrap().into_any().unbind());
        Ok(())
    }
}

#[cfg(feature = "python")]
pub fn store_error(py: Python<'_>, code: &str, message: &str, status: u16) -> PyErr {
    match py.get_type::<StoreError>().call1((code, message, status)) {
        Ok(obj) => PyErr::from_value(obj),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
pub fn sqlite_exc(py: Python<'_>, kind: SqlErrKind, message: &str) -> PyErr {
    let name = match kind {
        SqlErrKind::Operational => "OperationalError",
        SqlErrKind::Integrity => "IntegrityError",
        SqlErrKind::Programming => "ProgrammingError",
        SqlErrKind::Database => "DatabaseError",
        SqlErrKind::Data => "DataError",
    };
    match py.import("sqlite3").and_then(|m| m.getattr(name)).and_then(|t| t.call1((message,))) {
        Ok(obj) => PyErr::from_value(obj),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
pub fn err_to_py(py: Python<'_>, e: Error) -> PyErr {
    match e {
        Error::Store { code, message, status } => store_error(py, &code, &message, status),
        Error::Sqlite(kind, m) => sqlite_exc(py, kind, &m),
        Error::Value(m) | Error::Decode(m) => PyValueError::new_err(m),
        Error::Type(m) => PyTypeError::new_err(m),
        Error::Key(m) => PyKeyError::new_err(m),
        Error::Overflow(m) => PyOverflowError::new_err(m),
        Error::Attribute(m) => PyAttributeError::new_err(m),
        Error::Io(e) => PyErr::from(e),
        Error::External(b) => match b.downcast::<PyErr>() {
            Ok(err) => *err,
            Err(other) => PyValueError::new_err(other.to_string()),
        },
    }
}

#[cfg(feature = "python")]
pub fn py_err(e: PyErr) -> Error {
    Error::External(Box::new(e))
}

// ---------------------------------------------------------------- GIL

#[cfg(feature = "python")]
fn release_gil(f: &mut dyn FnMut()) {
    unsafe {
        if pyo3::ffi::Py_IsInitialized() != 0 && pyo3::ffi::PyGILState_Check() == 1 {
            let state = pyo3::ffi::PyEval_SaveThread();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f()));
            pyo3::ffi::PyEval_RestoreThread(state);
            if let Err(p) = r {
                std::panic::resume_unwind(p);
            }
        } else {
            f()
        }
    }
}

#[cfg(feature = "python")]
pub fn install(_py: Python<'_>) -> PyResult<()> {
    let _ = crate::db::BLOCKING.set(release_gil);
    Ok(())
}

// ---------------------------------------------------------------- sqlite3-like objects

#[cfg(feature = "python")]
/// sqlite3.Connection look-alike returned by Store.connect(); a context
/// manager that commits on success, rolls back on error and closes.
#[pyclass(module = "samsung_notes.storage", name = "Connection")]
pub struct Connection {
    db: Mutex<Option<DbHandle>>,
    owned: bool,
}

#[cfg(feature = "python")]
impl Connection {
    pub fn open(path: &std::path::Path) -> crate::db::Result<Connection> {
        Ok(Connection { db: Mutex::new(Some(DbHandle(Arc::new(Db::open(path)?)))), owned: true })
    }
    pub fn borrowed(handle: DbHandle) -> Connection {
        Connection { db: Mutex::new(Some(handle)), owned: false }
    }
    #[cfg(feature = "python")]
    pub fn handle(&self, py: Python<'_>) -> PyResult<DbHandle> {
        self.db
            .lock()
            .clone()
            .ok_or_else(|| sqlite_exc(py, SqlErrKind::Programming, "Cannot operate on a closed database."))
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl Connection {
    #[cfg(feature = "python")]
    #[pyo3(signature = (sql, parameters = None, /))]
    fn execute(&self, py: Python<'_>, sql: &str, parameters: Option<&Bound<'_, PyAny>>) -> PyResult<PyCursor> {
        let params = params_from_py(parameters)?;
        let h = self.handle(py)?;
        let cur = h.exec(sql, &params).map_err(|e| err_to_py(py, e))?;
        Ok(PyCursor::new(cur))
    }

    #[cfg(feature = "python")]
    fn executemany(&self, py: Python<'_>, sql: &str, seq_of_parameters: &Bound<'_, PyAny>) -> PyResult<PyCursor> {
        let h = self.handle(py)?;
        let mut total = 0i64;
        let mut last = Cursor::default();
        for params in seq_of_parameters.try_iter()? {
            let params = params_from_py(Some(&params?))?;
            last = h.exec(sql, &params).map_err(|e| err_to_py(py, e))?;
            total += last.rowcount.max(0);
        }
        last.rows.clear();
        last.rowcount = total;
        Ok(PyCursor::new(last))
    }

    #[cfg(feature = "python")]
    fn executescript(&self, py: Python<'_>, sql_script: &str) -> PyResult<PyCursor> {
        let h = self.handle(py)?;
        h.0.executescript(sql_script).map_err(|e| err_to_py(py, e))?;
        Ok(PyCursor::new(Cursor { rowcount: -1, ..Default::default() }))
    }

    #[cfg(feature = "python")]
    fn commit(&self, py: Python<'_>) -> PyResult<()> {
        let h = self.handle(py)?;
        h.0.commit().map_err(|e| err_to_py(py, e))
    }

    #[cfg(feature = "python")]
    fn rollback(&self, py: Python<'_>) -> PyResult<()> {
        let h = self.handle(py)?;
        h.0.rollback().map_err(|e| err_to_py(py, e))
    }

    fn close(&self) {
        if self.owned {
            self.db.lock().take();
        }
    }

    #[cfg(feature = "python")]
    #[getter]
    fn in_transaction(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self.handle(py)?.0.in_transaction())
    }

    #[cfg(feature = "python")]
    #[getter]
    fn total_changes(&self, py: Python<'_>) -> PyResult<i64> {
        Ok(self.handle(py)?.0.total_changes())
    }

    #[cfg(feature = "python")]
    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[cfg(feature = "python")]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: &Bound<'_, PyAny>,
        _exc: &Bound<'_, PyAny>,
        _tb: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let h = self.handle(py)?;
        let result = if exc_type.is_none() {
            match h.0.commit() {
                Ok(()) => Ok(()),
                Err(e) => {
                    let _ = h.0.rollback();
                    Err(err_to_py(py, e))
                }
            }
        } else {
            let _ = h.0.rollback();
            Ok(())
        };
        self.close();
        result.map(|_| false)
    }
}

#[cfg(feature = "python")]
#[pyclass(module = "samsung_notes.storage", name = "Cursor")]
pub struct PyCursor {
    cols: Arc<Vec<String>>,
    rows: std::vec::IntoIter<Vec<SqlVal>>,
    #[pyo3(get)]
    rowcount: i64,
    #[pyo3(get)]
    lastrowid: Option<i64>,
    #[pyo3(get, set)]
    arraysize: usize,
}

#[cfg(feature = "python")]
impl PyCursor {
    pub fn new(cur: Cursor) -> PyCursor {
        PyCursor { cols: cur.cols, rows: cur.rows.into_iter(), rowcount: cur.rowcount, lastrowid: cur.lastrowid, arraysize: 1 }
    }
    #[cfg(feature = "python")]
    fn row(&self, py: Python<'_>, vals: Vec<SqlVal>) -> PyResult<PyRow> {
        let vals = vals.iter().map(|v| sql_to_py(py, v)).collect::<PyResult<Vec<_>>>()?;
        Ok(PyRow { cols: self.cols.clone(), vals })
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl PyCursor {
    #[cfg(feature = "python")]
    fn fetchone(&mut self, py: Python<'_>) -> PyResult<Option<PyRow>> {
        match self.rows.next() {
            Some(vals) => Ok(Some(self.row(py, vals)?)),
            None => Ok(None),
        }
    }

    #[cfg(feature = "python")]
    fn fetchall(&mut self, py: Python<'_>) -> PyResult<Vec<PyRow>> {
        let rest: Vec<_> = self.rows.by_ref().collect();
        rest.into_iter().map(|v| self.row(py, v)).collect()
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (size = None))]
    fn fetchmany(&mut self, py: Python<'_>, size: Option<usize>) -> PyResult<Vec<PyRow>> {
        let n = size.unwrap_or(self.arraysize);
        let rest: Vec<_> = self.rows.by_ref().take(n).collect();
        rest.into_iter().map(|v| self.row(py, v)).collect()
    }

    #[cfg(feature = "python")]
    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[cfg(feature = "python")]
    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyRow>> {
        self.fetchone(py)
    }

    #[cfg(feature = "python")]
    #[getter]
    fn description(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if self.cols.is_empty() {
            return Ok(py.None());
        }
        let items = self
            .cols
            .iter()
            .map(|c| PyTuple::new(py, [c.into_pyobject(py)?.into_any(), py.None().into_bound(py), py.None().into_bound(py), py.None().into_bound(py), py.None().into_bound(py), py.None().into_bound(py), py.None().into_bound(py)]))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(PyTuple::new(py, items)?.into_any().unbind())
    }

    fn close(&mut self) {
        self.rows = Vec::new().into_iter();
    }
}

#[cfg(feature = "python")]
/// sqlite3.Row look-alike: index, case-insensitive name, keys(), iteration.
#[pyclass(frozen, module = "samsung_notes.storage", name = "Row", sequence)]
pub struct PyRow {
    cols: Arc<Vec<String>>,
    vals: Vec<Py<PyAny>>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyRow {
    fn __len__(&self) -> usize {
        self.vals.len()
    }

    #[cfg(feature = "python")]
    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        if let Ok(name) = key.cast::<PyString>() {
            let name = name.to_str()?;
            return match self.cols.iter().position(|c| c.eq_ignore_ascii_case(name)) {
                Some(at) => Ok(self.vals[at].clone_ref(py)),
                None => Err(pyo3::exceptions::PyIndexError::new_err("No item with that key")),
            };
        }
        if key.is_instance_of::<pyo3::types::PySlice>() {
            let t = PyTuple::new(py, self.vals.iter().map(|v| v.clone_ref(py)))?;
            return Ok(t.get_item_any(key)?);
        }
        let mut i: isize = key.extract()?;
        if i < 0 {
            i += self.vals.len() as isize;
        }
        if i < 0 || i as usize >= self.vals.len() {
            return Err(pyo3::exceptions::PyIndexError::new_err("tuple index out of range"));
        }
        Ok(self.vals[i as usize].clone_ref(py))
    }

    fn keys(&self) -> Vec<String> {
        self.cols.to_vec()
    }

    #[cfg(feature = "python")]
    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let t = PyTuple::new(py, self.vals.iter().map(|v| v.clone_ref(py)))?;
        Ok(t.try_iter()?.into_any().unbind())
    }

    #[cfg(feature = "python")]
    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        let Ok(other) = other.cast::<PyRow>() else { return Ok(false) };
        let other = other.get();
        if other.cols != self.cols || other.vals.len() != self.vals.len() {
            return Ok(false);
        }
        for (a, b) in self.vals.iter().zip(other.vals.iter()) {
            if !a.bind(py).eq(b.bind(py))? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[cfg(feature = "python")]
    fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
        let t = PyTuple::new(py, self.vals.iter().map(|v| v.clone_ref(py)))?;
        t.hash()
    }

    fn __repr__(&self) -> String {
        format!("<Row {:?}>", self.cols)
    }
}

trait TupleAny {
    #[cfg(feature = "python")]
    fn get_item_any(&self, key: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>>;
}
#[cfg(feature = "python")]
impl TupleAny for Bound<'_, PyTuple> {
    #[cfg(feature = "python")]
    fn get_item_any(&self, key: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Ok(self.as_any().get_item(key)?.unbind())
    }
}

#[cfg(feature = "python")]
/// A foreign Python DB-API connection (e.g. sqlite3.Connection) passed to
/// `migrate(db)`: statements run through its own `execute`.
pub struct PySql(pub Py<PyAny>);

#[cfg(feature = "python")]
impl Sql for PySql {
    #[cfg(feature = "python")]
    fn exec(&self, sql: &str, params: &[SqlVal]) -> crate::db::Result<Cursor> {
        Python::attach(|py| -> PyResult<Cursor> {
            let args = PyTuple::new(py, params.iter().map(|p| sql_to_py(py, p)).collect::<PyResult<Vec<_>>>()?)?;
            let cur = self.0.bind(py).call_method1("execute", (sql, args))?;
            let description = cur.getattr("description")?;
            let cols: Vec<String> = if description.is_none() {
                Vec::new()
            } else {
                description.try_iter()?.map(|d| d?.get_item(0)?.extract::<String>()).collect::<PyResult<_>>()?
            };
            let mut rows = Vec::new();
            for row in cur.call_method0("fetchall")?.try_iter()? {
                let mut vals = Vec::new();
                for v in row?.try_iter()? {
                    let v = v?;
                    vals.push(match to_val(&v)? {
                        Val::Null => SqlVal::Null,
                        Val::Int(i) => SqlVal::Integer(i),
                        Val::Float(f) => SqlVal::Real(f),
                        Val::Str(s) => SqlVal::Text(s),
                        Val::Bytes(b) => SqlVal::Blob(b),
                        other => SqlVal::Text(other.type_name().to_string()),
                    });
                }
                rows.push(vals);
            }
            let rowcount: i64 = cur.getattr("rowcount")?.extract().unwrap_or(-1);
            Ok(Cursor { cols: Arc::new(cols), rows, rowcount, lastrowid: None })
        })
        .map_err(py_err)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(feature = "python")]
/// Python object usable as `db` by Python code: our own handle or the
/// foreign connection it came from.
pub fn db_object(py: Python<'_>, db: &dyn Sql) -> PyResult<Py<PyAny>> {
    if let Some(h) = db.as_any().downcast_ref::<DbHandle>() {
        return Ok(Py::new(py, Connection::borrowed(h.clone()))?.into_any());
    }
    if let Some(p) = db.as_any().downcast_ref::<PySql>() {
        return Ok(p.0.clone_ref(py));
    }
    Err(PyTypeError::new_err("unsupported connection"))
}

#[cfg(feature = "python")]
/// Accept `db` from Python: our Connection (native) or any DB-API object.
pub enum DbArg {
    Native(DbHandle),
    Foreign(PySql),
}

#[cfg(feature = "python")]
impl DbArg {
    #[cfg(feature = "python")]
    pub fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<DbArg> {
        if let Ok(c) = obj.cast::<Connection>() {
            return Ok(DbArg::Native(c.borrow().handle(obj.py())?));
        }
        Ok(DbArg::Foreign(PySql(obj.clone().unbind())))
    }
    pub fn sql(&self) -> &dyn Sql {
        match self {
            DbArg::Native(h) => h,
            DbArg::Foreign(p) => p,
        }
    }
}
