//! Rust port of the pure request builder of `tools/notes_auth_bridge.py`:
//! uat_request.
//!
//! perform_once (the real UAT / authorize / token requests) and the
//! authenticate / authorize_notes / exchange_notes_code orchestration stay in
//! Python; they reach the network and are flagged for live re-validation
//! (see PORTING.md).
use crate::common::*;
#[cfg(feature = "python")]
use crate::exchange_account_code::prepare;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyModule, PyTuple};
use std::path::{Path, PathBuf};

#[cfg(feature = "python")]
/// `notes_auth_bridge.<name>` resolved at call time, so the oracle's
/// `patch.object(bridge, 'perform_once'/'load_bound_inputs', ...)` is honored.
fn module_attr<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import("notes_auth_bridge")?.getattr(name)
}

#[cfg(feature = "python")]
/// notes_auth_bridge.uat_request(transaction, profile, decoded, now).
#[pyfunction]
fn uat_request<'py>(
    py: Python<'py>,
    transaction: &Bound<'py, PyAny>,
    profile: &Bound<'py, PyAny>,
    decoded: &Bound<'py, PyAny>,
    now: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let base = prepare(py, transaction, profile, decoded, now)?;
    let username_v = get(decoded, "loginId")?;
    let username = match username_v.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(value_error("callback login identity is missing or invalid")),
    };
    let bad = username.is_empty()
        || username.len() > 8192
        || username.chars().any(|c| (c as u32) < 32 || c as u32 == 127);
    if bad {
        return Err(value_error("callback login identity is missing or invalid"));
    }

    // fields = dict(parse_qsl(base.data.decode(), strict_parsing=True))
    let urlparse = py.import("urllib.parse")?;
    let data_str = base.getattr("data")?.call_method0("decode")?;
    let kw = PyDict::new(py);
    kw.set_item("strict_parsing", true)?;
    let pairs = urlparse.call_method("parse_qsl", (data_str,), Some(&kw))?;
    let builtins = py.import("builtins")?;
    let fields = builtins.getattr("dict")?.call1((pairs,))?.cast_into::<PyDict>()?;
    fields.set_item("serviceType", "M")?;
    fields.set_item("username", &username)?;

    let full_url: String = base.getattr("full_url")?.extract()?;
    let target = format!("{}/authenticate", full_url.strip_suffix("/token").unwrap_or(&full_url));
    let encoded: String = urlparse.call_method1("urlencode", (&fields,))?.extract()?;
    let headers = builtins.getattr("dict")?.call1((base.call_method0("header_items")?,))?.cast_into::<PyDict>()?;
    request(py, &target, Some(encoded.as_bytes()), "POST", &headers)
}

#[cfg(feature = "python")]
/// notes_auth_bridge.perform_once(directory, name, claim_name, request).
#[pyfunction]
fn perform_once<'py>(
    py: Python<'py>,
    directory: PathBuf,
    name: &str,
    claim_name: &str,
    request: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyTuple>> {
    rs_cloud::util::private_write(&directory.join(claim_name), b"claimed-before-transport\n")?;
    let receipt = PyDict::new(py);
    let started = py.import("builtins")?.getattr("int")?.call1((py.import("time")?.getattr("time")?.call0()?,))?;
    receipt.set_item("schemaVersion", 1)?;
    receipt.set_item("startedUnixSeconds", started)?;
    receipt.set_item("method", "POST")?;
    receipt.set_item("requestCount", 1)?;
    receipt.set_item("automaticRetry", false)?;
    receipt.set_item("redirectsFollowed", false)?;
    receipt.set_item("cookiesAttached", false)?;
    receipt.set_item("tlsVerified", true)?;
    receipt.set_item("result", "transport_outcome_unknown")?;

    let body = match perform_once_inner(py, &directory, name, request, &receipt) {
        Ok(b) => b,
        Err(e) if is_os_value_unicode(py, &e) => {
            receipt.set_item("result", "transport_or_response_error")?;
            py.None().into_bound(py)
        }
        Err(e) => return Err(e),
    };
    rs_cloud::glue::save_json(&directory.join(format!("{name}-receipt.json")), receipt.as_any())?;
    let dumps = py.import("json")?.getattr("dumps")?.call1((&receipt,))?;
    py.import("builtins")?.getattr("print")?.call1((dumps,))?;
    PyTuple::new(py, [receipt.call_method1("get", ("httpStatus",))?, body])
}

