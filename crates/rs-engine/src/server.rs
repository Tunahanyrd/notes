//! Replacement for the Python module `server`: `AppServer` (a
//! `ThreadingHTTPServer` look-alike) and `Handler`. The HTTP work happens in
//! `server_core` (pure Rust); this file binds the socket, runs the accept loop
//! without the GIL and adapts `server_core::Backend` to the Python `Engine`.
use std::ffi::CString;
use std::net::{TcpListener, ToSocketAddrs};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[cfg(feature = "python")]
use pyo3::exceptions::{PyOSError, PyTypeError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyString, PyTuple};

use crate::pyconv;
use crate::server_core::{self, Arg, Backend, Call, Config, Failure, Target};

/// `session.RESEARCH`, computed like the Python module does at import.
pub fn research_dir() -> PathBuf {
    // Linux: $XDG_STATE_HOME or ~/.local/state; macOS: ~/Library/Application
    // Support/org.linuxnotes.desktop (notes_platform::dirs).
    notes_platform::dirs::research_dir(&home_dir())
}

/// `Path.home()` (`os.path.expanduser('~')`).
pub fn home_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) => {
            let s = h.to_string_lossy().trim_end_matches('/').to_string();
            if s.is_empty() {
                PathBuf::from("/")
            } else {
                PathBuf::from(h)
            }
        }
        None => passwd_home().unwrap_or_else(|| PathBuf::from("/")),
    }
}

/// The passwd home directory of the current user. Uses the re-entrant
/// `getpwuid_r` with a caller-owned buffer: the engine is multi-threaded and
/// `getpwuid` returns a pointer into a shared static buffer.
fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = vec![0u8; 16 * 1024];
    // SAFETY: passwd is plain old data; all-zero is a valid initial value.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to a live, exclusively borrowed local of
    // the stated size; getuid has no preconditions.
    let rc = unsafe {
        libc::getpwuid_r(libc::getuid(), &mut entry, buffer.as_mut_ptr().cast(), buffer.len(), &mut result)
    };
    if rc != 0 || result.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: on success pw_dir points to a NUL-terminated string inside
    // `buffer`, which is alive for this borrow.
    let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

#[cfg(feature = "python")]
/// Calls the Python engine. Exceptions map exactly like `Handler.dispatch`.
pub struct PyBackend {
    pub engine: Py<PyAny>,
}

#[cfg(feature = "python")]
fn class<'py>(py: Python<'py>, module: &str, name: &str) -> Option<Bound<'py, PyAny>> {
    py.import(module).ok()?.getattr(name).ok()
}

#[cfg(feature = "python")]
pub fn map_error(py: Python<'_>, err: &PyErr) -> Failure {
    let value = err.value(py);
    let is = |module: &str, name: &str| class(py, module, name).map(|c| value.is_instance(&c).unwrap_or(false)).unwrap_or(false);
    let text = |name: &str| value.getattr(name).ok().and_then(|v| v.str().ok()).map(|s| s.to_string()).unwrap_or_default();
    if is("samsung_notes.storage", "StoreError") {
        let status = value.getattr("status").ok().and_then(|s| s.extract::<u16>().ok()).unwrap_or(500);
        return Failure::Api { code: text("code"), message: text("message"), status };
    }
    if is("samsung_notes.cloud_api", "APIError") {
        let code = text("code");
        let status = if code == "conflict" { 409 } else { 503 };
        return Failure::Api { code, message: text("message"), status };
    }
    if is("samsung_notes.document", "DocumentError") {
        let message = value.str().map(|s| s.to_string()).unwrap_or_default();
        return Failure::Api { code: "unsupported_document".into(), message, status: 422 };
    }
    if is("builtins", "BrokenPipeError") || is("builtins", "ConnectionResetError") || is("builtins", "TimeoutError") {
        return Failure::Disconnect;
    }
    Failure::Internal
}

