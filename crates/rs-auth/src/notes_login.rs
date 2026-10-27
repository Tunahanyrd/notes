//! Rust port of the pure file/desktop helpers of `tools/notes_login.py`:
//! private_write, load_json, save_json, desktop_quote.
//!
//! The network / xdg desktop-handler / subprocess functions (ensure_state,
//! command, install_handler, dispatch, uninstall_handler, start, status, main)
//! stay in Python: they change the ms-app URL handler and reach the network,
//! and cannot be re-validated here (see PORTING.md).
#[cfg(feature = "python")]
use crate::common::value_error;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyModule, PyTuple};
use std::path::PathBuf;

const DESKTOP_NAME: &str = "samsung-notes-research-callback.desktop";

#[cfg(feature = "python")]
fn module<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    Ok(py.import("notes_login")?.into_any())
}
#[cfg(feature = "python")]
fn state_path<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    module(py)?.getattr("STATE")
}
#[cfg(feature = "python")]
fn py_path<'py>(py: Python<'py>, p: &std::path::Path) -> PyResult<Bound<'py, PyAny>> {
    py.import("pathlib")?.getattr("Path")?.call1((p.to_path_buf(),))
}
#[cfg(feature = "python")]
fn env_path<'py>(py: Python<'py>, var: &str, default_rel: &str) -> PyResult<Bound<'py, PyAny>> {
    let os = py.import("os")?;
    // XDG Base Directory rule (tools/_xdg.py xdg_home): unset, empty or
    // relative values are ignored in favour of the spec default.
    let val = os.getattr("environ")?.call_method1("get", (var, ""))?;
    let pathlib = py.import("pathlib")?.getattr("Path")?;
    if !os.getattr("path")?.call_method1("isabs", (&val,))?.extract::<bool>()? {
        Ok(pathlib.call_method0("home")?.call_method1("__truediv__", (default_rel,))?)
    } else {
        Ok(pathlib.call1((val,))?)
    }
}
#[cfg(feature = "python")]
fn print_line(py: Python<'_>, msg: &str) -> PyResult<()> {
    py.import("builtins")?.getattr("print")?.call1((msg,))?;
    Ok(())
}

#[cfg(feature = "python")]
/// notes_login.private_write: O_CREAT|O_EXCL|O_WRONLY 0600, write, flush, fsync.
#[pyfunction]
fn private_write(path: PathBuf, data: &[u8]) -> PyResult<()> {
    Ok(rs_cloud::util::private_write(&path, data)?)
}

#[cfg(feature = "python")]
/// notes_login.load_json(path, limit=131072).
#[pyfunction]
#[pyo3(signature = (path, limit = 128 * 1024))]
fn load_json<'py>(py: Python<'py>, path: PathBuf, limit: usize) -> PyResult<Bound<'py, PyAny>> {
    rs_cloud::glue::load_json(py, &path, limit)
}

#[cfg(feature = "python")]
/// notes_login.save_json: private_write(json.dumps(data, indent=2) + '\n').
#[pyfunction]
fn save_json(path: PathBuf, data: &Bound<'_, PyAny>) -> PyResult<()> {
    rs_cloud::glue::save_json(&path, data)
}