#[cfg(feature = "python")]
fn perform_once_inner<'py>(
    py: Python<'py>,
    directory: &Path,
    name: &str,
    request: &Bound<'py, PyAny>,
    receipt: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    let opener = build_opener(py)?;
    let response = opener_open(py, &opener, request, 20)?;
    response.call_method0("__enter__")?;
    let read = (|| -> PyResult<Vec<u8>> {
        receipt.set_item("httpStatus", response.getattr("code")?)?;
        let raw = response.call_method1("read", (128 * 1024 + 1,))?;
        Ok(raw.cast::<PyBytes>()?.as_bytes().to_vec())
    })();
    response.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
    let raw = read?;
    receipt.set_item("responseBytes", raw.len())?;
    if raw.len() > 128 * 1024 {
        receipt.set_item("result", "response_limit_exceeded")?;
        return Ok(py.None().into_bound(py));
    }
    rs_cloud::util::private_write(&directory.join(format!("{name}-response.body")), &raw)?;
    let body = json_loads(py, &raw)?;
    if !body.is_instance_of::<PyDict>() {
        return Err(value_error("response is not a JSON object"));
    }
    receipt.set_item("result", "json_response")?;
    let types = PyDict::new(py);
    for (k, v) in body.cast::<PyDict>()?.iter() {
        types.set_item(k, v.get_type().getattr("__name__")?)?;
    }
    receipt.set_item("responseFieldTypes", types)?;
    Ok(body)
}

#[cfg(feature = "python")]
/// notes_auth_bridge.authenticate(directory).
#[pyfunction]
fn authenticate<'py>(py: Python<'py>, directory: PathBuf) -> PyResult<bool> {
    let bound = module_attr(py, "load_bound_inputs")?.call1((directory.clone(),))?;
    let txn = bound.get_item(0)?;
    let profile = bound.get_item(1)?;
    let decoded = bound.get_item(2)?;
    let now = py.import("time")?.getattr("time")?.call0()?;
    let request = uat_request(py, &txn, &profile, &decoded, &now)?;
    let result = module_attr(py, "perform_once")?
        .call1((directory.clone(), "uat", ".account-code-claim", request))?;
    let status = result.get_item(0)?;
    let body = result.get_item(1)?;
    if !pyo3::types::PyAnyMethods::eq(&status, 200)? || !body.is_instance_of::<PyDict>() {
        return Ok(false);
    }
    let expiry = get(&body, "userauth_token_expires_in")?;
    if !(safe_text(&get(&body, "userauth_token")?, 65536) && safe_text(&get(&body, "userId")?, 65536))
        || !is_strict_number(&expiry)
        || !(expiry.gt(0)? && expiry.le(366 * 86400)?)
    {
        return Ok(false);
    }
    let token = PyDict::new(py);
    token.set_item("schemaVersion", 1)?;
    token.set_item("credentialKind", "SamsungUserAuth")?;
    token.set_item("receivedUnixSeconds", now_int(py)?)?;
    token.set_item("expiresIn", &expiry)?;
    token.set_item("clientId", profile.get_item("clientId")?)?;
    token.set_item("authServerUrl", decoded.get_item("authServerUrl")?)?;
    token.set_item("response", &body)?;
    token.set_item("sourceBindings", decoded.get_item("bindings")?)?;
    rs_cloud::glue::save_json(&directory.join("uat-token.json"), token.as_any())?;
    print_line(py, "Samsung ara oturum tokenı özel dosyaya kaydedildi. Cloud erişimi henüz yok.")?;
    Ok(true)
}

