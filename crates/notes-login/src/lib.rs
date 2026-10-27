//! Pure-Rust Samsung sign-in for the Python-free engine.
//!
//! Faithful in-process ports of `SessionManager._login`/`_register`
//! (samsung_notes/session.py), `tools/notes_login.py` (`start`, `dispatch`,
//! `install_handler`, `desktop_quote`, `ensure_state`),
//! `tools/capture_oauth_callback.py`, `tools/exchange_account_code.py`
//! (`prepare`, `load_bound_inputs`), `tools/notes_auth_bridge.py` and the
//! registration half of `tools/notes_cloud.py`, plus the `notes-cli`
//! `prepare-signin` / `decrypt-callback` / `verify-callback-state` commands.
//! Every private file is written like the Python tools (indent-2 `json.dumps`
//! plus a newline, O_EXCL 0600, fsync) with the same keys and order;
//! single-use claims precede every credential request and nothing is retried.
//!
//! No PyO3, no `python3` subprocess. The network is the [`http::Network`]
//! trait ([`net::NotesNet`] is the production implementation on `notes_net`).
//!
//! Known divergences from CPython (all reject/fail where Python would
//! fail, unless noted): JSON bytes are accepted as UTF-8 only (CPython also
//! auto-detects UTF-16/32) and lone surrogate escapes are rejected; a corrupt
//! gzip body is recorded as `transport_or_response_error` with a receipt
//! (CPython's `zlib.error` escapes without one); form values that are JSON
//! lists/objects are refused instead of being sent as their Python `repr`.
pub mod bridge;
pub mod callback;
pub mod cloud;
pub mod decrypt;
pub mod error;
pub mod files;
pub mod flow;
pub mod handler;
pub mod http;
pub mod json;
pub mod login;
pub mod net;
pub mod runtime;
pub mod urls;

pub use error::Error;
pub use flow::{dispatch, start};
pub use handler::{declared_by_package, install_handler, HandlerExec, CALLBACK_SUBCOMMAND, DESKTOP_NAME};
pub use http::{HttpRequest, HttpResponse, NetError, Network};
pub use login::{login, Closed, LoginContext, LoginError, LoginStore, LOGIN_FAILED};
pub use runtime::{Layout, Runtime, SystemRuntime};

use std::ffi::OsString;

/// What `notes_login.main()` prints for every handled failure.
pub const CALLBACK_FAILED: &str = "Giriş işlemi tamamlanamadı; özel işlem kaydını kontrol et.";

/// Entry point for the desktop handler. The engine's `main()` calls this
/// first; it returns `Some(exit_code)` when `args[1]` is the hidden
/// `__notes-login-callback` subcommand (the `ms-app` handler's `Exec=` line),
/// otherwise `None` and the engine starts normally.
pub fn run_callback(args: &[String]) -> Option<i32> {
    let os: Vec<OsString> = args.iter().map(OsString::from).collect();
    run_callback_os(&os)
}

/// [`run_callback`] for raw `std::env::args_os()` (non-UTF-8 safe).
pub fn run_callback_os(args: &[OsString]) -> Option<i32> {
    if args.get(1).map(|a| a.as_os_str()) != Some(std::ffi::OsStr::new(CALLBACK_SUBCOMMAND)) {
        return None;
    }
    Some(callback_main(&args[2..], &Layout::from_env(), &SystemRuntime))
}

/// `notes_login.main()` for `callback URI`: exit 0 on success, 1 with the
/// fixed Turkish message on any handled failure, 2 on a usage error. The URI
/// and any private value are never printed.
pub fn callback_main(rest: &[OsString], layout: &Layout, rt: &dyn Runtime) -> i32 {
    if rest.len() != 1 {
        eprintln!("usage: notes-engine {CALLBACK_SUBCOMMAND} URI");
        return 2;
    }
    let result = match rest[0].to_str() {
        Some(uri) => dispatch(layout, rt, uri).map(|_| ()),
        None => non_utf8_dispatch(layout, rt),
    };
    match result {
        Ok(()) => 0,
        Err(_) => {
            eprintln!("{CALLBACK_FAILED}");
            1
        }
    }
}

/// A non-UTF-8 URI cannot be valid; Python records the handler invocation and
/// then fails while encoding the argument, without consuming the transaction.
fn non_utf8_dispatch(layout: &Layout, rt: &dyn Runtime) -> error::Result<()> {
    files::ensure_state(&layout.state)?;
    let pending = files::load(&layout.state.join("pending.json"))?;
    let Some(json::Value::Str(directory)) = pending.get("transactionDirectory") else {
        return Err(Error::value("invalid pending transaction path"));
    };
    let directory = std::path::PathBuf::from(directory);
    let is_link = std::fs::symlink_metadata(&directory).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    if directory.parent() != Some(layout.sessions().as_path()) || is_link {
        return Err(Error::value("invalid pending transaction path"));
    }
    files::save_json(
        &directory.join(format!("handler-invocation-{}.json", runtime::token_hex(rt, 4))),
        &json::Value::obj(vec![("invokedUnixSeconds", json::Value::Int(runtime::unix_seconds(rt)))]),
    )?;
    Err(Error::value("callback URI is not valid UTF-8"))
}
