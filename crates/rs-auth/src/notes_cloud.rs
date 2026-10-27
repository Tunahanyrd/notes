//! Rust port of the pure request builders of `tools/notes_cloud.py`:
//! endpoint, registration_request, changes_request.
//!
//! send_once (the real single HTTPS request path, used by CloudAPI when no
//! transport is injected) and the register / accept_registration / changes /
//! scan_for_note / fetch_candidate orchestration stay in Python; they reach the
//! network and are flagged for live re-validation (see PORTING.md).
use crate::common::*;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyModule, PyTuple};
use std::path::PathBuf;

const MAX_BYTES: usize = 2 * 1024 * 1024;

#[cfg(feature = "python")]
/// Read the response inside `with response:` and (after it) decode/limit-check.
/// Fills receipt httpStatus/contentType/wireBytes/decodedBytes/result and writes
/// the meta + body files. Returns the decoded payload.
fn send_once_inner<'py>(
    py: Python<'py>,
    directory: &std::path::Path,
    name: &str,
    request: &Bound<'py, PyAny>,
    max_bytes: usize,
    receipt: &Bound<'py, PyDict>,
) -> PyResult<Option<Vec<u8>>> {
    let opener = build_opener(py)?;
    let response = opener_open(py, &opener, request, 25)?;
    response.call_method0("__enter__")?;
    let read = (|| -> PyResult<(String, Vec<u8>)> {
        receipt.set_item("httpStatus", response.getattr("code")?)?;
        let headers = response.getattr("headers")?;
        receipt.set_item("contentType", headers.call_method0("get_content_type")?)?;
        let full_type: String = headers.call_method1("get", ("Content-Type", ""))?.extract()?;
        if full_type.chars().count() > 8192 {
            return Err(value_error("response content type too large"));
        }
        let meta = PyDict::new(py);
        meta.set_item("contentType", &full_type)?;
        rs_cloud::glue::save_json(&directory.join(format!("{name}-response-meta.json")), meta.as_any())?;
        let encoding: String =
            headers.call_method1("get", ("Content-Encoding", ""))?.str()?.to_str()?.to_lowercase();
        let raw_obj = response.call_method1("read", (max_bytes + 1,))?;
        let raw = raw_obj.cast::<PyBytes>()?.as_bytes().to_vec();
        Ok((encoding, raw))
    })();
    response.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
    let (encoding, raw) = read?;

    if raw.len() > max_bytes {
        return Err(value_error("response wire limit exceeded"));
    }
    let payload = match encoding.as_str() {
        "gzip" => rs_cloud::util::gunzip_limited(&raw, max_bytes + 1)
            .map_err(|_| pyo3::exceptions::PyOSError::new_err("gzip decode failed"))?,
        "" | "identity" => raw.clone(),
        _ => return Err(value_error("unsupported response encoding")),
    };
    if payload.len() > max_bytes {
        return Err(value_error("response decoded limit exceeded"));
    }
    rs_cloud::util::private_write(&directory.join(format!("{name}-response.body")), &payload)?;
    receipt.set_item("wireBytes", raw.len())?;
    receipt.set_item("decodedBytes", payload.len())?;
    receipt.set_item("result", "response_saved")?;
    Ok(Some(payload))
}

