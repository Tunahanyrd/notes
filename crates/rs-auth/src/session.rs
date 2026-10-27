//! Rust port of `samsung_notes/session.py` (SessionManager).
//!
//! State lives in the instance `__dict__` (like rs-cloud's CloudAPI) so
//! `SessionManager.__new__` + manual attribute setting, engine patching and
//! subclassing keep working. Module globals that tests patch
//! (`samsung_notes.session.RESEARCH`, `samsung_notes.cloud_api.CloudAPI`) are
//! resolved at call time, and `StoreError` is raised through
//! `samsung_notes.storage` (the sanctioned Python-facing name). The browser
//! sign-in thread (`_login`) and Cloud registration (`_register`) drive the
//! real login and need a live session to validate (see PORTING.md).
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyModule, PyTuple};

#[cfg(feature = "python")]
fn session_module<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    Ok(py.import("samsung_notes.session")?.into_any())
}
#[cfg(feature = "python")]
fn research<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    session_module(py)?.getattr("RESEARCH")
}
#[cfg(feature = "python")]
fn root<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    session_module(py)?.getattr("ROOT")
}
#[cfg(feature = "python")]
fn tools<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    root(py)?.call_method1("__truediv__", ("tools",))
}
#[cfg(feature = "python")]
fn pathlib<'py>(py: Python<'py>, p: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    py.import("pathlib")?.getattr("Path")?.call1((p,))
}
#[cfg(feature = "python")]
fn load_json<'py>(py: Python<'py>, path: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    py.import("notes_login")?.getattr("load_json")?.call1((path,))
}
#[cfg(feature = "python")]
fn save_json<'py>(py: Python<'py>, path: &Bound<'py, PyAny>, data: &Bound<'py, PyAny>) -> PyResult<()> {
    py.import("notes_login")?.getattr("save_json")?.call1((path, data))?;
    Ok(())
}
#[cfg(feature = "python")]
fn store_error(py: Python<'_>, code: &str, message: &str, status: i64) -> PyErr {
    match py.import("samsung_notes.storage").and_then(|m| m.getattr("StoreError")).and_then(|c| c.call1((code, message, status))) {
        Ok(inst) => PyErr::from_value(inst),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
#[pyclass(subclass, dict, module = "samsung_notes.session", name = "SessionManager")]
pub struct SessionManager;

#[cfg(feature = "python")]
/// `with self.lock:` guard.
fn with_lock<'py, T>(slf: &Bound<'py, PyAny>, body: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let lock = slf.getattr("lock")?;
    lock.call_method0("__enter__")?;
    let out = body();
    let py = slf.py();
    lock.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
    out
}

#[cfg(feature = "python")]
#[pymethods]
impl SessionManager {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        SessionManager
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (store, *, offline = false, session = None))]
    fn __init__(slf: &Bound<'_, Self>, store: &Bound<'_, PyAny>, offline: bool, session: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.as_any();
        this.setattr("store", store)?;
        this.setattr("offline", offline)?;
        this.setattr("path", py.None())?;
        this.setattr("pending", false)?;
        this.setattr("error", py.None())?;
        let threading = py.import("threading")?;
        this.setattr("lock", threading.getattr("Lock")?.call0()?)?;
        this.setattr("closed", threading.getattr("Event")?.call0()?)?;
        this.setattr("on_connected", wrap_pyfunction!(_noop, py)?)?;

        if !offline && !store.call_method1("meta", ("disconnected", false))?.is_truthy()? {
            let mut selected = match session {
                Some(s) if !s.is_none() => s.clone(),
                _ => store.call_method1("meta", ("session_path",))?,
            };
            if !selected.is_truthy()? {
                let pending_file = research(py)?.call_method1("__truediv__", ("pending.json",))?;
                if pending_file.call_method0("exists")?.is_truthy()? {
                    selected = load_json(py, &pending_file)?.call_method1("get", ("transactionDirectory",))?;
                }
            }
            if selected.is_truthy()? {
                let result = Self::adopt(slf, &pathlib(py, &selected)?);
                if let Err(e) = result {
                    if e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
                        || e.is_instance_of::<pyo3::exceptions::PyValueError>(py)
                        || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
                        || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
                    {
                        this.setattr("error", "Oturum yüklenemedi. Samsung hesabına yeniden giriş yapabilirsin.")?;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn adopt(slf: &Bound<'_, Self>, path: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.as_any();
        let path = path.call_method0("resolve")?;
        let expected_parent = research(py)?.call_method1("__truediv__", ("sessions",))?.call_method0("resolve")?;
        let token_file = path.call_method1("__truediv__", ("cloud/cloud-token.json",))?;
        if path.getattr("parent")?.ne(&expected_parent)? || !token_file.call_method0("is_file")?.is_truthy()? {
            return Err(crate::common::value_error("only completed owned research sessions can be adopted"));
        }
        let stat = path.call_method0("stat")?;
        let uid: u32 = stat.getattr("st_uid")?.extract()?;
        let mode: u32 = stat.getattr("st_mode")?.extract()?;
        let getuid: u32 = py.import("os")?.getattr("getuid")?.call0()?.extract()?;
        if uid != getuid || mode & 0o077 != 0 {
            return Err(crate::common::value_error("session directory is not private"));
        }
        let token = load_json(py, &path.call_method1("__truediv__", ("notes-account-token.json",))?)?;
        let client_id: String = token.get_item("clientId")?.extract()?;
        let user_id: String = token.get_item("response")?.get_item("userId")?.extract()?;
        let account = rs_cloud::util::sha256_hex(format!("{client_id}:{user_id}").as_bytes());
        let store = this.getattr("store")?;
        let previous = store.call_method1("meta", ("account_key",))?;
        if previous.is_truthy()? && previous.ne(&account)? {
            return Err(store_error(
                py,
                "different_account",
                "Bu yerel arşiv başka bir Samsung hesabına ait. Mevcut notlar o hesaba gönderilmedi.",
                409,
            ));
        }
        store.call_method1("set_meta", ("account_key", &account))?;
        store.call_method1("set_meta", ("session_path", path.call_method0("__str__")?))?;
        store.call_method1("set_meta", ("disconnected", false))?;
        this.setattr("path", &path)?;
        this.setattr("error", py.None())?;
        Ok(())
    }

    #[cfg(feature = "python")]
    fn valid(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let py = slf.py();
        let this = slf.as_any();
        if this.getattr("offline")?.is_truthy()? || this.getattr("path")?.is_none() {
            return Ok(false);
        }
        let cloud_api_mod = py.import("samsung_notes.cloud_api")?;
        let cloud_api = cloud_api_mod.getattr("CloudAPI")?;
        let api_error = cloud_api_mod.getattr("APIError")?;
        let work = this.getattr("store")?.getattr("directory")?.call_method1("__truediv__", ("cloud",))?;
        match (|| -> PyResult<bool> {
            let status = cloud_api.call1((this.getattr("path")?, work))?.call_method0("auth_status")?;
            Ok(status.get_item("valid")?.is_truthy()? || status.get_item("renewable")?.is_truthy()?)
        })() {
            Ok(v) => Ok(v),
            Err(e) => {
                let caught = e.value(py).is_instance(&api_error)?
                    || e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
                    || e.is_instance_of::<pyo3::exceptions::PyValueError>(py)
                    || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
                    || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py);
                if caught {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }

    #[cfg(feature = "python")]
    fn disconnect<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let this = slf.as_any();
        with_lock(this, || {
            if this.getattr("pending")?.is_truthy()? {
                return Err(store_error(
                    py,
                    "login_pending",
                    "Devam eden girişi tarayıcıda iptal et veya tamamlanmasını bekle.",
                    409,
                ));
            }
            this.setattr("path", py.None())?;
            this.getattr("store")?.call_method1("set_meta", ("disconnected", true))?;
            Ok(())
        })?;
        let d = PyDict::new(py);
        d.set_item("connected", false)?;
        Ok(d)
    }

    #[cfg(feature = "python")]
    fn start_login(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let py = slf.py();
        let this = slf.as_any();
        if this.getattr("offline")?.is_truthy()? {
            return Err(store_error(
                py,
                "offline_mode",
                "Bu oturum çevrimdışı başlatıldı. Normal başlatıcıyla yeniden aç.",
                409,
            ));
        }
        let proceed = with_lock(this, || {
            if this.getattr("pending")?.is_truthy()? {
                return Ok(false);
            }
            this.setattr("pending", true)?;
            this.setattr("error", py.None())?;
            Ok(true)
        })?;
        if !proceed {
            return Ok(false);
        }
        let threading = py.import("threading")?;
        let kw = PyDict::new(py);
        kw.set_item("target", this.getattr("_login")?)?;
        kw.set_item("name", "notes-login")?;
        kw.set_item("daemon", true)?;
        threading.getattr("Thread")?.call((), Some(&kw))?.call_method0("start")?;
        Ok(true)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (args, timeout = 60))]
    fn run(slf: &Bound<'_, Self>, args: &Bound<'_, PyAny>, timeout: i64) -> PyResult<()> {
        let py = slf.py();
        let subprocess = py.import("subprocess")?;
        let kw = PyDict::new(py);
        kw.set_item("cwd", root(py)?)?;
        kw.set_item("stdin", subprocess.getattr("DEVNULL")?)?;
        kw.set_item("stdout", subprocess.getattr("PIPE")?)?;
        kw.set_item("stderr", subprocess.getattr("PIPE")?)?;
        kw.set_item("timeout", timeout)?;
        let result = subprocess.getattr("run")?.call((args,), Some(&kw))?;
        if result.getattr("returncode")?.is_truthy()? {
            return Err(crate::common::value_error("login step failed; private research receipts retained"));
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _login(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.as_any();
        let result = Self::login_body(slf);
        match result {
            Ok(()) => {}
            Err(e) => {
                let store_error_cls = py.import("samsung_notes.storage")?.getattr("StoreError")?;
                if e.value(py).is_instance(&store_error_cls)? {
                    this.setattr("error", e.value(py).getattr("message")?)?;
                } else if login_caught(py, &e) {
                    this.setattr(
                        "error",
                        "Giriş tamamlanamadı. Tek kullanımlık kod tekrar gönderilmedi; yeniden giriş yapabilirsin.",
                    )?;
                } else {
                    this.setattr("pending", false)?;
                    return Err(e);
                }
            }
        }
        this.setattr("pending", false)?;
        Ok(())
    }

    #[cfg(feature = "python")]
    /// Class attribute: exact bytes of session.py's LOGIN_DONE_PAGE.
    #[classattr]
    const LOGIN_DONE_PAGE: &'static str = include_str!("login_done_page.html");

    #[cfg(feature = "python")]
    /// Write session/'login-done.html' (O_WRONLY|O_CREAT|O_TRUNC|O_NOFOLLOW, 0600) and
    /// `subprocess.Popen(['xdg-open', path], ...)` via samsung_notes.session.subprocess;
    /// OSError is swallowed (best effort, never fails the login).
    fn _show_login_done(slf: &Bound<'_, Self>, session: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let result = (|| -> PyResult<()> {
            let path = session.call_method1("__truediv__", ("login-done.html",))?;
            let fs_path: std::path::PathBuf = path.extract()?;
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let page: String = slf.getattr("LOGIN_DONE_PAGE")?.extract()?;
                let mut f = std::fs::OpenOptions::new()
                    .write(true).create(true).truncate(true)
                    .custom_flags(libc::O_NOFOLLOW).mode(0o600)
                    .open(&fs_path)?;
                f.write_all(page.as_bytes())?;
            }
            let subprocess = session_module(py)?.getattr("subprocess")?;
            let devnull = subprocess.getattr("DEVNULL")?;
            let kw = PyDict::new(py);
            kw.set_item("stdin", &devnull)?;
            kw.set_item("stdout", &devnull)?;
            kw.set_item("stderr", &devnull)?;
            kw.set_item("start_new_session", true)?;
            subprocess.getattr("Popen")?.call((vec![notes_platform::open::OPENER.to_string(), path.str()?.to_string()],), Some(&kw))?;
            Ok(())
        })();
        match result {
            Err(e) if e.is_instance_of::<pyo3::exceptions::PyOSError>(py) => Ok(()),
            other => other,
        }
    }

    #[cfg(feature = "python")]
    fn _register(slf: &Bound<'_, Self>, session: &Bound<'_, PyAny>, login_profile: &Bound<'_, PyAny>) -> PyResult<()> {
        register_body(slf, session, login_profile)
    }
}

#[cfg(feature = "python")]
impl SessionManager {
    #[cfg(feature = "python")]
    fn login_body(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.as_any();
        let storage = py.import("samsung_notes.storage")?;
        let secrets = py.import("secrets")?;
        let token_hex6: String = secrets.call_method1("token_hex", (6,))?.extract()?;
        let store = this.getattr("store")?;
        let inputs = storage.getattr("private_directory")?.call1((
            store.getattr("directory")?.call_method1("__truediv__", (format!("login-{token_hex6}"),))?,
        ))?;
        let path = this.getattr("path")?;
        let profile_path = if path.is_none() {
            root(py)?.call_method1("__truediv__", ("samsung_notes/login_profile.json",))?
        } else {
            path.call_method1("__truediv__", ("profile.json",))?
        };
        let profile = load_json(py, &profile_path)?;
        if path.is_none() {
            let existing = store.call_method1("meta", ("client_device_id",))?;
            let identity = if existing.is_truthy()? { existing.extract::<String>()? } else { secrets.call_method1("token_hex", (16,))?.extract()? };
            store.call_method1("set_meta", ("client_device_id", &identity))?;
            let update = PyDict::new(py);
            update.set_item("devicePhysicalAddressText", &identity)?;
            update.set_item("deviceUniqueID", format!("PCID:{identity}"))?;
            profile.call_method1("update", (update,))?;
        }
        save_json(py, &inputs.call_method1("__truediv__", ("profile.json",))?, &profile)?;
        let meta_client = PyDict::new(py);
        meta_client.set_item("value", "a4558shhl3")?;
        save_json(py, &inputs.call_method1("__truediv__", ("metadata-client.json",))?, meta_client.as_any())?;

        let exe = py.import("sys")?.getattr("executable")?;
        SessionManager::run(
            slf,
            &PyTuple::new(
                py,
                [
                    exe.clone(),
                    tools(py)?.call_method1("__truediv__", ("notes_login.py",))?.call_method0("__str__")?,
                    crate::common::s(py, "start"),
                    crate::common::s(py, "--profile"),
                    inputs.call_method1("__truediv__", ("profile.json",))?.call_method0("__str__")?,
                    crate::common::s(py, "--app-id"),
                    inputs.call_method1("__truediv__", ("metadata-client.json",))?.call_method0("__str__")?,
                    crate::common::s(py, "--open"),
                ],
            )?
            .into_any(),
            60,
        )?;

        let research_pending = research(py)?.call_method1("__truediv__", ("pending.json",))?;
        let session = pathlib(py, &load_json(py, &research_pending)?.get_item("transactionDirectory")?)?;
        let sessions_dir = research(py)?.call_method1("__truediv__", ("sessions",))?;
        if session.getattr("parent")?.ne(&sessions_dir)? {
            return Err(crate::common::value_error("invalid login session location"));
        }
        let deadline = load_json(py, &session.call_method1("__truediv__", ("transaction.json",))?)?.get_item("expiresUnixSeconds")?;
        let time = py.import("time")?;
        let closed = this.getattr("closed")?;
        let mut broke = false;
        loop {
            let now = time.call_method0("time")?;
            if !(now.lt(&deadline)? && !closed.call_method0("is_set")?.is_truthy()?) {
                break;
            }
            if session.call_method1("__truediv__", ("callback.json",))?.call_method0("exists")?.is_truthy()? {
                broke = true;
                break;
            }
            closed.call_method1("wait", (0.5,))?;
        }
        if !broke {
            return Err(crate::common::value_error("login expired or application closed"));
        }
        if load_json(py, &session.call_method1("__truediv__", ("callback.json",))?)?.get_item("outcome")?.ne("authorization_code_captured")? {
            return Err(crate::common::value_error("login did not return an authorization code"));
        }
        let notes_cli = root(py)?.call_method1("__truediv__", ("target/debug/notes-cli",))?.call_method0("__str__")?;
        SessionManager::run(
            slf,
            &PyTuple::new(py, [notes_cli, crate::common::s(py, "decrypt-callback"), session.call_method0("__str__")?])?.into_any(),
            60,
        )?;
        for step in ["authenticate", "authorize-notes", "exchange-notes-code"] {
            SessionManager::run(
                slf,
                &PyTuple::new(
                    py,
                    [
                        py.import("sys")?.getattr("executable")?,
                        tools(py)?.call_method1("__truediv__", ("notes_auth_bridge.py",))?.call_method0("__str__")?,
                        crate::common::s(py, step),
                        session.call_method0("__str__")?,
                    ],
                )?
                .into_any(),
                60,
            )?;
        }
        SessionManager::_register(slf, &session, &profile)?;
        with_lock(this, || SessionManager::adopt(slf, &session))?;
        this.getattr("on_connected")?.call0()?;
        this.call_method1("_show_login_done", (&session,))?;
        Ok(())
    }
}

#[cfg(feature = "python")]
fn register_body(slf: &Bound<'_, SessionManager>, session: &Bound<'_, PyAny>, login_profile: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = slf.py();
    let headers = PyDict::new(py);
    for (k, v) in [
        ("x-sc-app-id", "tcl0p60gnz"),
        ("x-sc-device-model", "LinuxResearch"),
        ("x-sc-os-type", "win"),
        ("x-sc-os-version", "1"),
        ("x-sc-device-cc", "TUR"),
        ("Accept", "application/json"),
        ("User-Agent", "SamsungNotesLinux/0.1"),
    ] {
        headers.set_item(k, v)?;
    }
    let request = crate::common::request(py, "https://ers.samsungcloud.com/ers/v1/endpoints", None, "GET", &headers)?;
    let opener = crate::common::build_opener(py)?;
    let open_kw = PyDict::new(py);
    open_kw.set_item("timeout", 20)?;
    let response = opener.call_method("open", (&request,), Some(&open_kw))?;
    response.call_method0("__enter__")?;
    let read = (|| -> PyResult<Vec<u8>> {
        if response.getattr("status")?.ne(200)? {
            return Err(crate::common::value_error("discovery failed"));
        }
        let raw = response.call_method1("read", (128 * 1024 + 1,))?;
        Ok(raw.cast::<pyo3::types::PyBytes>()?.as_bytes().to_vec())
    })();
    response.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
    let raw = read?;
    if raw.len() > 128 * 1024 {
        return Err(crate::common::value_error("discovery response too large"));
    }
    let endpoints = crate::common::json_loads(py, &raw)?;

    let b64 = |data: &[u8]| -> PyResult<String> {
        py.import("base64")?.call_method1("b64encode", (data.to_vec(),))?.call_method0("decode")?.extract()
    };
    let profile = PyDict::new(py);
    profile.set_item("appId", "tcl0p60gnz")?;
    profile.set_item("clientDeviceId", login_profile.get_item("devicePhysicalAddressText")?)?;
    let reg_body = PyDict::new(py);
    let app = PyDict::new(py);
    app.set_item("version", "4.3.831.0")?;
    app.set_item("sdkVersion", "0.1.0")?;
    reg_body.set_item("app", app)?;
    let device = PyDict::new(py);
    for (k, v) in [
        ("osType", "win"), ("osVersion", "1"), ("platformVersion", "Linux"), ("type", "pc"),
        ("countryCode", "TUR"), ("modelName", "LinuxResearch"), ("alias", "Linux Notes"), ("osUserId", "0"),
    ] {
        device.set_item(k, v)?;
    }
    reg_body.set_item("device", device)?;
    profile.set_item("registrationBody", reg_body)?;
    let notes_headers = PyDict::new(py);
    notes_headers.set_item("x-sn-sdk-ver", "5500")?;
    notes_headers.set_item("x-sn-app-ver", "4.3.831.0")?;
    notes_headers.set_item("x-sn-xml-ver", "2")?;
    notes_headers.set_item("x-sn-model-name", b64(b"LinuxResearch")?)?;
    notes_headers.set_item("x-sc-network", "wifi")?;
    notes_headers.set_item("x-sc-e2ee-state", "NONE")?;
    profile.set_item("notesHeaders", notes_headers)?;
    let cursor_obj = PyDict::new(py);
    for (k, v) in [("version", 0i64), ("requestTimeStamp", 0), ("modifiedTime", 0)] {
        cursor_obj.set_item(k, v)?;
    }
    cursor_obj.set_item("encryptedGuid", "")?;
    cursor_obj.set_item("appVersion", "0")?;
    cursor_obj.set_item("sdkVersion", "0")?;
    cursor_obj.set_item("purpose", "INITIAL")?;
    cursor_obj.set_item("type", "NOTE")?;
    let cursor_json = rs_cloud::pyjson::dumps(&rs_cloud::pyjson::from_py(cursor_obj.as_any())?, &rs_cloud::pyjson::COMPACT);
    profile.set_item("initialCursor", b64(cursor_json.as_bytes())?)?;

    save_json(py, &session.call_method1("__truediv__", ("app-endpoints.json",))?, &endpoints)?;
    save_json(py, &session.call_method1("__truediv__", ("app-cloud-profile.json",))?, profile.as_any())?;
    SessionManager::run(
        slf,
        &PyTuple::new(
            py,
            [
                py.import("sys")?.getattr("executable")?,
                tools(py)?.call_method1("__truediv__", ("notes_cloud.py",))?.call_method0("__str__")?,
                crate::common::s(py, "register"),
                session.call_method0("__str__")?,
                crate::common::s(py, "--endpoints"),
                session.call_method1("__truediv__", ("app-endpoints.json",))?.call_method0("__str__")?,
                crate::common::s(py, "--profile"),
                session.call_method1("__truediv__", ("app-cloud-profile.json",))?.call_method0("__str__")?,
            ],
        )?
        .into_any(),
        60,
    )?;
    Ok(())
}

#[cfg(feature = "python")]
fn login_caught(py: Python<'_>, e: &PyErr) -> bool {
    if e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyValueError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
    {
        return true;
    }
    match py.import("subprocess").and_then(|m| m.getattr("SubprocessError")) {
        Ok(cls) => e.value(py).is_instance(&cls).unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn _noop() {}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<SessionManager>()?;
    rs_common::export(m, &["SessionManager"])
}
