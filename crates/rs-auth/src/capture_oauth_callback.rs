//! Rust port of `tools/capture_oauth_callback.py`:
//! CallbackError, validate, capture (the whole module except `main`, which is a
//! CLI wrapper that stays in Python).
//!
//! No network: validate is pure, capture only reads/writes private files with
//! the same O_EXCL single-use claim semantics.
#[cfg(feature = "python")]
use crate::common::{get, is_os_value_unicode, is_strict_int, json_loads, read_bounded};
#[cfg(feature = "python")]
use pyo3::create_exception;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyException, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyModule};
use std::io::Write;
use std::path::PathBuf;

#[cfg(feature = "python")]
create_exception!(capture_oauth_callback, CallbackError, PyException);

const MAX_CALLBACK_BYTES: usize = 64 * 1024;

#[cfg(feature = "python")]
fn cb(message: &str) -> PyErr {
    CallbackError::new_err(message.to_string())
}

#[cfg(feature = "python")]
/// urlsplit both URLs, enforce destination binding, and parse the query.
/// CallbackError (destination mismatch) propagates; ValueError/UnicodeError
/// from urlsplit/parse_qsl become `invalid callback URI` at the call site.
fn parse_section<'py>(py: Python<'py>, expected_callback: &str, callback: &str) -> PyResult<Bound<'py, PyAny>> {
    let urlparse = py.import("urllib.parse")?;
    let expected = urlparse.call_method1("urlsplit", (expected_callback,))?;
    let received = urlparse.call_method1("urlsplit", (callback,))?;
    let attr = |o: &Bound<'py, PyAny>, n: &str| -> PyResult<Bound<'py, PyAny>> { o.getattr(n) };

    let mismatch = attr(&expected, "scheme")?.ne("ms-app")?
        || !attr(&expected, "hostname")?.is_truthy()?
        || !attr(&expected, "username")?.is_none()
        || !attr(&expected, "password")?.is_none()
        || !attr(&expected, "port")?.is_none()
        || attr(&expected, "query")?.is_truthy()?
        || attr(&expected, "fragment")?.is_truthy()?
        || !attr(&received, "username")?.is_none()
        || !attr(&received, "password")?.is_none()
        || !attr(&received, "port")?.is_none()
        || attr(&received, "fragment")?.is_truthy()?
        || attr(&received, "scheme")?.ne(attr(&expected, "scheme")?)?
        || attr(&received, "hostname")?.ne(attr(&expected, "hostname")?)?
        || attr(&received, "path")?.ne(attr(&expected, "path")?)?;
    if mismatch {
        return Err(cb("callback destination does not match transaction"));
    }
    let kw = PyDict::new(py);
    kw.set_item("keep_blank_values", true)?;
    kw.set_item("strict_parsing", true)?;
    kw.set_item("errors", "strict")?;
    kw.set_item("max_num_fields", 32)?;
    urlparse.call_method("parse_qsl", (attr(&received, "query")?,), Some(&kw))
}

