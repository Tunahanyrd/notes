//! Replacement for the Python module `engine`: the `Engine` class.
//!
//! State lives in the instance `__dict__` (tests monkeypatch attributes such
//! as `_api`, `previews`, `api_session`), and every internal `self.x(...)`
//! call goes through Python attribute lookup exactly like the Python class.
//! Lower areas (store, session, cloud, render, ops, doc) are reached through
//! `rs_common::py_fallback` until their crates are STABLE. Deterministic and
//! file-system logic lives in `engine_core` (pure Rust).
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[cfg(feature = "python")]
use pyo3::exceptions::{PyAttributeError, PyKeyError, PyOSError, PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyList, PySet, PyString, PyTuple};

use crate::engine_core as core;
use crate::pyconv;
use crate::pyjson;

#[cfg(feature = "python")]
type B<'py> = Bound<'py, PyAny>;

// ------------------------------------------------------------------ helpers

#[cfg(feature = "python")]
/// A Python-facing object owned by a STABLE area (store, doc, editor, ink, ops,
/// cloud, render), resolved through its `samsung_notes.*` module: under NOTES_IMPL=rust
/// this is that area's Rust implementation (cross-extension rule: never link
/// another cdylib's pyclasses directly).
fn area<'py>(py: Python<'py>, module: &str, name: &str) -> PyResult<B<'py>> {
    py.import(module)?.getattr(name)
}

#[cfg(feature = "python")]
fn exc_class<'py>(py: Python<'py>, module: &str, name: &str) -> PyResult<B<'py>> {
    py.import(module)?.getattr(name)
}

#[cfg(feature = "python")]
fn is_exc(py: Python<'_>, err: &PyErr, module: &str, name: &str) -> bool {
    exc_class(py, module, name).map(|c| err.value(py).is_instance(&c).unwrap_or(false)).unwrap_or(false)
}

#[cfg(feature = "python")]
fn is_store_error(py: Python<'_>, e: &PyErr) -> bool {
    is_exc(py, e, "samsung_notes.storage", "StoreError")
}
#[cfg(feature = "python")]
fn is_api_error(py: Python<'_>, e: &PyErr) -> bool {
    is_exc(py, e, "samsung_notes.cloud_api", "APIError")
}
#[cfg(feature = "python")]
fn is_conflict(py: Python<'_>, e: &PyErr) -> bool {
    is_exc(py, e, "samsung_notes.cloud_api", "ConflictError")
}
#[cfg(feature = "python")]
fn is_document_error(py: Python<'_>, e: &PyErr) -> bool {
    is_exc(py, e, "samsung_notes.document", "DocumentError")
}
#[cfg(feature = "python")]
/// `except (OSError, ValueError, KeyError, TypeError)`
fn is_io_value(py: Python<'_>, e: &PyErr) -> bool {
    e.is_instance_of::<PyOSError>(py) || e.is_instance_of::<PyValueError>(py) || e.is_instance_of::<PyKeyError>(py) || e.is_instance_of::<PyTypeError>(py)
}

