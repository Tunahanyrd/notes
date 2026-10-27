//! `python -m samsung_notes` (`samsung_notes/__main__.py`) without Python:
//! argument parsing, private data directory + `engine.lock`, runtime file
//! (0600, fsync), optional browser launch file, signals, serve loop.
//! [`start`] is the embeddable part used by the Tauri shell.
use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rs_engine::launcher::{runtime_json, token_hex, token_urlsafe, Args, Parsed, USAGE};
use rs_engine::pyjson::{self, JValue};

use crate::engine::{Engine, Options};
use crate::server::AppServer;

/// Project root: `$LINUX_NOTES_PROJECT_ROOT`, else the first ancestor of the
/// executable that contains `desktop/`, else the current directory.
pub fn project_root() -> PathBuf {
    if let Some(root) = std::env::var_os("LINUX_NOTES_PROJECT_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    if let Ok(exe) = std::env::current_exe() {
        let exe = rs_engine::server_core::realpath(&exe);
        for dir in exe.ancestors().skip(1) {
            if dir.join("desktop").is_dir() && dir.join("crates").is_dir() {
                return dir.to_path_buf();
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Exclusive per-library lock (`engine.lock`, flock), released on drop.
pub struct LibraryLock(i32);

impl Drop for LibraryLock {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the fd opened by `lock_library`, owned only by
        // this LibraryLock and closed exactly once, here.
        unsafe { libc::close(self.0) };
    }
}

pub enum LockError {
    /// "Bu yerel arşiv için Notlar hizmeti zaten açık."
    Busy,
    Io(std::io::Error),
}

pub fn lock_library(directory: &Path) -> Result<LibraryLock, LockError> {
    let path = CString::new(directory.join("engine.lock").as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a NUL-terminated CString alive for the call.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_CREAT | libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
    if fd < 0 {
        return Err(LockError::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: `fd` is the valid descriptor opened above.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fd` is owned here and not used after this close.
        unsafe { libc::close(fd) };
        return Err(if error.raw_os_error() == Some(libc::EWOULDBLOCK) { LockError::Busy } else { LockError::Io(error) });
    }
    Ok(LibraryLock(fd))
}

/// `storage.private_directory(path)` with the launcher's error text.
pub fn prepare_data_dir(path: &Path) -> Result<PathBuf, String> {
    match rs_engine::engine_core::private_directory(path) {
        Ok(d) => Ok(d),
        Err(Some(e)) => Err(e.to_string()),
        Err(None) => Err(rs_engine::engine_core::PRIVATE_DIRECTORY_ERROR.into()),
    }
}

fn write_private_new(path: &Path, data: &[u8], nofollow: bool) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    if nofollow {
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = options.open(path)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()
}

/// Everything the embedded engine needs.
pub struct StartOptions {
    pub data_dir: PathBuf,
    pub port: u16,
    pub offline: bool,
    pub auto_sync: bool,
    pub session: Option<PathBuf>,
    pub frontend_dir: PathBuf,
    pub font_dir: Option<PathBuf>,
}

/// A running engine + loopback server (in-process host for the desktop shell).
pub struct Running {
    pub engine: Arc<Engine>,
    pub server: AppServer,
    accept: Option<std::thread::JoinHandle<()>>,
    _lock: LibraryLock,
}

impl Running {
    pub fn url(&self) -> String {
        self.server.url()
    }
    pub fn port(&self) -> u16 {
        self.server.port
    }
    /// Shut the server down, then `engine.close()`.
    pub fn stop(mut self) {
        self.server.close();
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        self.engine.close();
    }
}

/// Default engine options for the standalone/in-process app.
pub fn engine_options(offline: bool, session: Option<PathBuf>, auto_sync: bool) -> Options {
    Options {
        offline,
        session,
        auto_sync,
        interval: 20.0,
        cloud: crate::cloud_factory(),
        previews: crate::preview_factory(),
        login: Arc::new(crate::login_impl::NativeLogin),
    }
}

/// Prepare the data dir, take the library lock, build the engine and start
/// the server on a background thread.
pub fn start(options: StartOptions) -> Result<Running, String> {
    // SAFETY: umask only sets the process file-mode mask and cannot fail. It is process-wide by design: everything the engine creates (library, sessions, runtime file) must be private.
    unsafe { libc::umask(0o077) };
    crate::report_missing_tools();
    let directory = prepare_data_dir(&options.data_dir)?;
    let lock = match lock_library(&directory) {
        Ok(l) => l,
        Err(LockError::Busy) => return Err("Bu yerel arşiv için Notlar hizmeti zaten açık.".into()),
        Err(LockError::Io(e)) => return Err(e.to_string()),
    };
    let engine = Engine::new(&directory, engine_options(options.offline, options.session.clone(), options.auto_sync)).map_err(|e| e.message)?;
    let token = token_urlsafe(32);
    let server = match AppServer::bind(options.port, Arc::clone(&engine), &token, &options.frontend_dir, options.font_dir.clone()) {
        Ok(s) => s,
        Err(e) => {
            engine.close();
            return Err(e.to_string());
        }
    };
    let accept = Some(server.spawn());
    Ok(Running { engine, server, accept, _lock: lock })
}

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// `main()`; returns the process exit status.
pub fn main(argv: &[String]) -> i32 {
    let root = project_root();
    let args = match Args::parse(argv, &root) {
        Parsed::Run(a) => a,
        Parsed::Help => {
            println!("{}\n\nLaunch the private loopback engine used by the native desktop wrapper.", USAGE);
            return 0;
        }
        Parsed::Error(message) => {
            eprintln!("{}\n__main__.py: error: {}", USAGE, message);
            return 2;
        }
    };
    // SAFETY: umask only sets the process file-mode mask and cannot fail. It is process-wide by design: everything the engine creates (library, sessions, runtime file) must be private.
    unsafe { libc::umask(0o077) };
    crate::report_missing_tools();
    let directory = match prepare_data_dir(&args.data_dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    let lock = match lock_library(&directory) {
        Ok(l) => l,
        Err(LockError::Busy) => {
            println!("Bu yerel arşiv için Notlar hizmeti zaten açık.");
            return 1;
        }
        Err(LockError::Io(e)) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    let runtime = args.runtime_file.clone().unwrap_or_else(|| directory.join(format!("runtime-{}.json", token_hex(6))));
    if let Some(parent) = runtime.parent() {
        if !parent.is_dir() {
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
        }
    }
    let pid = std::process::id();
    let mut engine: Option<Arc<Engine>> = None;
    let mut server: Option<AppServer> = None;
    let outcome = (|| -> Result<(), String> {
        let e = Engine::new(&directory, engine_options(args.offline, args.session.clone(), !args.no_auto_sync)).map_err(|e| e.message)?;
        engine = Some(Arc::clone(&e));
        let token = token_urlsafe(32);
        let s = AppServer::bind(args.port as u16, e, &token, &args.frontend_dir, None).map_err(|e| e.to_string())?;
        server = Some(s.clone());
        let url = s.url();
        write_private_new(&runtime, &runtime_json(s.port, &url, &token, pid), true).map_err(|e| e.to_string())?;
        // SAFETY: `on_signal` is an `extern "C" fn(c_int)` that only stores
        // to an AtomicBool, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        }
        {
            let s = s.clone();
            std::thread::spawn(move || loop {
                if SIGNALLED.load(Ordering::SeqCst) {
                    s.shutdown();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            });
        }
        if !args.no_browser {
            // Private file avoids placing the local API capability in process argv.
            let launch = directory.join(format!("open-{}.html", token_hex(6)));
            let url_json = String::from_utf8(pyjson::dumps(&JValue::str(&url)).unwrap()).unwrap();
            let html = format!("<!doctype html><meta name=\"referrer\" content=\"no-referrer\"><script>location.replace({})</script>", url_json);
            write_private_new(&launch, html.as_bytes(), false).map_err(|e| e.to_string())?;
            let _ = notes_platform::open::command().arg(&launch).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        }
        println!("Notlar yerel hizmeti hazır.");
        let _ = std::io::stdout().flush();
        s.serve_forever(Duration::from_millis(200));
        Ok(())
    })();
    if let Some(s) = &server {
        s.close();
    }
    if let Some(e) = &engine {
        e.close();
    }
    if runtime.exists() {
        if let Ok(raw) = std::fs::read(&runtime) {
            if let Ok(value) = pyjson::loads_lenient(&raw) {
                if pyjson::eq_int(value.get("pid"), pid as i64) {
                    let _ = std::fs::remove_file(&runtime);
                }
            }
        }
    }
    drop(lock);
    match outcome {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("{}", message);
            1
        }
    }
}