#[cfg(feature = "python")]
/// notes_auth_bridge.authorize_notes(directory).
#[pyfunction]
fn authorize_notes<'py>(py: Python<'py>, directory: PathBuf) -> PyResult<bool> {
    let bound = module_attr(py, "load_bound_inputs")?.call1((directory.clone(),))?;
    let txn = bound.get_item(0)?;
    let profile = bound.get_item(1)?;
    let decoded = bound.get_item(2)?;
    let uat = rs_cloud::glue::load_json(py, &directory.join("uat-token.json"), 128 * 1024)?;
    let now = py.import("time")?.getattr("time")?.call0()?;
    let received = uat.get_item("receivedUnixSeconds")?;
    let expires_in = uat.get_item("expiresIn")?;
    let bindings = decoded.get_item("bindings")?;
    if get(&uat, "credentialKind")?.ne("SamsungUserAuth")?
        || get(&uat, "sourceBindings")?.ne(&bindings)?
        || !(received.le(&now)? && now.lt(&received.add(&expires_in)?)?)
    {
        return Err(value_error("invalid or expired UAT"));
    }
    let base = prepare(py, &txn, &profile, &decoded, &now)?;
    let target_v = get(&profile, "originalClientId")?;
    let target = match target_v.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(value_error("invalid original application identity")),
    };
    if !target.is_ascii() || !target.chars().all(|c| c.is_ascii_alphanumeric()) || target.is_empty() || target.len() > 128 {
        return Err(value_error("invalid original application identity"));
    }
    let secrets = py.import("secrets")?;
    let verifier: String = secrets.call_method1("token_urlsafe", (32,))?.extract()?;
    let challenge = s256_challenge(py, &verifier)?;
    let now_i = now_int(py)?;
    let app = PyDict::new(py);
    app.set_item("schemaVersion", 1)?;
    app.set_item("clientId", &target)?;
    app.set_item("codeVerifier", &verifier)?;
    app.set_item("codeChallenge", &challenge)?;
    app.set_item("createdUnixSeconds", &now_i)?;
    app.set_item("sourceBindings", &bindings)?;
    rs_cloud::glue::save_json(&directory.join("notes-app-transaction.json"), app.as_any())?;

    let userauth_token: String = uat.get_item("response")?.get_item("userauth_token")?.extract()?;
    let physical: String = profile.get_item("devicePhysicalAddressText")?.extract()?;
    let scope_v = get(&profile, "scope")?;
    let scope = if scope_v.is_truthy()? { scope_v.extract::<String>()? } else { String::new() };
    let data = rs_cloud::util::urlencode(&[
        ("response_type", "code".to_string()),
        ("serviceType", "M".to_string()),
        ("client_id", target.clone()),
        ("userauth_token", userauth_token),
        ("code_challenge", challenge.clone()),
        ("code_challenge_method", "S256".to_string()),
        ("physical_address_text", physical),
        ("scope", scope),
    ]);
    let full_url: String = base.getattr("full_url")?.extract()?;
    let url = format!("{}/v2/authorize", full_url.strip_suffix("/token").unwrap_or(&full_url));
    let headers = py.import("builtins")?.getattr("dict")?.call1((base.call_method0("header_items")?,))?.cast_into::<PyDict>()?;
    let request = request(py, &url, Some(data.as_bytes()), "POST", &headers)?;
    let result = module_attr(py, "perform_once")?
        .call1((directory.clone(), "notes-authorize", ".notes-authorize-claim", request))?;
    let status = result.get_item(0)?;
    let body = result.get_item(1)?;
    if !pyo3::types::PyAnyMethods::eq(&status, 200)? || !body.is_instance_of::<PyDict>() {
        return Ok(false);
    }
    for flag in ["requireTncAccepted", "privacyAccepted", "requireChildAccountGraduated"] {
        let v = get(&body, flag)?;
        let allowed = v.is_none() || pyo3::types::PyAnyMethods::eq(&v, false)? || v.eq("N")? || v.eq("false")?;
        if !allowed {
            print_line(py, "Samsung ek kullanıcı işlemi istiyor; yanıt özel dosyada korundu.")?;
            return Ok(false);
        }
    }
    if !safe_text(&get(&body, "code")?, 8192) {
        return Ok(false);
    }
    let app_sha = rs_cloud::util::sha256_hex(&std::fs::read(directory.join("notes-app-transaction.json"))?);
    let grant = PyDict::new(py);
    grant.set_item("schemaVersion", 1)?;
    grant.set_item("credentialKind", "NotesAuthorizationCode")?;
    grant.set_item("clientId", &target)?;
    grant.set_item("receivedUnixSeconds", now_int(py)?)?;
    grant.set_item("response", &body)?;
    grant.set_item("appTransactionSha256", app_sha)?;
    rs_cloud::glue::save_json(&directory.join("notes-app-code.json"), grant.as_any())?;
    print_line(py, "Notes uygulamasına özel kod özel dosyaya kaydedildi; access token henüz alınmadı.")?;
    Ok(true)
}