#[cfg(feature = "python")]
/// notes_cloud.send_once(directory, name, request, max_bytes=MAX_BYTES, quiet=False).
///
/// Transport goes through `urllib.request.build_opener`/`opener.open` (the seam
/// the oracle patches) — a single request, no redirects, TLS verified, no cookie
/// jar — so the suite injects a fake opener and makes no external request.
#[pyfunction]
#[pyo3(signature = (directory, name, request, max_bytes = MAX_BYTES, quiet = false))]
fn send_once<'py>(
    py: Python<'py>,
    directory: PathBuf,
    name: &str,
    request: &Bound<'py, PyAny>,
    max_bytes: usize,
    quiet: bool,
) -> PyResult<Bound<'py, PyTuple>> {
    if !(1..=32 * 1024 * 1024).contains(&max_bytes) {
        return Err(value_error("unsupported response size bound"));
    }
    rs_cloud::util::private_write(&directory.join(format!(".{name}-claim")), b"claimed-before-transport\n")?;
    let receipt = PyDict::new(py);
    let started = py.import("builtins")?.getattr("int")?.call1((py.import("time")?.getattr("time")?.call0()?,))?;
    receipt.set_item("schemaVersion", 1)?;
    receipt.set_item("startedUnixSeconds", started)?;
    receipt.set_item("method", request.call_method0("get_method")?)?;
    receipt.set_item("requestCount", 1)?;
    receipt.set_item("automaticRetry", false)?;
    receipt.set_item("redirectsFollowed", false)?;
    receipt.set_item("tlsVerified", true)?;
    receipt.set_item("cookiesAttached", false)?;
    receipt.set_item("result", "transport_outcome_unknown")?;

    let payload = match send_once_inner(py, &directory, name, request, max_bytes, &receipt) {
        Ok(p) => p,
        Err(e) if is_transport_error(py, &e) => {
            receipt.set_item("result", "transport_or_response_error")?;
            None
        }
        Err(e) => return Err(e),
    };
    rs_cloud::glue::save_json(&directory.join(format!("{name}-receipt.json")), receipt.as_any())?;

    let status = receipt.call_method1("get", ("httpStatus",))?;
    let is_200 = pyo3::types::PyAnyMethods::eq(&status, 200).unwrap_or(false);
    if !quiet || !is_200 {
        let dumps = py.import("json")?.getattr("dumps")?.call1((&receipt,))?;
        py.import("builtins")?.getattr("print")?.call1((dumps,))?;
    }
    let payload_obj = match payload {
        Some(b) => PyBytes::new(py, &b).into_any(),
        None => py.None().into_bound(py),
    };
    PyTuple::new(py, [status, payload_obj])
}

#[cfg(feature = "python")]
/// notes_cloud.endpoint(endpoints, role).
#[pyfunction]
fn endpoint(_py: Python<'_>, endpoints: &Bound<'_, PyAny>, role: &str) -> PyResult<String> {
    let value = get(endpoints, role)?;
    let v = rs_cloud::pyjson::from_py(&value)?;
    rs_cloud::util::endpoint(Some(&v)).map_err(value_error)
}

#[cfg(feature = "python")]
fn require_number(v: &Bound<'_, PyAny>, message: &str) -> PyResult<()> {
    if is_strict_number(v) { Ok(()) } else { Err(value_error(message)) }
}