/// Local launch page (same bytes as notes_login.LANDING_TEMPLATE).
pub const LANDING_TEMPLATE: &str = r##"<!doctype html><html lang="tr"><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Notlar · Samsung hesabıyla giriş</title><style>:root{color-scheme:light dark;--bg:#f3f3f6;--card:#fff;--text:#1f1f24;--muted:#5f6068;--line:#e3e3e8;--accent:#1259c3;--accent-hover:#0e4aa6}@media (prefers-color-scheme:dark){:root{--bg:#1b1b1f;--card:#26262b;--text:#f2f2f5;--muted:#a8a8b2;--line:#38383f;--accent:#4c8dff;--accent-hover:#6aa0ff}}*{box-sizing:border-box}body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--text);font:15px/1.5 "Segoe UI","Noto Sans",system-ui,sans-serif;padding:24px}main{width:min(440px,100%);background:var(--card);border:1px solid var(--line);border-radius:12px;padding:32px;box-shadow:0 8px 24px rgba(0,0,0,.08)}.mark{width:44px;height:44px;border-radius:10px;background:linear-gradient(135deg,#f26549,#e04a2f);display:grid;place-items:center;margin-bottom:20px}.mark span{width:18px;height:22px;border-radius:3px;background:#fff;box-shadow:inset 0 -6px 0 rgba(242,101,73,.25)}h1{font-size:22px;font-weight:600;margin:0 0 6px}p{margin:0;color:var(--muted)}a.button{display:flex;justify-content:center;align-items:center;margin:24px 0;padding:12px 16px;border-radius:8px;background:var(--accent);color:#fff;text-decoration:none;font-weight:600}a.button:hover{background:var(--accent-hover)}a.button:focus-visible{outline:2px solid var(--accent);outline-offset:3px}ol{margin:0;padding:0;list-style:none;counter-reset:step;border-top:1px solid var(--line);padding-top:18px}li{counter-increment:step;display:grid;grid-template-columns:28px 1fr;gap:10px;align-items:start;margin:0 0 12px;color:var(--muted)}li::before{content:counter(step);width:24px;height:24px;border-radius:50%;display:grid;place-items:center;font-size:12px;font-weight:600;color:var(--accent);border:1px solid var(--line)}li b{color:var(--text);font-weight:600}small{display:block;margin-top:8px;color:var(--muted);font-size:12px}</style></head><body><main><div class="mark" aria-hidden="true"><span></span></div><h1>Samsung hesabınla giriş yap</h1><p>Notlar, notlarını Samsung Cloud ile eşitlemek için hesabına bağlanacak.</p><a class="button" rel="noreferrer" href="{{URL}}" autofocus>Samsung hesabıyla devam et</a><ol><li><span><b>Samsung giriş sayfasında</b> hesabınla oturum aç.</span></li><li><span>İsteğe bağlı <b>pazarlama ve kişiselleştirme</b> izinlerini kapalı bırakabilirsin.</span></li><li><span>Sonunda tarayıcı uygulamayı açmak isterse <b>izin ver</b>; Notlar bağlanır ve eşitlemeye başlar.</span></li></ol><small>Bu bağlantı 10 dakika geçerli ve yalnız bir kez kullanılabilir.</small></main></body></html>"##;

#[cfg(feature = "python")]
/// notes_login.login_landing: template with the HTML-escaped sign-in URL.
#[pyfunction]
fn login_landing(py: Python<'_>, url: &str) -> PyResult<String> {
    let escaped: String = py.import("html")?.call_method("escape", (url,), Some(&{
        let d = PyDict::new(py);
        d.set_item("quote", true)?;
        d
    }))?.extract()?;
    Ok(LANDING_TEMPLATE.replace("{{URL}}", &escaped))
}

#[cfg(feature = "python")]
/// notes_login.desktop_quote.
#[pyfunction]
fn desktop_quote(py: Python<'_>, value: &str) -> PyResult<String> {
    if crate::common::re_fullmatch(py, r"[/A-Za-z0-9_.-]+", value)? {
        return Ok(value.to_string());
    }
    if value.contains('%') || value.contains('\n') || value.contains('\r') {
        return Err(value_error("unsupported desktop executable path"));
    }
    // Desktop value escaping applied after Exec argument quoting.
    let mut quoted = String::new();
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    let quoted = quoted.replace('\\', "\\\\");
    Ok(format!("\"{}\"", quoted))
}

#[cfg(feature = "python")]
/// notes_login.ensure_state() — uses the module-global STATE (patchable).
#[pyfunction]
fn ensure_state(py: Python<'_>) -> PyResult<()> {
    let state = state_path(py)?;
    let kw = PyDict::new(py);
    kw.set_item("parents", true)?;
    kw.set_item("exist_ok", true)?;
    kw.set_item("mode", 0o700)?;
    state.call_method("mkdir", (), Some(&kw))?;
    let stat = state.call_method0("stat")?;
    let uid: u32 = stat.getattr("st_uid")?.extract()?;
    let mode: u32 = stat.getattr("st_mode")?.extract()?;
    let getuid: u32 = py.import("os")?.getattr("getuid")?.call0()?.extract()?;
    if state.call_method0("is_symlink")?.is_truthy()? || uid != getuid || mode & 0o077 != 0 {
        return Err(value_error("state directory must be owned by this user and private"));
    }
    Ok(())
}

