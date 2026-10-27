//! Rust port of the pure functions of `tools/exchange_account_code.py`:
//! safe_text, prepare, load_bound_inputs.
//!
//! The network exchange (`exchange`) and the disabled legacy `main` stay in
//! Python; `exchange` performs the one real `/token` request and is flagged for
//! live re-validation (see PORTING.md).
use crate::common::*;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyModule, PyTuple};
use std::path::PathBuf;

#[cfg(feature = "python")]
/// exchange_account_code.safe_text(value, maximum=8192).
#[pyfunction]
#[pyo3(name = "safe_text", signature = (value, maximum = 8192))]
fn safe_text_py(value: &Bound<'_, PyAny>, maximum: usize) -> bool {
    safe_text(value, maximum)
}

#[cfg(feature = "python")]
/// exchange_account_code.prepare(transaction, profile, decoded, now).
#[pyfunction]
pub(crate) fn prepare<'py>(
    py: Python<'py>,
    transaction: &Bound<'py, PyAny>,
    profile: &Bound<'py, PyAny>,
    decoded: &Bound<'py, PyAny>,
    now: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let created = get(transaction, "createdUnixSeconds")?;
    let expires = get(transaction, "expiresUnixSeconds")?;
    let bad_txn = get(transaction, "schemaVersion")?.ne(3)?
        || get(transaction, "purpose")?.ne("experimental-signin-preparation")?
        || !is_strict_int(&created)
        || !is_strict_int(&expires)
        || expires.sub(&created)?.ne(600)?
        || !(created.le(now)? && now.lt(&expires)?);
    if bad_txn {
        return Err(value_error("invalid or expired transaction"));
    }
    if get(decoded, "schemaVersion")?.ne(2)?
        || get(decoded, "hostSource")?.ne("decrypted-state-bound-samsung-callback")?
    {
        return Err(value_error("callback must be decrypted and validated first"));
    }
    let host_v = get(decoded, "authServerUrl")?;
    let host = match host_v.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(value_error("unsupported regional token host")),
    };
    if !re_fullmatch(py, r"(?:[a-z]{2}-)?auth2\.samsungosp\.com", &host)? {
        return Err(value_error("unsupported regional token host"));
    }
    let verifier_v = get(transaction, "codeVerifier")?;
    let verifier = match verifier_v.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(value_error("invalid PKCE verifier")),
    };
    if !re_fullmatch(py, r"[A-Za-z0-9._~-]{43,128}", &verifier)? {
        return Err(value_error("invalid PKCE verifier"));
    }
    let client_v = get(profile, "clientId")?;
    let client = match client_v.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(value_error("invalid OAuth client identity")),
    };
    if !re_fullmatch(py, r"[A-Za-z0-9]{1,128}", &client)? {
        return Err(value_error("invalid OAuth client identity"));
    }
    let code_v = get(decoded, "code")?;
    let physical_v = get(profile, "devicePhysicalAddressText")?;
    let os_version_v = get(profile, "deviceOSVersion")?;
    let model_v = get(profile, "deviceModelID")?;
    if !(safe_text(&code_v, 8192)
        && safe_text(&physical_v, 8192)
        && safe_text(&os_version_v, 8192)
        && safe_text(&model_v, 8192))
    {
        return Err(value_error("invalid code or client metadata"));
    }
    let code = code_v.extract::<String>()?;
    let physical = physical_v.extract::<String>()?;
    let os_version = os_version_v.extract::<String>()?;
    let model = model_v.extract::<String>()?;

    let data = rs_cloud::util::urlencode(&[
        ("grant_type", "authorization_code".to_string()),
        ("client_id", client),
        ("code", code),
        ("code_verifier", verifier),
        ("physical_address_text", physical),
    ]);
    let headers = PyDict::new(py);
    headers.set_item("Content-Type", "application/x-www-form-urlencoded")?;
    headers.set_item("Accept", "application/json")?;
    headers.set_item("x-osp-clientosversion", os_version)?;
    headers.set_item("x-osp-clientmodel", model)?;
    headers.set_item("x-osp-packagename", "SAMSUNGELECTRONICSCO.LTD.SamsungAccount")?;
    headers.set_item("x-osp-packageversion", "1.26.6021.0")?;
    headers.set_item("User-Agent", "SamsungNotesLinuxResearch/0.1")?;
    request(py, &format!("https://{}/auth/oauth2/token", host), Some(data.as_bytes()), "POST", &headers)
}