#[cfg(feature = "python")]
/// notes_auth_bridge.exchange_notes_code(directory).
#[pyfunction]
fn exchange_notes_code<'py>(py: Python<'py>, directory: PathBuf) -> PyResult<bool> {
    let bound = module_attr(py, "load_bound_inputs")?.call1((directory.clone(),))?;
    let txn = bound.get_item(0)?;
    let profile = bound.get_item(1)?;
    let decoded = bound.get_item(2)?;
    let app_raw = std::fs::read(directory.join("notes-app-transaction.json"))?;
    if app_raw.len() > 65536 {
        return Err(value_error("app transaction exceeds limit"));
    }
    let app = json_loads(py, &app_raw)?;
    let grant = rs_cloud::glue::load_json(py, &directory.join("notes-app-code.json"), 128 * 1024)?;
    let now = py.import("time")?.getattr("time")?.call0()?;
    let bindings = decoded.get_item("bindings")?;
    let app_sha = rs_cloud::util::sha256_hex(&app_raw);
    let grant_sha = get(&grant, "appTransactionSha256")?;
    let grant_sha_s = if grant_sha.is_none() { String::new() } else { grant_sha.extract::<String>().unwrap_or_default() };
    let binding_ok = get(&grant, "credentialKind")?.eq("NotesAuthorizationCode")?
        && get(&app, "schemaVersion")?.eq(1)?
        && get(&grant, "schemaVersion")?.eq(1)?
        && !get(&app, "sourceBindings")?.ne(&bindings)?
        && !get(&app, "clientId")?.ne(get(&profile, "originalClientId")?)?
        && !get(&grant, "clientId")?.ne(get(&app, "clientId")?)?
        && compare_digest(py, &grant_sha_s, &app_sha)?;
    if !binding_ok {
        return Err(value_error("application grant binding failed"));
    }
    let lifetime = grant.get_item("response")?.call_method1("get", ("code_expires_in",))?;
    let received = grant.get_item("receivedUnixSeconds")?;
    if !is_strict_int(&lifetime)
        || !(lifetime.gt(0)? && lifetime.le(600)?)
        || !(received.le(&now)? && now.lt(&received.add(&lifetime)?)?)
    {
        return Err(value_error("application grant expired or has unknown lifetime"));
    }
    let builtins = py.import("builtins")?;
    let token_txn = builtins.getattr("dict")?.call1((&txn,))?;
    token_txn.set_item("codeVerifier", app.get_item("codeVerifier")?)?;
    let token_profile = builtins.getattr("dict")?.call1((&profile,))?;
    token_profile.set_item("clientId", app.get_item("clientId")?)?;
    let token_decoded = builtins.getattr("dict")?.call1((&decoded,))?;
    token_decoded.set_item("code", grant.get_item("response")?.get_item("code")?)?;
    let request = prepare(py, &token_txn, &token_profile, &token_decoded, &now)?;
    let result = module_attr(py, "perform_once")?
        .call1((directory.clone(), "notes-token", ".notes-code-claim", request))?;
    let status = result.get_item(0)?;
    let body = result.get_item(1)?;
    if !pyo3::types::PyAnyMethods::eq(&status, 200)? || !body.is_instance_of::<PyDict>() {
        return Ok(false);
    }
    if !(safe_text(&get(&body, "access_token")?, 65536) && safe_text(&get(&body, "userId")?, 65536)) {
        return Ok(false);
    }
    let uat = rs_cloud::glue::load_json(py, &directory.join("uat-token.json"), 128 * 1024)?;
    if body.get_item("userId")?.ne(uat.get_item("response")?.get_item("userId")?)? {
        return Err(value_error("token account does not match authenticated account"));
    }
    let token = PyDict::new(py);
    token.set_item("schemaVersion", 1)?;
    token.set_item("credentialKind", "NotesServiceOAuth")?;
    token.set_item("clientId", app.get_item("clientId")?)?;
    token.set_item("receivedUnixSeconds", now_int(py)?)?;
    token.set_item("authServerUrl", decoded.get_item("authServerUrl")?)?;
    token.set_item("response", &body)?;
    token.set_item("cloudEligibilityVerified", false)?;
    rs_cloud::glue::save_json(&directory.join("notes-account-token.json"), token.as_any())?;
    print_line(py, "Notes hesap tokenı alındı; ayrı Cloud kaydı henüz yapılmadı.")?;
    Ok(true)
}