#[cfg(feature = "python")]
/// notes_cloud.registration_request(account, endpoints, profile, now).
#[pyfunction]
fn registration_request<'py>(
    py: Python<'py>,
    account: &Bound<'py, PyAny>,
    endpoints: &Bound<'py, PyAny>,
    profile: &Bound<'py, PyAny>,
    now: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    if get(account, "credentialKind")?.ne("NotesServiceOAuth")? || get(account, "schemaVersion")?.ne(1)? {
        return Err(value_error("Notes-specific Account token required"));
    }
    let response = account.get_item("response")?;
    let client_id = get(account, "clientId")?;
    let token = get(&response, "access_token")?;
    let user = get(&response, "userId")?;
    let cdid = get(profile, "clientDeviceId")?;
    if !(safe_text(&client_id, 65536) && safe_text(&token, 65536) && safe_text(&user, 65536) && safe_text(&cdid, 65536)) {
        return Err(value_error("invalid registration identity"));
    }
    if get(profile, "appId")?.ne(&client_id)? {
        return Err(value_error("Cloud app identity must match Notes OAuth client"));
    }
    let lifetime = {
        let fallback = get(&response, "expires_in")?;
        response.call_method1("get", ("access_token_expires_in", fallback))?
    };
    let received = account.get_item("receivedUnixSeconds")?;
    require_number(&lifetime, "Notes Account token expired or unknown lifetime")?;
    if lifetime.le(0)?
        || !(received.le(now)? && now.lt(&received.add(&lifetime)?)?)
    {
        return Err(value_error("Notes Account token expired or unknown lifetime"));
    }
    let body = get(profile, "registrationBody")?;
    if !body.is_instance_of::<PyDict>() {
        return Err(value_error("registration body must have app and device objects"));
    }
    let body_dict = body.cast::<PyDict>()?;
    let mut keys: Vec<String> = body_dict.keys().iter().map(|k| k.extract::<String>()).collect::<PyResult<_>>()?;
    keys.sort();
    if keys != ["app", "device"] {
        return Err(value_error("registration body must have app and device objects"));
    }
    let app = body.get_item("app")?;
    let device = body.get_item("device")?;
    if !app.is_instance_of::<PyDict>() || !device.is_instance_of::<PyDict>() {
        return Err(value_error("invalid registration body"));
    }
    for section in [&app, &device] {
        for (_k, v) in section.cast::<PyDict>()?.iter() {
            let ok = match v.cast::<pyo3::types::PyString>() {
                Ok(s) => {
                    let t = s.to_str()?;
                    t.chars().count() <= 1024 && !t.chars().any(|c| (c as u32) < 32)
                }
                Err(_) => false,
            };
            if !ok {
                return Err(value_error("invalid registration metadata"));
            }
        }
    }
    let version = get(&app, "version")?;
    if !safe_text(&version, 128) || !safe_text(&get(&app, "sdkVersion")?, 128) {
        return Err(value_error("application and SDK versions required"));
    }

    let headers = PyDict::new(py);
    headers.set_item("x-sc-app-id", &client_id)?;
    headers.set_item("x-sc-cdid", &cdid)?;
    headers.set_item("x-sc-access-token", &token)?;
    headers.set_item("x-sc-uid", &user)?;
    headers.set_item("x-sc-app-version", &version)?;
    headers.set_item("Content-Type", "application/json")?;
    headers.set_item("Accept", "application/json")?;
    headers.set_item("User-Agent", "SamsungNotesLinuxResearch/0.1")?;

    let millis = rs_cloud::glue::millis(now)?;
    let query = rs_cloud::util::urlencode(&[("deviceCurrentTime", millis.to_string())]);
    let url = format!("{}/identity/v2/user/register?{}", endpoint(py, endpoints, "play")?, query);
    let body_value = rs_cloud::pyjson::from_py(&body)?;
    let data = rs_cloud::pyjson::dumps(&body_value, &rs_cloud::pyjson::COMPACT);
    request(py, &url, Some(data.as_bytes()), "POST", &headers)
}

const ALLOWED_HEADERS: [&str; 6] = [
    "x-sn-sdk-ver", "x-sn-app-ver", "x-sn-xml-ver", "x-sn-model-name", "x-sc-network", "x-sc-e2ee-state",
];
const REQUIRED_HEADERS: [&str; 4] = ["x-sn-sdk-ver", "x-sn-app-ver", "x-sn-xml-ver", "x-sn-model-name"];

#[cfg(feature = "python")]
/// notes_cloud.changes_request(cloud, endpoints, profile, cursor, now).
#[pyfunction]
fn changes_request<'py>(
    py: Python<'py>,
    cloud: &Bound<'py, PyAny>,
    endpoints: &Bound<'py, PyAny>,
    profile: &Bound<'py, PyAny>,
    cursor: &Bound<'py, PyAny>,
    now: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    if get(cloud, "credentialKind")?.ne("CloudUser")? || get(cloud, "schemaVersion")?.ne(1)? {
        return Err(value_error("valid Cloud user token required"));
    }
    let received = cloud.get_item("receivedUnixSeconds")?;
    let expires_in = cloud.get_item("expiresIn")?;
    let access = get(cloud, "accessToken")?;
    if !(received.le(now)? && now.lt(&received.add(&expires_in)?)?) || !safe_text(&access, 65536) {
        return Err(value_error("valid Cloud user token required"));
    }
    let cursor_str = match cursor.cast::<pyo3::types::PyString>() {
        Ok(s) => s.to_str()?.to_string(),
        Err(_) => return Err(value_error("invalid opaque cursor")),
    };
    if cursor_str.chars().count() > 65536 || cursor_str.chars().any(|c| (c as u32) < 32) {
        return Err(value_error("invalid opaque cursor"));
    }

    let notes_headers = get_or_empty(profile, "notesHeaders")?;
    let headers = py.import("builtins")?.getattr("dict")?.call1((&notes_headers,))?.cast_into::<PyDict>()?;
    let mut present: Vec<String> = Vec::new();
    for (k, v) in headers.iter() {
        let key: String = k.extract()?;
        if !ALLOWED_HEADERS.contains(&key.as_str()) || !safe_text(&v, 1024) {
            return Err(value_error("explicit Notes protocol headers required"));
        }
        present.push(key);
    }
    if !REQUIRED_HEADERS.iter().all(|r| present.iter().any(|p| p == r)) {
        return Err(value_error("explicit Notes protocol headers required"));
    }

    let access_str: String = access.extract()?;
    headers.set_item("Authorization", format!("Bearer {}", access_str))?;
    headers.set_item("X-SN-Transaction-ID", format!("ND-{}", secrets_choice(10)))?;
    headers.set_item("Accept-Encoding", "gzip")?;
    headers.set_item("User-Agent", "SamsungNotesLinuxResearch/0.1")?;

    let query = rs_cloud::util::urlencode(&[("lastChangePoint", cursor_str)]);
    let url = format!("{}/samsungnotes/v3/notes/changes?{}", endpoint(py, endpoints, "default")?, query);
    request(py, &url, None, "GET", &headers)
}