#[cfg(feature = "python")]
fn store_error(py: Python<'_>, code: &str, message: &str, status: u16) -> PyErr {
    match exc_class(py, "samsung_notes.storage", "StoreError").and_then(|c| c.call1((code, message, status))) {
        Ok(v) => PyErr::from_value(v),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
fn api_error(py: Python<'_>, code: &str, message: &str, ambiguous: bool) -> PyErr {
    let kwargs = PyDict::new(py);
    let made = (|| {
        if ambiguous {
            kwargs.set_item("ambiguous", true)?;
        }
        exc_class(py, "samsung_notes.cloud_api", "APIError")?.call((code, message), Some(&kwargs))
    })();
    match made {
        Ok(v) => PyErr::from_value(v),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
fn err_attr<'py>(py: Python<'py>, e: &PyErr, name: &str) -> PyResult<B<'py>> {
    e.value(py).getattr(name)
}

#[cfg(feature = "python")]
fn err_code_in(py: Python<'_>, e: &PyErr, codes: &[&str]) -> PyResult<bool> {
    let code = err_attr(py, e, "code")?;
    for c in codes {
        if code.eq(*c)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(feature = "python")]
fn int_obj(py: Python<'_>, v: i64) -> B<'_> {
    pyo3::types::PyInt::new(py, v).into_any()
}

#[cfg(feature = "python")]
fn bool_obj(py: Python<'_>, v: bool) -> B<'_> {
    pyo3::types::PyBool::new(py, v).to_owned().into_any()
}

#[cfg(feature = "python")]
fn none(py: Python<'_>) -> B<'_> {
    py.None().into_bound(py)
}

#[cfg(feature = "python")]
fn kw<'py>(py: Python<'py>, items: &[(&str, B<'py>)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in items {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

#[cfg(feature = "python")]
/// `with cm as value: f(value)` (exceptions are passed to `__exit__`).
fn with_cm<'py, T>(cm: B<'py>, f: impl FnOnce(&B<'py>) -> PyResult<T>) -> PyResult<T> {
    let py = cm.py();
    let value = cm.call_method0("__enter__")?;
    match f(&value) {
        Ok(v) => {
            cm.call_method1("__exit__", (none(py), none(py), none(py)))?;
            Ok(v)
        }
        Err(e) => {
            let tb = e.traceback(py).map(|t| t.into_any()).unwrap_or_else(|| none(py));
            cm.call_method1("__exit__", (e.get_type(py), e.value(py), tb))?;
            Err(e)
        }
    }
}

#[cfg(feature = "python")]
/// `json.loads(value)` for a `str`/`bytes` column.
fn json_loads<'py>(value: &B<'py>) -> PyResult<B<'py>> {
    let py = value.py();
    if let Ok(s) = value.cast::<PyString>() {
        if let Ok(text) = s.to_str() {
            let parsed = pyjson::loads_str_lenient(text).map_err(|_| PyValueError::new_err("Expecting value"))?;
            return pyconv::to_py(py, &parsed);
        }
    }
    if let Ok(b) = value.cast::<PyBytes>() {
        return pyconv::loads_py(py, b.as_bytes());
    }
    py.import("json")?.call_method1("loads", (value,))
}

#[cfg(feature = "python")]
fn path_of<'py>(py: Python<'py>, p: &std::path::Path) -> PyResult<B<'py>> {
    py.import("pathlib")?.getattr("Path")?.call1((p,))
}

#[cfg(feature = "python")]
/// `normalize_commit(value)` on a Python value (None/'' -> '').
fn normalize_commit_py(value: &B<'_>) -> PyResult<String> {
    if !value.is_truthy()? {
        return Ok(String::new());
    }
    let s = value.cast::<PyString>().map_err(|_| PyAttributeError::new_err("object has no attribute 'find'"))?;
    Ok(core::normalize_commit(s.to_str()?))
}

#[cfg(feature = "python")]
fn uuid5_py(namespace: &B<'_>, name: &str) -> PyResult<String> {
    let ns: String = namespace.str()?.extract()?;
    core::uuid5(&ns, name).map_err(PyValueError::new_err)
}

#[cfg(feature = "python")]
/// `{**mapping, key: value}`
fn dict_with<'py>(mapping: &B<'py>, key: &str, value: B<'py>) -> PyResult<B<'py>> {
    let d = PyDict::new(mapping.py());
    d.call_method1("update", (mapping,))?;
    d.set_item(key, value)?;
    Ok(d.into_any())
}

#[cfg(feature = "python")]
fn is_word_char(py: Python<'_>, c: char) -> bool {
    if c.is_ascii() {
        return c == '_' || c.is_ascii_alphanumeric();
    }
    PyString::new(py, c.encode_utf8(&mut [0u8; 4])).call_method0("isalnum").and_then(|v| v.is_truthy()).unwrap_or(false)
}

// ------------------------------------------------------------------ Engine

#[cfg(feature = "python")]
#[pyclass(subclass, dict, module = "samsung_notes.engine", name = "Engine")]
pub struct Engine {}

macro_rules! g {
    ($o:expr, $name:expr) => {
        $o.getattr($name)?
    };
}

#[cfg(feature = "python")]
#[pymethods]
#[allow(non_upper_case_globals)]
impl Engine {
    #[cfg(feature = "python")]
    #[classattr]
    const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
    #[cfg(feature = "python")]
    #[classattr]
    const DOWNLOAD_WORKERS: usize = 4;

    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Engine {}
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (directory, *, offline=None, session=None, auto_sync=None, api_factory=None, interval=None, **extra))]
    fn __init__<'py>(
        slf: &Bound<'py, Self>,
        directory: B<'py>,
        offline: Option<B<'py>>,
        session: Option<B<'py>>,
        auto_sync: Option<B<'py>>,
        api_factory: Option<B<'py>>,
        interval: Option<B<'py>>,
        extra: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let s = slf.as_any();
        let offline = offline.unwrap_or_else(|| bool_obj(py, false));
        let auto_sync = auto_sync.unwrap_or_else(|| bool_obj(py, true));
        let interval = interval.unwrap_or_else(|| int_obj(py, 20));
        let mut metadata_factory = None;
        if let Some(extra) = extra {
            for (k, v) in extra.iter() {
                if k.eq("metadata_factory")? {
                    metadata_factory = Some(v);
                } else {
                    return Err(PyTypeError::new_err(format!("Engine.__init__() got an unexpected keyword argument {}", k.repr()?)));
                }
            }
        }
        let store = area(py, "samsung_notes.storage", "Store")?.call1((directory,))?;
        s.setattr("store", &store)?;
        let sessions = rs_common::py_fallback(py, "samsung_notes.session", "SessionManager")?.call(
            (&store,),
            Some(&kw(py, &[("offline", offline.clone()), ("session", session.unwrap_or_else(|| none(py)))])?),
        )?;
        s.setattr("sessions", &sessions)?;
        sessions.setattr("on_connected", s.getattr("request_sync")?)?;
        s.setattr("auto_sync", &auto_sync)?;
        let builtins = py.import("builtins")?;
        s.setattr("interval", builtins.call_method1("max", (10, interval))?)?;
        let api_factory = match api_factory {
            Some(f) => f,
            None => area(py, "samsung_notes.cloud_api", "CloudAPI")?,
        };
        s.setattr("api_factory", api_factory)?;
        s.setattr("api_instance", none(py))?;
        s.setattr("api_session", none(py))?;
        let metadata_factory = match metadata_factory {
            Some(f) => f,
            None => area(py, "samsung_notes.cloud_metadata", "CloudMetadata")?,
        };
        s.setattr("metadata_factory", metadata_factory)?;
        let threading = py.import("threading")?;
        s.setattr("lock", threading.call_method0("RLock")?)?;
        s.setattr("sync_lock", threading.call_method0("Lock")?)?;
        s.setattr("wake", threading.call_method0("Event")?)?;
        s.setattr("stop", threading.call_method0("Event")?)?;
        s.setattr("state", if offline.is_truthy()? { "offline" } else { "idle" })?;
        s.setattr("error", none(py))?;
        s.setattr("processed", 0)?;
        s.setattr("total", none(py))?;
        s.call_method0("_bootstrap_cache")?;
        s.setattr("previews", area(py, "samsung_notes.previews", "PreviewManager")?.call1((s,))?)?;
        s.setattr("warmup", none(py))?;
        s.setattr("thumbnails", area(py, "samsung_notes.thumbnails", "ThumbnailManager")?.call1((s,))?)?;
        let worker = threading.getattr("Thread")?.call(
            (),
            Some(&kw(py, &[("target", s.getattr("_loop")?), ("name", PyString::new(py, "notes-sync").into_any()), ("daemon", bool_obj(py, true))])?),
        )?;
        s.setattr("worker", &worker)?;
        worker.call_method0("start")?;
        if auto_sync.is_truthy()? && g!(sessions, "path").is_truthy()? {
            s.call_method0("request_sync")?;
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _api<'py>(slf: &Bound<'py, Self>) -> PyResult<B<'py>> {
        let py = slf.py();
        let s = slf.as_any();
        let sessions = g!(s, "sessions");
        let path = g!(sessions, "path");
        if path.is_none() || g!(sessions, "offline").is_truthy()? {
            return Err(store_error(py, "not_connected", "Samsung hesabına giriş yap. Yerel notların korunuyor.", 401));
        }
        with_cm(g!(s, "lock"), |_| {
            let instance = g!(s, "api_instance");
            if instance.is_none() || g!(s, "api_session").ne(&path)? {
                let cloud = g!(g!(s, "store"), "directory").call_method1("__truediv__", ("cloud",))?;
                s.setattr("api_instance", g!(s, "api_factory").call1((&path, cloud))?)?;
                s.setattr("api_session", &path)?;
            }
            Ok(g!(s, "api_instance"))
        })
    }

    #[cfg(feature = "python")]
    fn status<'py>(slf: &Bound<'py, Self>) -> PyResult<B<'py>> {
        let py = slf.py();
        let s = slf.as_any();
        let mut connected = false;
        let sessions = g!(s, "sessions");
        if g!(sessions, "path").is_truthy()? && !g!(sessions, "offline").is_truthy()? {
            let r = (|| -> PyResult<bool> {
                let auth = s.call_method0("_api")?.call_method0("auth_status")?;
                let valid = auth.get_item("valid")?;
                if valid.is_truthy()? {
                    return Ok(true);
                }
                auth.get_item("renewable")?.is_truthy()
            })();
            match r {
                Ok(v) => connected = v,
                Err(e) if is_api_error(py, &e) || is_io_value(py, &e) => {}
                Err(e) => return Err(e),
            }
        }
        let store = g!(s, "store");
        let counts = store.call_method0("counts")?;
        with_cm(g!(s, "lock"), |_| {
            let sync = PyDict::new(py);
            sync.set_item("state", g!(s, "state"))?;
            sync.set_item("lastSyncedAt", store.call_method1("meta", ("last_synced_at",))?)?;
            sync.set_item("pending", counts.get_item("pending")?)?;
            sync.set_item("conflicts", counts.get_item("conflicts")?)?;
            let progress = PyDict::new(py);
            progress.set_item("processed", g!(s, "processed"))?;
            progress.set_item("total", g!(s, "total"))?;
            sync.set_item("progress", progress)?;
            sync.set_item("error", g!(s, "error"))?;
            let caps = PyDict::new(py);
            caps.set_item("textEditing", true)?;
            caps.set_item("richTextEditing", true)?;
            caps.set_item("cloudDelete", false)?;
            caps.set_item("attachments", false)?;
            for key in ["cloudTrash", "cloudFavorites", "cloudFolders"] {
                caps.set_item(key, store.call_method1("meta", ("folder_initialized", false))?)?;
            }
            let out = PyDict::new(py);
            out.set_item("connected", connected)?;
            out.set_item("accountLabel", if connected { PyString::new(py, "Samsung hesabı").into_any() } else { none(py) })?;
            out.set_item("loginPending", g!(sessions, "pending"))?;
            out.set_item("loginError", g!(sessions, "error"))?;
            out.set_item("sync", sync)?;
            out.set_item("capabilities", caps)?;
            Ok(out.into_any())
        })
    }

    #[cfg(feature = "python")]
    fn request_sync(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let s = slf.as_any();
        let sessions = g!(s, "sessions");
        if g!(sessions, "offline").is_truthy()? || !g!(sessions, "path").is_truthy()? {
            return Ok(false);
        }
        let wake = g!(s, "wake");
        let already = wake.call_method0("is_set")?.is_truthy()? || g!(s, "state").eq("syncing")?;
        wake.call_method0("set")?;
        Ok(!already)
    }

    #[cfg(feature = "python")]
    fn create<'py>(slf: &Bound<'py, Self>, title: B<'py>, body: B<'py>) -> PyResult<B<'py>> {
        let result = g!(slf, "store").call_method1("create", (title, body))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn save<'py>(slf: &Bound<'py, Self>, identity: B<'py>, title: B<'py>, body: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        let result = g!(slf, "store").call_method1("save", (identity, title, body, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn organize<'py>(slf: &Bound<'py, Self>, identity: B<'py>, changes: B<'py>) -> PyResult<B<'py>> {
        let store = g!(slf, "store");
        if !store.call_method1("meta", ("folder_initialized", false))?.is_truthy()? {
            return Err(store_error(slf.py(), "metadata_not_ready", "Önce bulut kütüphanesinin indirilmesini bekle.", 409));
        }
        let result = store.call_method1("organize_cloud", (identity, changes))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn folder_change<'py>(slf: &Bound<'py, Self>, identity: B<'py>, changes: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let store = g!(slf, "store");
        if !store.call_method1("meta", ("folder_initialized", false))?.is_truthy()? {
            return Err(store_error(py, "metadata_not_ready", "Önce bulut klasörlerinin indirilmesini bekle.", 409));
        }
        let result = if identity.is_none() {
            // set(changes) <= {'name', 'parentId'}
            let keys = py.import("builtins")?.getattr("set")?.call1((&changes,))?;
            let allowed = PySet::new(py, ["name", "parentId"])?;
            if !keys.call_method1("issubset", (allowed,))?.is_truthy()? {
                return Err(store_error(py, "invalid_change", "Geçersiz klasör isteği.", 400));
            }
            let name = changes.call_method1("get", ("name",))?;
            let parent = changes.call_method1("get", ("parentId",))?;
            store.call_method1("create_folder", (name, parent))?
        } else {
            store.call_method1("update_folder", (identity, changes))?
        };
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn resolve_folder<'py>(slf: &Bound<'py, Self>, identity: B<'py>, strategy: B<'py>) -> PyResult<B<'py>> {
        if strategy.ne("use_remote")? {
            return Err(store_error(slf.py(), "invalid_strategy", "Buluttaki klasör durumunu yeniden al.", 409));
        }
        let api = slf.call_method0("_api")?;
        let inventory = g!(slf, "metadata_factory").call1((api,))?.call_method0("inventory")?;
        g!(slf, "store").call_method1("accept_remote_folder", (identity, inventory))
    }

    #[cfg(feature = "python")]
    fn get<'py>(slf: &Bound<'py, Self>, identity: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let s = slf.as_any();
        let store = g!(s, "store");
        let mut note = store.call_method1("get", (&identity,))?;
        if !note.get_item("contentLoaded")?.is_truthy()? && note.get_item("source")?.eq("cloud")? && g!(g!(s, "sessions"), "path").is_truthy()? {
            let row = store.call_method1("raw", (&identity,))?;
            let server_id = row.get_item("server_id")?;
            let fetched = (|| -> PyResult<()> {
                let snapshot = s.call_method0("_api")?.call_method1("get_note", (&server_id,))?;
                store.call_method1("apply_document", (&server_id, snapshot.call_method0("to_dict")?))?;
                Ok(())
            })();
            if let Err(e) = fetched {
                if !is_api_error(py, &e) {
                    return Err(e);
                }
                store.call_method1("content_error", (&identity, err_attr(py, &e, "message")?))?;
            }
            note = store.call_method1("get", (&identity,))?;
        }
        // Polling an open note must not deserialize its potentially large
        // binary snapshot. Inspect it only once for each loaded Cloud version.
        let needs_capability = with_cm(store.call_method0("connect")?, |db| {
            db.call_method1(
                "execute",
                ("SELECT loaded=1 AND base_document IS NOT NULL\n                AND title_editable IS NULL FROM notes WHERE id=?", (&identity,)),
            )?
            .call_method0("fetchone")?
            .get_item(0)
        })?;
        if needs_capability.is_truthy()? {
            let row = store.call_method1("raw", (&identity,))?;
            let editor_projection = area(py, "samsung_notes.editor_document", "editor_projection")?;
            let document = json_loads(&row.get_item("base_document")?)?;
            let probed = (|| -> PyResult<B<'py>> {
                let snapshot = area(py, "samsung_notes.document", "DocumentSnapshot")?.call_method1("from_dict", (&document,))?;
                let projection = editor_projection.call1((snapshot,))?;
                let supported = projection.get_item("capabilities")?.get_item("titleEdit")?;
                let state = row.get_item("sync_state")?;
                if state.eq("synced")? || state.eq("local")? {
                    store.call_method1("cache_editor", (&identity, note.get_item("revision")?, &projection))?;
                }
                Ok(supported)
            })();
            let supported = match probed {
                Ok(v) => v,
                Err(e) if is_document_error(py, &e) || e.is_instance_of::<PyValueError>(py) || e.is_instance_of::<PyKeyError>(py) || e.is_instance_of::<PyTypeError>(py) => {
                    bool_obj(py, false)
                }
                Err(e) => return Err(e),
            };
            store.call_method(
                "title_capability",
                (&identity, supported),
                Some(&kw(py, &[("raw_sha256", document.call_method1("get", ("raw_sha256",))?)])?),
            )?;
            note = store.call_method1("get", (&identity,))?;
        }
        Ok(note)
    }

    #[cfg(feature = "python")]
    fn save_title<'py>(slf: &Bound<'py, Self>, identity: B<'py>, title: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("get", (&identity,))?; // Establish operation-specific capability once.
        let result = g!(slf, "store").call_method1("save_title", (identity, title, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    /// Save as file into the user's Downloads folder (desktop app: no browser download).
    fn export_file<'py>(slf: &Bound<'py, Self>, identity: B<'py>, fmt: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let note = slf.call_method1("get", (&identity,))?;
        let fmt = if fmt.eq("pdf")? {
            "pdf"
        } else if fmt.eq("txt")? {
            "txt"
        } else {
            return Err(store_error(py, "invalid_format", "Dosya türü desteklenmiyor.", 400));
        };
        let home = crate::server::home_dir();
        let folder = py.detach(|| core::download_folder(&home));
        let title_obj = note.get_item("title")?;
        let title_or = if title_obj.is_truthy()? { title_obj.clone() } else { PyString::new(py, "Not").into_any() };
        let title_text: String = title_or.cast::<PyString>().map_err(|_| PyTypeError::new_err("expected string or bytes-like object"))?.to_str()?.to_owned();
        let replaced: String = title_text.chars().map(|c| if is_word_char(py, c) || " .()-".contains(c) { c } else { '_' }).collect();
        let base: String = core::py_strip(&replaced).chars().take(80).collect();
        let base = if base.is_empty() { "Not".to_string() } else { base };
        let target = core::free_target(&folder, &base, fmt);
        let result = PyDict::new(py);
        if fmt == "txt" {
            let title: String = note.get_item("title")?.cast_into::<PyString>().map_err(|_| PyTypeError::new_err("title"))?.to_str()?.to_owned();
            let body: String = note.get_item("body")?.cast_into::<PyString>().map_err(|_| PyTypeError::new_err("body"))?.to_str()?.to_owned();
            std::fs::write(&target, format!("{}\n\n{}", title, body)).map_err(PyErr::from)?;
            result.set_item("path", target.to_string_lossy().into_owned())?;
            result.set_item("name", target.file_name().unwrap().to_string_lossy().into_owned())?;
            result.set_item("pages", 0)?;
            result.set_item("skipped", 0)?;
            return Ok(result.into_any());
        }
        if core::which("magick").is_none() {
            return Err(store_error(py, "pdf_unavailable", "PDF oluşturmak için ImageMagick gerekli.", 503));
        }
        let previews = g!(slf, "previews");
        let mut pages = Vec::new();
        for page in previews.call_method1("manifest", (&identity,))?.get_item("pages")?.try_iter()? {
            let page = page?;
            if page.call_method1("get", ("available",))?.is_truthy()? {
                pages.push(page);
            }
        }
        if pages.is_empty() {
            return Err(store_error(py, "no_pages", "Bu notta dışa aktarılabilecek sayfa görüntüsü yok.", 422));
        }
        let work = core::TempDir::new("notes-export-").map_err(PyErr::from)?;
        let (mut files, mut skipped) = (Vec::new(), 0);
        for page in pages {
            let index = page.get_item("index")?;
            let raw = previews.call_method(
                "request",
                (&identity, &index),
                Some(&kw(py, &[("priority", int_obj(py, 0)), ("wait", int_obj(py, 90))])?),
            )?;
            if raw.is_none() {
                skipped += 1;
                continue;
            }
            let label: String = PyString::new(py, "{:05d}").call_method1("format", (&index,))?.extract()?;
            let path = work.0.join(format!("{}.png", label));
            std::fs::write(&path, raw.extract::<Vec<u8>>()?).map_err(PyErr::from)?;
            files.push(path);
        }
        if files.is_empty() {
            return Err(store_error(py, "no_pages", "Sayfa görüntüleri hazırlanamadı.", 503));
        }
        let made = py.detach(|| core::images_to_pdf(&files, &target));
        if !made {
            return Err(store_error(py, "pdf_failed", "PDF oluşturulamadı.", 500));
        }
        drop(work);
        result.set_item("path", target.to_string_lossy().into_owned())?;
        result.set_item("name", target.file_name().unwrap().to_string_lossy().into_owned())?;
        result.set_item("pages", files.len())?;
        result.set_item("skipped", skipped)?;
        Ok(result.into_any())
    }

    #[cfg(feature = "python")]
    fn page_ops<'py>(slf: &Bound<'py, Self>, identity: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("get", (&identity,))?;
        g!(slf, "store").call_method1("page_ops", (identity,))
    }

    #[cfg(feature = "python")]
    /// Shapes DTO; addShape only on a synced note without pending work.
    fn shapes<'py>(slf: &Bound<'py, Self>, identity: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let project_shapes = area(py, "samsung_notes.page_shapes", "project_shapes")?;
        let note = slf.call_method1("get", (&identity,))?;
        let store = g!(slf, "store");
        let row = store.call_method1("raw", (&identity,))?;
        let base = row.get_item("base_document")?;
        if !base.is_truthy()? {
            let p = PyDict::new(py);
            p.set_item("schemaVersion", 1)?;
            p.set_item("kinds", PyList::empty(py))?;
            p.set_item("pages", PyList::empty(py))?;
            p.set_item("reason", "Notun çizim verisi henüz hazır değil.")?;
            let rendering = PyDict::new(py);
            rendering.set_item("nativeEquivalent", false)?;
            rendering.set_item("mode", "geometry-only")?;
            p.set_item("rendering", rendering)?;
            p.set_item("revision", note.get_item("revision")?)?;
            return Ok(p.into_any());
        }
        let mut projection = project_shapes.call1((json_loads(&base)?,))?;
        let blocked = note.get_item("trashed")?.is_truthy()?
            || store.call_method1("raw_state", (&identity,))?.get_item("deleted_remote")?.is_truthy()?
            || note.get_item("syncState")?.ne("synced")?
            || note.get_item("metadataSync")?.get_item("state")?.ne("synced")?;
        if blocked {
            let pages = PyList::empty(py);
            for page in projection.get_item("pages")?.try_iter()? {
                pages.append(dict_with(&page?, "addShape", bool_obj(py, false))?)?;
            }
            projection.set_item("pages", pages)?;
        }
        projection = dict_with(&projection, "revision", note.get_item("revision")?)?;
        Ok(projection)
    }

    #[cfg(feature = "python")]
    fn save_page_op<'py>(slf: &Bound<'py, Self>, identity: B<'py>, op: B<'py>, spec: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("get", (&identity,))?;
        let result = g!(slf, "store").call_method1("save_page_op", (identity, op, spec, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn page_style<'py>(slf: &Bound<'py, Self>, identity: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("get", (&identity,))?;
        g!(slf, "store").call_method1("page_style", (identity,))
    }

    #[cfg(feature = "python")]
    fn save_page_background<'py>(slf: &Bound<'py, Self>, identity: B<'py>, color: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("get", (&identity,))?;
        let result = g!(slf, "store").call_method1("save_page_background", (identity, color, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn _pdf_staging<'py>(slf: &Bound<'py, Self>) -> PyResult<B<'py>> {
        let py = slf.py();
        let directory: PathBuf = g!(g!(slf, "store"), "directory").extract()?;
        match core::private_directory(&directory.join("pdf-uploads")) {
            Ok(p) => path_of(py, &p),
            Err(Some(e)) => Err(PyErr::from(e)),
            Err(None) => Err(PyValueError::new_err(core::PRIVATE_DIRECTORY_ERROR)),
        }
    }

    #[cfg(feature = "python")]
    /// Insert a local PDF file's pages at the end of a note (queued Cloud write).
    #[pyo3(signature = (identity, pdf, file_name, revision, *, page_indices=None, template_type=None))]
    fn add_pdf<'py>(
        slf: &Bound<'py, Self>,
        identity: B<'py>,
        pdf: B<'py>,
        file_name: B<'py>,
        revision: B<'py>,
        page_indices: Option<B<'py>>,
        template_type: Option<B<'py>>,
    ) -> PyResult<B<'py>> {
        let py = slf.py();
        slf.call_method1("get", (&identity,))?;
        let limit: u64 = g!(slf, "MAX_PDF_BYTES").extract()?;
        let data: Vec<u8> = match pdf.cast::<PyBytes>() {
            Ok(b) if b.as_bytes().starts_with(b"%PDF-") && b.as_bytes().len() as u64 <= limit => b.as_bytes().to_vec(),
            _ => return Err(store_error(py, "invalid_pdf", "Dosya PDF değil ya da çok büyük.", 400)),
        };
        let raw_name: String = if file_name.is_truthy()? { file_name.str()?.to_str()?.to_owned() } else { String::new() };
        let name = core::pdf_file_name(&raw_name);
        let sha = core::sha256_hex(&data);
        let staging: PathBuf = slf.call_method0("_pdf_staging")?.extract()?;
        let target = staging.join(format!("{}.pdf", sha));
        py.detach(|| core::stage_file(&target, &data)).map_err(PyErr::from)?;
        let sizes = match area(py, "samsung_notes.page_template", "pdf_page_sizes")?.call1((path_of(py, &target)?,)) {
            Ok(v) => v,
            Err(e) if is_document_error(py, &e) => {
                let message = e.value(py).str()?.to_string();
                return Err(store_error(py, "invalid_pdf", &message, 422));
            }
            Err(e) => return Err(e),
        };
        let kwargs = kw(
            py,
            &[
                ("file_sha256", PyString::new(py, &sha).into_any()),
                ("file_name", PyString::new(py, &name).into_any()),
                ("page_sizes", sizes),
                ("page_indices", page_indices.unwrap_or_else(|| none(py))),
                ("template_type", template_type.unwrap_or_else(|| none(py))),
            ],
        )?;
        let result = g!(slf, "store").call_method("save_pdf_pages", (identity, revision), Some(&kwargs))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    /// Note style templates already used in this library (PDF pages with templateType).
    ///
    /// Scans only the stored resource XML and remembers each note's result per
    /// revision in `_template_cache`. With `note_id`, templates whose PDF that
    /// note already holds are marked `inNote`.
    #[pyo3(signature = (note_id=None))]
    fn templates<'py>(slf: &Bound<'py, Self>, note_id: Option<B<'py>>) -> PyResult<B<'py>> {
        let py = slf.py();
        let note_id = note_id.unwrap_or_else(|| none(py));
        let rows: Vec<B<'py>> = with_cm(g!(slf, "store").call_method0("connect")?, |db| {
            db.call_method1(
                "execute",
                ("SELECT id,revision,json_extract(base_document,'$.resource_xml') AS xml FROM notes WHERE base_document IS NOT NULL AND trashed=0 AND deleted_remote=0",),
            )?
            .call_method0("fetchall")?
            .try_iter()?
            .collect::<PyResult<Vec<_>>>()
        })?;
        let cache = g!(slf, "__dict__").call_method1("setdefault", ("_template_cache", PyDict::new(py)))?;
        let found = PyDict::new(py);
        let mut files_in_note = PySet::empty(py)?.into_any();
        for row in rows {
            let id = row.get_item("id")?;
            let revision = row.get_item("revision")?;
            let mut cached = cache.call_method1("get", (&id,))?;
            if cached.is_none() || cached.get_item(0)?.ne(&revision)? {
                let scanned = slf.call_method1("_scan_templates", (row.get_item("xml")?,))?;
                let mut items = vec![revision.clone()];
                for item in scanned.try_iter()? {
                    items.push(item?);
                }
                cached = PyTuple::new(py, items)?.into_any();
                cache.set_item(&id, &cached)?;
            }
            let unpacked: Vec<B<'py>> = cached.try_iter()?.collect::<PyResult<_>>()?;
            let [_, entries, file_ids] = <[B<'py>; 3]>::try_from(unpacked).map_err(|_| PyValueError::new_err("expected 3 values to unpack"))?;
            if id.eq(&note_id)? {
                files_in_note = file_ids;
            }
            for entry in entries.try_iter()? {
                let entry = entry?;
                found.call_method1("setdefault", (entry.get_item("id")?, dict_with(&entry, "sourceNoteId", id.clone())?))?;
            }
        }
        let key = py.import("operator")?.call_method1("itemgetter", ("name",))?;
        let mut result = py.import("builtins")?.call_method("sorted", (found.call_method0("values")?,), Some(&kw(py, &[("key", key)])?))?;
        if !note_id.is_none() {
            let marked = PyList::empty(py);
            for t in result.try_iter()? {
                let t = t?;
                let in_note = files_in_note.contains(t.get_item("id")?)?;
                marked.append(dict_with(&t, "inNote", bool_obj(py, in_note))?)?;
            }
            result = marked.into_any();
        }
        let out = PyDict::new(py);
        out.set_item("templates", result)?;
        Ok(out.into_any())
    }

    #[cfg(feature = "python")]
    /// `(entries, frozenset(contentFile ids))` for one note's base64 resource XML.
    #[staticmethod]
    fn _scan_templates<'py>(py: Python<'py>, resource_b64: B<'py>) -> PyResult<B<'py>> {
        let empty = || -> PyResult<B<'py>> {
            Ok(PyTuple::new(py, [PyTuple::empty(py).into_any(), pyo3::types::PyFrozenSet::empty(py)?.into_any()])?.into_any())
        };
        // base64.b64decode(resource_b64 or ''): str (ASCII) or bytes; anything else is TypeError.
        let text: String = if !resource_b64.is_truthy()? {
            String::new()
        } else if let Ok(s) = resource_b64.cast::<PyString>() {
            match s.to_str() {
                Ok(t) => t.to_owned(),
                Err(_) => return empty(),
            }
        } else if let Ok(b) = resource_b64.cast::<PyBytes>() {
            match std::str::from_utf8(b.as_bytes()) {
                Ok(t) if t.is_ascii() => t.to_owned(),
                _ => return empty(),
            }
        } else {
            return empty();
        };
        let Ok(xml) = core::b64decode_lenient(&text) else { return empty() };
        let (groups, ids) = match core::scan_templates(&xml) {
            core::TemplateScan::Skip => return empty(),
            core::TemplateScan::Error(m) => return Err(PyValueError::new_err(m)),
            core::TemplateScan::Found(g, ids) => (g, ids),
        };
        let mut entries = Vec::new();
        for group in groups {
            let Some(file_name) = group.file_name.clone() else { continue };
            if !core::py_isdigit(&group.template_type) {
                continue;
            }
            let template_type = core::py_int(&group.template_type)
                .ok_or_else(|| PyValueError::new_err(format!("invalid literal for int() with base 10: '{}'", group.template_type)))?;
            let entry = PyDict::new(py);
            entry.set_item("id", group.sha.clone())?;
            entry.set_item("name", core::template_display_name(&file_name))?;
            entry.set_item("fileName", &file_name)?;
            entry.set_item("templateType", template_type)?;
            entry.set_item("pageIndices", group.indices.clone())?;
            entry.set_item("pageCount", group.indices.len())?;
            entries.push(entry.into_any());
        }
        let ids = pyo3::types::PyFrozenSet::new(py, ids.iter().map(|i| i.as_deref()))?;
        Ok(PyTuple::new(py, [PyTuple::new(py, entries)?.into_any(), ids.into_any()])?.into_any())
    }

    #[cfg(feature = "python")]
    /// Append a known template's pages; the PDF comes from the Cloud copy of the source note.
    fn apply_template<'py>(slf: &Bound<'py, Self>, identity: B<'py>, template_id: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let payload_from_snapshot = area(py, "samsung_notes.previews", "_payload_from_snapshot")?;
        // PdfBackgroundError is an exception class: imported from its Python module.
        let pdf_background = py.import("samsung_notes.pdf_background")?;
        let inspect = area(py, "samsung_notes.pdf_background", "inspect_pdf_backgrounds")?;
        let mut template = None;
        for t in slf.call_method0("templates")?.get_item("templates")?.try_iter()? {
            let t = t?;
            if t.get_item("id")?.eq(&template_id)? {
                template = Some(t);
                break;
            }
        }
        let Some(template) = template else {
            return Err(store_error(py, "template_not_found", "Şablon bulunamadı.", 404));
        };
        let store = g!(slf, "store");
        let source = store.call_method1("raw", (template.get_item("sourceNoteId")?,))?;
        let candidate = (|| -> PyResult<Option<B<'py>>> {
            let payload = payload_from_snapshot.call1((json_loads(&source.get_item("base_document")?)?,))?;
            for c in inspect.call1((payload,))?.try_iter()? {
                let c = c?;
                if c.getattr("sha256")?.eq(&template_id)? {
                    return Ok(Some(c));
                }
            }
            Ok(None)
        })();
        let unavailable = || store_error(py, "template_unavailable", "Şablon dosyası bu sürümde okunamadı.", 422);
        let candidate = match candidate {
            Ok(Some(c)) => c,
            Ok(None) => return Err(unavailable()),
            Err(e) if e.value(py).is_instance(&pdf_background.getattr("PdfBackgroundError")?)? => return Err(unavailable()),
            Err(e) => return Err(e),
        };
        let directory = g!(g!(slf, "previews"), "directory").call_method1("__truediv__", ("pdf-files",))?;
        let account = store.call_method1("meta", ("account_key", "offline"))?;
        let mut path = area(py, "samsung_notes.pdf_background", "cached_pdf")?.call(
            (&candidate, &directory),
            Some(&kw(py, &[("account", account.clone())])?),
        )?;
        if path.is_none() {
            let sessions = g!(slf, "sessions");
            if !g!(sessions, "path").is_truthy()? || g!(sessions, "offline").is_truthy()? {
                return Err(store_error(py, "template_offline", "Şablon dosyası henüz indirilmemiş; bağlantı gerekli.", 503));
            }
            let api = slf.call_method0("_api")?;
            path = match area(py, "samsung_notes.pdf_background", "fetch_pdf")?.call(
                (&candidate, api, &directory),
                Some(&kw(py, &[("account", account)])?),
            ) {
                Ok(p) => p,
                Err(e) if e.value(py).is_instance(&pdf_background.getattr("PdfBackgroundError")?)? => {
                    let message: String = err_attr(py, &e, "message")?.extract()?;
                    return Err(store_error(py, "template_unavailable", &message, 503));
                }
                Err(e) => return Err(e),
            };
        }
        let path: PathBuf = path.extract()?;
        let data = std::fs::read(&path).map_err(PyErr::from)?;
        let kwargs = kw(py, &[("page_indices", template.get_item("pageIndices")?), ("template_type", template.get_item("templateType")?)])?;
        slf.call_method("add_pdf", (identity, PyBytes::new(py, &data), template.get_item("fileName")?, revision), Some(&kwargs))
    }

    #[cfg(feature = "python")]
    fn editor<'py>(slf: &Bound<'py, Self>, identity: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let note = slf.call_method1("get", (&identity,))?;
        let store = g!(slf, "store");
        let revision = note.get_item("revision")?;
        let mut projection = store.call_method1("cached_editor", (&identity, &revision))?;
        if projection.is_none() {
            let row = store.call_method1("raw", (&identity,))?;
            let sync_state = row.get_item("sync_state")?;
            let dirty = !(sync_state.eq("synced")? || sync_state.eq("local")?);
            let prior = if dirty { store.call_method1("cached_editor", (&identity,))? } else { none(py) };
            let prior_ok = prior.is_truthy()?
                && prior.get_item("capabilities")?.get_item("textEdit")?.is_truthy()?
                && !prior.call_method1("get", ("model",))?.is_none()
                && prior.get_item("model")?.get_item("text")?.eq(row.get_item("body")?)?;
            let base = row.get_item("base_document")?;
            if prior_ok {
                // Organization-only revision changes must not discard a
                // newer local rich model while an older job is in flight.
                projection = prior;
            } else if row.get_item("editable")?.is_truthy()? && (dirty || !base.is_truthy()?) {
                // A plain PUT may have changed this body after the last
                // projection. Never bind the old Cloud body to its new revision.
                let normalize_model = area(py, "samsung_notes.editor_document", "normalize_model")?;
                let model = PyDict::new(py);
                model.set_item("schemaVersion", 1)?;
                model.set_item("text", note.get_item("body")?)?;
                model.set_item("spans", PyList::empty(py))?;
                model.set_item("paragraphs", PyList::empty(py))?;
                let caps = PyDict::new(py);
                caps.set_item("titleEdit", true)?;
                caps.set_item("textEdit", true)?;
                caps.set_item("richStyle", true)?;
                caps.set_item("inkEdit", false)?;
                let p = PyDict::new(py);
                p.set_item("schemaVersion", 1)?;
                p.set_item("capabilities", caps)?;
                p.set_item("reason", none(py))?;
                p.set_item("model", normalize_model.call1((model,))?)?;
                projection = p.into_any();
            } else if base.is_truthy()? {
                let editor_projection = area(py, "samsung_notes.editor_document", "editor_projection")?;
                let snapshot = area(py, "samsung_notes.document", "DocumentSnapshot")?.call_method1("from_dict", (json_loads(&base)?,))?;
                projection = editor_projection.call1((snapshot,))?;
                let caps = projection.get_item("capabilities")?;
                if dirty && caps.get_item("textEdit")?.is_truthy()? {
                    caps.set_item("textEdit", false)?;
                    caps.set_item("richStyle", false)?;
                    projection.set_item("reason", "Yerel düzenlemenin biçim bilgisi eksik; bulut metni düzenlemenin yerine kullanılmadı.")?;
                }
            } else {
                let caps = PyDict::new(py);
                for k in ["titleEdit", "textEdit", "richStyle", "inkEdit"] {
                    caps.set_item(k, false)?;
                }
                let p = PyDict::new(py);
                p.set_item("schemaVersion", 1)?;
                p.set_item("capabilities", caps)?;
                p.set_item("model", none(py))?;
                p.set_item("reason", "Not içeriği henüz hazır değil.")?;
                projection = p.into_any();
            }
            store.call_method1("cache_editor", (&identity, &revision, &projection))?;
        }
        if note.get_item("trashed")?.is_truthy()? || store.call_method1("raw_state", (&identity,))?.get_item("deleted_remote")?.is_truthy()? {
            let caps = PyDict::new(py);
            for key in projection.get_item("capabilities")?.try_iter()? {
                caps.set_item(key?, false)?;
            }
            projection = dict_with(&projection, "capabilities", caps.into_any())?;
        }
        dict_with(&projection, "revision", revision)
    }

    #[cfg(feature = "python")]
    fn create_editor<'py>(slf: &Bound<'py, Self>, title: B<'py>, model: B<'py>) -> PyResult<B<'py>> {
        let result = g!(slf, "store").call_method1("create_editor", (title, model))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn save_editor<'py>(slf: &Bound<'py, Self>, identity: B<'py>, title: B<'py>, model: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("editor", (&identity,))?;
        let result = g!(slf, "store").call_method1("save_editor", (identity, title, model, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn ink<'py>(slf: &Bound<'py, Self>, identity: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let project_ink = area(py, "samsung_notes.ink_projection", "project_ink")?;
        let recipe = area(py, "samsung_notes.ink_projection", "PROJECTION_RECIPE")?;
        let note = slf.call_method1("get", (&identity,))?;
        let store = g!(slf, "store");
        let revision = note.get_item("revision")?;
        let mut projection = store.call_method1("cached_ink", (&identity, &revision))?;
        if !projection.is_none()
            && projection.call_method1("get", ("recipe",))?.ne(&recipe)?
            && !projection.call_method1("get", ("pendingStrokes",))?.is_truthy()?
        {
            projection = none(py); // computed under older gate rules
        }
        if projection.is_none() {
            let row = store.call_method1("raw", (&identity,))?;
            let base = row.get_item("base_document")?;
            projection = if base.is_truthy()? {
                project_ink.call((json_loads(&base)?,), Some(&kw(py, &[("points", bool_obj(py, false))])?))?
            } else {
                let p = PyDict::new(py);
                p.set_item("schemaVersion", 1)?;
                p.set_item("pages", PyList::empty(py))?;
                let caps = PyDict::new(py);
                caps.set_item("appendInk", false)?;
                p.set_item("capabilities", caps)?;
                p.set_item("reason", "Notun çizim verisi henüz hazır değil.")?;
                let rendering = PyDict::new(py);
                rendering.set_item("nativeEquivalent", false)?;
                rendering.set_item("mode", "geometry-only")?;
                p.set_item("rendering", rendering)?;
                p.into_any()
            };
            let (pending, erase_job) = with_cm(store.call_method0("connect")?, |db| {
                let pending = db
                    .call_method1(
                        "execute",
                        ("SELECT ink_jobs.model FROM ink_jobs JOIN jobs ON jobs.id=ink_jobs.job_id\n                    WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') ORDER BY jobs.created_ms DESC LIMIT 1", (&identity,)),
                    )?
                    .call_method0("fetchone")?;
                let erase = db
                    .call_method1(
                        "execute",
                        ("SELECT ink_erase_jobs.model FROM ink_erase_jobs JOIN jobs ON jobs.id=ink_erase_jobs.job_id\n                    WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') ORDER BY jobs.created_ms DESC LIMIT 1", (&identity,)),
                    )?
                    .call_method0("fetchone")?;
                Ok((pending, erase))
            })?;
            let strokes = if pending.is_truthy()? { json_loads(&pending.get_item(0)?)?.get_item("strokes")? } else { PyList::empty(py).into_any() };
            projection.set_item("pendingStrokes", strokes)?;
            let erase_list = PyList::empty(py);
            if erase_job.is_truthy()? {
                for group in json_loads(&erase_job.get_item(0)?)?.get_item("groups")?.try_iter()? {
                    let group = group?;
                    let d = PyDict::new(py);
                    d.set_item("pageId", group.get_item("pageId")?)?;
                    d.set_item("strokeIds", group.get_item("strokeIds")?)?;
                    erase_list.append(d)?;
                }
            }
            projection.set_item("pendingErase", erase_list)?;
            let move_job = with_cm(store.call_method0("connect")?, |db| {
                db.call_method1(
                    "execute",
                    ("SELECT ink_move_jobs.model FROM ink_move_jobs JOIN jobs ON jobs.id=ink_move_jobs.job_id\n                    WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') ORDER BY jobs.created_ms DESC LIMIT 1", (&identity,)),
                )?
                .call_method0("fetchone")
            })?;
            if move_job.is_truthy()? {
                // key only present while a move waits for its Cloud write
                let move_list = PyList::empty(py);
                for group in json_loads(&move_job.get_item(0)?)?.get_item("groups")?.try_iter()? {
                    let group = group?;
                    let d = PyDict::new(py);
                    for name in ["pageId", "strokeIds", "dx", "dy"] {
                        d.set_item(name, group.get_item(name)?)?;
                    }
                    move_list.append(d)?;
                }
                projection.set_item("pendingMove", move_list)?;
            }
            projection.set_item("recipe", &recipe)?;
            store.call_method1("cache_ink", (&identity, &revision, &projection))?;
        }
        let blocked = note.get_item("trashed")?.is_truthy()?
            || store.call_method1("raw_state", (&identity,))?.get_item("deleted_remote")?.is_truthy()?
            || note.get_item("syncState")?.ne("synced")?
            || note.get_item("metadataSync")?.get_item("state")?.ne("synced")?;
        if blocked {
            // {key: False for key in projection['capabilities']}
            let caps = PyDict::new(py);
            for key in projection.get_item("capabilities")?.try_iter()? {
                caps.set_item(key?, false)?;
            }
            projection = dict_with(&projection, "capabilities", caps.into_any())?;
        }
        dict_with(&projection, "revision", revision)
    }

    #[cfg(feature = "python")]
    fn ink_draft<'py>(slf: &Bound<'py, Self>, identity: B<'py>) -> PyResult<B<'py>> {
        g!(slf, "store").call_method1("ink_draft", (identity,))
    }

    #[cfg(feature = "python")]
    fn save_ink_draft<'py>(slf: &Bound<'py, Self>, identity: B<'py>, revision: B<'py>, strokes: B<'py>) -> PyResult<B<'py>> {
        g!(slf, "store").call_method1("save_ink_draft", (identity, revision, strokes))
    }

    #[cfg(feature = "python")]
    fn save_ink<'py>(slf: &Bound<'py, Self>, identity: B<'py>, strokes: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("ink", (&identity,))?;
        let result = g!(slf, "store").call_method1("save_ink", (identity, strokes, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn append_ink_draft<'py>(slf: &Bound<'py, Self>, identity: B<'py>, revision: B<'py>, strokes: B<'py>) -> PyResult<B<'py>> {
        g!(slf, "store").call_method1("append_ink_draft", (identity, revision, strokes))
    }

    #[cfg(feature = "python")]
    fn save_ink_from_draft<'py>(slf: &Bound<'py, Self>, identity: B<'py>, strokes: B<'py>, revision: B<'py>, draft_ids: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("ink", (&identity,))?;
        let result = g!(slf, "store").call_method1("save_ink_from_draft", (identity, strokes, revision, draft_ids))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn _push_ink_erase<'py>(slf: &Bound<'py, Self>, api: B<'py>, row: B<'py>, job: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let s = slf.as_any();
        let erase_ink_strokes = area(py, "samsung_notes.cloud_ink_erase", "erase_ink_strokes")?;
        let snapshot_class = area(py, "samsung_notes.document", "DocumentSnapshot")?;
        let store = g!(s, "store");
        let unpacked: Vec<B<'py>> = store.call_method1("ink_erase_job", (&job,))?.try_iter()?.collect::<PyResult<_>>()?;
        let [model, cursor, snapshot] = <[B<'py>; 3]>::try_from(unpacked).map_err(|_| PyValueError::new_err("expected 3 values to unpack"))?;
        let mut snapshot = snapshot;
        let groups = model.get_item("groups")?;
        let start: i64 = cursor.extract()?;
        let count = groups.len()? as i64;
        let server_id = row.get_item("server_id")?;
        for index in start..count {
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))? {
                return Err(api_error(py, "cloud_unavailable", "Silme kaydedildi; eşitlemeye daha sonra devam edilecek.", false));
            }
            let group = groups.get_item(index)?;
            let operation_id = uuid5_py(&job.get_item("id")?, &format!("erase-{}", index))?;
            let result = erase_ink_strokes.call(
                (&api, &server_id, snapshot_class.call_method1("from_dict", (&snapshot,))?, group.get_item("pageId")?, group.get_item("layerId")?, group.get_item("strokeIds")?),
                Some(&kw(py, &[("operation_id", PyString::new(py, &operation_id).into_any())])?),
            )?;
            if !result.call_method1("get", ("verified",))?.is(&bool_obj(py, true)) {
                return Err(api_error(py, "write_uncertain", "Silmenin bulut kaydı henüz doğrulanamadı.", true));
            }
            snapshot = result.get_item("snapshot")?;
            store.call_method1("checkpoint_ink_erase", (&job, index, &snapshot))?;
        }
        let out = PyDict::new(py);
        out.set_item("verified", true)?;
        out.set_item("server_id", &server_id)?;
        out.set_item("local_id", snapshot.get_item("local_id")?)?;
        out.set_item("snapshot", &snapshot)?;
        Ok(out.into_any())
    }

    #[cfg(feature = "python")]
    fn _push_ink_move<'py>(slf: &Bound<'py, Self>, api: B<'py>, row: B<'py>, job: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let s = slf.as_any();
        let move_ink_strokes = area(py, "samsung_notes.cloud_ink_erase", "move_ink_strokes")?;
        let snapshot_class = area(py, "samsung_notes.document", "DocumentSnapshot")?;
        let store = g!(s, "store");
        let unpacked: Vec<B<'py>> = store.call_method1("ink_move_job", (&job,))?.try_iter()?.collect::<PyResult<_>>()?;
        let [model, cursor, snapshot] = <[B<'py>; 3]>::try_from(unpacked).map_err(|_| PyValueError::new_err("expected 3 values to unpack"))?;
        let mut snapshot = snapshot;
        let groups = model.get_item("groups")?;
        let start: i64 = cursor.extract()?;
        let count = groups.len()? as i64;
        let server_id = row.get_item("server_id")?;
        for index in start..count {
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))? {
                return Err(api_error(py, "cloud_unavailable", "Taşıma kaydedildi; eşitlemeye daha sonra devam edilecek.", false));
            }
            let group = groups.get_item(index)?;
            let operation_id = uuid5_py(&job.get_item("id")?, &format!("move-{}", index))?;
            let result = move_ink_strokes.call(
                (
                    &api,
                    &server_id,
                    snapshot_class.call_method1("from_dict", (&snapshot,))?,
                    group.get_item("pageId")?,
                    group.get_item("layerId")?,
                    group.get_item("strokeIds")?,
                    group.get_item("dx")?,
                    group.get_item("dy")?,
                ),
                Some(&kw(py, &[("operation_id", PyString::new(py, &operation_id).into_any())])?),
            )?;
            if !result.call_method1("get", ("verified",))?.is(&bool_obj(py, true)) {
                return Err(api_error(py, "write_uncertain", "Taşımanın bulut kaydı henüz doğrulanamadı.", true));
            }
            snapshot = result.get_item("snapshot")?;
            store.call_method1("checkpoint_ink_move", (&job, index, &snapshot))?;
        }
        let out = PyDict::new(py);
        out.set_item("verified", true)?;
        out.set_item("server_id", &server_id)?;
        out.set_item("local_id", snapshot.get_item("local_id")?)?;
        out.set_item("snapshot", &snapshot)?;
        Ok(out.into_any())
    }

    #[cfg(feature = "python")]
    fn save_ink_move<'py>(slf: &Bound<'py, Self>, identity: B<'py>, r#move: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("ink", (&identity,))?;
        let result = g!(slf, "store").call_method1("save_ink_move", (identity, r#move, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn save_ink_erase<'py>(slf: &Bound<'py, Self>, identity: B<'py>, erase: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        slf.call_method1("ink", (&identity,))?;
        let result = g!(slf, "store").call_method1("save_ink_erase", (identity, erase, revision))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn ink_geometry<'py>(slf: &Bound<'py, Self>, identity: B<'py>, page_id: B<'py>, revision: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let project_page_geometry = area(py, "samsung_notes.ink_projection", "project_page_geometry")?;
        if slf.call_method1("get", (&identity,))?.get_item("revision")?.ne(&revision)? {
            return Err(store_error(py, "revision_conflict", "Not değişti.", 409));
        }
        let row = g!(slf, "store").call_method1("raw", (&identity,))?;
        let base = row.get_item("base_document")?;
        if !base.is_truthy()? {
            return Err(store_error(py, "not_ready", "Notun çizim verisi henüz hazır değil.", 409));
        }
        project_page_geometry.call1((json_loads(&base)?, page_id))
    }

    #[cfg(feature = "python")]
    fn _push_ink_batch<'py>(slf: &Bound<'py, Self>, api: B<'py>, row: B<'py>, job: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let s = slf.as_any();
        let samples_from_points = area(py, "samsung_notes.ink_store", "samples_from_points")?;
        let snapshot_class = area(py, "samsung_notes.document", "DocumentSnapshot")?;
        let store = g!(s, "store");
        let unpacked: Vec<B<'py>> = store.call_method1("ink_job", (&job,))?.try_iter()?.collect::<PyResult<_>>()?;
        let [model, cursor, snapshot] = <[B<'py>; 3]>::try_from(unpacked).map_err(|_| PyValueError::new_err("expected 3 values to unpack"))?;
        let mut snapshot = snapshot;
        store.call_method1("guard_ink_job", (&job,))?;
        let strokes = model.get_item("strokes")?;
        let start: i64 = cursor.extract()?;
        let count = strokes.len()? as i64;
        let server_id = row.get_item("server_id")?;
        for index in start..count {
            store.call_method1("guard_ink_job", (&job,))?;
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))? {
                return Err(api_error(py, "cloud_unavailable", "Çizim kaydedildi; eşitlemeye daha sonra devam edilecek.", false));
            }
            let stroke = strokes.get_item(index)?;
            let recipe = model.call_method1("get", ("recipeVersion", 1))?;
            let operation_id = uuid5_py(&job.get_item("id")?, &index.to_string())?;
            let result = if recipe.eq(4)? || recipe.eq(5)? {
                // Recipe 4: first stroke on a page without a usable template
                // stroke; recipe 5: highlighter stroke. Same scratch transaction.
                let append_scratch = area(
                    py,
                    "samsung_notes.cloud_page_ops",
                    if recipe.eq(4)? { "append_first_stroke" } else { "append_highlighter_stroke" },
                )?;
                let kwargs = kw(
                    py,
                    &[
                        ("color_argb", stroke.get_item("colorARGB")?),
                        ("size", stroke.get_item("size")?),
                        ("new_stroke_id", stroke.get_item("id")?),
                        ("operation_id", PyString::new(py, &operation_id).into_any()),
                    ],
                )?;
                append_scratch.call(
                    (
                        &api,
                        &server_id,
                        snapshot_class.call_method1("from_dict", (&snapshot,))?,
                        stroke.get_item("pageId")?,
                        samples_from_points.call1((stroke.get_item("points")?,))?,
                    ),
                    Some(&kwargs),
                )?
            } else {
                // The Python dict literal evaluates all three bound methods first.
                let (v2, v3, v1) = (api.getattr("append_ink_stroke_v2")?, api.getattr("append_ink_stroke_v3")?, api.getattr("append_ink_stroke")?);
                let append = if recipe.eq(2)? { v2 } else if recipe.eq(3)? { v3 } else { v1 };
                let kwargs = kw(
                    py,
                    &[
                        ("samples", samples_from_points.call1((stroke.get_item("points")?,))?),
                        ("new_stroke_id", stroke.get_item("id")?),
                        ("operation_id", PyString::new(py, &operation_id).into_any()),
                    ],
                )?;
                if recipe.eq(3)? {
                    kwargs.set_item("color_argb", stroke.get_item("colorARGB")?)?;
                    kwargs.set_item("pen_size", stroke.get_item("size")?)?;
                }
                append.call((&server_id, snapshot_class.call_method1("from_dict", (&snapshot,))?, stroke.get_item("sourceStrokeId")?), Some(&kwargs))?
            };
            if !result.call_method1("get", ("verified",))?.is(&bool_obj(py, true)) {
                return Err(api_error(py, "write_uncertain", "Çizimin bulut kaydı henüz doğrulanamadı.", true));
            }
            snapshot = result.get_item("snapshot")?;
            store.call_method1("checkpoint_ink", (&job, index, &snapshot))?;
            store.call_method1("guard_ink_job", (&job,))?;
        }
        let out = PyDict::new(py);
        out.set_item("verified", true)?;
        out.set_item("server_id", &server_id)?;
        out.set_item("local_id", snapshot.get_item("local_id")?)?;
        out.set_item("snapshot", &snapshot)?;
        Ok(out.into_any())
    }

    #[cfg(feature = "python")]
    fn resolve<'py>(slf: &Bound<'py, Self>, identity: B<'py>, strategy: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let store = g!(slf, "store");
        let row = store.call_method1("raw", (&identity,))?;
        let org_state = row.get_item("org_state")?;
        if org_state.eq("conflict")? || org_state.eq("uncertain")? || org_state.eq("error")? {
            if strategy.ne("use_remote")? {
                return Err(store_error(py, "invalid_strategy", "Bu işlem için buluttaki durumu yeniden al.", 409));
            }
            let api = slf.call_method0("_api")?;
            let inventory = g!(slf, "metadata_factory").call1((&api,))?.call_method0("inventory")?;
            let snapshot = api.call_method1("get_note", (row.get_item("server_id")?,))?;
            store.call_method1("apply_folder_graph", (inventory,))?;
            store.call_method1("apply_document", (row.get_item("server_id")?, snapshot.call_method0("to_dict")?))?;
            return store.call_method1("accept_remote_organization", (identity,));
        }
        // Refresh remote before a resolution, never silently discard the local
        // draft based on a stale cached version.
        if row.get_item("server_id")?.is_truthy()? && !row.get_item("deleted_remote")?.is_truthy()? {
            let refreshed = (|| -> PyResult<()> {
                let snapshot = slf.call_method0("_api")?.call_method1("get_note", (row.get_item("server_id")?,))?;
                store.call_method1("apply_document", (row.get_item("server_id")?, snapshot.call_method0("to_dict")?))?;
                Ok(())
            })();
            if let Err(e) = refreshed {
                if !(is_api_error(py, &e) || is_store_error(py, &e)) {
                    return Err(e);
                }
                if strategy.eq("use_remote")? {
                    let err = store_error(py, "remote_unavailable", "Güncel bulut sürümü alınamadı; yerel metnin korunuyor.", 409);
                    err.set_cause(py, Some(e));
                    return Err(err);
                }
            }
        }
        let result = store.call_method1("resolve", (identity, strategy))?;
        slf.call_method0("request_sync")?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    /// Import already-verified metadata and the owned test identity once.
    fn _bootstrap_cache(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let store = g!(slf, "store");
        let session = g!(g!(slf, "sessions"), "path");
        if !session.is_truthy()? || store.call_method1("meta", ("research_seed_imported", false))?.is_truthy()? {
            return Ok(());
        }
        let session: PathBuf = session.extract()?;
        let scan = session.join("cloud/scan");
        let mut pages: Vec<(String, PathBuf)> = std::fs::read_dir(&scan)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        (name.starts_with("page-") && name.ends_with(".json") && name.len() >= 10).then(|| (name, e.path()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        pages.sort();
        for (name, parsed_path) in pages {
            let stem = &name[..name.len() - 5];
            if !core::py_isdigit(&stem[5..]) {
                continue;
            }
            let raw_path = parsed_path.with_file_name(format!("{}-response.body", stem));
            let step = (|| -> PyResult<bool> {
                let parsed_size = std::fs::metadata(&parsed_path).map_err(PyErr::from)?.len();
                let raw_size = std::fs::metadata(&raw_path).map_err(PyErr::from)?.len();
                if parsed_size > 8 * 1024 * 1024 || raw_size > 2 * 1024 * 1024 {
                    return Ok(false);
                }
                let page = pyconv::loads_py(py, &std::fs::read(&parsed_path).map_err(PyErr::from)?)?;
                let digest = core::sha256_hex(&std::fs::read(&raw_path).map_err(PyErr::from)?);
                if page.call_method1("get", ("sourceSha256",))?.ne(digest)? {
                    return Ok(false);
                }
                for change in page.get_item("page")?.get_item("changes")?.try_iter()? {
                    store.call_method1("stage_change", (change?,))?;
                }
                let cursor = page.get_item("page")?.get_item("last_change_point")?;
                store.call_method1("set_meta", ("cursor", cursor))?;
                Ok(true)
            })();
            match step {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) if is_io_value(py, &e) => break,
                Err(e) => return Err(e),
            }
        }
        let remote = session.join("cloud/sync-probe/remote.json");
        if remote.exists() {
            let step = (|| -> PyResult<()> {
                let value = pyconv::loads_py(py, &std::fs::read(&remote).map_err(PyErr::from)?)?;
                if value.call_method1("get", ("createdByThisClient",))?.is(&bool_obj(py, true)) {
                    let change = PyDict::new(py);
                    change.set_item("server_id", value.get_item("serverId")?)?;
                    change.set_item("resource_id", value.get_item("localNoteId")?)?;
                    change.set_item("commit_id", value.get_item("commitId")?)?;
                    change.set_item("is_deleted", false)?;
                    store.call_method1("stage_change", (change,))?;
                }
                Ok(())
            })();
            if let Err(e) = step {
                if !is_io_value(py, &e) {
                    return Err(e);
                }
            }
        }
        store.call_method1("set_meta", ("research_seed_imported", true))?;
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _push(slf: &Bound<'_, Self>, api: B<'_>) -> PyResult<()> {
        let py = slf.py();
        let s = slf.as_any();
        for _ in 0..100 {
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))? {
                return Ok(());
            }
            let store = g!(s, "store");
            let job = store.call_method0("next_job")?;
            if job.is_none() {
                return Ok(());
            }
            let row = store.call_method1("raw", (job.get_item("note_id")?,))?;
            if !store.call_method1("job_state", (&job, "running"))?.is_truthy()? {
                // Cancelled or resolved after selection; do not publish it.
                continue;
            }
            let attempt = (|| -> PyResult<()> {
                let kind = job.get_item("kind")?;
                let op_kw = || kw(py, &[("operation_id", job.get_item("id")?)]);
                let result = if kind.eq("append_ink")? {
                    s.call_method1("_push_ink_batch", (&api, &row, &job))?
                } else if kind.eq("erase_ink")? {
                    s.call_method1("_push_ink_erase", (&api, &row, &job))?
                } else if kind.eq("move_ink")? {
                    s.call_method1("_push_ink_move", (&api, &row, &job))?
                } else if kind.eq("create_editor")? {
                    let kwargs = kw(py, &[("operation_id", job.get_item("id")?), ("local_id", row.get_item("resource_id")?)])?;
                    api.call_method("create_editor", (job.get_item("title")?, store.call_method1("editor_job_model", (&job,))?), Some(&kwargs))?
                } else if kind.eq("create")? {
                    let kwargs = kw(py, &[("operation_id", job.get_item("id")?), ("local_id", row.get_item("resource_id")?)])?;
                    api.call_method("create", (job.get_item("title")?, job.get_item("body")?), Some(&kwargs))?
                } else {
                    if !job.get_item("snapshot")?.is_truthy()? {
                        return Err(api_error(py, "missing_base", "Düzenlemenin temel sürümü eksik; yerel metin korunuyor.", false));
                    }
                    let snapshot = area(py, "samsung_notes.document", "DocumentSnapshot")?
                        .call_method1("from_dict", (json_loads(&job.get_item("snapshot")?)?,))?;
                    let server_id = row.get_item("server_id")?;
                    if kind.eq("title")? {
                        api.call_method("update_title", (&server_id, &snapshot, job.get_item("title")?), Some(&op_kw()?))?
                    } else if kind.eq("pdf_pages")? {
                        let spec = store.call_method1("pdf_page_job", (&job,))?;
                        let staging: PathBuf = s.call_method0("_pdf_staging")?.extract()?;
                        let sha: String = spec.get_item("file_sha256")?.extract()?;
                        let pdf = std::fs::read(staging.join(format!("{}.pdf", sha))).map_err(PyErr::from)?;
                        let kwargs = kw(
                            py,
                            &[
                                ("file_name", spec.get_item("file_name")?),
                                ("page_sizes", spec.get_item("page_sizes")?),
                                ("page_indices", spec.get_item("page_indices")?),
                                ("template_type", spec.get_item("template_type")?),
                                ("operation_id", job.get_item("id")?),
                            ],
                        )?;
                        api.call_method("add_pdf_pages", (&server_id, &snapshot, PyBytes::new(py, &pdf)), Some(&kwargs))?
                    } else if kind.eq("page_op")? {
                        let unpacked: Vec<B<'_>> = store.call_method1("page_op_job", (&job,))?.try_iter()?.collect::<PyResult<_>>()?;
                        let [op, spec] = <[B<'_>; 2]>::try_from(unpacked).map_err(|_| PyValueError::new_err("expected 2 values to unpack"))?;
                        if op.eq("shape")? {
                            area(py, "samsung_notes.cloud_shapes", "add_shape")?
                                .call((&api, &server_id, &snapshot, spec.get_item("pageId")?, spec.get_item("shape")?), Some(&op_kw()?))?
                        } else if op.eq("add")? {
                            area(py, "samsung_notes.cloud_page_ops", "add_page")?
                                .call((&api, &server_id, &snapshot, spec.get_item("afterIndex")?), Some(&op_kw()?))?
                        } else if op.eq("delete")? {
                            area(py, "samsung_notes.cloud_page_ops", "delete_pages")?
                                .call((&api, &server_id, &snapshot, spec.get_item("pageIds")?), Some(&op_kw()?))?
                        } else {
                            area(py, "samsung_notes.cloud_page_ops", "reorder_pages")?
                                .call((&api, &server_id, &snapshot, spec.get_item("pageIds")?), Some(&op_kw()?))?
                        }
                    } else if kind.eq("page_style")? {
                        api.call_method("update_page_background", (&server_id, &snapshot, store.call_method1("page_style_job_color", (&job,))?), Some(&op_kw()?))?
                    } else if kind.eq("update_editor")? {
                        api.call_method(
                            "update_editor",
                            (&server_id, &snapshot, job.get_item("title")?, store.call_method1("editor_job_model", (&job,))?),
                            Some(&op_kw()?),
                        )?
                    } else {
                        api.call_method("update", (&server_id, &snapshot, job.get_item("title")?, job.get_item("body")?), Some(&op_kw()?))?
                    }
                };
                if !result.call_method1("get", ("verified",))?.is(&bool_obj(py, true)) {
                    return Err(api_error(py, "write_uncertain", "Bulut kaydı henüz doğrulanamadı.", true));
                }
                store.call_method1("complete_job", (&job, &result))?;
                if store.call_method1("meta", ("folder_initialized", false))?.is_truthy()? {
                    store.call_method1("register_note_graph", (job.get_item("note_id")?,))?;
                }
                Ok(())
            })();
            if let Err(e) = attempt {
                if is_conflict(py, &e) {
                    store.call_method1("job_state", (&job, "conflict", err_attr(py, &e, "message")?))?;
                    let refreshed = (|| -> PyResult<()> {
                        let fresh = api.call_method1("get_note", (row.get_item("server_id")?,))?;
                        store.call_method1("apply_document", (row.get_item("server_id")?, fresh.call_method0("to_dict")?))?;
                        Ok(())
                    })();
                    if let Err(inner) = refreshed {
                        if !is_api_error(py, &inner) {
                            return Err(inner);
                        }
                    }
                } else if is_api_error(py, &e) {
                    let ambiguous = err_attr(py, &e, "ambiguous")?.is_truthy()?;
                    if err_code_in(py, &e, &["cloud_unavailable", "reauth_required", "renew_failed"])? && !ambiguous {
                        // API immutable operation IDs guard the later continuation.
                        store.call_method1("job_state", (&job, "queued"))?;
                        return Err(e);
                    }
                    store.call_method1("job_state", (&job, if ambiguous { "uncertain" } else { "failed" }, err_attr(py, &e, "message")?))?;
                } else if is_document_error(py, &e) || is_store_error(py, &e) {
                    if is_store_error(py, &e) && err_attr(py, &e, "code")?.eq("ink_job_stopped")? {
                        // A concurrent conflict/resolution owns the job state. Do
                        // not turn cancelled/conflict back into failed or done.
                        store.call_method1("suspend_ink_job", (&job,))?;
                        continue;
                    }
                    store.call_method1("job_state", (&job, "failed", e.value(py).str()?))?;
                } else if is_io_value(py, &e) {
                    store.call_method1("job_state", (&job, "uncertain", "Gönderim sonucu bilinmiyor. Otomatik tekrar durduruldu; yerel metin korunuyor."))?;
                } else {
                    return Err(e);
                }
            }
            increment_processed(s)?;
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _prefetch_previews(slf: &Bound<'_, Self>) {
        let _ = slf.getattr("previews").and_then(|p| p.call_method0("prefetch"));
    }

    #[cfg(feature = "python")]
    /// Download note documents concurrently (reads only); results keep input order.
    fn _fetch_many<'py>(slf: &Bound<'py, Self>, api: B<'py>, server_ids: B<'py>) -> PyResult<B<'py>> {
        let py = slf.py();
        let ids: Vec<Py<PyAny>> = server_ids.try_iter()?.map(|i| i.map(|v| v.unbind())).collect::<PyResult<_>>()?;
        let engine = slf.clone().unbind();
        let api = api.unbind();
        let fetch = |py: Python<'_>, id: &Py<PyAny>| -> PyResult<Py<PyAny>> {
            let s = engine.bind(py);
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))? {
                return Ok(py.None());
            }
            match api.bind(py).call_method1("get_note", (id.bind(py),)) {
                Ok(v) => Ok(v.unbind()),
                Err(e) if is_api_error(py, &e) => Ok(e.value(py).clone().into_any().unbind()),
                Err(e) => Err(e),
            }
        };
        let results: Vec<PyResult<Py<PyAny>>> = if ids.len() <= 1 {
            ids.iter().map(|id| fetch(py, id)).collect()
        } else {
            let workers: usize = g!(slf, "DOWNLOAD_WORKERS").extract::<usize>()?.min(ids.len()).max(1);
            let next = AtomicUsize::new(0);
            let slots: Mutex<Vec<Option<PyResult<Py<PyAny>>>>> = Mutex::new((0..ids.len()).map(|_| None).collect());
            py.detach(|| {
                std::thread::scope(|scope| {
                    for _ in 0..workers {
                        scope.spawn(|| loop {
                            let i = next.fetch_add(1, Ordering::SeqCst);
                            if i >= ids.len() {
                                break;
                            }
                            let r = Python::attach(|py| fetch(py, &ids[i]));
                            slots.lock().unwrap()[i] = Some(r);
                        });
                    }
                })
            });
            slots.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
        };
        let list = PyList::empty(py);
        for r in results {
            list.append(r?)?;
        }
        Ok(list.into_any())
    }

    #[cfg(feature = "python")]
    fn _apply_fetched<'py>(slf: &Bound<'py, Self>, identity: B<'py>, server_id: B<'py>, result: B<'py>) -> PyResult<bool> {
        let py = slf.py();
        if result.is_none() {
            return Ok(false);
        }
        let store = g!(slf, "store");
        if result.is_instance(&exc_class(py, "samsung_notes.cloud_api", "APIError")?)? {
            store.call_method1("content_error", (&identity, result.getattr("message")?))?;
            let code = result.getattr("code")?;
            if code.eq("reauth_required")? || code.eq("cloud_unavailable")? {
                return Err(PyErr::from_value(result));
            }
            return Ok(true);
        }
        store.call_method1("apply_document", (server_id, result.call_method0("to_dict")?))?;
        Ok(true)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (api, limit=None))]
    fn _load_missing<'py>(slf: &Bound<'py, Self>, api: B<'py>, limit: Option<B<'py>>) -> PyResult<()> {
        let py = slf.py();
        let limit = limit.unwrap_or_else(|| int_obj(py, 20));
        let rows: Vec<B<'py>> = with_cm(g!(slf, "store").call_method0("connect")?, |db| {
            db.call_method1("execute", ("SELECT id,server_id FROM notes WHERE loaded=0 AND deleted_remote=0 LIMIT ?", (&limit,)))?
                .call_method0("fetchall")?
                .try_iter()?
                .collect::<PyResult<Vec<_>>>()
        })?;
        let ids = PyList::empty(py);
        for row in &rows {
            ids.append(row.get_item("server_id")?)?;
        }
        let results = slf.call_method1("_fetch_many", (&api, ids))?;
        for (row, result) in rows.iter().zip(results.try_iter()?) {
            if !slf.call_method1("_apply_fetched", (row.get_item("id")?, row.get_item("server_id")?, result?))?.is_truthy()? {
                return Ok(());
            }
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _pull_folders(slf: &Bound<'_, Self>, metadata: B<'_>) -> PyResult<()> {
        let s = slf.as_any();
        let store = g!(s, "store");
        let stopped = || -> PyResult<bool> {
            Ok(g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))?)
        };
        let inventory = metadata.call_method1("folder_changes", (store.call_method1("meta", ("folder_cursor",))?,))?;
        if stopped()? {
            return Ok(());
        }
        store.call_method1("apply_folder_graph", (&inventory,))?;
        if !store.call_method1("meta", ("note_inventory_imported", false))?.is_truthy()? {
            let records = metadata.call_method0("note_inventory")?;
            if stopped()? {
                return Ok(());
            }
            for record in records.get_item("notes")?.try_iter()? {
                // Tombstones cannot supply missing contents. Existing dirty
                // documents are protected by the same deletion conflict path.
                store.call_method1("stage_change", (record?,))?;
            }
            store.call_method1("set_meta", ("note_inventory_imported", true))?;
            // Newly staged notes now receive the graph's placement and trash.
            if inventory.call_method1("get", ("has_resource",))?.is_truthy()? {
                store.call_method1("apply_folder_graph", (&inventory,))?;
            }
        }
        // Upgrade only proven local creates missing a graph node. Old local
        // trash/favorite preferences are never transformed into remote writes.
        let created: Vec<B<'_>> = with_cm(store.call_method0("connect")?, |db| {
            db.call_method1("execute", ("SELECT DISTINCT note_id FROM jobs WHERE kind IN ('create','create_editor') AND state='done'",))?
                .call_method0("fetchall")?
                .try_iter()?
                .collect::<PyResult<Vec<_>>>()
        })?;
        for row in created {
            store.call_method1("register_note_graph", (row.get_item("note_id")?,))?;
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _push_organization(slf: &Bound<'_, Self>, api: B<'_>, metadata: B<'_>) -> PyResult<()> {
        let py = slf.py();
        let s = slf.as_any();
        let _ = api;
        for _ in 0..100 {
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))? {
                return Ok(());
            }
            let store = g!(s, "store");
            let job = store.call_method0("next_organization_job")?;
            if job.is_none() {
                return Ok(());
            }
            store.call_method1("organization_state", (&job, "running"))?;
            let row = store.call_method1("raw", (job.get_item("note_id")?,))?;
            let attempt = (|| -> PyResult<()> {
                let snapshot = area(py, "samsung_notes.document", "DocumentSnapshot")?
                    .call_method1("from_dict", (json_loads(&job.get_item("snapshot")?)?,))?;
                let baseline = job.get_item("baseline_node")?;
                let baseline = if baseline.is_truthy()? { json_loads(&baseline)? } else { none(py) };
                let result = metadata.call_method(
                    "update_organization",
                    (row.get_item("server_id")?, snapshot, json_loads(&job.get_item("changes")?)?),
                    Some(&kw(py, &[("operation_id", job.get_item("id")?), ("baseline_node", baseline)])?),
                )?;
                if !result.call_method1("get", ("verified",))?.is(&bool_obj(py, true)) {
                    return Err(api_error(py, "write_uncertain", "Bulut işleminin sonucu doğrulanamadı.", true));
                }
                store.call_method1("apply_document", (row.get_item("server_id")?, result.get_item("snapshot")?))?;
                if result.call_method1("get", ("node",))?.is_truthy()? {
                    store.call_method1("apply_organization_node", (result.get_item("node")?,))?;
                }
                store.call_method1("organization_completed", (&job,))?;
                Ok(())
            })();
            if let Err(e) = attempt {
                if is_conflict(py, &e) {
                    store.call_method1("organization_state", (&job, "conflict", err_attr(py, &e, "message")?))?;
                } else if is_api_error(py, &e) {
                    let ambiguous = err_attr(py, &e, "ambiguous")?.is_truthy()?;
                    if err_code_in(py, &e, &["cloud_unavailable", "reauth_required", "renew_failed"])? && !ambiguous {
                        store.call_method1("organization_state", (&job, "queued"))?;
                        return Err(e);
                    }
                    store.call_method1("organization_state", (&job, if ambiguous { "uncertain" } else { "failed" }, err_attr(py, &e, "message")?))?;
                } else if is_io_value(py, &e) {
                    store.call_method1("organization_state", (&job, "uncertain", "Bulut işleminin sonucu bilinmiyor. Otomatik tekrar durduruldu."))?;
                } else {
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _push_folders(slf: &Bound<'_, Self>, metadata: B<'_>) -> PyResult<()> {
        let py = slf.py();
        let s = slf.as_any();
        for _ in 0..100 {
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))? {
                return Ok(());
            }
            let store = g!(s, "store");
            let job = store.call_method0("next_folder_job")?;
            if job.is_none() {
                return Ok(());
            }
            store.call_method1("folder_job_state", (&job, "running"))?;
            let attempt = (|| -> PyResult<()> {
                let changes = json_loads(&job.get_item("changes")?)?;
                let result = if job.get_item("kind")?.eq("create")? {
                    metadata.call_method(
                        "create_folder",
                        (changes.get_item("name")?,),
                        Some(&kw(py, &[("parent_id", changes.get_item("parent_id")?), ("operation_id", job.get_item("id")?), ("folder_id", job.get_item("folder_id")?)])?),
                    )?
                } else {
                    metadata.call_method(
                        "update_folder",
                        (job.get_item("folder_id")?, &changes),
                        Some(&kw(py, &[("baseline_node", json_loads(&job.get_item("baseline_node")?)?), ("operation_id", job.get_item("id")?)])?),
                    )?
                };
                if !result.call_method1("get", ("verified",))?.is(&bool_obj(py, true)) {
                    return Err(api_error(py, "write_uncertain", "Klasör işleminin sonucu doğrulanamadı.", true));
                }
                store.call_method1("folder_completed", (&job, result.get_item("node")?))?;
                Ok(())
            })();
            if let Err(e) = attempt {
                if is_conflict(py, &e) {
                    store.call_method1("folder_job_state", (&job, "conflict", err_attr(py, &e, "message")?))?;
                } else if is_api_error(py, &e) {
                    let ambiguous = err_attr(py, &e, "ambiguous")?.is_truthy()?;
                    if err_code_in(py, &e, &["cloud_unavailable", "reauth_required", "renew_failed"])? && !ambiguous {
                        store.call_method1("folder_job_state", (&job, "queued"))?;
                        return Err(e);
                    }
                    store.call_method1("folder_job_state", (&job, if ambiguous { "uncertain" } else { "failed" }, err_attr(py, &e, "message")?))?;
                } else if is_io_value(py, &e) {
                    store.call_method1("folder_job_state", (&job, "uncertain", "Klasör işleminin sonucu bilinmiyor. Yeniden gönderilmedi."))?;
                } else {
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _pull(slf: &Bound<'_, Self>, api: B<'_>) -> PyResult<bool> {
        let py = slf.py();
        let s = slf.as_any();
        let store = g!(s, "store");
        let stored = store.call_method1("meta", ("cursor",))?;
        let mut cursor = if stored.is_truthy()? { stored } else { api.getattr("initial_cursor")? };
        let seen = PySet::empty(py)?;
        let stopped = || -> PyResult<bool> {
            Ok(g!(s, "stop").call_method0("is_set")?.is_truthy()? || g!(g!(s, "sessions"), "path").ne(g!(s, "api_session"))?)
        };
        for _ in 0..500 {
            if stopped()? {
                return Ok(false);
            }
            if seen.contains(&cursor)? {
                return Err(api_error(py, "cursor_cycle", "Bulut değişiklik listesi aynı konumu tekrarladı; ilerleme kaydedildi.", false));
            }
            seen.add(&cursor)?;
            let page = api.call_method1("changes", (&cursor,))?;
            let mut downloads: Vec<(B<'_>, B<'_>)> = Vec::new();
            for change in page.get_item("changes")?.try_iter()? {
                let change = change?;
                if stopped()? {
                    return Ok(false);
                }
                let old = store.call_method1("by_server", (change.get_item("server_id")?,))?;
                let identity = store.call_method1("stage_change", (&change,))?;
                if identity.is_truthy()? && !change.call_method1("get", ("is_deleted",))?.is(&bool_obj(py, true)) {
                    let old_commit = if old.is_truthy()? && old.get_item("base_document")?.is_truthy()? {
                        json_loads(&old.get_item("base_document")?)?.call_method1("get", ("commit_id",))?
                    } else {
                        none(py)
                    };
                    if !old.is_truthy()?
                        || !old.get_item("loaded")?.is_truthy()?
                        || normalize_commit_py(&old_commit)? != normalize_commit_py(&change.call_method1("get", ("commit_id",))?)?
                    {
                        downloads.push((identity, change.get_item("server_id")?));
                        continue;
                    }
                }
                increment_processed(s)?;
            }
            // Changed documents of this page download in parallel; the cursor
            // only advances after every one of them is applied or recorded.
            let ids = PyList::empty(py);
            for (_, server_id) in &downloads {
                ids.append(server_id)?;
            }
            let results = s.call_method1("_fetch_many", (&api, ids))?;
            for ((identity, server_id), result) in downloads.iter().zip(results.try_iter()?) {
                if !s.call_method1("_apply_fetched", (identity, server_id, result?))?.is_truthy()? {
                    return Ok(false);
                }
                increment_processed(s)?;
            }
            cursor = page.get_item("last_change_point")?;
            store.call_method1("set_meta", ("cursor", &cursor))?;
            if page.get_item("has_next")?.is(&bool_obj(py, false)) {
                return Ok(true);
            }
            if g!(s, "wake").call_method0("is_set")?.is_truthy()? && store.call_method0("counts")?.get_item("pending")?.is_truthy()? {
                return Ok(false);
            }
        }
        // Resume bounded initial imports on the next worker iteration.
        g!(s, "wake").call_method0("set")?;
        Ok(false)
    }

    #[cfg(feature = "python")]
    fn sync_once(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let py = slf.py();
        let s = slf.as_any();
        let sync_lock = g!(s, "sync_lock");
        if !sync_lock.call_method("acquire", (), Some(&kw(py, &[("blocking", bool_obj(py, false))])?))?.is_truthy()? {
            return Ok(false);
        }
        let outcome = (|| -> PyResult<bool> {
            with_cm(g!(s, "lock"), |_| {
                s.setattr("state", "syncing")?;
                s.setattr("error", none(py))?;
                s.setattr("processed", 0)?;
                s.setattr("total", none(py))
            })?;
            let api = s.call_method0("_api")?;
            let factory = g!(s, "metadata_factory");
            let metadata = if factory.is_truthy()? { factory.call1((&api,))? } else { none(py) };
            let has_metadata = metadata.is_truthy()?;
            if has_metadata {
                s.call_method1("_pull_folders", (&metadata,))?;
            }
            s.call_method1("_push", (&api,))?;
            if has_metadata {
                s.call_method1("_push_folders", (&metadata,))?;
                s.call_method1("_push_organization", (&api, &metadata))?;
            }
            s.call_method1("_load_missing", (&api,))?;
            let mut complete = s.call_method1("_pull", (&api,))?.is_truthy()?;
            let store = g!(s, "store");
            if has_metadata {
                let graph = PyDict::new(py);
                graph.set_item("has_resource", false)?;
                graph.set_item("last_change_point", store.call_method1("meta", ("folder_cursor",))?)?;
                store.call_method1("apply_folder_graph", (graph,))?;
            }
            let missing: i64 = with_cm(store.call_method0("connect")?, |db| {
                db.call_method1("execute", ("SELECT COUNT(*) FROM notes WHERE loaded=0 AND deleted_remote=0",))?
                    .call_method0("fetchone")?
                    .get_item(0)?
                    .extract()
            })?;
            if missing != 0 {
                complete = false;
            }
            if complete {
                store.call_method1("set_meta", ("last_synced_at", core::now_ms()))?;
                // Warm first-page previews so opening a note is immediate.
                let warmup = g!(s, "warmup");
                let busy = warmup.is_truthy()? && warmup.call_method0("is_alive")?.is_truthy()?;
                if !g!(s, "stop").call_method0("is_set")?.is_truthy()? && !busy {
                    let thread = py.import("threading")?.getattr("Thread")?.call(
                        (),
                        Some(&kw(
                            py,
                            &[
                                ("target", s.getattr("_prefetch_previews")?),
                                ("daemon", bool_obj(py, true)),
                                ("name", PyString::new(py, "notes-preview-warmup").into_any()),
                            ],
                        )?),
                    )?;
                    s.setattr("warmup", &thread)?;
                    thread.call_method0("start")?;
                }
            }
            with_cm(g!(s, "lock"), |_| {
                s.setattr("state", if missing != 0 { "error" } else { "idle" })?;
                if missing != 0 {
                    s.setattr("error", format!("{} not henüz indirilemedi. İndirilen notlar çevrimdışı okunabilir; eşitleme yeniden denenecek.", missing))?;
                }
                Ok(())
            })?;
            Ok(complete)
        })();
        let result = match outcome {
            Ok(v) => Ok(v),
            Err(e) if is_api_error(py, &e) || is_store_error(py, &e) => with_cm(g!(s, "lock"), |_| {
                let offline = err_attr(py, &e, "code")?.eq("cloud_unavailable")?;
                s.setattr("state", if offline { "offline" } else { "error" })?;
                s.setattr("error", err_attr(py, &e, "message")?)?;
                Ok(false)
            }),
            Err(e) if is_io_value(py, &e) => with_cm(g!(s, "lock"), |_| {
                s.setattr("state", "error")?;
                s.setattr("error", "Eşitleme tamamlanamadı. Yerel notlar ve işlem kayıtları korunuyor.")?;
                Ok(false)
            }),
            Err(e) => Err(e),
        };
        sync_lock.call_method0("release")?;
        result
    }

    #[cfg(feature = "python")]
    fn _loop(slf: &Bound<'_, Self>) -> PyResult<()> {
        let s = slf.as_any();
        while !g!(s, "stop").call_method0("is_set")?.is_truthy()? {
            let triggered = g!(s, "wake").call_method1("wait", (g!(s, "interval"),))?.is_truthy()?;
            g!(s, "wake").call_method0("clear")?;
            if g!(s, "stop").call_method0("is_set")?.is_truthy()? {
                break;
            }
            if g!(g!(s, "sessions"), "path").is_truthy()? && (triggered || g!(s, "auto_sync").is_truthy()?) {
                s.call_method0("sync_once")?;
            }
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn close(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let s = slf.as_any();
        let timeout = || kw(py, &[("timeout", int_obj(py, 2))]);
        g!(s, "previews").call_method0("close")?;
        g!(g!(s, "sessions"), "closed").call_method0("set")?;
        g!(s, "stop").call_method0("set")?;
        g!(s, "wake").call_method0("set")?;
        // An in-flight HTTP request may still finish its immutable receipt.
        g!(s, "worker").call_method("join", (), Some(&timeout()?))?;
        let warmup = g!(s, "warmup");
        if warmup.is_truthy()? {
            warmup.call_method("join", (), Some(&timeout()?))?;
        }
        g!(s, "previews").call_method("join", (), Some(&timeout()?))?;
        Ok(())
    }
}

#[cfg(feature = "python")]
fn increment_processed(s: &B<'_>) -> PyResult<()> {
    with_cm(s.getattr("lock")?, |_| {
        let value = s.getattr("processed")?.call_method1("__add__", (1,))?;
        s.setattr("processed", value)
    })
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    rs_common::export(m, &["Engine"])
}
