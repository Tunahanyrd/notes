//! Rust port of the pure helper of `tools/probe_account_entrypoint.py`:
//! save_private.
//!
//! NoRedirect (a urllib.request.HTTPRedirectHandler subclass) and `main` (one
//! anonymous metadata request) stay in Python (see PORTING.md).
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyModule, PyTuple};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

#[cfg(feature = "python")]
/// `NoRedirect.redirect_request(...) -> None` (refuse every redirect).
#[pyfunction]
#[pyo3(signature = (*_args, **_kwargs))]
fn _noredirect_redirect_request(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {
    // returns None
}

#[cfg(feature = "python")]
/// probe_account_entrypoint.save_private: open 'xb' (O_CREAT|O_EXCL), chmod 0600, write.
#[pyfunction]
fn save_private(path: PathBuf, data: &[u8]) -> PyResult<()> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(data)?;
    Ok(())
}

const MAX_BYTES: usize = 128 * 1024;
const ENDPOINT: &str = "https://account.samsung.com/accounts/WINSDK/getEntryPoint";

#[cfg(feature = "python")]
/// probe_account_entrypoint.main(): one bounded anonymous metadata probe.
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    use crate::common::value_error;
    let argparse = py.import("argparse")?;
    let pkw = PyDict::new(py);
    pkw.set_item("description", py.import("probe_account_entrypoint")?.getattr("__doc__")?)?;
    let parser = argparse.getattr("ArgumentParser")?.call((), Some(&pkw))?;
    let path_type = py.import("pathlib")?.getattr("Path")?;
    let add = |name: &str, kw: &Bound<'_, PyDict>| -> PyResult<()> {
        parser.call_method("add_argument", (name,), Some(kw))?;
        Ok(())
    };
    let kw = PyDict::new(py);
    kw.set_item("required", true)?;
    kw.set_item("type", &path_type)?;
    add("--client-id-file", &kw)?;
    let kw = PyDict::new(py);
    kw.set_item("default", "TR")?;
    kw.set_item("help", "Research locale, not account country detection")?;
    add("--country", &kw)?;
    let kw = PyDict::new(py);
    kw.set_item("type", &path_type)?;
    kw.set_item("default", path_type.call1(("artifacts/analysis/account-entrypoint-probes",))?)?;
    add("--output", &kw)?;
    let args = parser.call_method0("parse_args")?;
    let country: String = args.getattr("country")?.extract()?;
    if !crate::common::re_fullmatch(py, r"[A-Z]{2}", &country)? {
        parser.call_method1("error", ("country must be two uppercase ASCII letters",))?;
    }
    let raw_id = std::fs::read(args.getattr("client_id_file")?.call_method0("__fspath__")?.extract::<std::path::PathBuf>()?)?;
    let source = crate::common::json_loads(py, &raw_id)?;
    let app_id_v = source.get_item("value")?;
    let app_id = match app_id_v.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => {
            parser.call_method1("error", ("invalid shipped application identifier",))?;
            return Ok(2);
        }
    };
    if !crate::common::re_fullmatch(py, r"[A-Za-z0-9]{1,128}", &app_id)? {
        parser.call_method1("error", ("invalid shipped application identifier",))?;
    }
    let datetime = py.import("datetime")?;
    let utc = datetime.getattr("timezone")?.getattr("utc")?;
    let now = datetime.getattr("datetime")?.call_method1("now", (&utc,))?;
    let prefix: String = now.call_method1("strftime", ("%Y%m%dT%H%M%SZ-",))?.extract()?;
    let directory = args.getattr("output")?.call_method1("__truediv__", (format!("{prefix}{}", rs_cloud::util::token_hex(4)),))?;
    let mkkw = PyDict::new(py);
    mkkw.set_item("parents", true)?;
    mkkw.set_item("mode", 0o700)?;
    directory.call_method("mkdir", (), Some(&mkkw))?;

    let query = rs_cloud::util::urlencode(&[("countryCode", country.clone())]);
    let auth_b64: String = py.import("base64")?.call_method1("b64encode", (format!("{app_id}:").into_bytes(),))?.call_method0("decode")?.extract()?;
    let authorization = format!("Basic {auth_b64}");
    let headers = PyDict::new(py);
    headers.set_item("x-osp-appId", &app_id)?;
    headers.set_item("Authorization", &authorization)?;
    headers.set_item("Accept", "application/json")?;
    headers.set_item("User-Agent", "SamsungNotesLinuxResearch/0.1")?;
    let request = crate::common::request(py, &format!("{ENDPOINT}?{query}"), None, "GET", &headers)?;

    let hashlib = py.import("hashlib")?;
    let source_sha: String = hashlib.call_method1("sha256", (raw_id.clone(),))?.call_method0("hexdigest")?.extract()?;
    let app_sha: String = hashlib.call_method1("sha256", (app_id.clone().into_bytes(),))?.call_method0("hexdigest")?.extract()?;
    let header_names = py.import("builtins")?.getattr("sorted")?.call1((request.getattr("headers")?,))?;

    let receipt = PyDict::new(py);
    receipt.set_item("startedAtUtc", now.call_method0("isoformat")?)?;
    receipt.set_item("purpose", "Anonymous entrypoint metadata compatibility probe, not user login or Cloud access")?;
    receipt.set_item("method", "GET")?;
    receipt.set_item("endpoint", ENDPOINT)?;
    receipt.set_item("countryCode", &country)?;
    receipt.set_item("countrySource", "Explicit research locale; not inferred from a user account")?;
    receipt.set_item("clientIdentifierSourceSha256", source_sha)?;
    receipt.set_item("clientIdentifierSha256", app_sha)?;
    receipt.set_item("authorizationRecipe", "Basic Base64(UTF8(shipped appId + colon + empty string))")?;
    receipt.set_item("requestHeaderNames", header_names)?;
    receipt.set_item("userSessionAttached", false)?;
    receipt.set_item("deviceIdentityAttached", false)?;
    receipt.set_item("cookieJar", false)?;
    receipt.set_item("redirectsFollowed", false)?;
    receipt.set_item("optionalClientMetadataHeaders", "omitted in this minimal experiment")?;
    receipt.set_item("tlsCertificateVerification", true)?;
    receipt.set_item("maximumResponseBytes", MAX_BYTES)?;

    let outcome = (|| -> PyResult<()> {
        let opener = crate::common::build_opener(py)?;
        let response = crate::common::opener_open(py, &opener, &request, 15)?;
        response.call_method0("__enter__")?;
        let read = (|| -> PyResult<Vec<u8>> {
            receipt.set_item("httpStatus", response.getattr("code")?)?;
            receipt.set_item("contentType", response.getattr("headers")?.call_method0("get_content_type")?)?;
            let body = response.call_method1("read", (MAX_BYTES + 1,))?;
            Ok(body.cast::<pyo3::types::PyBytes>()?.as_bytes().to_vec())
        })();
        response.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
        let body = read?;
        receipt.set_item("responseBytesRead", body.len())?;
        if body.len() > MAX_BYTES {
            receipt.set_item("result", "response_limit_exceeded")?;
            return Ok(());
        }
        receipt.set_item("bodySha256", rs_cloud::util::sha256_hex(&body))?;
        match crate::common::json_loads(py, &body) {
            Err(e) if crate::common::is_os_value_unicode(py, &e) => {
                receipt.set_item("result", "non_json_response")?;
            }
            Err(e) => return Err(e),
            Ok(decoded) => {
                receipt.set_item("result", "json_response")?;
                receipt.set_item("jsonRootType", decoded.get_type().getattr("__name__")?)?;
                if decoded.is_instance_of::<PyDict>() {
                    receipt.set_item("topLevelFields", py.import("builtins")?.getattr("sorted")?.call1((&decoded,))?)?;
                }
                save_private(directory.call_method0("__fspath__")?.extract::<std::path::PathBuf>()?.join("response.json"), &body)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = outcome {
        if is_transport_probe_error(py, &e) {
            receipt.set_item("result", "transport_error")?;
            receipt.set_item("errorType", e.get_type(py).name()?)?;
        } else {
            return Err(e);
        }
    }
    let finished = datetime.getattr("datetime")?.call_method1("now", (&utc,))?.call_method0("isoformat")?;
    receipt.set_item("finishedAtUtc", finished)?;
    let dumped: String = py.import("json")?.getattr("dumps")?.call((&receipt,), Some(&{
        let d = PyDict::new(py);
        d.set_item("indent", 2)?;
        d
    }))?.extract()?;
    save_private(directory.call_method0("__fspath__")?.extract::<std::path::PathBuf>()?.join("receipt.json"), dumped.as_bytes())?;

    let summary = PyDict::new(py);
    for k in ["result", "httpStatus", "contentType", "responseBytesRead", "topLevelFields", "errorType"] {
        if receipt.contains(k)? {
            summary.set_item(k, receipt.get_item(k)?.unwrap())?;
        }
    }
    let sk = PyDict::new(py);
    sk.set_item("indent", 2)?;
    let summary_s = py.import("json")?.getattr("dumps")?.call((&summary,), Some(&sk))?;
    let print = py.import("builtins")?.getattr("print")?;
    print.call1((summary_s,))?;
    print.call1(("Evidence:", &directory))?;
    let _ = value_error;
    let is_ok = receipt.call_method1("get", ("httpStatus",))?.eq(200).unwrap_or(false)
        && pyo3::types::PyAnyMethods::eq(&receipt.call_method1("get", ("result",))?, "json_response")?;
    Ok(if is_ok { 0 } else { 1 })
}

#[cfg(feature = "python")]
fn is_transport_probe_error(py: Python<'_>, e: &PyErr) -> bool {
    // (urllib.error.URLError, TimeoutError, OSError) — URLError ⊂ OSError.
    e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyTimeoutError>(py)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(save_private, m)?)?;
    m.add_function(wrap_pyfunction!(main, m)?)?;
    // NoRedirect: a genuine urllib.request.HTTPRedirectHandler subclass whose
    // redirect_request returns None, built via type() (PyO3 cannot #[pyclass]
    // a subclass of an arbitrary Python base).
    let base = py.import("urllib.request")?.getattr("HTTPRedirectHandler")?;
    let ns = PyDict::new(py);
    ns.set_item("redirect_request", wrap_pyfunction!(_noredirect_redirect_request, m)?)?;
    let cls = py.import("builtins")?.getattr("type")?.call1(("NoRedirect", (base,), ns))?;
    m.add("NoRedirect", cls)?;
    rs_common::export(m, &["save_private", "NoRedirect", "main"])
}