// ------------------------------------------------------------ orchestration
// The following are not exercised by the suite (they shell out to notes-cli and
// perform real requests). Logic is in Rust; I/O uses the Python stdlib seams.
// Runtime correctness needs a live session (see PORTING.md).

#[cfg(feature = "python")]
fn root<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    py.import("notes_login")?.getattr("ROOT")
}

#[cfg(feature = "python")]
fn is_true(py: Python<'_>, v: &Bound<'_, PyAny>) -> bool {
    v.is(&pyo3::types::PyBool::new(py, true))
}
#[cfg(feature = "python")]
fn is_false(py: Python<'_>, v: &Bound<'_, PyAny>) -> bool {
    v.is(&pyo3::types::PyBool::new(py, false))
}

#[cfg(feature = "python")]
/// `print(json.dumps(value))` (optionally flush).
fn print_json(py: Python<'_>, value: &Bound<'_, PyAny>, flush: bool) -> PyResult<()> {
    let dumps = py.import("json")?.getattr("dumps")?.call1((value,))?;
    let kw = PyDict::new(py);
    if flush {
        kw.set_item("flush", true)?;
    }
    py.import("builtins")?.getattr("print")?.call((dumps,), Some(&kw))?;
    Ok(())
}

#[cfg(feature = "python")]
fn counts_with<'py>(py: Python<'py>, counts: &Bound<'py, PyDict>, extra: &[(&str, Bound<'py, PyAny>)]) -> PyResult<Bound<'py, PyDict>> {
    let d = py.import("builtins")?.getattr("dict")?.call1((counts,))?.cast_into::<PyDict>()?;
    for (k, v) in extra {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

#[cfg(feature = "python")]
/// notes_cloud.accept_registration(directory, raw).
#[pyfunction]
fn accept_registration<'py>(py: Python<'py>, directory: PathBuf, raw: &[u8]) -> PyResult<bool> {
    let body = json_loads(py, raw)?;
    if !body.is_instance_of::<PyDict>() || !safe_text(&get(&body, "dvcId")?, 4096) {
        return Err(value_error("registration response has no device identity"));
    }
    let tokens = get(&body, "tokens")?;
    if !tokens.is_instance_of::<pyo3::types::PyList>() || tokens.len()? > 16 {
        return Err(value_error("registration token list invalid"));
    }
    let mut users: Vec<Bound<'py, PyAny>> = Vec::new();
    for t in tokens.try_iter()? {
        let t = t?;
        if t.is_instance_of::<PyDict>() && get(&t, "type")?.eq("user")? {
            users.push(t);
        }
    }
    if users.len() != 1 || !safe_text(&get(&users[0], "accessToken")?, 65536) {
        return Err(value_error("registration has no unique user token"));
    }
    let expiry = get(&users[0], "expiresIn")?;
    if !is_strict_number(&expiry) || !(expiry.gt(0)? && expiry.le(366 * 86400)?) {
        return Err(value_error("Cloud token expiry invalid"));
    }
    let token = PyDict::new(py);
    token.set_item("schemaVersion", 1)?;
    token.set_item("credentialKind", "CloudUser")?;
    token.set_item("receivedUnixSeconds", int_now(py)?)?;
    token.set_item("expiresIn", &expiry)?;
    token.set_item("dvcId", body.get_item("dvcId")?)?;
    token.set_item("accessToken", users[0].get_item("accessToken")?)?;
    rs_cloud::glue::save_json(&directory.join("cloud-token.json"), token.as_any())?;
    print_line(py, "Ayrı Cloud kullanıcı tokenı kaydedildi. Notlara yazılmadı.")?;
    Ok(true)
}