#[cfg(feature = "python")]
impl PyBackend {
    #[cfg(feature = "python")]
    fn target<'py>(&self, py: Python<'py>, target: Target) -> PyResult<Bound<'py, PyAny>> {
        let engine = self.engine.bind(py).clone();
        Ok(match target {
            Target::Engine => engine,
            Target::Store | Target::Preferences | Target::Shortcuts => engine.getattr("store")?,
            Target::Previews => engine.getattr("previews")?,
            Target::Thumbnails => engine.getattr("thumbnails")?,
            Target::Sessions => engine.getattr("sessions")?,
        })
    }

    #[cfg(feature = "python")]
    fn invoke<'py>(&self, py: Python<'py>, call: &Call) -> PyResult<Bound<'py, PyAny>> {
        let object = self.target(py, call.target)?;
        let mut args: Vec<Bound<'py, PyAny>> = Vec::new();
        let kwargs = PyDict::new(py);
        for arg in &call.args {
            match arg {
                Arg::Json(v) => args.push(pyconv::to_py(py, v)?),
                Arg::Str(s) => args.push(PyString::new(py, s).into_any()),
                Arg::Int(i) => args.push(i.into_pyobject(py)?.into_any()),
                Arg::None => args.push(py.None().into_bound(py)),
                Arg::Bytes(b) => args.push(PyBytes::new(py, b).into_any()),
                Arg::KwFloat(k, v) => kwargs.set_item(*k, *v)?,
            }
        }
        match call.target {
            Target::Preferences | Target::Shortcuts => {
                // server.py: get/set_preferences(engine.store, ...) from the (STABLE)
                // store area, resolved through its samsung_notes module.
                let module = if call.target == Target::Preferences { "samsung_notes.preferences" } else { "samsung_notes.shortcuts" };
                let function = py.import(module)?.getattr(call.method)?;
                args.insert(0, object);
                function.call(PyTuple::new(py, args)?, Some(&kwargs))
            }
            _ => object.getattr(call.method)?.call(PyTuple::new(py, args)?, Some(&kwargs)),
        }
    }

    #[cfg(feature = "python")]
    fn run<T>(&self, f: impl for<'py> FnOnce(Python<'py>) -> PyResult<T>) -> Result<T, Failure> {
        Python::attach(|py| f(py).map_err(|e| map_error(py, &e)))
    }
}

#[cfg(feature = "python")]
impl Backend for PyBackend {
    #[cfg(feature = "python")]
    fn json(&self, call: Call) -> Result<Vec<u8>, Failure> {
        self.run(|py| {
            let result = self.invoke(py, &call)?;
            pyconv::dumps_py(&result)
        })
    }

    #[cfg(feature = "python")]
    fn bytes_or_none(&self, call: Call) -> Result<Option<Vec<u8>>, Failure> {
        self.run(|py| {
            let result = self.invoke(py, &call)?;
            if result.is_none() {
                return Ok(None);
            }
            Ok(Some(result.extract::<Vec<u8>>().map_err(|_| PyTypeError::new_err("preview is not bytes"))?))
        })
    }

    #[cfg(feature = "python")]
    fn bytes_and_flag(&self, call: Call) -> Result<(Option<Vec<u8>>, bool), Failure> {
        self.run(|py| {
            let result = self.invoke(py, &call)?;
            let items: Vec<Bound<'_, PyAny>> = result.try_iter()?.collect::<PyResult<_>>()?;
            if items.len() != 2 {
                return Err(pyo3::exceptions::PyValueError::new_err("expected 2 values to unpack"));
            }
            let complete = items[1].is_truthy()?;
            let raw = if items[0].is_none() {
                None
            } else {
                Some(items[0].extract::<Vec<u8>>().map_err(|_| PyTypeError::new_err("thumbnail is not bytes"))?)
            };
            Ok((raw, complete))
        })
    }

    #[cfg(feature = "python")]
    fn note_text(&self, call: Call) -> Result<(String, String), Failure> {
        self.run(|py| {
            let note = self.invoke(py, &call)?;
            let title: String = note.get_item("title")?.cast_into::<PyString>().map_err(|_| PyTypeError::new_err("title"))?.to_str()?.to_owned();
            let body: String = note.get_item("body")?.cast_into::<PyString>().map_err(|_| PyTypeError::new_err("body"))?.to_str()?.to_owned();
            Ok((title, body))
        })
    }

    fn max_pdf_bytes(&self) -> Result<u64, Failure> {
        self.run(|py| self.engine.bind(py).getattr("MAX_PDF_BYTES")?.extract::<u64>())
    }