#[cfg(feature = "python")]
/// notes_login.command(*args): subprocess.run(args, check=True,
/// capture_output=True, text=True).stdout.strip().
#[pyfunction]
#[pyo3(signature = (*args))]
fn command(py: Python<'_>, args: &Bound<'_, PyTuple>) -> PyResult<String> {
    let kw = PyDict::new(py);
    kw.set_item("check", true)?;
    kw.set_item("capture_output", true)?;
    kw.set_item("text", true)?;
    let result = py.import("subprocess")?.getattr("run")?.call((args,), Some(&kw))?;
    result.getattr("stdout")?.call_method0("strip")?.extract()
}

#[cfg(feature = "python")]
fn command_str(py: Python<'_>, args: &[&str]) -> PyResult<String> {
    command(py, &PyTuple::new(py, args)?)
}

#[cfg(feature = "python")]
/// notes_login.install_handler().
#[pyfunction]
fn install_handler(py: Python<'_>) -> PyResult<()> {
    ensure_state(py)?;
    let applications = env_path(py, "XDG_DATA_HOME", ".local/share")?.call_method1("__truediv__", ("applications",))?;
    let mkkw = PyDict::new(py);
    mkkw.set_item("parents", true)?;
    mkkw.set_item("exist_ok", true)?;
    applications.call_method("mkdir", (), Some(&mkkw))?;
    let destination = applications.call_method1("__truediv__", (DESKTOP_NAME,))?;
    let receipt = state_path(py)?.call_method1("__truediv__", ("handler-install.json",))?;
    if receipt.call_method0("exists")?.is_truthy()? {
        if command_str(py, &["xdg-mime", "query", "default", "x-scheme-handler/ms-app"])? != DESKTOP_NAME {
            return Err(value_error("callback handler changed since installation; inspect before replacing"));
        }
        return Ok(());
    }
    let old = command_str(py, &["xdg-mime", "query", "default", "x-scheme-handler/ms-app"])?;
    if !old.is_empty() || destination.call_method0("exists")?.is_truthy()? {
        return Err(value_error("an existing ms-app handler or desktop file must be reviewed first"));
    }
    let config = env_path(py, "XDG_CONFIG_HOME", ".config")?.call_method1("__truediv__", ("mimeapps.list",))?;
    if config.call_method0("exists")?.is_truthy()? {
        let before = config.call_method0("read_bytes")?;
        let backup = state_path(py)?.call_method1("__truediv__", ("mimeapps-before-install.list",))?;
        py.import("notes_login")?.getattr("private_write")?.call1((backup, before))?;
    }
    let exe = py.import("sys")?.getattr("executable")?.extract::<String>()?;
    let self_path: String = module(py)?.getattr("__file__")?.extract()?;
    let resolved: String = py_path(py, std::path::Path::new(&self_path))?.call_method0("resolve")?.str()?.extract()?;
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=Samsung Notes Research Login\nNoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/ms-app;\nExec={} {} callback %u\n",
        desktop_quote(py, &exe)?,
        desktop_quote(py, &resolved)?,
    );
    py.import("notes_login")?.getattr("private_write")?.call1((&destination, desktop.as_bytes()))?;
    let shutil = py.import("shutil")?;
    if shutil.call_method1("which", ("desktop-file-validate",))?.is_truthy()? {
        command_str(py, &["desktop-file-validate", &destination.str()?.to_string()])?;
    }
    if shutil.call_method1("which", ("update-desktop-database",))?.is_truthy()? {
        command_str(py, &["update-desktop-database", &applications.str()?.to_string()])?;
    }
    command_str(py, &["xdg-mime", "default", DESKTOP_NAME, "x-scheme-handler/ms-app"])?;
    if command_str(py, &["xdg-mime", "query", "default", "x-scheme-handler/ms-app"])? != DESKTOP_NAME {
        return Err(value_error("desktop handler registration could not be verified"));
    }
    let data = PyDict::new(py);
    data.set_item("desktopFile", destination.str()?)?;
    data.set_item("previousDefault", &old)?;
    data.set_item("mimeappsFile", config.str()?)?;
    data.set_item("installed", true)?;
    save_json_obj(py, &receipt, &data)?;
    Ok(())
}