#[cfg(feature = "python")]
/// capture_oauth_callback.validate(transaction, callback, now, state_verifier=None).
#[pyfunction]
#[pyo3(signature = (transaction, callback, now, state_verifier = None))]
pub(crate) fn validate<'py>(
    py: Python<'py>,
    transaction: &Bound<'py, PyAny>,
    callback: &Bound<'py, PyAny>,
    now: &Bound<'py, PyAny>,
    state_verifier: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let schema = get(transaction, "schemaVersion")?;
    if !(schema.eq(2)? || schema.eq(3)?)
        || get(transaction, "purpose")?.ne("experimental-signin-preparation")?
    {
        return Err(cb("unsupported transaction; prepare a new v2 transaction"));
    }
    let created = get(transaction, "createdUnixSeconds")?;
    let expires = get(transaction, "expiresUnixSeconds")?;
    if !is_strict_int(&created)
        || !is_strict_int(&expires)
        || expires.sub(&created)?.ne(600)?
        || !(created.le(now)? && now.lt(&expires)?)
    {
        return Err(cb("transaction expired or has invalid timing"));
    }
    let expected_state = get(transaction, "state")?;
    let expected_callback = get(transaction, "expectedCallback")?;
    let state_ok = match expected_state.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.chars().count() == 43 && s.to_str()?.is_ascii(),
        Err(_) => false,
    };
    if !state_ok || !expected_callback.is_instance_of::<pyo3::types::PyString>() {
        return Err(cb("invalid transaction binding"));
    }
    let callback_str = match callback.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(cb("invalid callback URI")),
    };
    if callback_str.len() > MAX_CALLBACK_BYTES
        || callback_str.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
    {
        return Err(cb("invalid callback URI"));
    }
    let expected_callback_str: String = expected_callback.extract()?;

    let pairs = match parse_section(py, &expected_callback_str, &callback_str) {
        Ok(p) => p,
        Err(e) if e.is_instance_of::<CallbackError>(py) => return Err(e),
        Err(e) if e.is_instance_of::<PyValueError>(py) => return Err(cb("invalid callback URI")),
        Err(e) => return Err(e),
    };

    let fields = PyDict::new(py);
    for item in pairs.try_iter()? {
        let item = item?;
        let key = item.get_item(0)?;
        let value = item.get_item(1)?;
        let value_str: String = value.extract()?;
        if fields.contains(&key)? || value_str.chars().count() > 8192 {
            return Err(cb("ambiguous or oversized callback parameters"));
        }
        fields.set_item(key, value)?;
    }
    let state = fields.call_method1("get", ("state", ""))?;
    let state_matches = match state_verifier {
        Some(verifier) => verifier.call1((&state, transaction))?.is_truthy()?,
        None => {
            let state_s: String = state.extract()?;
            let expected_state_s: String = expected_state.extract()?;
            state_s.is_ascii() && crate::common::compare_digest(py, &state_s, &expected_state_s)?
        }
    };
    if !state_matches {
        return Err(cb("callback state does not match transaction"));
    }

    let outcome;
    if fields.contains("error")? {
        let error_val = fields.get_item("error")?.ok_or_else(|| cb("invalid authorization error response"))?;
        if fields.contains("code")? || !error_val.is_truthy()? {
            return Err(cb("invalid authorization error response"));
        }
        outcome = "authorization_error";
    } else {
        let code = fields.call_method1("get", ("code", ""))?;
        let code_s: String = code.extract()?;
        if code_s.is_empty()
            || !code_s.is_ascii()
            || code_s.chars().any(|c| (c as u32) <= 32 || c as u32 == 127)
        {
            return Err(cb("authorization code missing or malformed"));
        }
        outcome = "authorization_code_captured";
    }
    let captured = py.import("builtins")?.getattr("int")?.call1((now,))?;
    let result = PyDict::new(py);
    result.set_item("schemaVersion", 1)?;
    result.set_item("outcome", outcome)?;
    result.set_item("capturedUnixSeconds", captured)?;
    result.set_item("parameters", &fields)?;
    result.set_item("tokenExchangePerformed", false)?;
    result.set_item(
        "stateValidation",
        if state_verifier.is_some() { "encrypted-full-state" } else { "literal-full-state" },
    )?;
    Ok(result.into_any())
}

fn excl_write(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()
}