#[cfg(feature = "python")]
/// notes_auth_bridge.main().
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    let argparse = py.import("argparse")?;
    let kw = PyDict::new(py);
    kw.set_item("description", py.import("notes_auth_bridge")?.getattr("__doc__")?)?;
    let parser = argparse.getattr("ArgumentParser")?.call((), Some(&kw))?;
    let step_kw = PyDict::new(py);
    step_kw.set_item("choices", ("authenticate", "authorize-notes", "exchange-notes-code"))?;
    parser.call_method("add_argument", ("step",), Some(&step_kw))?;
    let dir_kw = PyDict::new(py);
    dir_kw.set_item("type", py.import("pathlib")?.getattr("Path")?)?;
    parser.call_method("add_argument", ("transaction_directory",), Some(&dir_kw))?;
    let args = parser.call_method0("parse_args")?;
    let step: String = args.getattr("step")?.extract()?;
    let directory = args.getattr("transaction_directory")?;
    let name = match step.as_str() {
        "authenticate" => "authenticate",
        "authorize-notes" => "authorize_notes",
        _ => "exchange_notes_code",
    };
    let result = (|| -> PyResult<bool> { module_attr(py, name)?.call1((directory,))?.extract() })();
    match result {
        Ok(ok) => Ok(if ok { 0 } else { 1 }),
        Err(e) if is_login_error(py, &e) => {
            print_stderr(py, "Giriş köprüsü durdu; işlem dosyaları korunuyor, otomatik tekrar yok.")?;
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
fn now_int<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    py.import("builtins")?.getattr("int")?.call1((py.import("time")?.getattr("time")?.call0()?,))
}

#[cfg(feature = "python")]
fn s256_challenge(py: Python<'_>, verifier: &str) -> PyResult<String> {
    let digest = py.import("hashlib")?.call_method1("sha256", (verifier.as_bytes(),))?.call_method0("digest")?;
    let b64 = py.import("base64")?.call_method1("urlsafe_b64encode", (digest,))?.call_method0("decode")?;
    b64.call_method1("rstrip", ("=",))?.extract()
}

#[cfg(feature = "python")]
fn print_line(py: Python<'_>, msg: &str) -> PyResult<()> {
    py.import("builtins")?.getattr("print")?.call1((msg,))?;
    Ok(())
}

#[cfg(feature = "python")]
fn print_stderr(py: Python<'_>, msg: &str) -> PyResult<()> {
    let kw = PyDict::new(py);
    kw.set_item("file", py.import("sys")?.getattr("stderr")?)?;
    py.import("builtins")?.getattr("print")?.call((msg,), Some(&kw))?;
    Ok(())
}

#[cfg(feature = "python")]
fn is_login_error(py: Python<'_>, e: &PyErr) -> bool {
    e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyValueError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(uat_request, m)?)?;
    m.add_function(wrap_pyfunction!(perform_once, m)?)?;
    m.add_function(wrap_pyfunction!(authenticate, m)?)?;
    m.add_function(wrap_pyfunction!(authorize_notes, m)?)?;
    m.add_function(wrap_pyfunction!(exchange_notes_code, m)?)?;
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(
        m,
        &["uat_request", "perform_once", "authenticate", "authorize_notes", "exchange_notes_code", "main"],
    )
}