#[cfg(feature = "python")]
fn save_json_obj(py: Python<'_>, path: &Bound<'_, PyAny>, data: &Bound<'_, PyAny>) -> PyResult<()> {
    py.import("notes_login")?.getattr("save_json")?.call1((path, data))?;
    Ok(())
}
#[cfg(feature = "python")]
fn load_json_obj<'py>(py: Python<'py>, path: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    py.import("notes_login")?.getattr("load_json")?.call1((path,))
}

#[cfg(feature = "python")]
/// Bound verify_state(ciphertext, transaction) for dispatch (directory is bound
/// via functools.partial).
#[pyfunction]
fn _verify_callback_state(py: Python<'_>, directory: &Bound<'_, PyAny>, ciphertext: &Bound<'_, PyAny>, transaction: &Bound<'_, PyAny>) -> PyResult<bool> {
    let cipher_ok = match ciphertext.cast::<pyo3::types::PyString>() {
        Ok(s) => crate::common::re_fullmatch(py, r"[0-9a-fA-F]{32,32768}", s.to_str()?)?,
        Err(_) => false,
    };
    if transaction.call_method1("get", ("schemaVersion",))?.ne(3)? || !cipher_ok {
        return Ok(false);
    }
    let root = module(py)?.getattr("ROOT")?;
    let notes_cli = root.call_method1("__truediv__", ("target/debug/notes-cli",))?.str()?.to_string();
    let kw = PyDict::new(py);
    kw.set_item("input", ciphertext)?;
    kw.set_item("text", true)?;
    kw.set_item("capture_output", true)?;
    kw.set_item("timeout", 10)?;
    let result = py.import("subprocess")?.getattr("run")?.call(
        (vec![notes_cli, "verify-callback-state".into(), directory.str()?.to_string()],),
        Some(&kw),
    )?;
    Ok(pyo3::types::PyAnyMethods::eq(&result.getattr("returncode")?, 0)?)
}

#[cfg(feature = "python")]
/// notes_login.dispatch(uri).
#[pyfunction]
fn dispatch<'py>(py: Python<'py>, uri: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    ensure_state(py)?;
    let state = state_path(py)?;
    let pending = load_json_obj(py, &state.call_method1("__truediv__", ("pending.json",))?)?;
    let directory = py_path(py, std::path::Path::new(&pending.get_item("transactionDirectory")?.extract::<String>()?))?;
    let sessions = state.call_method1("__truediv__", ("sessions",))?;
    if directory.getattr("parent")?.ne(&sessions)? || directory.call_method0("is_symlink")?.is_truthy()? {
        return Err(value_error("invalid pending transaction path"));
    }
    let time = py.import("time")?;
    let now_int = py.import("builtins")?.getattr("int")?.call1((time.call_method0("time")?,))?;
    let invocation = PyDict::new(py);
    invocation.set_item("invokedUnixSeconds", now_int)?;
    let inv_name = format!("handler-invocation-{}.json", rs_cloud::util::token_hex(4));
    save_json_obj(py, &directory.call_method1("__truediv__", (inv_name,))?, invocation.as_any())?;

    let verify = py.import("functools")?.getattr("partial")?.call1((wrap_pyfunction!(_verify_callback_state, &module(py)?.cast_into::<PyModule>()?)?, &directory))?;
    let capture = py.import("capture_oauth_callback")?.getattr("capture")?;
    let cap_kw = PyDict::new(py);
    cap_kw.set_item("state_verifier", &verify)?;
    let outcome = match capture.call((&directory, uri), Some(&cap_kw)) {
        Ok(o) => o,
        Err(e) => {
            let callback_error = py.import("capture_oauth_callback")?.getattr("CallbackError")?;
            if e.value(py).is_instance(&callback_error)? {
                let urlparse = py.import("urllib.parse")?;
                let transaction = load_json_obj(py, &directory.call_method1("__truediv__", ("transaction.json",))?)?;
                let expected = urlparse.call_method1("urlsplit", (transaction.get_item("expectedCallback")?,))?;
                let received = urlparse.call_method1("urlsplit", (uri,))?;
                let uri_len = uri.call_method0("encode")?.len()?;
                if uri_len <= 65536
                    && received.getattr("scheme")?.eq(expected.getattr("scheme")?)?
                    && received.getattr("hostname")?.eq(expected.getattr("hostname")?)?
                {
                    let rej = PyDict::new(py);
                    rej.set_item("uri", uri)?;
                    rej.set_item("reason", e.value(py).str()?)?;
                    let now2 = py.import("builtins")?.getattr("int")?.call1((time.call_method0("time")?,))?;
                    rej.set_item("receivedUnixSeconds", now2)?;
                    let rej_name = format!("rejected-callback-{}.json", rs_cloud::util::token_hex(4));
                    save_json_obj(py, &directory.call_method1("__truediv__", (rej_name,))?, rej.as_any())?;
                }
            }
            return Err(e);
        }
    };
    let shutil = py.import("shutil")?;
    if shutil.call_method1("which", ("notify-send",))?.is_truthy()? {
        let msg = if outcome.eq("authorization_code_captured")? {
            "Giriş yanıtı alındı. İşleme terminalde devam ediliyor."
        } else {
            "Samsung giriş işlemi hata yanıtıyla tamamlandı."
        };
        let kw = PyDict::new(py);
        let subprocess = py.import("subprocess")?;
        kw.set_item("stdout", subprocess.getattr("DEVNULL")?)?;
        kw.set_item("stderr", subprocess.getattr("DEVNULL")?)?;
        subprocess.getattr("run")?.call(
            (vec!["notify-send".to_string(), "Samsung Notes araştırması".into(), msg.into()],),
            Some(&kw),
        )?;
    }
    Ok(outcome)
}