#[cfg(feature = "python")]
/// capture_oauth_callback.capture(directory, callback, now=None, state_verifier=None).
#[pyfunction]
#[pyo3(signature = (directory, callback, now = None, state_verifier = None))]
fn capture<'py>(
    py: Python<'py>,
    directory: PathBuf,
    callback: &Bound<'py, PyAny>,
    now: Option<&Bound<'py, PyAny>>,
    state_verifier: Option<&Bound<'py, PyAny>>,
) -> PyResult<String> {
    let raw = read_bounded(&directory.join("transaction.json"), MAX_CALLBACK_BYTES)?;
    if raw.len() > MAX_CALLBACK_BYTES {
        return Err(cb("transaction file exceeds size limit"));
    }
    let transaction = match json_loads(py, &raw) {
        Ok(t) => t,
        Err(e) if e.is_instance_of::<PyValueError>(py) => return Err(cb("invalid transaction file")),
        Err(e) => return Err(e),
    };
    if !transaction.is_instance_of::<PyDict>() {
        return Err(cb("invalid transaction file"));
    }
    let now_owned;
    let now_ref = match now {
        Some(n) if !n.is_none() => n,
        _ => {
            now_owned = py.import("time")?.getattr("time")?.call0()?;
            &now_owned
        }
    };
    let result = validate(py, &transaction, callback, now_ref, state_verifier)?;

    match excl_write(&directory.join(".callback-claim"), b"claimed\n") {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(cb("transaction callback was already claimed"));
        }
        Err(e) => return Err(e.into()),
    }
    let serialized = rs_cloud::pyjson::dumps(&rs_cloud::pyjson::from_py(&result)?, &rs_cloud::pyjson::DEFAULT);
    excl_write(&directory.join("callback.json"), serialized.as_bytes())?;
    result.get_item("outcome")?.extract()
}

#[cfg(feature = "python")]
/// capture_oauth_callback.main().
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    let argparse = py.import("argparse")?;
    let pkw = PyDict::new(py);
    pkw.set_item("description", py.import("capture_oauth_callback")?.getattr("__doc__")?)?;
    let parser = argparse.getattr("ArgumentParser")?.call((), Some(&pkw))?;
    let dkw = PyDict::new(py);
    dkw.set_item("type", py.import("pathlib")?.getattr("Path")?)?;
    parser.call_method("add_argument", ("transaction_directory",), Some(&dkw))?;
    let args = parser.call_method0("parse_args")?;
    let directory: PathBuf = args.getattr("transaction_directory")?.extract()?;

    let outcome = (|| -> PyResult<String> {
        let raw_obj = py.import("sys")?.getattr("stdin")?.getattr("buffer")?.call_method1("read", (MAX_CALLBACK_BYTES + 1,))?;
        let raw = raw_obj.cast::<pyo3::types::PyBytes>()?.as_bytes().to_vec();
        if raw.len() > MAX_CALLBACK_BYTES {
            return Err(cb("callback exceeds size limit"));
        }
        let decoded = py.import("builtins")?.getattr("bytes")?.call1((raw,))?.call_method1("decode", ("utf-8",))?;
        let callback = decoded.call_method1("removesuffix", ("\n",))?;
        let out = capture(py, directory.clone(), &callback, None, None)?;
        Ok(out)
    })();
    match outcome {
        Ok(out) => {
            let msg = if out == "authorization_code_captured" {
                "Callback recorded privately; token exchange not performed."
            } else {
                "Authorization error recorded privately; transaction consumed."
            };
            py.import("builtins")?.getattr("print")?.call1((msg,))?;
            Ok(0)
        }
        Err(e) if e.is_instance_of::<CallbackError>(py) => {
            print_stderr(py, &e.value(py).str()?.to_string())?;
            Ok(1)
        }
        Err(e) if is_os_value_unicode(py, &e) => {
            print_stderr(py, "could not read or persist private callback data")?;
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
fn print_stderr(py: Python<'_>, msg: &str) -> PyResult<()> {
    let kw = PyDict::new(py);
    kw.set_item("file", py.import("sys")?.getattr("stderr")?)?;
    py.import("builtins")?.getattr("print")?.call((msg,), Some(&kw))?;
    Ok(())
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CallbackError", m.py().get_type::<CallbackError>())?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    m.add_function(wrap_pyfunction!(capture, m)?)?;
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(m, &["CallbackError", "validate", "capture", "main"])
}