    #[cfg(feature = "python")]
    fn is_word_char(&self, c: char) -> bool {
        if c.is_ascii() {
            return c == '_' || c.is_ascii_alphanumeric();
        }
        Python::attach(|py| {
            PyString::new(py, c.encode_utf8(&mut [0u8; 4])).call_method0("isalnum").and_then(|v| v.is_truthy()).unwrap_or(false)
        })
    }
}

struct Control {
    shutdown_request: bool,
    is_shut_down: bool,
}

#[cfg(feature = "python")]
struct Shared {
    cfg: Config,
    backend: PyBackend,
    listener: Mutex<Option<TcpListener>>,
    control: Mutex<Control>,
    changed: Condvar,
}

#[cfg(feature = "python")]
impl Shared {
    fn serve_forever(self: &Arc<Self>, poll_interval: f64) -> Result<(), String> {
        self.control.lock().unwrap().is_shut_down = false;
        let result = (|| {
            loop {
                if self.control.lock().unwrap().shutdown_request {
                    return Ok(());
                }
                let fd = match self.listener.lock().unwrap().as_ref() {
                    Some(l) => l.as_raw_fd(),
                    None => return Err("Invalid file descriptor: -1".to_string()),
                };
                let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                let timeout = if poll_interval.is_finite() && poll_interval >= 0.0 { (poll_interval * 1000.0).ceil() as i32 } else { -1 };
                // SAFETY: `pfd` is a valid, exclusively borrowed pollfd array of length 1.
                let ready = unsafe { libc::poll(&mut pfd, 1, timeout) };
                // Like the Python selector loop: let pending signal handlers
                // (SIGTERM/SIGINT -> shutdown) run on the main thread.
                if ready <= 0 {
                    if let Err(e) = Python::attach(|py| py.check_signals().map_err(|e| e.to_string())) {
                        return Err(e);
                    }
                }
                if self.control.lock().unwrap().shutdown_request {
                    return Ok(());
                }
                if ready > 0 {
                    let accepted = match self.listener.lock().unwrap().as_ref() {
                        Some(l) => l.accept(),
                        None => continue,
                    };
                    if let Ok((stream, _)) = accepted {
                        let _ = stream.set_nonblocking(false);
                        let shared = Arc::clone(self);
                        // daemon_threads = True: connection threads never block shutdown.
                        let _ = std::thread::Builder::new()
                            .name("notes-http".into())
                            .stack_size(64 << 20)
                            .spawn(move || server_core::serve_connection(&shared.cfg, &shared.backend, stream));
                    }
                }
            }
        })();
        let mut control = self.control.lock().unwrap();
        control.shutdown_request = false;
        control.is_shut_down = true;
        self.changed.notify_all();
        result
    }

    fn shutdown(&self) {
        let mut control = self.control.lock().unwrap();
        control.shutdown_request = true;
        while !control.is_shut_down {
            control = self.changed.wait_timeout(control, Duration::from_millis(200)).unwrap().0;
        }
    }
}

/// Bind like `TCPServer(server_bind_and_activate)` with allow_reuse_address=False.
fn bind_ipv4(host: &str, port: u16) -> std::io::Result<TcpListener> {
    let address = (host, port)
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "Address family for hostname not supported"))?;
    let std::net::SocketAddr::V4(v4) = address else { unreachable!() };
    notes_platform::net::bind_tcp_v4(v4, 5)
}

#[cfg(feature = "python")]
fn os_error(e: std::io::Error) -> PyErr {
    match e.raw_os_error() {
        Some(code) => {
            let message = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
            PyOSError::new_err((code, message))
        }
        None => PyOSError::new_err(e.to_string()),
    }
}

#[cfg(feature = "python")]
fn path_object<'py>(py: Python<'py>, path: &std::path::Path) -> PyResult<Bound<'py, PyAny>> {
    py.import("pathlib")?.getattr("Path")?.call1((path,))
}

#[cfg(feature = "python")]
/// ThreadingHTTPServer-compatible loopback server (Rust accept loop + handler).
#[pyclass(subclass, dict, module = "samsung_notes.server", name = "AppServer")]
pub struct AppServer {
    shared: Arc<Shared>,
    #[pyo3(get)]
    engine: Py<PyAny>,
    #[pyo3(get)]
    token: String,
    #[pyo3(get)]
    static_dir: Py<PyAny>,
    #[pyo3(get)]
    font_dir: Py<PyAny>,
    #[pyo3(get)]
    origin: String,
    #[pyo3(get)]
    expected_host: String,
    #[pyo3(get)]
    server_port: u16,
    #[pyo3(get)]
    server_address: (String, u16),
}