#[cfg(feature = "python")]
/// notes_login.uninstall_handler().
#[pyfunction]
fn uninstall_handler(py: Python<'_>) -> PyResult<()> {
    ensure_state(py)?;
    let receipt = load_json_obj(py, &state_path(py)?.call_method1("__truediv__", ("handler-install.json",))?)?;
    let current = command_str(py, &["xdg-mime", "query", "default", "x-scheme-handler/ms-app"])?;
    if current != DESKTOP_NAME {
        return Err(value_error("handler selection changed; refusing to overwrite another preference"));
    }
    let config = py_path(py, std::path::Path::new(&receipt.get_item("mimeappsFile")?.extract::<String>()?))?;
    let before = config.call_method0("read_bytes")?;
    let expected = format!("x-scheme-handler/ms-app={}\n", DESKTOP_NAME);
    let expected_b = pyo3::types::PyBytes::new(py, expected.as_bytes());
    let count: usize = before.call_method1("count", (&expected_b,))?.extract()?;
    if count != 1 || receipt.get_item("previousDefault")?.is_truthy()? {
        return Err(value_error("handler association changed; manual reconciliation needed"));
    }
    let stamp = rs_cloud::util::token_hex(4);
    let state = state_path(py)?;
    let backup = state.call_method1("__truediv__", (format!("mimeapps-before-uninstall-{stamp}.list"),))?;
    py.import("notes_login")?.getattr("private_write")?.call1((backup, &before))?;
    let staged = config.call_method1("with_name", (format!("mimeapps-notes-rollback-{stamp}.list"),))?;
    let replaced = before.call_method1("replace", (&expected_b, pyo3::types::PyBytes::new(py, b""), 1))?;
    py.import("notes_login")?.getattr("private_write")?.call1((&staged, replaced))?;
    let os = py.import("os")?;
    let mode: u32 = config.call_method0("stat")?.getattr("st_mode")?.extract::<u32>()? & 0o777;
    os.call_method1("chmod", (&staged, mode))?;
    os.call_method1("replace", (&staged, &config))?;
    let destination = py_path(py, std::path::Path::new(&receipt.get_item("desktopFile")?.extract::<String>()?))?;
    os.call_method1("replace", (&destination, state.call_method1("__truediv__", (format!("removed-handler-{stamp}.desktop"),))?))?;
    let shutil = py.import("shutil")?;
    if shutil.call_method1("which", ("update-desktop-database",))?.is_truthy()? {
        command_str(py, &["update-desktop-database", &destination.getattr("parent")?.str()?.to_string()])?;
    }
    os.call_method1(
        "replace",
        (
            state.call_method1("__truediv__", ("handler-install.json",))?,
            state.call_method1("__truediv__", (format!("handler-uninstalled-{stamp}.json"),))?,
        ),
    )?;
    print_line(py, "Araştırma giriş işleyicisi kaldırıldı; diğer masaüstü tercihleri korundu.")
}