#[cfg(feature = "python")]
/// exchange_account_code.load_bound_inputs(directory).
#[pyfunction]
fn load_bound_inputs<'py>(py: Python<'py>, directory: PathBuf) -> PyResult<Bound<'py, PyTuple>> {
    let decoded = rs_cloud::glue::load_json(py, &directory.join("decoded-callback.json"), 128 * 1024)?;
    if !decoded.is_instance_of::<PyDict>() || get(&decoded, "schemaVersion")?.ne(2)? {
        return Err(value_error("callback binding schema is required"));
    }
    let bindings = get(&decoded, "bindings")?;
    if !bindings.is_instance_of::<PyDict>() {
        return Err(value_error("callback input bindings are required"));
    }
    let specs = [
        ("transaction", "transactionSha256", 65536usize),
        ("callback", "callbackSha256", 65536),
        ("profile", "profileSha256", 32768),
    ];
    let mut parsed: Vec<Bound<'py, PyAny>> = Vec::new();
    for (name, field, limit) in specs {
        let raw = read_bounded(&directory.join(format!("{name}.json")), limit)?;
        let expected = get(&bindings, field)?;
        let expected_str = expected.cast::<pyo3::types::PyString>().ok().and_then(|s| s.to_str().ok().map(str::to_string));
        let digest = rs_cloud::util::sha256_hex(&raw);
        let ok = raw.len() <= limit
            && match &expected_str {
                Some(e) => re_fullmatch(py, r"[0-9a-f]{64}", e)? && compare_digest(py, e, &digest)?,
                None => false,
            };
        if !ok {
            return Err(value_error("callback input binding does not match"));
        }
        let value = json_loads(py, &raw)?;
        if !value.is_instance_of::<PyDict>() {
            return Err(value_error("bound input must be a JSON object"));
        }
        parsed.push(value);
    }
    // parsed order: transaction, callback, profile → return transaction, profile, decoded.
    PyTuple::new(py, [parsed[0].clone(), parsed[2].clone(), decoded])
}

#[cfg(feature = "python")]
/// Body of the one `/token` exchange, inside the (OSError, ValueError,
/// UnicodeError) guard. Fills receipt and may write the token; returns whether
/// an account token was received.
fn exchange_inner<'py>(
    py: Python<'py>,
    directory: &std::path::Path,
    request: &Bound<'py, PyAny>,
    profile: &Bound<'py, PyAny>,
    decoded: &Bound<'py, PyAny>,
    receipt: &Bound<'py, PyDict>,
) -> PyResult<bool> {
    let opener = build_opener(py)?;
    let response = opener_open(py, &opener, request, 20)?;
    response.call_method0("__enter__")?;
    let read = (|| -> PyResult<Vec<u8>> {
        receipt.set_item("httpStatus", response.getattr("code")?)?;
        let raw = response.call_method1("read", (128 * 1024 + 1,))?;
        Ok(raw.cast::<pyo3::types::PyBytes>()?.as_bytes().to_vec())
    })();
    response.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
    let raw = read?;
    receipt.set_item("responseBytes", raw.len())?;
    if raw.len() > 128 * 1024 {
        receipt.set_item("result", "response_limit_exceeded")?;
        return Ok(false);
    }
    rs_cloud::util::private_write(&directory.join("account-response.body"), &raw)?;
    let body = json_loads(py, &raw)?;
    if !body.is_instance_of::<PyDict>() {
        return Err(value_error("unexpected response shape"));
    }
    let code = response.getattr("code")?;
    let ok = pyo3::types::PyAnyMethods::eq(&code, 200)?
        && safe_text(&get(&body, "access_token")?, 65536)
        && safe_text(&get(&body, "userId")?, 65536);
    if ok {
        let token = PyDict::new(py);
        token.set_item("schemaVersion", 1)?;
        let now_int = py.import("builtins")?.getattr("int")?.call1((py.import("time")?.getattr("time")?.call0()?,))?;
        token.set_item("receivedUnixSeconds", now_int)?;
        token.set_item("clientId", profile.get_item("clientId")?)?;
        token.set_item("authServerUrl", decoded.get_item("authServerUrl")?)?;
        token.set_item("response", &body)?;
        token.set_item("credentialKind", "SamsungAccountOAuth")?;
        token.set_item("cloudEligibilityVerified", false)?;
        rs_cloud::glue::save_json(&directory.join("account-token.json"), token.as_any())?;
        receipt.set_item("result", "account_token_received")?;
    } else {
        receipt.set_item("result", "account_token_not_received")?;
    }
    Ok(ok)
}