#[cfg(feature = "python")]
/// notes_cloud.register(session, endpoints_file, profile_file).
#[pyfunction]
fn register<'py>(py: Python<'py>, session: PathBuf, endpoints_file: PathBuf, profile_file: PathBuf) -> PyResult<bool> {
    let account = rs_cloud::glue::load_json(py, &session.join("notes-account-token.json"), 128 * 1024)?;
    let endpoints = rs_cloud::glue::load_json(py, &endpoints_file, 128 * 1024)?;
    let profile = rs_cloud::glue::load_json(py, &profile_file, 128 * 1024)?;
    let now = py.import("time")?.getattr("time")?.call0()?;
    let request = registration_request(py, &account, &endpoints, &profile, &now)?;
    let output = session.join("cloud");
    rs_cloud::util::mkdir_private(&output, false)?;
    rs_cloud::glue::save_json(&output.join("endpoints.json"), &endpoints)?;
    rs_cloud::glue::save_json(&output.join("profile.json"), &profile)?;
    let result = send_once(py, output.clone(), "register", &request, MAX_BYTES, false)?;
    let status = result.get_item(0)?;
    let raw = result.get_item(1)?;
    if !pyo3::types::PyAnyMethods::eq(&status, 200)? || raw.is_none() {
        return Ok(false);
    }
    accept_registration(py, output, &raw.cast::<PyBytes>()?.as_bytes().to_vec())
}

#[cfg(feature = "python")]
/// notes_cloud.changes(session).
#[pyfunction]
fn changes<'py>(py: Python<'py>, session: PathBuf) -> PyResult<bool> {
    let directory = session.join("cloud");
    let profile = rs_cloud::glue::load_json(py, &directory.join("profile.json"), 128 * 1024)?;
    let cloud = rs_cloud::glue::load_json(py, &directory.join("cloud-token.json"), 128 * 1024)?;
    let endpoints = rs_cloud::glue::load_json(py, &directory.join("endpoints.json"), 128 * 1024)?;
    let now = py.import("time")?.getattr("time")?.call0()?;
    let request = changes_request(py, &cloud, &endpoints, &profile, &profile.get_item("initialCursor")?, &now)?;
    let result = send_once(py, directory, "changes-initial", &request, MAX_BYTES, false)?;
    if !pyo3::types::PyAnyMethods::eq(&result.get_item(0)?, 200)? || result.get_item(1)?.is_none() {
        return Ok(false);
    }
    print_line(py, "İlk not değişiklik yanıtı özel dosyada. Cursor henüz ilerletilmedi.")?;
    Ok(true)
}