#[cfg(feature = "python")]
/// notes_login.status().
#[pyfunction]
fn status(py: Python<'_>) -> PyResult<()> {
    ensure_state(py)?;
    let pending = load_json_obj(py, &state_path(py)?.call_method1("__truediv__", ("pending.json",))?)?;
    let directory = py_path(py, std::path::Path::new(&pending.get_item("transactionDirectory")?.extract::<String>()?))?;
    let txn = load_json_obj(py, &directory.call_method1("__truediv__", ("transaction.json",))?)?;
    let callback_present = directory.call_method1("__truediv__", ("callback.json",))?.call_method0("exists")?;
    let now: f64 = py.import("time")?.call_method0("time")?.extract()?;
    let expires: f64 = txn.get_item("expiresUnixSeconds")?.extract()?;
    let result = PyDict::new(py);
    result.set_item("callbackPresent", &callback_present)?;
    result.set_item("expired", now >= expires)?;
    if callback_present.is_truthy()? {
        let callback = load_json_obj(py, &directory.call_method1("__truediv__", ("callback.json",))?)?;
        result.set_item("outcome", callback.get_item("outcome")?)?;
        let sorted = py.import("builtins")?.getattr("sorted")?.call1((callback.get_item("parameters")?,))?;
        result.set_item("parameterNames", sorted)?;
    }
    let dumps = py.import("json")?.getattr("dumps")?.call1((&result,))?;
    py.import("builtins")?.getattr("print")?.call1((dumps,))?;
    Ok(())
}

#[cfg(feature = "python")]
/// notes_login.start(profile_path, app_id_path, open_browser).
#[pyfunction]
fn start(py: Python<'_>, profile_path: &Bound<'_, PyAny>, app_id_path: &Bound<'_, PyAny>, open_browser: bool) -> PyResult<()> {
    start_impl(py, profile_path, app_id_path, open_browser)
}