#[cfg(feature = "python")]
#[pymethods]
#[allow(non_upper_case_globals)]
impl AppServer {
    #[cfg(feature = "python")]
    #[classattr]
    const daemon_threads: bool = true;
    #[cfg(feature = "python")]
    #[classattr]
    const allow_reuse_address: bool = false;

    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (address, engine, token, static_dir, *, font_dir=None))]
    fn new(py: Python<'_>, address: &Bound<'_, PyAny>, engine: Py<PyAny>, token: String, static_dir: PathBuf, font_dir: Option<PathBuf>) -> PyResult<Self> {
        let (host, port): (String, u16) = address.extract()?;
        let static_resolved = server_core::realpath(&static_dir);
        let font = font_dir.unwrap_or_else(|| research_dir().join("ui-reference/fonts"));
        let listener = py.detach(|| bind_ipv4(&host, port)).map_err(os_error)?;
        let local = listener.local_addr().map_err(os_error)?;
        let cfg = Config { token: token.clone(), port: local.port(), static_dir: static_resolved.clone(), font_dir: font.clone() };
        let shared = Arc::new(Shared {
            backend: PyBackend { engine: engine.clone_ref(py) },
            listener: Mutex::new(Some(listener)),
            control: Mutex::new(Control { shutdown_request: false, is_shut_down: false }),
            changed: Condvar::new(),
            cfg: cfg.clone(),
        });
        Ok(AppServer {
            shared,
            engine,
            token,
            static_dir: path_object(py, &static_resolved)?.unbind(),
            font_dir: path_object(py, &font)?.unbind(),
            origin: cfg.origin(),
            expected_host: cfg.expected_host(),
            server_port: local.port(),
            server_address: (local.ip().to_string(), local.port()),
        })
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (poll_interval=0.5))]
    fn serve_forever(&self, py: Python<'_>, poll_interval: f64) -> PyResult<()> {
        let shared = Arc::clone(&self.shared);
        py.detach(move || shared.serve_forever(poll_interval)).map_err(pyo3::exceptions::PyValueError::new_err)
    }

    #[cfg(feature = "python")]
    fn shutdown(&self, py: Python<'_>) {
        let shared = Arc::clone(&self.shared);
        py.detach(move || shared.shutdown());
    }

    fn server_close(&self) {
        self.shared.listener.lock().unwrap().take();
    }

    fn fileno(&self) -> i32 {
        self.shared.listener.lock().unwrap().as_ref().map(|l| l.as_raw_fd()).unwrap_or(-1)
    }

    #[cfg(feature = "python")]
    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (*_args))]
    fn __exit__(&self, _args: &Bound<'_, PyTuple>) {
        self.server_close();
    }
}

#[cfg(feature = "python")]
/// The request handler (BaseHTTPRequestHandler subclass in Python). Requests
/// are handled by `server_core::serve_connection`; this class carries the
/// Python-visible constants.
#[pyclass(subclass, dict, module = "samsung_notes.server", name = "Handler")]
pub struct Handler {}

#[cfg(feature = "python")]
#[pymethods]
#[allow(non_upper_case_globals)]
impl Handler {
    #[cfg(feature = "python")]
    #[classattr]
    const server_version: &'static str = crate::pyhttp::SERVER_VERSION;
    #[cfg(feature = "python")]
    #[classattr]
    const sys_version: &'static str = "";
    #[cfg(feature = "python")]
    #[classattr]
    const protocol_version: &'static str = crate::pyhttp::PROTOCOL_VERSION;

    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Handler {}
    }

    fn log_message(&self) {}
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let _ = CString::new("");
    m.add_class::<AppServer>()?;
    m.add_class::<Handler>()?;
    m.add("MAX_REQUEST", server_core::MAX_REQUEST)?;
    m.add("PREVIEW_WAIT_SECONDS", server_core::PREVIEW_WAIT_SECONDS as i64)?;
    rs_common::export(m, &["AppServer", "Handler", "MAX_REQUEST", "PREVIEW_WAIT_SECONDS"])
}
