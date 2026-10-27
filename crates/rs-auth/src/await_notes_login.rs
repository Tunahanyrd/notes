//! Rust port of `tools/await_notes_login.py` (one function: main).
//!
//! Waits for the one fixed pending session and runs its UAT exchange. Uses the
//! Python stdlib seams (time/subprocess) and the module globals
//! (notes_login.STATE/ROOT, notes_auth_bridge.authenticate) resolved at call
//! time. Not exercised by the suite; needs a live session to validate.
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyModule};
use std::path::PathBuf;

#[cfg(feature = "python")]
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    let notes_login = py.import("notes_login")?;
    let load_json = notes_login.getattr("load_json")?;
    let state: PathBuf = notes_login.getattr("STATE")?.extract()?;
    let root = notes_login.getattr("ROOT")?;
    let pending = load_json.call1((pathlib(py, &state.join("pending.json"))?,))?;
    let directory_s: String = pending.get_item("transactionDirectory")?.extract()?;
    let directory = PathBuf::from(&directory_s);
    let txn = load_json.call1((pathlib(py, &directory.join("transaction.json"))?,))?;

    let time = py.import("time")?;
    let now0: f64 = time.call_method0("time")?.extract()?;
    let expires: f64 = txn.get_item("expiresUnixSeconds")?.extract()?;
    let deadline = (now0 + 600.0).min(expires);
    let print = py.import("builtins")?.getattr("print")?;

    loop {
        let now: f64 = time.call_method0("time")?.extract()?;
        if now >= deadline {
            break;
        }
        if directory.join("callback.json").exists() {
            let callback = load_json.call1((pathlib(py, &directory.join("callback.json"))?,))?;
            if callback.get_item("outcome")?.ne("authorization_code_captured")? {
                print.call1(("Authorization did not return a code.",))?;
                return Ok(1);
            }
            if !directory.join("decoded-callback.json").exists() {
                let notes_cli = root.call_method1("__truediv__", ("target/debug/notes-cli",))?.str()?.to_string();
                let kw = PyDict::new(py);
                kw.set_item("check", true)?;
                py.import("subprocess")?.call_method(
                    "run",
                    (vec![notes_cli, "decrypt-callback".into(), directory_s.clone()],),
                    Some(&kw),
                )?;
            }
            if directory.join(".account-code-claim").exists() {
                print.call1(("Code already claimed; no repeated request.",))?;
                return Ok(1);
            }
            let ok: bool = py.import("notes_auth_bridge")?.getattr("authenticate")?.call1((pathlib(py, &directory)?,))?.extract()?;
            return Ok(if ok { 0 } else { 1 });
        }
        time.call_method1("sleep", (0.5,))?;
    }
    print.call1(("Login transaction expired without a validated callback.",))?;
    Ok(1)
}

#[cfg(feature = "python")]
fn pathlib<'py>(py: Python<'py>, p: &std::path::Path) -> PyResult<Bound<'py, PyAny>> {
    py.import("pathlib")?.getattr("Path")?.call1((p.to_path_buf(),))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(m, &["main"])
}
