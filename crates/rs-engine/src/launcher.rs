//! Replacement for `python -m samsung_notes` (`samsung_notes/__main__.py`):
//! argument parsing, private data directory + single-instance lock, runtime
//! file (port/url/token/pid, 0600, fsync), optional browser launch file, and
//! the serve loop. Pure-Rust parts: `Args::parse`, `token_urlsafe`,
//! `token_hex`, `runtime_json`.
use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyCFunction, PyDict, PyTuple};

use crate::engine_core;
use crate::pyjson::{self, JValue};

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub data_dir: PathBuf,
    pub port: i64,
    pub runtime_file: Option<PathBuf>,
    pub no_browser: bool,
    pub no_auto_sync: bool,
    pub offline: bool,
    pub session: Option<PathBuf>,
    pub frontend_dir: PathBuf,
}

pub const USAGE: &str = "usage: __main__.py [-h] [--data-dir DATA_DIR] [--port PORT] [--runtime-file RUNTIME_FILE] [--no-browser]\n                   [--no-auto-sync] [--offline] [--session SESSION] [--frontend-dir FRONTEND_DIR]";
const OPTIONS: [&str; 9] = ["--help", "--data-dir", "--port", "--runtime-file", "--no-browser", "--no-auto-sync", "--offline", "--session", "--frontend-dir"];

pub enum Parsed {
    Run(Args),
    Help,
    /// argparse error message (exit status 2).
    Error(String),
}

impl Args {
    /// argparse semantics: `--opt value`, `--opt=value`, unique prefixes.
    pub fn parse(argv: &[String], root: &Path) -> Parsed {
        let frontend = match std::env::var("LINUX_NOTES_FRONTEND_DIR") {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => root.join("desktop/dist"),
        };
        let mut args = Args {
            // <state home>/samsung-notes-linux (XDG on Linux, Application Support on macOS).
            data_dir: notes_platform::dirs::library_dir(&crate::server::home_dir()),
            port: 0,
            runtime_file: None,
            no_browser: false,
            no_auto_sync: false,
            offline: false,
            session: None,
            frontend_dir: frontend,
        };
        let mut i = 0;
        while i < argv.len() {
            let arg = &argv[i];
            if arg == "-h" {
                return Parsed::Help;
            }
            let (name, inline) = match arg.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
                _ => (arg.clone(), None),
            };
            if !name.starts_with("--") {
                return Parsed::Error(format!("unrecognized arguments: {}", argv[i..].join(" ")));
            }
            let matches: Vec<&str> = if OPTIONS.contains(&name.as_str()) { vec![OPTIONS.iter().find(|o| **o == name).copied().unwrap()] } else { OPTIONS.iter().copied().filter(|o| o.starts_with(name.as_str())).collect() };
            let option = match matches.len() {
                1 => matches[0],
                0 => return Parsed::Error(format!("unrecognized arguments: {}", argv[i..].join(" "))),
                _ => return Parsed::Error(format!("ambiguous option: {} could match {}", name, matches.join(", "))),
            };
            let flag = matches!(option, "--help" | "--no-browser" | "--no-auto-sync" | "--offline");
            if flag {
                if inline.is_some() {
                    return Parsed::Error(format!("argument {}: ignored explicit argument '{}'", option, inline.unwrap()));
                }
                match option {
                    "--help" => return Parsed::Help,
                    "--no-browser" => args.no_browser = true,
                    "--no-auto-sync" => args.no_auto_sync = true,
                    _ => args.offline = true,
                }
                i += 1;
                continue;
            }
            let value = match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    match argv.get(i) {
                        Some(v) if !(v.starts_with('-') && v.len() > 1 && !v[1..].chars().all(|c| c.is_ascii_digit() || c == '.')) => v.clone(),
                        _ => {
                            let metavar = option.trim_start_matches("--").replace('-', "_").to_uppercase();
                            return Parsed::Error(format!("argument {} {}: expected one argument", option, metavar));
                        }
                    }
                }
            };
            match option {
                "--data-dir" => args.data_dir = PathBuf::from(value),
                "--runtime-file" => args.runtime_file = Some(PathBuf::from(value)),
                "--session" => args.session = Some(PathBuf::from(value)),
                "--frontend-dir" => args.frontend_dir = PathBuf::from(value),
                _ => match engine_core::py_int(&value) {
                    Some(p) => args.port = p,
                    None => return Parsed::Error(format!("argument --port: invalid int value: '{}'", value)),
                },
            }
            i += 1;
        }
        if !(0..=65535).contains(&args.port) {
            return Parsed::Error("port must be between 0 and 65535".into());
        }
        Parsed::Run(args)
    }
}