#[cfg(feature = "python")]
/// exchange_account_code.exchange(directory).
#[pyfunction]
fn exchange<'py>(py: Python<'py>, directory: PathBuf) -> PyResult<bool> {
    let bound = load_bound_inputs(py, directory.clone())?;
    let transaction = bound.get_item(0)?;
    let profile = bound.get_item(1)?;
    let decoded = bound.get_item(2)?;
    let now = py.import("time")?.getattr("time")?.call0()?;
    let request = prepare(py, &transaction, &profile, &decoded, &now)?;
    // Atomic claim before transport; a single-use code cannot be reused.
    crate::common::excl_write(&directory.join(".account-code-claim"), b"claimed-before-transport\n")?;

    let receipt = PyDict::new(py);
    let started = py.import("builtins")?.getattr("int")?.call1((py.import("time")?.getattr("time")?.call0()?,))?;
    receipt.set_item("schemaVersion", 1)?;
    receipt.set_item("startedUnixSeconds", started)?;
    receipt.set_item("requestCount", 1)?;
    receipt.set_item("automaticRetry", false)?;
    receipt.set_item("redirectsFollowed", false)?;
    receipt.set_item("tlsVerified", true)?;
    receipt.set_item("cookiesAttached", false)?;
    receipt.set_item("result", "transport_outcome_unknown")?;
    receipt.set_item("cloudRequestPerformed", false)?;

    match exchange_inner(py, &directory, &request, &profile, &decoded, &receipt) {
        Ok(_) => {}
        Err(e) if is_os_value_unicode(py, &e) => {
            receipt.set_item("result", "transport_or_response_error")?;
        }
        Err(e) => return Err(e),
    }
    rs_cloud::glue::save_json(&directory.join("account-exchange-receipt.json"), receipt.as_any())?;
    let dumps = py.import("json")?.getattr("dumps")?.call1((&receipt,))?;
    py.import("builtins")?.getattr("print")?.call1((dumps,))?;
    Ok(pyo3::types::PyAnyMethods::eq(&receipt.call_method1("get", ("result",))?, "account_token_received")?)
}

#[cfg(feature = "python")]
/// exchange_account_code.main: the legacy direct /token exchange is disabled for
/// browser callbacks; prints the notice to stderr and returns 2.
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    let msg = "This browser flow requires notes_auth_bridge.py authenticate first. \
Direct /token exchange is disabled for browser callbacks.";
    let sys = py.import("sys")?;
    py.import("builtins")?.getattr("print")?.call(
        (msg,),
        Some(&{
            let d = PyDict::new(py);
            d.set_item("file", sys.getattr("stderr")?)?;
            d
        }),
    )?;
    Ok(2)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(safe_text_py, m)?)?;
    m.add_function(wrap_pyfunction!(prepare, m)?)?;
    m.add_function(wrap_pyfunction!(load_bound_inputs, m)?)?;
    m.add_function(wrap_pyfunction!(exchange, m)?)?;
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(m, &["safe_text", "prepare", "load_bound_inputs", "exchange", "main"])
}