#[cfg(feature = "python")]
/// notes_cloud.scan_for_note(session, max_pages=100).
#[pyfunction]
#[pyo3(signature = (session, max_pages = 100))]
fn scan_for_note<'py>(py: Python<'py>, session: PathBuf, max_pages: i64) -> PyResult<bool> {
    let directory = session.join("cloud");
    let scan = directory.join("scan");
    rs_cloud::util::mkdir_private(&scan, true)?;
    let profile = rs_cloud::glue::load_json(py, &directory.join("profile.json"), 128 * 1024)?;
    let cloud = rs_cloud::glue::load_json(py, &directory.join("cloud-token.json"), 128 * 1024)?;
    let endpoints = rs_cloud::glue::load_json(py, &directory.join("endpoints.json"), 128 * 1024)?;
    let mut cursor = profile.get_item("initialCursor")?;
    let seen = pyo3::types::PySet::empty(py)?;
    let counts = PyDict::new(py);
    for k in ["pages", "changes", "deleted", "candidates"] {
        counts.set_item(k, 0)?;
    }
    let notes_cli: String = root(py)?.call_method1("__truediv__", ("target/debug/notes-cli",))?.str()?.extract()?;
    let subprocess = py.import("subprocess")?;

    for index in 0..max_pages {
        if seen.contains(&cursor)? {
            return Err(value_error("change feed cursor cycle"));
        }
        seen.add(&cursor)?;
        let stem = format!("page-{index:05}");
        let body_file = scan.join(format!("{stem}-response.body"));
        let parsed_file = scan.join(format!("{stem}.json"));
        if index == 0 && !body_file.exists() {
            let source = directory.join("changes-initial-response.body");
            if source.exists()
                && pyo3::types::PyAnyMethods::eq(
                    &rs_cloud::glue::load_json(py, &directory.join("changes-initial-receipt.json"), 128 * 1024)?
                        .call_method1("get", ("httpStatus",))?,
                    200,
                )?
            {
                rs_cloud::util::private_write(&body_file, &std::fs::read(&source)?)?;
            }
        }
        if !body_file.exists() {
            let now = py.import("time")?.getattr("time")?.call0()?;
            let request = changes_request(py, &cloud, &endpoints, &profile, &cursor, &now)?;
            let result = send_once(py, scan.clone(), &stem, &request, MAX_BYTES, true)?;
            if !pyo3::types::PyAnyMethods::eq(&result.get_item(0)?, 200)? || result.get_item(1)?.is_none() {
                return Ok(false);
            }
            py.import("time")?.call_method1("sleep", (0.2,))?;
        }
        if !parsed_file.exists() {
            let kw = PyDict::new(py);
            kw.set_item("check", true)?;
            kw.set_item("stdout", subprocess.getattr("DEVNULL")?)?;
            subprocess.call_method(
                "run",
                (vec![notes_cli.clone(), "export-changes".into(), body_file.to_string_lossy().into_owned(), parsed_file.to_string_lossy().into_owned()],),
                Some(&kw),
            )?;
        }
        let parsed = rs_cloud::glue::load_json(py, &parsed_file, 4 * MAX_BYTES)?;
        let digest = rs_cloud::util::sha256_hex(&std::fs::read(&body_file)?);
        if parsed.call_method1("get", ("sourceSha256",))?.ne(&digest)? {
            return Err(value_error("cached parsed feed does not match source"));
        }
        let page = parsed.get_item("page")?;
        counts.set_item("pages", counts.get_item("pages")?.unwrap().add(1)?)?;
        for change in page.get_item("changes")?.try_iter()? {
            let change = change?;
            counts.set_item("changes", counts.get_item("changes")?.unwrap().add(1)?)?;
            if is_true(py, &change.get_item("is_deleted")?) {
                counts.set_item("deleted", counts.get_item("deleted")?.unwrap().add(1)?)?;
            }
            if is_false(py, &change.get_item("is_deleted")?)
                && is_false(py, &change.get_item("is_locked")?)
                && is_false(py, &change.get_item("is_encrypted")?)
            {
                counts.set_item("candidates", counts.get_item("candidates")?.unwrap().add(1)?)?;
                if !scan.join("candidate.json").exists() {
                    let cand = PyDict::new(py);
                    cand.set_item("schemaVersion", 1)?;
                    cand.set_item("sourcePage", &stem)?;
                    cand.set_item("change", &change)?;
                    cand.set_item("currentContentVerified", false)?;
                    rs_cloud::glue::save_json(&scan.join("candidate.json"), cand.as_any())?;
                }
                let out = counts_with(py, &counts, &[("result", s(py, "unlocked_candidate_found")), ("durableCursorAdvanced", b_false(py))])?;
                print_json(py, out.as_any(), false)?;
                return Ok(true);
            }
        }
        if is_false(py, &page.get_item("has_next")?) {
            let out = counts_with(py, &counts, &[("result", s(py, "feed_finished_without_candidate")), ("durableCursorAdvanced", b_false(py))])?;
            print_json(py, out.as_any(), false)?;
            return Ok(false);
        }
        cursor = page.get_item("last_change_point")?;
        if index % 20 == 19 {
            let out = counts_with(py, &counts, &[("result", s(py, "scan_in_progress"))])?;
            print_json(py, out.as_any(), true)?;
        }
    }
    let out = counts_with(py, &counts, &[("result", s(py, "page_limit_reached")), ("durableCursorAdvanced", b_false(py))])?;
    print_json(py, out.as_any(), false)?;
    Ok(false)
}