/// Bytes from the operating system CSPRNG. Fails closed: the bearer token of
/// the local API is built from these, so an unavailable random source must
/// stop the launch instead of yielding a predictable (all-zero) token.
fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("operating system random source unavailable");
    b
}

/// `secrets.token_hex(n)`
pub fn token_hex(n: usize) -> String {
    random_bytes(n).iter().map(|b| format!("{:02x}", b)).collect()
}

/// `secrets.token_urlsafe(n)` (base64url without padding).
pub fn token_urlsafe(n: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let data = random_bytes(n);
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for k in 0..chunk.len() + 1 {
            out.push(ALPHABET[((v >> (18 - 6 * k)) & 63) as usize] as char);
        }
    }
    out
}

/// `json.dump({'port':..., 'url':..., 'token':..., 'pid':...})`
pub fn runtime_json(port: u16, url: &str, token: &str, pid: u32) -> Vec<u8> {
    pyjson::dumps(&JValue::object(vec![
        ("port", JValue::Int(port.to_string())),
        ("url", JValue::str(url)),
        ("token", JValue::str(token)),
        ("pid", JValue::Int(pid.to_string())),
    ]))
    .unwrap()
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

#[cfg(feature = "python")]
/// `main()`; returns the process exit status (argparse errors raise SystemExit(2)).
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    let argv: Vec<String> = py.import("sys")?.getattr("argv")?.extract()?;
    let package_file: PathBuf = py.import("samsung_notes")?.getattr("__file__")?.extract()?;
    let root = crate::server_core::realpath(&package_file).parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let args = match Args::parse(argv.get(1..).unwrap_or(&[]), &root) {
        Parsed::Run(a) => a,
        Parsed::Help => {
            println!("{}\n\nLaunch the private loopback engine used by the native desktop wrapper.", USAGE);
            return Err(pyo3::exceptions::PySystemExit::new_err(0));
        }
        Parsed::Error(message) => {
            eprintln!("{}\n__main__.py: error: {}", USAGE, message);
            return Err(pyo3::exceptions::PySystemExit::new_err(2));
        }
    };
    unsafe { libc::umask(0o077) };
    let directory = match engine_core::private_directory(&args.data_dir) {
        Ok(d) => d,
        Err(Some(e)) => return Err(PyErr::from(e)),
        Err(None) => return Err(pyo3::exceptions::PyValueError::new_err(engine_core::PRIVATE_DIRECTORY_ERROR)),
    };
    let lock_path = CString::new(directory.join("engine.lock").as_os_str().as_bytes()).unwrap();
    let descriptor = unsafe { libc::open(lock_path.as_ptr(), libc::O_CREAT | libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
    if descriptor < 0 {
        return Err(PyErr::from(std::io::Error::last_os_error()));
    }
    if unsafe { libc::flock(descriptor, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            println!("Bu yerel arşiv için Notlar hizmeti zaten açık.");
            unsafe { libc::close(descriptor) };
            return Ok(1);
        }
        unsafe { libc::close(descriptor) };
        return Err(PyErr::from(error));
    }
    let runtime = args.runtime_file.clone().unwrap_or_else(|| directory.join(format!("runtime-{}.json", token_hex(6))));
    if let Some(parent) = runtime.parent() {
        if !parent.is_dir() {
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
        }
    }
    let pid = std::process::id();
    let mut engine: Option<Bound<'_, PyAny>> = None;
    let mut server: Option<Bound<'_, PyAny>> = None;
    let outcome = (|| -> PyResult<()> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("offline", args.offline)?;
        kwargs.set_item("session", args.session.clone())?;
        kwargs.set_item("auto_sync", !args.no_auto_sync)?;
        let e = py.get_type::<crate::engine::Engine>().call((directory.clone(),), Some(&kwargs))?;
        engine = Some(e.clone());
        let token = token_urlsafe(32);
        let s = py.get_type::<crate::server::AppServer>().call1((("127.0.0.1", args.port as u16), &e, &token, args.frontend_dir.clone()))?;
        server = Some(s.clone());
        let port: u16 = s.getattr("server_port")?.extract()?;
        let url = format!("{}/#token={}", s.getattr("origin")?.extract::<String>()?, token);
        write_private_new(&runtime, &runtime_json(port, &url, &token, pid), true).map_err(PyErr::from)?;
        let target = s.clone().unbind();
        let shutdown = PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
            let py = args.py();
            let method = target.bind(py).getattr("shutdown")?;
            py.import("threading")?.getattr("Thread")?.call((), Some(&{
                let k = PyDict::new(py);
                k.set_item("target", method)?;
                k.set_item("daemon", true)?;
                k
            }))?.call_method0("start")?;
            Ok(())
        })?;
        let signal = py.import("signal")?;
        signal.call_method1("signal", (signal.getattr("SIGTERM")?, &shutdown))?;
        signal.call_method1("signal", (signal.getattr("SIGINT")?, &shutdown))?;
        if !args.no_browser {
            // Private file avoids placing the local API capability in process argv.
            let launch = directory.join(format!("open-{}.html", token_hex(6)));
            let url_json = String::from_utf8(pyjson::dumps(&JValue::str(&url)).unwrap()).unwrap();
            let html = format!("<!doctype html><meta name=\"referrer\" content=\"no-referrer\"><script>location.replace({})</script>", url_json);
            write_private_new(&launch, html.as_bytes(), false).map_err(PyErr::from)?;
            let _ = std::process::Command::new("xdg-open").arg(&launch).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        }
        println!("Notlar yerel hizmeti hazır.");
        let _ = std::io::stdout().flush();
        let k = PyDict::new(py);
        k.set_item("poll_interval", 0.2)?;
        s.call_method("serve_forever", (), Some(&k))?;
        Ok(())
    })();
    let cleanup = || -> PyResult<()> {
        if let Some(s) = &server {
            s.call_method0("server_close")?;
        }
        if let Some(e) = &engine {
            e.call_method0("close")?;
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
        Ok(())
    };
    let cleaned = cleanup();
    unsafe { libc::close(descriptor) };
    outcome?;
    cleaned?;
    Ok(0)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(m, &["main"])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn argparse_like() {
        let root = Path::new("/r");
        let argv: Vec<String> = ["--port=0", "--runtime-file", "/x/rt.json", "--no-b", "--off", "--frontend", "/d"].iter().map(|s| s.to_string()).collect();
        let Parsed::Run(a) = Args::parse(&argv, root) else { panic!() };
        assert_eq!((a.port, a.no_browser, a.offline, a.frontend_dir.clone()), (0, true, true, PathBuf::from("/d")));
        assert!(matches!(Args::parse(&["--port".into(), "70000".into()], root), Parsed::Error(_)));
        assert!(matches!(Args::parse(&["--no".into()], root), Parsed::Error(_)));
        assert_eq!(token_urlsafe(32).len(), 43);
        assert_eq!(runtime_json(5, "u", "t", 9), br#"{"port": 5, "url": "u", "token": "t", "pid": 9}"#.to_vec());
    }

    /// The bearer token comes from the OS CSPRNG: never the all-zero
    /// fallback the old `/dev/urandom` reader produced on failure, and two
    /// launches never share a token.
    #[test]
    fn bearer_token_is_random() {
        let a = token_urlsafe(32);
        let b = token_urlsafe(32);
        assert_ne!(a, b);
        assert!(!a.starts_with("AAAAAAAAAAAA"));
        assert_ne!(token_hex(16), "0".repeat(32));
    }
}