#[cfg(feature = "python")]
/// notes_login.main().
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    let argparse = py.import("argparse")?;
    let pkw = PyDict::new(py);
    pkw.set_item("description", module(py)?.getattr("__doc__")?)?;
    let parser = argparse.getattr("ArgumentParser")?.call((), Some(&pkw))?;
    let subkw = PyDict::new(py);
    subkw.set_item("dest", "command")?;
    subkw.set_item("required", true)?;
    let sub = parser.call_method("add_subparsers", (), Some(&subkw))?;
    let path_type = py.import("pathlib")?.getattr("Path")?;
    let begin = sub.call_method1("add_parser", ("start",))?;
    for (opt, required) in [("--profile", true), ("--app-id", true)] {
        let kw = PyDict::new(py);
        kw.set_item("required", required)?;
        kw.set_item("type", &path_type)?;
        begin.call_method("add_argument", (opt,), Some(&kw))?;
    }
    let open_kw = PyDict::new(py);
    open_kw.set_item("action", "store_true")?;
    begin.call_method("add_argument", ("--open",), Some(&open_kw))?;
    let cb = sub.call_method1("add_parser", ("callback",))?;
    cb.call_method1("add_argument", ("uri",))?;
    sub.call_method1("add_parser", ("status",))?;
    sub.call_method1("add_parser", ("install-handler",))?;
    sub.call_method1("add_parser", ("uninstall-handler",))?;
    let args = parser.call_method0("parse_args")?;
    let cmd: String = args.getattr("command")?.extract()?;

    let run = || -> PyResult<()> {
        match cmd.as_str() {
            "start" => start(py, &args.getattr("profile")?, &args.getattr("app_id")?, args.getattr("open")?.extract()?),
            "callback" => {
                dispatch(py, &args.getattr("uri")?)?;
                Ok(())
            }
            "install-handler" => install_handler(py),
            "uninstall-handler" => uninstall_handler(py),
            _ => status(py),
        }
    };
    match run() {
        Ok(()) => Ok(0),
        Err(e) if is_login_error(py, &e) => {
            let kw = PyDict::new(py);
            kw.set_item("file", py.import("sys")?.getattr("stderr")?)?;
            py.import("builtins")?.getattr("print")?.call(("Giriş işlemi tamamlanamadı; özel işlem kaydını kontrol et.",), Some(&kw))?;
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
fn is_login_error(py: Python<'_>, e: &PyErr) -> bool {
    if e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyValueError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
    {
        return true;
    }
    // CallbackError, subprocess.SubprocessError
    let classes = [("capture_oauth_callback", "CallbackError"), ("subprocess", "SubprocessError")];
    for (module, name) in classes {
        if let Ok(cls) = py.import(module).and_then(|m| m.getattr(name)) {
            if e.value(py).is_instance(&cls).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

#[cfg(feature = "python")]
fn start_impl(py: Python<'_>, profile_path: &Bound<'_, PyAny>, app_id_path: &Bound<'_, PyAny>, open_browser: bool) -> PyResult<()> {
    ensure_state(py)?;
    install_handler(py)?;
    let state = state_path(py)?;
    let sessions = state.call_method1("__truediv__", ("sessions",))?;
    let mkkw = PyDict::new(py);
    mkkw.set_item("mode", 0o700)?;
    mkkw.set_item("exist_ok", true)?;
    sessions.call_method("mkdir", (), Some(&mkkw))?;
    let pending_path = state.call_method1("__truediv__", ("pending.json",))?;
    let time = py.import("time")?;
    if pending_path.call_method0("exists")?.is_truthy()? {
        let previous = load_json_obj(py, &pending_path)?;
        let txn = py_path(py, std::path::Path::new(&previous.get_item("transactionDirectory")?.extract::<String>()?))?;
        if txn.getattr("parent")?.ne(&sessions)? {
            return Err(value_error("invalid previous transaction path"));
        }
        let metadata = load_json_obj(py, &txn.call_method1("__truediv__", ("transaction.json",))?)?;
        let now: f64 = time.call_method0("time")?.extract()?;
        let expires: f64 = metadata.get_item("expiresUnixSeconds")?.extract()?;
        if now < expires && !txn.call_method1("__truediv__", (".callback-claim",))?.call_method0("exists")?.is_truthy()? {
            return Err(value_error("a live login transaction is already pending"));
        }
    }
    let stamp = rs_cloud::util::token_hex(4);
    let datetime = py.import("datetime")?;
    let utc = datetime.getattr("timezone")?.getattr("utc")?;
    let now_dt = datetime.getattr("datetime")?.call_method1("now", (utc,))?;
    let prefix: String = now_dt.call_method1("strftime", ("%Y%m%dT%H%M%SZ-",))?.extract()?;
    let name = format!("{prefix}{stamp}");
    let work = sessions.call_method1("__truediv__", (format!("{name}-inputs"),))?;
    let wkw = PyDict::new(py);
    wkw.set_item("mode", 0o700)?;
    work.call_method("mkdir", (), Some(&wkw))?;
    let app_id_v = load_json_obj(py, app_id_path)?.get_item("value")?;
    let app_id = match app_id_v.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(value_error("invalid metadata application identity")),
    };
    if !crate::common::re_fullmatch(py, r"[A-Za-z0-9]{1,128}", &app_id)? {
        return Err(value_error("invalid metadata application identity"));
    }
    let profile = py.import("notes_login")?.getattr("load_json")?.call1((profile_path, 32 * 1024))?;
    save_json_obj(py, &work.call_method1("__truediv__", ("profile.json",))?, &profile)?;

    let endpoint: String = py.import("probe_account_entrypoint")?.getattr("ENDPOINT")?.extract()?;
    let auth = {
        let b64 = py.import("base64")?.call_method1("b64encode", (format!("{app_id}:").into_bytes(),))?.call_method0("decode")?;
        format!("Basic {}", b64.extract::<String>()?)
    };
    let headers = PyDict::new(py);
    headers.set_item("x-osp-appId", &app_id)?;
    headers.set_item("Authorization", auth)?;
    headers.set_item("Accept", "application/json")?;
    headers.set_item("User-Agent", "SamsungNotesLinuxResearch/0.1")?;
    let request = crate::common::request(py, &format!("{endpoint}?countryCode=TR"), None, "GET", &headers)?;
    let opener = crate::common::build_opener(py)?;
    let open_kw = PyDict::new(py);
    open_kw.set_item("timeout", 15)?;
    let response = opener.call_method("open", (&request,), Some(&open_kw))?;
    response.call_method0("__enter__")?;
    let read = (|| -> PyResult<Vec<u8>> {
        if response.getattr("status")?.ne(200)? {
            return Err(value_error("metadata endpoint did not return success"));
        }
        let raw = response.call_method1("read", (128 * 1024 + 1,))?;
        Ok(raw.cast::<pyo3::types::PyBytes>()?.as_bytes().to_vec())
    })();
    response.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
    let body = read?;
    if body.len() > 128 * 1024 {
        return Err(value_error("metadata exceeds size limit"));
    }
    rs_cloud::util::private_write(&std::path::PathBuf::from(work.call_method1("__truediv__", ("entrypoint.json",))?.str()?.to_string()), &body)?;

    let root = module(py)?.getattr("ROOT")?;
    let notes_cli = root.call_method1("__truediv__", ("target/debug/notes-cli",))?.str()?.to_string();
    let directory = sessions.call_method1("__truediv__", (&name,))?;
    command(
        py,
        &PyTuple::new(
            py,
            [
                notes_cli,
                "prepare-signin".into(),
                work.call_method1("__truediv__", ("entrypoint.json",))?.str()?.to_string(),
                work.call_method1("__truediv__", ("profile.json",))?.str()?.to_string(),
                directory.str()?.to_string(),
            ],
        )?,
    )?;
    let profile_bytes = work.call_method1("__truediv__", ("profile.json",))?.call_method0("read_bytes")?;
    py.import("notes_login")?.getattr("private_write")?.call1((directory.call_method1("__truediv__", ("profile.json",))?, profile_bytes))?;
    let pending = state.call_method1("__truediv__", (format!("pending-{}.json", rs_cloud::util::token_hex(4)),))?;
    let pdata = PyDict::new(py);
    pdata.set_item("transactionDirectory", directory.str()?)?;
    save_json_obj(py, &pending, pdata.as_any())?;
    py.import("os")?.call_method1("replace", (&pending, &pending_path))?;

    let url: String = directory.call_method1("__truediv__", ("url.txt",))?.call_method0("read_text")?.extract()?;
    let landing = login_landing(py, &url)?;
    let landing_path = directory.call_method1("__truediv__", ("open-login.html",))?;
    py.import("notes_login")?.getattr("private_write")?.call1((&landing_path, landing.as_bytes()))?;
    if open_browser {
        let subprocess = py.import("subprocess")?;
        let kw = PyDict::new(py);
        kw.set_item("stdout", subprocess.getattr("DEVNULL")?)?;
        kw.set_item("stderr", subprocess.getattr("DEVNULL")?)?;
        kw.set_item("start_new_session", true)?;
        subprocess.getattr("Popen")?.call((vec![notes_platform::open::OPENER.to_string(), landing_path.str()?.to_string()],), Some(&kw))?;
    }
    py.import("builtins")?.getattr("print")?.call1(("Giriş hazır. Yerel bağlantı sayfası:", &landing_path))?;
    print_line(py, "10 dakika geçerli; Samsung girişinden sonra uygulamayı açma isteğini kabul et.")
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(private_write, m)?)?;
    m.add_function(wrap_pyfunction!(load_json, m)?)?;
    m.add_function(wrap_pyfunction!(save_json, m)?)?;
    m.add_function(wrap_pyfunction!(desktop_quote, m)?)?;
    m.add_function(wrap_pyfunction!(login_landing, m)?)?;
    m.add_function(wrap_pyfunction!(ensure_state, m)?)?;
    m.add_function(wrap_pyfunction!(command, m)?)?;
    m.add_function(wrap_pyfunction!(install_handler, m)?)?;
    m.add_function(wrap_pyfunction!(dispatch, m)?)?;
    m.add_function(wrap_pyfunction!(uninstall_handler, m)?)?;
    m.add_function(wrap_pyfunction!(start, m)?)?;
    m.add_function(wrap_pyfunction!(status, m)?)?;
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(
        m,
        &[
            "private_write", "load_json", "save_json", "desktop_quote", "login_landing", "ensure_state", "command",
            "install_handler", "dispatch", "uninstall_handler", "start", "status", "main",
        ],
    )
}