#[cfg(feature = "python")]
/// notes_cloud.fetch_candidate(session).
#[pyfunction]
fn fetch_candidate<'py>(py: Python<'py>, session: PathBuf) -> PyResult<bool> {
    let directory = session.join("cloud");
    let candidate = rs_cloud::glue::load_json(py, &directory.join("scan").join("candidate.json"), 128 * 1024)?.get_item("change")?;
    if !is_false(py, &candidate.get_item("is_deleted")?)
        || !is_false(py, &candidate.get_item("is_locked")?)
        || !is_false(py, &candidate.get_item("is_encrypted")?)
    {
        return Err(value_error("candidate is not an unlocked ordinary note"));
    }
    let server_id_v = get(&candidate, "server_id")?;
    if !safe_text(&server_id_v, 512) {
        return Err(value_error("invalid server note identity"));
    }
    let server_id: String = server_id_v.extract()?;
    let cloud = rs_cloud::glue::load_json(py, &directory.join("cloud-token.json"), 128 * 1024)?;
    let profile = rs_cloud::glue::load_json(py, &directory.join("profile.json"), 128 * 1024)?;
    let endpoints = rs_cloud::glue::load_json(py, &directory.join("endpoints.json"), 128 * 1024)?;
    let now = py.import("time")?.getattr("time")?.call0()?;
    let base = changes_request(py, &cloud, &endpoints, &profile, &profile.get_item("initialCursor")?, &now)?;
    let note_url = format!("{}/samsungnotes/v3/notes/{}", endpoint(py, &endpoints, "default")?, rs_cloud::util::quote(&server_id));
    let output = directory.join("candidate-content");
    rs_cloud::util::mkdir_private(&output, false)?;
    let selection = PyDict::new(py);
    selection.set_item("schemaVersion", 1)?;
    selection.set_item("change", &candidate)?;
    selection.set_item("purpose", "read-only compatibility verification")?;
    rs_cloud::glue::save_json(&output.join("selection.json"), selection.as_any())?;

    let base_headers = py.import("builtins")?.getattr("dict")?.call1((base.call_method0("header_items")?,))?.cast_into::<PyDict>()?;
    let info = request(py, &format!("{note_url}/info"), None, "GET", &base_headers)?;
    let result = send_once(py, output.clone(), "info", &info, MAX_BYTES, false)?;
    if !pyo3::types::PyAnyMethods::eq(&result.get_item(0)?, 200)? || result.get_item(1)?.is_none() {
        return Ok(false);
    }
    let headers = py.import("builtins")?.getattr("dict")?.call1((base.call_method0("header_items")?,))?.cast_into::<PyDict>()?;
    headers.set_item("Content-Type", "application/xml;Charset=utf-8")?;
    headers.set_item("Content-Encoding", "gzip")?;
    headers.set_item("Accept", "multipart/form-data")?;
    let body = rs_cloud::util::gzip_mtime0(b"<noteTree ver=\"0\" isStrokeGrouped=\"false\" />");
    let request = request(py, &format!("{note_url}/compareForDownsync"), Some(&body), "POST", &headers)?;
    let result = send_once(py, output, "compare", &request, 32 * 1024 * 1024, false)?;
    if pyo3::types::PyAnyMethods::eq(&result.get_item(0)?, 200)? && !result.get_item(1)?.is_none() {
        print_line(py, "İlk notun metadata ve karşılaştırma içeriği özel dosyalara alındı. Not değiştirilmedi.")?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(feature = "python")]
/// notes_cloud.main().
#[pyfunction]
fn main(py: Python<'_>) -> PyResult<i32> {
    let argparse = py.import("argparse")?;
    let pkw = PyDict::new(py);
    pkw.set_item("description", py.import("notes_cloud")?.getattr("__doc__")?)?;
    let parser = argparse.getattr("ArgumentParser")?.call((), Some(&pkw))?;
    let subkw = PyDict::new(py);
    subkw.set_item("dest", "step")?;
    subkw.set_item("required", true)?;
    let sub = parser.call_method("add_subparsers", (), Some(&subkw))?;
    let path_type = py.import("pathlib")?.getattr("Path")?;
    let add_session = |name: &str| -> PyResult<Bound<'_, PyAny>> {
        let p = sub.call_method1("add_parser", (name,))?;
        let kw = PyDict::new(py);
        kw.set_item("type", &path_type)?;
        p.call_method("add_argument", ("session",), Some(&kw))?;
        Ok(p)
    };
    let reg = add_session("register")?;
    for opt in ["--endpoints", "--profile"] {
        let kw = PyDict::new(py);
        kw.set_item("type", &path_type)?;
        kw.set_item("required", true)?;
        reg.call_method("add_argument", (opt,), Some(&kw))?;
    }
    add_session("changes")?;
    let scan = add_session("scan")?;
    let scan_kw = PyDict::new(py);
    scan_kw.set_item("type", py.import("builtins")?.getattr("int")?)?;
    scan_kw.set_item("default", 100)?;
    scan.call_method("add_argument", ("--max-pages",), Some(&scan_kw))?;
    add_session("fetch-candidate")?;
    let args = parser.call_method0("parse_args")?;
    let step: String = args.getattr("step")?.extract()?;

    let run = || -> PyResult<bool> {
        let session: PathBuf = args.getattr("session")?.extract()?;
        match step.as_str() {
            "register" => register(py, session, args.getattr("endpoints")?.extract()?, args.getattr("profile")?.extract()?),
            "scan" => {
                let mp: i64 = args.getattr("max_pages")?.extract()?;
                if !(1..=1000).contains(&mp) {
                    return Err(value_error("invalid page bound"));
                }
                scan_for_note(py, session, mp)
            }
            "fetch-candidate" => fetch_candidate(py, session),
            _ => changes(py, session),
        }
    };
    match run() {
        Ok(ok) => Ok(if ok { 0 } else { 1 }),
        Err(e) if is_cloud_error(py, &e) => {
            print_stderr(py, "Cloud işlemi tamamlanmadı; özel yanıt ve işlem kaydı korunuyor. Otomatik tekrar yok.")?;
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
fn int_now<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    py.import("builtins")?.getattr("int")?.call1((py.import("time")?.getattr("time")?.call0()?,))
}
#[cfg(feature = "python")]
fn s<'py>(py: Python<'py>, v: &str) -> Bound<'py, PyAny> {
    pyo3::types::PyString::new(py, v).into_any()
}
#[cfg(feature = "python")]
fn b_false(py: Python<'_>) -> Bound<'_, PyAny> {
    pyo3::types::PyBool::new(py, false).to_owned().into_any()
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
fn is_cloud_error(py: Python<'_>, e: &PyErr) -> bool {
    e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyValueError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
}

#[cfg(feature = "python")]
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(endpoint, m)?)?;
    m.add_function(wrap_pyfunction!(registration_request, m)?)?;
    m.add_function(wrap_pyfunction!(changes_request, m)?)?;
    m.add_function(wrap_pyfunction!(send_once, m)?)?;
    m.add_function(wrap_pyfunction!(register, m)?)?;
    m.add_function(wrap_pyfunction!(accept_registration, m)?)?;
    m.add_function(wrap_pyfunction!(changes, m)?)?;
    m.add_function(wrap_pyfunction!(scan_for_note, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_candidate, m)?)?;
    m.add_function(wrap_pyfunction!(main, m)?)?;
    rs_common::export(
        m,
        &[
            "endpoint", "registration_request", "changes_request", "send_once", "register",
            "accept_registration", "changes", "scan_for_note", "fetch_candidate", "main",
        ],
    )
}
