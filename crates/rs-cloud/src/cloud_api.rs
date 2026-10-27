//! Replacement for the Python module `cloud_api`.
//!
//! CloudAPI keeps its state in the instance `__dict__` (Path objects, the
//! injected transport/clock, profile dicts) and dispatches every internal
//! call through Python attribute lookup, so tests and engine code that patch
//! instance attributes or subclass CloudAPI keep working. Login/renewal
//! internals delegate to the Python implementation (see `auth_impl`).
use crate::glue::*;
use crate::guards::{self, GuardError};
use crate::pyjson::{self, Value};
use crate::util;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyCFunction, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use std::path::{Path, PathBuf};

pub const MAX_CONTENT_FILE: usize = 64 * 1024 * 1024;
const MIB2: usize = 2 * 1024 * 1024;
const RESULT_LIMIT: usize = 96 * 1024 * 1024;

// ------------------------------------------------------------ dependencies

#[cfg(feature = "python")]
/// Python-facing document objects (DocumentSnapshot, parse_document for
/// snapshots handed to callers): the doc area's live module.
fn doc<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    crate::areas::live(py, "samsung_notes.document", name)
}

#[cfg(feature = "python")]
/// Python-facing editor functions (models, projections, snapshot edits).
fn editor<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    crate::areas::editor_fn(py, name)
}

#[cfg(feature = "python")]
pub fn max_body_bytes(py: Python<'_>) -> PyResult<usize> {
    crate::areas::max_body_bytes(py)
}

#[cfg(feature = "python")]
/// The Python CloudAPI, loaded as a private copy of samsung_notes/cloud_api.py
/// whose exception names are rebound to the Rust classes. Only the login /
/// token-renewal methods use it (ported last by design).
fn auth_impl<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    const NAME: &str = "samsung_notes._cloud_api_python_auth";
    let modules = py.import("sys")?.getattr("modules")?;
    if !modules.contains(NAME)? {
        let path = py.import("samsung_notes.cloud_api")?.getattr("__file__")?;
        let iu = py.import("importlib.util")?;
        let spec = iu.call_method1("spec_from_file_location", (NAME, path))?;
        let module = iu.call_method1("module_from_spec", (&spec,))?;
        modules.set_item(NAME, &module)?;
        if let Err(e) = spec.getattr("loader")?.call_method1("exec_module", (&module,)) {
            modules.del_item(NAME)?;
            return Err(e);
        }
        module.setattr("APIError", py.get_type::<APIError>())?;
        module.setattr("ConflictError", py.get_type::<ConflictError>())?;
        module.setattr("UnsupportedDocumentError", py.get_type::<UnsupportedDocumentError>())?;
    }
    rs_common::py_fallback(py, NAME, "CloudAPI")?.getattr(name)
}

#[cfg(feature = "python")]
fn delegate<'py>(slf: &Bound<'py, PyAny>, name: &str, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
    let py = slf.py();
    let function = auth_impl(py, name)?;
    let mut all: Vec<Bound<'py, PyAny>> = vec![slf.clone()];
    all.extend(args.iter());
    function.call(PyTuple::new(py, all)?, kwargs)
}

// ------------------------------------------------------------ small helpers

#[cfg(feature = "python")]
pub fn work_dir(api: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
    fspath(&api.getattr("work_dir")?)
}

#[cfg(feature = "python")]
/// cloud_api._uuid
pub fn check_uuid(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
    match opt_str(value) {
        Some(s) if util::is_canonical_uuid(&s) => Ok(s),
        _ => Err(api_error(py, "invalid_operation", "İşlem kimliği geçersiz.")),
    }
}

#[cfg(feature = "python")]
/// document.normalize_commit (private copy; raises document.DocumentError)
pub fn normalize_commit(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
    guards::normalize_commit(opt_str(value).as_deref()).map_err(|_| document_error(py, "Senkronizasyon sürümü eksik veya geçersiz."))
}

#[cfg(feature = "python")]
/// `snapshot = DocumentSnapshot.from_dict(...)` + type/server binding check.
pub fn coerce_snapshot<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let cls = doc(py, "DocumentSnapshot")?;
    let snap = if snapshot.is_instance_of::<PyDict>() { cls.call_method1("from_dict", (snapshot,))? } else { snapshot.clone() };
    if !snap.is_instance(&cls)? {
        return Err(conflict(py, None));
    }
    let sid = snap.getattr("server_id")?;
    if !(sid.is_none() || sid.is(server_id) || sid.eq(server_id)?) {
        return Err(conflict(py, None));
    }
    Ok(snap)
}

#[cfg(feature = "python")]
/// `type(now_ms) is not int or not 0 < now_ms < 2**63` check.
pub fn check_now_ms(py: Python<'_>, now_ms: &Bound<'_, PyAny>, message: &str) -> PyResult<()> {
    if now_ms.is_none() {
        return Ok(());
    }
    let ok = now_ms.get_type().is(py.get_type::<PyInt>())
        && now_ms.extract::<i128>().map(|v| 0 < v && v < (1i128 << 63)).unwrap_or(false);
    if !ok {
        return Err(unsupported(py, Some(message)));
    }
    Ok(())
}

#[cfg(feature = "python")]
pub fn resource_sha(snapshot: &Bound<'_, PyAny>) -> PyResult<String> {
    sha256_obj(&snapshot.getattr("resource_xml")?)
}

#[cfg(feature = "python")]
pub fn body_sha(snapshot: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(util::sha256_hex(&utf8_of(&snapshot.getattr("body")?)?))
}

#[cfg(feature = "python")]
/// The shared "fresh GET equals the caller's snapshot" check.
pub fn fresh_matches<'py>(py: Python<'py>, api: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, baseline: &Bound<'py, PyAny>, opaque: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let fresh = api.call_method1("get_note", (server_id,))?;
    let mismatch = fresh.getattr("local_id")?.ne(snapshot.getattr("local_id")?)?
        || normalize_commit(py, &fresh.getattr("commit_id")?)? != normalize_commit(py, baseline)?
        || fresh.getattr("resource_xml")?.ne(snapshot.getattr("resource_xml")?)?
        || match opaque {
            Some(o) => api.call_method1("_opaque_content_fingerprint", (&fresh,))?.ne(o)?,
            None => false,
        };
    if mismatch {
        return Err(conflict(py, None));
    }
    Ok(fresh)
}

#[cfg(feature = "python")]
/// prepared.json: freeze the operation timestamp once.
pub fn prepared_at<'py>(py: Python<'py>, api: &Bound<'py, PyAny>, output: &Path, now_ms: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let prepared = output.join("prepared.json");
    if prepared.exists() {
        return load_json(py, &prepared, DEFAULT_LIMIT)?.get_item("now_ms");
    }
    let at = if now_ms.is_none() { int(py, millis(&api.call_method0("_clock")?)?) } else { now_ms.clone() };
    json_once(py, &prepared, dict(py, &[("now_ms", at.clone())])?.as_any())?;
    Ok(at)
}

#[cfg(feature = "python")]
/// `int(ET.fromstring(draft).get('syncModifiedTime'))`
fn draft_time<'py>(py: Python<'py>, path: &Path) -> PyResult<Bound<'py, PyAny>> {
    let raw = std::fs::read(path)?;
    let root = crate::etree::parse_bytes(&raw, true).map_err(|e| parse_error(py, &e))?;
    let value = root.get("syncModifiedTime").map(str::to_string);
    py.import("builtins")?.getattr("int")?.call1((value,))
}

#[cfg(feature = "python")]
pub fn parse_error(py: Python<'_>, message: &str) -> PyErr {
    match py.import("xml.etree.ElementTree").and_then(|m| m.getattr("ParseError")).and_then(|c| c.call1((message,))) {
        Ok(inst) => PyErr::from_value(inst),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
/// `with self._lock:`
fn with_lock<'py, T>(api: &Bound<'py, PyAny>, body: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let lock = api.getattr("_lock")?;
    lock.call_method0("__enter__")?;
    let result = body();
    let py = api.py();
    lock.call_method1("__exit__", (none(py), none(py), none(py)))?;
    result
}

#[cfg(feature = "python")]
fn guard_err(py: Python<'_>, e: GuardError) -> PyErr {
    match e {
        GuardError::Conflict => conflict(py, None),
        GuardError::Invalid => PyValueError::new_err(()),
    }
}

#[cfg(feature = "python")]
fn raw_arg(v: &Bound<'_, PyAny>) -> Option<Vec<u8>> {
    bytes_of(v)
}

#[cfg(feature = "python")]
fn opt_str_arg(v: &Bound<'_, PyAny>) -> Option<String> {
    opt_str(v)
}

#[cfg(feature = "python")]
/// Accept the guard with or without a leading `self` (Python methods were
/// borrowed onto duck-typed fakes: `_compare_guard = CloudAPI._compare_guard`).
fn guard_args<'py>(args: &Bound<'py, PyTuple>, n: usize) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let all: Vec<Bound<'py, PyAny>> = args.iter().collect();
    match all.len() {
        l if l == n => Ok(all),
        l if l == n + 1 => Ok(all[1..].to_vec()),
        l => Err(PyTypeError::new_err(format!("guard takes {} positional arguments but {} were given", n, l))),
    }
}

#[cfg(feature = "python")]
fn expiry<'py>(py: Python<'py>, token: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if !token.is_instance_of::<PyDict>() {
        return Ok(int(py, 0));
    }
    let mut lifetime = token.call_method1("get", ("expiresIn",))?;
    if token.call_method1("get", ("credentialKind",))?.eq("NotesServiceOAuth")? {
        let response = token.call_method1("get", ("response", PyDict::new(py)))?;
        let fallback = token.call_method1("get", ("response", PyDict::new(py)))?.call_method1("get", ("expires_in",))?;
        lifetime = response.call_method1("get", ("access_token_expires_in", fallback))?;
    }
    let received = token.call_method1("get", ("receivedUnixSeconds",))?;
    let exact_num = |v: &Bound<'py, PyAny>| v.get_type().is(py.get_type::<PyInt>()) || v.get_type().is(py.get_type::<PyFloat>());
    let isfinite = py.import("math")?.getattr("isfinite")?;
    if exact_num(&received)
        && exact_num(&lifetime)
        && isfinite.call1((&received,))?.is_truthy()?
        && isfinite.call1((&lifetime,))?.is_truthy()?
        && lifetime.gt(0)?
    {
        return received.add(&lifetime);
    }
    Ok(int(py, 0))
}

#[cfg(feature = "python")]
fn exact_number(py: Python<'_>, v: &Bound<'_, PyAny>) -> bool {
    v.get_type().is(py.get_type::<PyInt>()) || v.get_type().is(py.get_type::<PyFloat>())
}

#[cfg(feature = "python")]
fn endpoint_of(endpoints: &Bound<'_, PyAny>, role: &str) -> PyResult<String> {
    let value = endpoints.call_method1("get", (role,))?;
    let v = match opt_str(&value) {
        Some(s) => Value::Str(s),
        None => Value::Null,
    };
    util::endpoint(Some(&v)).map_err(PyValueError::new_err)
}

// ------------------------------------------------------------ pure builders

/// The ink/erase multipart body (noteResource + strokeResource parts).
pub fn ink_multipart(boundary: &str, upload_xml: &[u8], parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"noteResource\"\r\nContent-Type: application/xml; charset=UTF-8\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(upload_xml);
    for (filename, data) in parts {
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"strokeResource\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(data);
    }
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// createUploadURLs request body and validation key.
pub fn upload_request_body(file_hash: &str, size: usize, token: &str) -> String {
    let digest = util::sha256(token.as_bytes());
    let file = (0..32).map(|i| u8::from_str_radix(&file_hash[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>();
    let key: Vec<u8> = file.iter().zip(digest.iter()).map(|(a, b)| a ^ b).collect();
    format!(
        "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><request><fileInfoList><fileInfo><hash>{}</hash><contentType>application/octet-stream</contentType><size>{}</size><validationKey>{}</validationKey></fileInfo></fileInfoList></request>",
        file_hash,
        size,
        util::hex(&key)
    )
}

/// One opaque-part record for the fingerprint, in Python tuple order.
pub type OpaqueRecord = (bool, String, bool, String, String, String);

/// `sha256(json.dumps(sorted(records), ensure_ascii=True, separators=(',', ':')))`
pub fn opaque_digest(mut records: Vec<OpaqueRecord>) -> String {
    records.sort();
    let list = Value::List(
        records
            .into_iter()
            .map(|(a, b, c, d, e, f)| Value::List(vec![Value::Bool(a), Value::Str(b), Value::Bool(c), Value::Str(d), Value::Str(e), Value::Str(f)]))
            .collect(),
    );
    util::sha256_hex(pyjson::dumps(&list, &pyjson::COMPACT).as_bytes())
}

#[cfg(feature = "python")]
/// Opaque records of a snapshot's MIME parts; `removed` stroke filenames are
/// skipped (cloud_page_ops._opaque_without).
pub fn opaque_records(py: Python<'_>, snapshot: &Bound<'_, PyAny>, removed: Option<&[String]>) -> PyResult<Vec<OpaqueRecord>> {
    let mut records = Vec::new();
    for part in crate::areas::snapshot_parts(py, snapshot)? {
        if part.content_type == "application/xml" || part.content_type == "text/xml" {
            continue;
        }
        if let (Some(removed), Some("strokeResource"), Some(f)) = (removed, part.name.as_deref(), part.filename.as_ref()) {
            if removed.contains(f) {
                continue;
            }
        }
        records.push((
            part.name.is_some(),
            part.name.clone().unwrap_or_default(),
            part.filename.is_some(),
            part.filename.clone().unwrap_or_default(),
            part.content_type.clone(),
            util::sha256_hex(&part.body),
        ));
    }
    Ok(records)
}

#[cfg(feature = "python")]
pub fn appended_records(appended: &Bound<'_, PyAny>) -> PyResult<Vec<OpaqueRecord>> {
    let mut records = Vec::new();
    for part in appended.try_iter()? {
        let part = part?;
        records.push((true, str_of(&part.getattr("name")?)?, true, str_of(&part.getattr("filename")?)?, "application/octet-stream".to_string(), sha256_obj(&part.getattr("body")?)?));
    }
    Ok(records)
}

#[cfg(feature = "python")]
fn check_opaque_size(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<()> {
    let raw = snapshot.getattr("raw_body")?;
    let max = max_body_bytes(py)?;
    match raw.cast::<PyBytes>() {
        Ok(b) if !b.as_bytes().is_empty() && b.as_bytes().len() <= max => Ok(()),
        _ => Err(unsupported(py, Some("Belge boyutu sınırını aşıyor."))),
    }
}

// ------------------------------------------------------------ transport

#[cfg(feature = "python")]
/// CloudAPI._call
pub fn call_transport<'py>(api: &Bound<'py, PyAny>, directory: &Bound<'py, PyAny>, name: &str, request: &Bound<'py, PyAny>, limit: usize) -> PyResult<Bound<'py, PyTuple>> {
    let py = api.py();
    let dir = fspath(directory)?;
    let transport = api.getattr("_transport")?;
    if transport.is_truthy()? {
        let result = transport.call1((directory, name, request, limit))?;
        let items: Vec<Bound<'py, PyAny>> = result.try_iter()?.collect::<PyResult<_>>()?;
        if items.len() != 3 {
            return Err(PyValueError::new_err(format!("expected 3 values to unpack (got {})", items.len())));
        }
        let (status, raw, ctype) = (&items[0], &items[1], &items[2]);
        if !raw.is_none() {
            let ok = raw.cast::<PyBytes>().map(|b| b.as_bytes().len() <= limit).unwrap_or(false);
            if !ok {
                return Err(api_error(py, "invalid_response", "Sunucu yanıtı boyut sınırını aşıyor."));
            }
        }
        save_json(&dir.join(format!("{name}-receipt.json")), dict(py, &[("httpStatus", status.clone()), ("automaticRetry", boolean(py, false))])?.as_any())?;
        if let Some(data) = bytes_of(raw) {
            private_write(&dir.join(format!("{name}-response.body")), &data)?;
        }
        save_json(&dir.join(format!("{name}-response-meta.json")), dict(py, &[("contentType", ctype.clone())])?.as_any())?;
        return PyTuple::new(py, [status.clone(), raw.clone(), ctype.clone()]);
    }
    // Real network: tools/notes_cloud.send_once (single request, no retry).
    let send_once = rs_common::py_fallback(py, "notes_cloud", "send_once")?;
    let kw = dict(py, &[("max_bytes", int(py, limit as i64)), ("quiet", boolean(py, true))])?;
    let result = send_once.call((directory, name, request), Some(&kw))?;
    let status = result.get_item(0)?;
    let raw = result.get_item(1)?;
    let meta = dir.join(format!("{name}-response-meta.json"));
    let ctype = if meta.exists() {
        load_json(py, &meta, DEFAULT_LIMIT)?.call_method1("get", ("contentType", ""))?
    } else {
        s(py, "")
    };
    PyTuple::new(py, [status, raw, ctype])
}

#[cfg(feature = "python")]
fn call_api<'py>(api: &Bound<'py, PyAny>, directory: &Bound<'py, PyAny>, name: &str, request: &Bound<'py, PyAny>, limit: Option<usize>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let py = api.py();
    let result = match limit {
        Some(l) => api.call_method("_call", (directory, name, request), Some(&dict(py, &[("limit", int(py, l as i64))])?))?,
        None => api.call_method1("_call", (directory, name, request))?,
    };
    let items: Vec<Bound<'py, PyAny>> = result.try_iter()?.collect::<PyResult<_>>()?;
    if items.len() != 3 {
        return Err(PyValueError::new_err(format!("expected 3 values to unpack (got {})", items.len())));
    }
    Ok((items[0].clone(), items[1].clone(), items[2].clone()))
}

#[cfg(feature = "python")]
pub fn read_status(py: Python<'_>, status: &Bound<'_, PyAny>, raw: &Bound<'_, PyAny>) -> PyResult<()> {
    let code = status_code(status);
    if code == Some(404) {
        return Err(api_error(py, "not_found", "Not sunucuda bulunamadı."));
    }
    if matches!(code, Some(401) | Some(403)) {
        return Err(api_error(py, "reauth_required", "Samsung oturumunu yenilemek gerekiyor."));
    }
    if code != Some(200) || raw.is_none() {
        return Err(api_error(py, "cloud_unavailable", "Cloud yanıtı alınamadı; yerel veriler korunuyor."));
    }
    Ok(())
}

#[cfg(feature = "python")]
/// headers + compareForUpsync request + checks/<uuid> attempt + _call + _read_status.
pub fn compare_upsync<'py>(api: &Bound<'py, PyAny>, output: &Path, server_id: &Bound<'py, PyAny>, tree_payload: &[u8], attempt_dir: Option<&Path>) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let headers = api.call_method1("_headers", (true,))?;
    headers.call_method1("update", (dict(py, &[("Content-Type", s(py, "application/xml;Charset=utf-8")), ("Content-Encoding", s(py, "gzip"))])?,))?;
    let url = str_of(&api.call_method1("_url", (server_id,))?)? + "/compareForUpsync";
    let request = request(py, &url, Some(tree_payload), Some("POST"), &headers)?;
    let attempt = match attempt_dir {
        Some(parent) => {
            let a = parent.join(util::uuid4());
            util::mkdir_private(&a, false)?;
            a
        }
        None => {
            let checks = output.join("checks");
            util::mkdir_private(&checks, true)?;
            let a = checks.join(util::uuid4());
            util::mkdir_private(&a, false)?;
            a
        }
    };
    let (status, raw, _) = call_api(api, &py_path(py, &attempt)?, "compare", &request, None)?;
    api.call_method1("_read_status", (&status, &raw))?;
    Ok(raw)
}

#[cfg(feature = "python")]
/// `try: guard() except (ValueError, TypeError, ET.ParseError): raise ConflictError(message)`
pub fn guarded(py: Python<'_>, r: PyResult<Bound<'_, PyAny>>, message: &str) -> PyResult<()> {
    try_map(py, r, &[K::Value, K::Type, K::Parse], || conflict(py, Some(message))).map(|_| ())
}

#[cfg(feature = "python")]
/// CloudAPI._operation (shared layout; `reconcile` is the published-path hook).
pub fn operation<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, intent: &Bound<'py, PyAny>, reconcile: impl FnOnce(&Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyTuple>> {
    let py = api.py();
    let id = check_uuid(py, operation_id)?;
    let root = work_dir(api)?.join("operations");
    util::mkdir_private(&root, true)?;
    let output = root.join(&id);
    let value = dict(py, &[("schemaVersion", int(py, 1)), ("accountFingerprint", api.getattr("_identity")?)])?;
    for (k, v) in intent.cast::<PyDict>()?.iter() {
        value.set_item(k, v)?;
    }
    let out_py = py_path(py, &output)?;
    match util::mkdir_private(&output, false) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if load_json(py, &output.join("intent.json"), MIB2)?.ne(&value)? {
                return Err(api_error(py, "operation_mismatch", "Bu işlem kimliği farklı bir kayıt için kullanılmış."));
            }
            if output.join("result.json").exists() {
                return PyTuple::new(py, [out_py, load_json(py, &output.join("result.json"), RESULT_LIMIT)?]);
            }
            if output.join("published.json").exists() {
                return PyTuple::new(py, [out_py, reconcile(operation_id)?]);
            }
            if output.join(".mutation-claim").exists() {
                return Err(uncertain(py, "write_uncertain", "Önceki işlemin sonucu belirsiz; tekrar gönderilmedi."));
            }
            return PyTuple::new(py, [out_py, none(py)]);
        }
        Err(e) => return Err(e.into()),
    }
    save_json(&output.join("intent.json"), value.as_any())?;
    PyTuple::new(py, [out_py, none(py)])
}

#[cfg(feature = "python")]
/// CloudAPI._publish body shared with cloud_ink_erase (`rejected` differs).
pub fn publish<'py>(
    api: &Bound<'py, PyAny>,
    output: &Bound<'py, PyAny>,
    url: &str,
    payload: &Bound<'py, PyAny>,
    content_type: &Bound<'py, PyAny>,
    rejected: &str,
    extra_view_errors: &[K],
    reconcile: impl FnOnce(&Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let out = fspath(output)?;
    let headers = api.call_method1("_headers", (true,))?;
    headers.call_method1("update", (dict(py, &[("Content-Type", content_type.clone()), ("Content-Encoding", s(py, "gzip"))])?,))?;
    let payload_file = out.join("request.multipart.gz");
    let mut payload = payload.clone();
    if payload_file.exists() {
        let old = std::fs::read(&payload_file)?;
        let meta = load_json(py, &out.join("request-meta.json"), DEFAULT_LIMIT)?;
        if old.len() > max_body_bytes(py)? || meta.get_item("sha256")?.ne(util::sha256_hex(&old))? {
            return Err(api_error(py, "operation_mismatch", "Hazırlanmış gönderimin bütünlüğü değişti."));
        }
        let limit = max_body_bytes(py)? + 1;
        let decode_parts = |data: &[u8], ctype: &Bound<'py, PyAny>| {
            let raw = util::gunzip_limited(data, limit).map_err(|util::GunzipError::Io(e)| PyErr::from(e))?;
            crate::areas::payload_parts(py, &raw, ctype)
        };
        let meta_ctype = meta.get_item("content_type")?;
        let new_bytes = payload.cast::<PyBytes>()?.as_bytes().to_vec();
        if decode_parts(&old, &meta_ctype)? != decode_parts(&new_bytes, content_type)? {
            return Err(api_error(py, "operation_mismatch", "Hazırlanmış gönderimin içeriği değişti."));
        }
        payload = b(py, &old);
        headers.set_item("Content-Type", &meta_ctype)?;
    } else {
        let data = payload.cast::<PyBytes>()?.as_bytes().to_vec();
        private_write(&payload_file, &data)?;
        save_json(&out.join("request-meta.json"), dict(py, &[("content_type", content_type.clone()), ("sha256", s(py, &util::sha256_hex(&data)))])?.as_any())?;
    }
    private_write(&out.join(".mutation-claim"), b"claimed before write; never automatically repeat\n")?;
    let data = payload.cast::<PyBytes>()?.as_bytes().to_vec();
    let request = request(py, url, Some(&data), Some("POST"), &headers)?;
    let (status, raw, _) = try_map(py, call_api(api, output, "publish", &request, None), &[K::OS, K::Value, K::Api], || {
        uncertain(py, "write_uncertain", "Gönderimin sonucu doğrulanamadı; tekrar gönderilmedi.")
    })?;
    let code = status_code(&status);
    if code != Some(201) || raw.is_none() {
        if code == Some(409) {
            return Err(conflict(py, None));
        }
        if matches!(code, Some(400) | Some(401) | Some(403) | Some(404) | Some(413) | Some(422)) {
            return Err(api_error(py, "write_rejected", rejected));
        }
        return Err(uncertain(py, "write_uncertain", "Gönderimin sonucu doğrulanamadı; tekrar gönderilmedi."));
    }
    let view = view_result(py, &raw);
    let mut kinds = vec![K::Value, K::Type, K::Parse];
    kinds.extend_from_slice(extra_view_errors);
    let result = try_map(py, view, &kinds, || uncertain(py, "write_uncertain", "Sunucu kaydetti ancak sürüm yanıtı doğrulanamadı."))?;
    save_json(&out.join("published.json"), &result)?;
    reconcile(&output.getattr("name")?)
}

#[cfg(feature = "python")]
/// `result = default_view(raw); normalize_commit(result['commitId'])`
fn view_result<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let result = default_view_py(py, raw)?;
    normalize_commit(py, &result.get_item("commitId")?)?;
    Ok(result)
}

#[cfg(feature = "python")]
/// tools/notes_sync_probe.default_view (ValueError / ET.ParseError).
pub fn default_view_py<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let data = raw.cast::<PyBytes>().map_err(|_| PyTypeError::new_err("a bytes-like object is required"))?.as_bytes().to_vec();
    match guards::default_view(&data) {
        Ok(pairs) => {
            let d = PyDict::new(py);
            for (k, v) in pairs {
                d.set_item(k, v)?;
            }
            Ok(d.into_any())
        }
        Err(guards::ViewError::Value(m)) => Err(PyValueError::new_err(m)),
        Err(guards::ViewError::Parse(m)) => Err(parse_error(py, &m)),
    }
}

fn ink_boundary() -> String {
    format!("notes-ink-{}", util::token_hex(16))
}

#[cfg(feature = "python")]
/// Ink multipart from `edit.upload_xml` + `edit.binary_parts`, size-checked, gzip.
pub fn ink_payload<'py>(py: Python<'py>, edit: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let boundary = ink_boundary();
    let upload = bytes_of(&edit.getattr("upload_xml")?).ok_or_else(|| PyTypeError::new_err("can't concat to bytes"))?;
    let mut parts = Vec::new();
    for p in edit.getattr("binary_parts")?.try_iter()? {
        let p = p?;
        let filename = p.getattr("filename")?.str()?.to_string();
        let body = bytes_of(&p.getattr("body")?).ok_or_else(|| PyTypeError::new_err("can't concat to bytes"))?;
        parts.push((filename, body));
    }
    let body = ink_multipart(&boundary, &upload, &parts);
    if body.len() > max_body_bytes(py)? {
        return Err(unsupported(py, Some("Çizim gönderimi boyut sınırını aşıyor.")));
    }
    Ok((b(py, &util::gzip_mtime0(&body)), s(py, &format!("multipart/form-data;boundary={boundary}"))))
}

#[cfg(feature = "python")]
fn gz<'py>(py: Python<'py>, data: &Bound<'py, PyAny>) -> PyResult<Vec<u8>> {
    let raw = bytes_of(data).ok_or_else(|| PyTypeError::new_err("memoryview: a bytes-like object is required"))?;
    let _ = py;
    Ok(util::gzip_mtime0(&raw))
}

#[cfg(feature = "python")]
/// expected.json / draft.xml / compare-request.gz receipts, in Python order.
pub fn save_receipts<'py>(py: Python<'py>, output: &Path, expected: &Bound<'py, PyAny>, edit: &Bound<'py, PyAny>) -> PyResult<Vec<u8>> {
    json_once(py, &output.join("expected.json"), expected)?;
    save_once(py, &output.join("draft.xml"), &bytes_of(&edit.getattr("full_xml")?).ok_or_else(|| PyTypeError::new_err("bytes expected"))?)?;
    let tree = gz(py, &edit.getattr("tree_xml")?)?;
    save_once(py, &output.join("compare-request.gz"), &tree)?;
    Ok(tree)
}

#[cfg(feature = "python")]
fn tuple2<'py>(t: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let items: Vec<Bound<'py, PyAny>> = t.try_iter()?.collect::<PyResult<_>>()?;
    if items.len() != 2 {
        return Err(PyValueError::new_err(format!("expected 2 values to unpack (got {})", items.len())));
    }
    Ok((items[0].clone(), items[1].clone()))
}

#[cfg(feature = "python")]
fn kw<'py>(py: Python<'py>, items: &[(&str, Bound<'py, PyAny>)]) -> PyResult<Bound<'py, PyDict>> {
    dict(py, items)
}

// ------------------------------------------------------------ CloudAPI

#[cfg(feature = "python")]
#[pyclass(subclass, dict, module = "samsung_notes.cloud_api", name = "CloudAPI")]
pub struct CloudAPI;

fn mkdir_parents(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match p.parent() {
                Some(parent) if parent != p && !parent.as_os_str().is_empty() => {
                    match std::fs::create_dir_all(parent) {
                        Ok(()) => {}
                        Err(e2) if parent.is_dir() => drop(e2),
                        Err(e2) => return Err(e2),
                    }
                }
                _ => return Err(e),
            }
            match std::fs::DirBuilder::new().mode(0o700).create(p) {
                Err(e) if !p.is_dir() => Err(e),
                _ => Ok(()),
            }
        }
        Err(e) => {
            if p.is_dir() {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

#[cfg(feature = "python")]
impl CloudAPI {
    #[cfg(feature = "python")]
    fn opaque_fingerprint(py: Python<'_>, snapshot: &Bound<'_, PyAny>, appended: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
        check_opaque_size(py, snapshot)?;
        let mut records = opaque_records(py, snapshot, None)?;
        if let Some(a) = appended {
            records.extend(appended_records(a)?);
        }
        Ok(opaque_digest(records))
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl CloudAPI {
    #[cfg(feature = "python")]
    #[classattr]
    const MAX_CONTENT_FILE: usize = MAX_CONTENT_FILE;

    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        CloudAPI
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (session_path, work_dir, *, transport = None, changes_parser = None, clock = None))]
    fn __init__(
        slf: &Bound<'_, Self>,
        session_path: &Bound<'_, PyAny>,
        work_dir: &Bound<'_, PyAny>,
        transport: Option<&Bound<'_, PyAny>>,
        changes_parser: Option<&Bound<'_, PyAny>>,
        clock: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let this = slf.as_any();
        let pathlib = py.import("pathlib")?.getattr("Path")?;
        let session = pathlib.call1((session_path,))?;
        let work = pathlib.call1((work_dir,))?;
        this.setattr("session_path", &session)?;
        this.setattr("work_dir", &work)?;
        let wd = fspath(&work)?;
        mkdir_parents(&wd)?;
        let unsafe_dir = std::fs::symlink_metadata(&wd).map(|m| m.file_type().is_symlink()).unwrap_or(false) || {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&wd)?.permissions().mode() & 0o077 != 0
        };
        if unsafe_dir {
            return Err(api_error(py, "unsafe_storage", "Oturum çalışma dizini özel olmalı."));
        }
        this.setattr("_transport", transport.cloned().unwrap_or_else(|| none(py)))?;
        this.setattr("_changes_parser", changes_parser.cloned().unwrap_or_else(|| none(py)))?;
        let clock = match clock {
            Some(c) => c.clone(),
            None => py.import("time")?.getattr("time")?,
        };
        this.setattr("_clock", clock)?;
        this.setattr("_lock", py.import("threading")?.getattr("RLock")?.call0()?)?;
        let sp = fspath(&session)?;
        let profile = load_json(py, &sp.join("cloud").join("profile.json"), DEFAULT_LIMIT)?;
        this.setattr("profile", &profile)?;
        let endpoints = load_json(py, &sp.join("cloud").join("endpoints.json"), DEFAULT_LIMIT)?;
        this.setattr("endpoints", &endpoints)?;
        let base = endpoint_of(&endpoints, "default")?;
        let play = endpoint_of(&endpoints, "play")?;
        this.setattr("base", base)?;
        this.setattr("play", play)?;
        let cursor = profile.call_method1("get", ("initialCursor",))?;
        this.setattr("initial_cursor", &cursor)?;
        if !cursor.is_instance_of::<PyString>() {
            return Err(api_error(py, "invalid_profile", "Başlangıç senkronizasyon bilgisi eksik."));
        }
        this.call_method0("_validate_identity")?;
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _cloud<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let path = work_dir(this)?.join("cloud-token.json");
        if path.exists() {
            let cached = load_json(py, &path, DEFAULT_LIMIT)?;
            if cached.call_method1("get", ("accountFingerprint",))?.eq(this.getattr("_identity")?)? {
                return Ok(cached);
            }
        }
        load_json(py, &fspath(&this.getattr("session_path")?)?.join("cloud").join("cloud-token.json"), DEFAULT_LIMIT)
    }

    #[cfg(feature = "python")]
    fn _original_account<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        load_json(slf.py(), &fspath(&slf.as_any().getattr("session_path")?)?.join("notes-account-token.json"), DEFAULT_LIMIT)
    }

    #[cfg(feature = "python")]
    fn _account<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let original = this.call_method0("_original_account")?;
        let path = work_dir(this)?.join("notes-account-token.json");
        if path.exists() {
            let cached = load_json(py, &path, DEFAULT_LIMIT)?;
            let client = original.get_item("clientId")?;
            let user = original.get_item("response")?.get_item("userId")?;
            let joined = client.add("\0")?.add(&user)?;
            let identity = util::sha256_hex(&utf8_of(&joined)?);
            let cached_response = cached.call_method1("get", ("response", PyDict::new(py)))?;
            if cached.call_method1("get", ("accountFingerprint",))?.eq(&identity)?
                && cached.call_method1("get", ("credentialKind",))?.eq("NotesServiceOAuth")?
                && cached.call_method1("get", ("schemaVersion",))?.eq(1)?
                && cached.call_method1("get", ("clientId",))?.eq(&client)?
                && cached_response.call_method1("get", ("userId",))?.eq(&user)?
                && cached.call_method1("get", ("authServerUrl",))?.eq(original.call_method1("get", ("authServerUrl",))?)?
            {
                return Ok(cached);
            }
        }
        Ok(original)
    }

    // ---- login / renewal: Python implementation (ported last) ----

    #[cfg(feature = "python")]
    #[pyo3(signature = (*args, **kwargs))]
    fn _credential_lock<'py>(slf: &Bound<'py, Self>, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
        delegate(slf.as_any(), "_credential_lock", args, kwargs)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (*args, **kwargs))]
    fn _replace_credential<'py>(slf: &Bound<'py, Self>, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
        delegate(slf.as_any(), "_replace_credential", args, kwargs)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (*args, **kwargs))]
    fn _uat_context<'py>(slf: &Bound<'py, Self>, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
        delegate(slf.as_any(), "_uat_context", args, kwargs)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (*args, **kwargs))]
    fn _auth_request<'py>(slf: &Bound<'py, Self>, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
        delegate(slf.as_any(), "_auth_request", args, kwargs)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (*args, **kwargs))]
    fn renew_notes_token<'py>(slf: &Bound<'py, Self>, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
        delegate(slf.as_any(), "renew_notes_token", args, kwargs)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (*args, **kwargs))]
    fn _renew_notes_token_locked<'py>(slf: &Bound<'py, Self>, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
        delegate(slf.as_any(), "_renew_notes_token_locked", args, kwargs)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (*args, **kwargs))]
    fn renew_cloud_token<'py>(slf: &Bound<'py, Self>, args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Bound<'py, PyAny>> {
        delegate(slf.as_any(), "renew_cloud_token", args, kwargs)
    }

    #[cfg(feature = "python")]
    fn _validate_identity(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.as_any();
        let account = this.call_method0("_original_account")?;
        let user = account.call_method1("get", ("response", PyDict::new(py)))?.call_method1("get", ("userId",))?;
        let profile = this.getattr("profile")?;
        if account.call_method1("get", ("schemaVersion",))?.ne(1)?
            || account.call_method1("get", ("credentialKind",))?.ne("NotesServiceOAuth")?
            || account.call_method1("get", ("clientId",))?.ne(profile.call_method1("get", ("appId",))?)?
            || !is_string(&user, 4096)
        {
            return Err(api_error(py, "identity_mismatch", "Notes hesabı ile Cloud uygulama kimliği uyuşmuyor."));
        }
        let joined = account.get_item("clientId")?.add("\0")?.add(account.get_item("response")?.get_item("userId")?)?;
        this.setattr("_identity", util::sha256_hex(&utf8_of(&joined)?))?;
        Ok(())
    }

    #[cfg(feature = "python")]
    fn auth_status<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let cloud = this.call_method0("_cloud")?;
        let account = this.call_method0("_account")?;
        let now = this.call_method0("_clock")?;
        let get = |o: &Bound<'py, PyAny>, k: &str| o.call_method1("get", (k,));
        let valid = get(&cloud, "credentialKind")?.eq("CloudUser")?
            && get(&cloud, "schemaVersion")?.eq(1)?
            && is_string(&get(&cloud, "accessToken")?, 65536)
            && get(&cloud, "accessToken")?.ne("-1")?
            && exact_number(py, &get(&cloud, "receivedUnixSeconds")?)
            && cloud.get_item("receivedUnixSeconds")?.le(&now)?
            && now.lt(expiry(py, &cloud)?)?;
        let response = account.call_method1("get", ("response", PyDict::new(py)))?;
        let mut renewable = expiry(py, &account)?.gt(&now)?
            && is_string(&response.call_method1("get", ("access_token",))?, 65536)
            && account.call_method1("get", ("response", PyDict::new(py)))?.call_method1("get", ("access_token",))?.ne("-1")?
            && exact_number(py, &get(&account, "receivedUnixSeconds")?)
            && account.get_item("receivedUnixSeconds")?.le(&now)?;
        let uat_expires = match this.call_method0("_uat_context") {
            Ok(t) => {
                let uat = t.get_item(0)?;
                uat.get_item("receivedUnixSeconds")?.add(uat.get_item("expiresIn")?)?
            }
            Err(e) if caught(py, &e, &[K::Api]) => int(py, 0),
            Err(e) => return Err(e),
        };
        let uat_live = uat_expires.gt(&now)?;
        renewable = renewable || uat_live;
        let d = dict(
            py,
            &[
                ("valid", boolean(py, valid)),
                ("needs_login", boolean(py, !valid && !renewable)),
                ("uat_expires_at", uat_expires.clone()),
                ("notes_renewable", boolean(py, uat_live)),
                ("cloud_expires_at", expiry(py, &cloud)?),
                ("notes_expires_at", expiry(py, &account)?),
                ("renewable", boolean(py, renewable)),
            ],
        )?;
        Ok(d.into_any())
    }

    #[cfg(feature = "python")]
    fn _directory<'py>(slf: &Bound<'py, Self>, kind: &str) -> PyResult<Bound<'py, PyAny>> {
        let root = work_dir(slf.as_any())?.join(kind);
        util::mkdir_private(&root, true)?;
        let directory = root.join(util::uuid4());
        util::mkdir_private(&directory, false)?;
        py_path(slf.py(), &directory)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (directory, name, request, *, limit = 2 * 1024 * 1024))]
    fn _call<'py>(slf: &Bound<'py, Self>, directory: &Bound<'py, PyAny>, name: &str, request: &Bound<'py, PyAny>, limit: usize) -> PyResult<Bound<'py, PyTuple>> {
        call_transport(slf.as_any(), directory, name, request, limit)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (mutation = false))]
    fn _headers<'py>(slf: &Bound<'py, Self>, mutation: bool) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        with_lock(this, || {
            this.call_method0("_validate_identity")?;
            let mut cloud = this.call_method0("_cloud")?;
            if expiry(py, &cloud)?.le(this.call_method0("_clock")?)? {
                this.call_method0("renew_cloud_token")?;
                cloud = this.call_method0("_cloud")?;
            }
            let now = this.call_method0("_clock")?;
            let pairs = try_map(py, changes_request_headers(py, &cloud, &this.getattr("endpoints")?, &this.getattr("profile")?, &this.getattr("initial_cursor")?, &now), &[K::Value, K::Key, K::Type], || {
                api_error(py, "reauth_required", "Geçerli Samsung oturumu gerekli.")
            })?;
            let headers = PyDict::new(py);
            for (k, v) in pairs {
                if k.to_lowercase() != "x-sn-transaction-id" {
                    headers.set_item(k, v)?;
                }
            }
            let prefix = if mutation { "NU-" } else { "ND-" };
            headers.set_item("X-SN-Transaction-ID", format!("{prefix}{}", util::random_choice("abcdefghijklmnopqrstuvwxyz", 10)))?;
            Ok(headers.into_any())
        })
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (cursor = None))]
    fn changes<'py>(slf: &Bound<'py, Self>, cursor: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let cursor = match cursor {
            Some(c) if !c.is_none() => c.clone(),
            _ => this.getattr("initial_cursor")?,
        };
        let text = match opt_str(&cursor) {
            Some(t) if t.chars().count() <= 65536 && !t.chars().any(|c| (c as u32) < 32) => t,
            _ => return Err(api_error(py, "invalid_cursor", "Senkronizasyon konumu geçersiz.")),
        };
        let output = this.call_method1("_directory", ("reads",))?;
        let out = fspath(&output)?;
        save_value(&out.join("request.json"), &Value::obj(vec![
            ("schemaVersion", Value::Int(1)),
            ("kind", Value::s("changes")),
            ("accountFingerprint", pyjson::from_py(&this.getattr("_identity")?)?),
            ("cursor", Value::Str(text.clone())),
        ]))?;
        let headers = this.call_method0("_headers")?;
        let url = format!("{}/samsungnotes/v3/notes/changes?{}", str_of(&this.getattr("base")?)?, util::urlencode(&[("lastChangePoint", text)]));
        let request = request(py, &url, None, None, &headers)?;
        let (status, raw, _) = call_api(this, &output, "changes", &request, None)?;
        this.call_method1("_read_status", (&status, &raw))?;
        let parser = this.getattr("_changes_parser")?;
        if parser.is_truthy()? {
            let parsed = parser.call1((&raw,))?;
            return parsed.call_method1("get", ("page", &parsed));
        }
        let root = PathBuf::from(py.import("samsung_notes.cloud_api")?.getattr("ROOT")?.str()?.to_string());
        let binary = root.join("target").join("debug").join("notes-cli");
        let source = out.join("changes-response.body");
        let target = out.join("changes.json");
        let raw_bytes = bytes_of(&raw).unwrap_or_default();
        let attempt = (|| -> PyResult<Bound<'py, PyAny>> {
            run_export(py, &binary, &source, &target)?;
            let parsed = load_json(py, &target, 8 * 1024 * 1024)?;
            if parsed.call_method1("get", ("sourceSha256",))?.ne(util::sha256_hex(&raw_bytes))? {
                return Err(value_error());
            }
            parsed.get_item("page")
        })();
        try_map(py, attempt, &[K::OS, K::Value, K::Key, K::Subprocess], || api_error(py, "feed_parse_failed", "Cloud değişiklik listesi çözümlenemedi."))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _read_status(py: Python<'_>, status: &Bound<'_, PyAny>, raw: &Bound<'_, PyAny>) -> PyResult<()> {
        read_status(py, status, raw)
    }

    #[cfg(feature = "python")]
    fn _url(slf: &Bound<'_, Self>, server_id: &Bound<'_, PyAny>) -> PyResult<String> {
        let py = slf.py();
        if !is_string(server_id, 1024) {
            return Err(api_error(py, "invalid_id", "Cloud not kimliği geçersiz."));
        }
        Ok(format!("{}/samsungnotes/v3/notes/{}", str_of(&slf.as_any().getattr("base")?)?, util::quote(&str_of(server_id)?)))
    }

    #[cfg(feature = "python")]
    fn get_note<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let headers = this.call_method0("_headers")?;
        headers.call_method1("update", (dict(py, &[("Content-Type", s(py, "application/xml;Charset=utf-8")), ("Content-Encoding", s(py, "gzip")), ("Accept", s(py, "multipart/form-data"))])?,))?;
        let url = str_of(&this.call_method1("_url", (server_id,))?)? + "/compareForDownsync";
        let body = util::gzip_mtime0(b"<noteTree ver=\"0\" isStrokeGrouped=\"false\" />");
        let request = request(py, &url, Some(&body), Some("POST"), &headers)?;
        let output = this.call_method1("_directory", ("reads",))?;
        save_json(&fspath(&output)?.join("request.json"), dict(py, &[
            ("schemaVersion", int(py, 1)),
            ("kind", s(py, "get_note")),
            ("accountFingerprint", this.getattr("_identity")?),
            ("server_id", server_id.clone()),
        ])?.as_any())?;
        let (status, raw, ctype) = call_api(this, &output, "downsync", &request, Some(max_body_bytes(py)?))?;
        this.call_method1("_read_status", (&status, &raw))?;
        let parsed = doc(py, "parse_document")?.call((&raw, &ctype), Some(&kw(py, &[("server_id", server_id.clone())])?));
        try_map(py, parsed, &[K::Value, K::Type, K::Parse], || api_error(py, "unsupported_payload", "Notun içeriği bu sürümde çözümlenemedi; özgün yanıt korundu."))
    }

    #[cfg(feature = "python")]
    fn upload_content_file<'py>(slf: &Bound<'py, Self>, data: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let data = match data.cast::<PyBytes>() {
            Ok(b) if !b.as_bytes().is_empty() && b.as_bytes().len() <= MAX_CONTENT_FILE => b.as_bytes().to_vec(),
            _ => return Err(api_error(py, "invalid_file", "Dosya boyutu desteklenmiyor.")),
        };
        let file_hash = util::sha256_hex(&data);
        let headers = this.call_method1("_headers", (true,))?;
        let authorization = opt_str(&headers.call_method1("get", ("Authorization", ""))?).unwrap_or_default();
        let token = authorization.strip_prefix("Bearer ").unwrap_or(&authorization).to_string();
        if token.is_empty() {
            return Err(api_error(py, "reauth_required", "Geçerli Samsung oturumu gerekli."));
        }
        let body = upload_request_body(&file_hash, data.len(), &token);
        headers.set_item("Content-Type", "application/xml;Charset=utf-8")?;
        let output = this.call_method1("_directory", ("uploads",))?;
        let out = fspath(&output)?;
        save_value(&out.join("request.json"), &Value::obj(vec![
            ("schemaVersion", Value::Int(1)),
            ("kind", Value::s("upload_content_file")),
            ("accountFingerprint", pyjson::from_py(&this.getattr("_identity")?)?),
            ("sha256", Value::Str(file_hash.clone())),
            ("size", Value::Int(data.len() as i64)),
        ]))?;
        let url = format!("{}/samsungnotes/v3/binaries/createUploadURLs", str_of(&this.getattr("base")?)?);
        let create = request(py, &url, Some(body.as_bytes()), Some("POST"), &headers)?;
        let (status, raw, _) = call_api(this, &output, "create-upload-urls", &create, None)?;
        let code = status_code(&status);
        if matches!(code, Some(401) | Some(403)) {
            return Err(api_error(py, "reauth_required", "Samsung oturumunu yenilemek gerekiyor."));
        }
        if code != Some(200) || raw.is_none() {
            return Err(api_error(py, "upload_unavailable", "Dosya yükleme adresi alınamadı."));
        }
        let raw_bytes = bytes_of(&raw).ok_or_else(|| PyTypeError::new_err("bytes expected"))?;
        let (kind, fields) = match guards::find_upload_entry(&raw_bytes, &file_hash) {
            guards::UploadAnswer::Unparsable => return Err(api_error(py, "upload_unavailable", "Dosya yükleme yanıtı çözümlenemedi.")),
            guards::UploadAnswer::Ambiguous => return Err(api_error(py, "upload_unavailable", "Dosya yükleme yanıtı belirsiz.")),
            guards::UploadAnswer::Missing => return Err(api_error(py, "upload_unavailable", "Dosya yükleme yanıtında bu dosya yok.")),
            guards::UploadAnswer::Found(k, f) => (k, f),
        };
        if kind == "failed" {
            return Err(api_error(py, "upload_rejected", "Sunucu dosyayı kabul etmedi."));
        }
        let field = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
        let upload_token = field("uploadtoken").unwrap_or_default();
        // A `duplicated` answer may carry only the hash (uploadToken "").
        let token_ok = (kind == "duplicated" && upload_token.is_empty())
            || ((8..=128).contains(&upload_token.len()) && upload_token.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'));
        if !token_ok {
            return Err(api_error(py, "upload_unavailable", "Dosya yükleme anahtarı geçersiz."));
        }
        if kind == "created" {
            let url = field("url").unwrap_or_default();
            let target = util::urlsplit(&url).map_err(|_| PyValueError::new_err("Invalid IPv6 URL"))?;
            let host = target.hostname().unwrap_or_default();
            let (user, password) = target.userinfo();
            let has_userinfo = user.map(|u| !u.is_empty()).unwrap_or(false) || password.map(|p| !p.is_empty()).unwrap_or(false);
            if target.scheme != "https" || !host.ends_with(".samsungcloud.com") || has_userinfo {
                return Err(api_error(py, "upload_unavailable", "Dosya yükleme adresi güvenilir değil."));
            }
            let put_headers = this.call_method1("_headers", (true,))?;
            put_headers.set_item("Content-Type", "application/octet-stream")?;
            put_headers.set_item("Range", format!("bytes=0-{}", data.len() - 1))?;
            let put = request(py, &url, Some(&data), Some("PUT"), &put_headers)?;
            let (put_status, _, _) = try_map(py, call_api(this, &output, "upload-put", &put, None), &[K::OS, K::Value, K::Api], || {
                uncertain(py, "upload_uncertain", "Dosya yüklemesinin sonucu bilinmiyor.")
            })?;
            let pc = status_code(&put_status);
            if !matches!(pc, Some(200) | Some(204)) {
                let rejected = matches!(pc, Some(400) | Some(401) | Some(403) | Some(413));
                return Err(api_error_ambiguous(py, if rejected { "upload_rejected" } else { "upload_uncertain" }, "Dosya yüklenemedi.", !rejected));
            }
        }
        save_value(&out.join("result.json"), &Value::obj(vec![
            ("sha256", Value::Str(file_hash.clone())),
            ("size", Value::Int(data.len() as i64)),
            ("status", Value::Str(kind.clone())),
        ]))?;
        let result = dict(py, &[
            ("sha256", s(py, &file_hash)),
            ("size", int(py, data.len() as i64)),
            ("uploadToken", s(py, &upload_token)),
            ("status", s(py, &kind)),
        ])?;
        Ok(result.into_any())
    }

    #[cfg(feature = "python")]
    fn _operation<'py>(slf: &Bound<'py, Self>, operation_id: &Bound<'py, PyAny>, intent: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let this = slf.as_any();
        operation(this, operation_id, intent, |id| this.call_method1("reconcile", (id,)))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    #[pyo3(signature = (*args))]
    fn _compare_guard(py: Python<'_>, args: &Bound<'_, PyTuple>) -> PyResult<()> {
        let a = guard_args(args, 3)?;
        let expected = opt_strings(&a[2])?;
        guards::compare_guard(raw_arg(&a[0]).as_deref(), opt_str_arg(&a[1]).as_deref(), &expected).map_err(|e| guard_err(py, e))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    #[pyo3(signature = (*args, content_file_ids = None))]
    fn _compare_ink_guard(py: Python<'_>, args: &Bound<'_, PyTuple>, content_file_ids: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let a = guard_args(args, 5)?;
        let files = match content_file_ids {
            Some(f) => opt_strings(f)?,
            None => Vec::new(),
        };
        guards::compare_ink_guard(
            raw_arg(&a[0]).as_deref(),
            opt_str_arg(&a[1]).as_deref(),
            opt_str_arg(&a[2]).as_deref(),
            opt_str_arg(&a[3]).as_deref(),
            opt_str_arg(&a[4]).as_deref(),
            &files,
        )
        .map_err(|e| guard_err(py, e))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    #[pyo3(signature = (*args))]
    fn _compare_page_guard(py: Python<'_>, args: &Bound<'_, PyTuple>) -> PyResult<()> {
        let a = guard_args(args, 3)?;
        guards::compare_page_guard(raw_arg(&a[0]).as_deref(), opt_str_arg(&a[1]).as_deref(), &opt_strings(&a[2])?).map_err(|e| guard_err(py, e))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    #[pyo3(signature = (*args))]
    fn _compare_new_pages_guard(py: Python<'_>, args: &Bound<'_, PyTuple>) -> PyResult<()> {
        let a = guard_args(args, 5)?;
        guards::compare_new_pages_guard(raw_arg(&a[0]).as_deref(), opt_str_arg(&a[1]).as_deref(), &opt_strings(&a[2])?, &opt_strings(&a[3])?, opt_str_arg(&a[4]).as_deref())
            .map_err(|e| guard_err(py, e))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    #[pyo3(signature = (snapshot, *, appended = None))]
    fn _opaque_content_fingerprint(py: Python<'_>, snapshot: &Bound<'_, PyAny>, appended: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
        CloudAPI::opaque_fingerprint(py, snapshot, appended)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (title, body, *, operation_id, local_id = None))]
    fn create<'py>(slf: &Bound<'py, Self>, title: &Bound<'py, PyAny>, body: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, local_id: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        crate::areas::text_value(py, title)?;
        crate::areas::text_value(py, body)?;
        let local_id = local_id.cloned().unwrap_or_else(|| none(py));
        if !local_id.is_none() {
            check_uuid(py, &local_id)?;
        }
        this.call_method0("_validate_identity")?;
        let intent = dict(py, &[("kind", s(py, "create")), ("title", title.clone()), ("body", body.clone()), ("local_id", local_id.clone())])?;
        let (output, replay) = tuple2(&this.call_method1("_operation", (operation_id, intent))?)?;
        if !replay.is_none() {
            return Ok(replay);
        }
        let out = fspath(&output)?;
        let draft = out.join("draft.xml");
        let (identity, full_xml) = if draft.exists() {
            let raw = std::fs::read(&draft)?;
            let saved = crate::areas::parse_draft(py, &raw)?;
            let full = b(py, &raw);
            let identity = saved.local_id.as_deref().map(|i| s(py, i)).unwrap_or_else(|| none(py));
            if s(py, &saved.title).ne(title)? || s(py, &saved.body).ne(body)? || (!local_id.is_none() && identity.ne(&local_id)?) {
                return Err(api_error(py, "operation_mismatch", "Hazırlanmış yeni not farklı içerik taşıyor."));
            }
            (identity, full)
        } else {
            let now = millis(&this.call_method0("_clock")?)?;
            crate::areas::new_document(py, title, body, &local_id, now)?
        };
        let (payload, ctype) = crate::areas::multipart(py, &full_xml)?;
        json_once(py, &out.join("expected.json"), dict(py, &[("title", title.clone()), ("body", body.clone()), ("local_id", identity)])?.as_any())?;
        save_once(py, &draft, &bytes_of(&full_xml).ok_or_else(|| PyTypeError::new_err("bytes expected"))?)?;
        let url = format!("{}/samsungnotes/v3/notes", str_of(&this.getattr("base")?)?);
        this.call_method1("_publish", (output, url, payload, ctype))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, title, body, baseline_commit = None, *, operation_id))]
    fn update<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, title: &Bound<'py, PyAny>, body: &Bound<'py, PyAny>, baseline_commit: Option<&Bound<'py, PyAny>>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let snapshot = coerce_snapshot(py, snapshot, server_id)?;
        if !snapshot.getattr("editable")?.is_truthy()? {
            return Err(PyErr::from_value(py.get_type::<UnsupportedDocumentError>().call1((snapshot.getattr("read_only_reason")?,))?));
        }
        let baseline = match baseline_commit {
            Some(b) if !b.is_none() => b.clone(),
            _ => snapshot.getattr("commit_id")?,
        };
        if normalize_commit(py, &baseline)? != normalize_commit(py, &snapshot.getattr("commit_id")?)? {
            return Err(conflict(py, None));
        }
        crate::areas::text_value(py, title)?;
        crate::areas::text_value(py, body)?;
        this.call_method0("_validate_identity")?;
        let intent = dict(py, &[
            ("kind", s(py, "update")),
            ("server_id", server_id.clone()),
            ("local_id", snapshot.getattr("local_id")?),
            ("baseline_commit", baseline.clone()),
            ("title", title.clone()),
            ("body", body.clone()),
            ("snapshot_sha256", s(py, &resource_sha(&snapshot)?)),
        ])?;
        let (output, replay) = tuple2(&this.call_method1("_operation", (operation_id, intent))?)?;
        if !replay.is_none() {
            return Ok(replay);
        }
        let out = fspath(&output)?;
        let fresh = fresh_matches(py, this, server_id, &snapshot, &baseline, None)?;
        let edit = (|| -> PyResult<Bound<'py, PyAny>> {
            let mut at = int(py, millis(&this.call_method0("_clock")?)?);
            if out.join("draft.xml").exists() {
                at = draft_time(py, &out.join("draft.xml"))?;
            }
            crate::areas::prepare_edit(py, &fresh, title, body, &at)
        })();
        let edit = match edit {
            Ok(e) => e,
            Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
            Err(e) => return Err(e),
        };
        let expected = dict(py, &[
            ("title", title.clone()),
            ("body", body.clone()),
            ("local_id", snapshot.getattr("local_id")?),
            ("server_id", server_id.clone()),
            ("page_inventory", crate::areas::page_inventory(py, &snapshot)?),
        ])?;
        let tree = save_receipts(py, &out, expected.as_any(), &edit)?;
        let raw = compare_upsync(this, &out, server_id, &tree, None)?;
        guarded(py, this.call_method1("_compare_guard", (&raw, &baseline, edit.getattr("requested_ids")?)), "Sunucunun karşılaştırma yanıtı güvenli güncellemeye uygun değil.")?;
        let (payload, ctype) = crate::areas::multipart(py, &edit.getattr("upload_xml")?)?;
        let url = str_of(&this.call_method1("_url", (server_id,))?)? + "/update";
        this.call_method1("_publish", (output, url, payload, ctype))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, title, *, operation_id))]
    fn update_title<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, title: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let prepare_title_edit = editor(py, "prepare_title_edit")?;
        let snapshot = coerce_snapshot(py, snapshot, server_id)?;
        crate::areas::text_value(py, title)?;
        this.call_method0("_validate_identity")?;
        let opaque = this.call_method1("_opaque_content_fingerprint", (&snapshot,))?;
        let commit = snapshot.getattr("commit_id")?;
        let intent = dict(py, &[
            ("kind", s(py, "update_title")),
            ("server_id", server_id.clone()),
            ("local_id", snapshot.getattr("local_id")?),
            ("baseline_commit", commit.clone()),
            ("title", title.clone()),
            ("snapshot_sha256", s(py, &resource_sha(&snapshot)?)),
            ("opaque_sha256", opaque.clone()),
        ])?;
        let (output, replay) = tuple2(&this.call_method1("_operation", (operation_id, intent))?)?;
        if !replay.is_none() {
            return Ok(replay);
        }
        let out = fspath(&output)?;
        let fresh = fresh_matches(py, this, server_id, &snapshot, &commit, Some(&opaque))?;
        let prepared = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyDict>)> {
            let mut at = int(py, millis(&this.call_method0("_clock")?)?);
            if out.join("draft.xml").exists() {
                at = draft_time(py, &out.join("draft.xml"))?;
            }
            let edit = prepare_title_edit.call((&fresh, title), Some(&kw(py, &[("now_ms", at)])?))?;
            if edit.getattr("requested_ids")?.is_truthy()? {
                return Err(document_error(py, "Başlık değişikliği içerik nesnesi isteyemez."));
            }
            let expected = dict(py, &[
                ("title", title.clone()),
                ("local_id", snapshot.getattr("local_id")?),
                ("server_id", server_id.clone()),
                ("body_sha256", s(py, &body_sha(&snapshot)?)),
                ("content_sha256", crate::areas::title_content_fingerprint(py, &edit.getattr("full_xml")?)?),
                ("opaque_sha256", opaque.clone()),
            ])?;
            Ok((edit, expected))
        })();
        let (edit, expected) = match prepared {
            Ok(v) => v,
            Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
            Err(e) => return Err(e),
        };
        let tree = save_receipts(py, &out, expected.as_any(), &edit)?;
        let raw = compare_upsync(this, &out, server_id, &tree, None)?;
        guarded(py, this.call_method1("_compare_guard", (&raw, &commit, pyo3::types::PySet::empty(py)?)), "Sunucu başlık değişikliği için beklenmeyen içerik istedi.")?;
        let (payload, ctype) = crate::areas::multipart(py, &edit.getattr("upload_xml")?)?;
        let url = str_of(&this.call_method1("_url", (server_id,))?)? + "/update";
        this.call_method1("_publish", (output, url, payload, ctype))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (title, model, *, operation_id, local_id = None))]
    fn create_editor<'py>(slf: &Bound<'py, Self>, title: &Bound<'py, PyAny>, model: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, local_id: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let normalize_model = editor(py, "normalize_model")?;
        let new_editor_document = editor(py, "new_editor_document")?;
        let editor_projection = editor(py, "editor_projection")?;
        let editor_model_fingerprint = editor(py, "editor_model_fingerprint")?;
        crate::areas::text_value(py, title)?;
        let local_id = local_id.cloned().unwrap_or_else(|| none(py));
        if !local_id.is_none() {
            check_uuid(py, &local_id)?;
        }
        let model = match normalize_model.call1((model,)) {
            Ok(m) => m,
            Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
            Err(e) => return Err(e),
        };
        this.call_method0("_validate_identity")?;
        let intent = dict(py, &[("kind", s(py, "create_editor")), ("title", title.clone()), ("model", model.clone()), ("local_id", local_id.clone())])?;
        let (output, replay) = tuple2(&this.call_method1("_operation", (operation_id, intent))?)?;
        if !replay.is_none() {
            return Ok(replay);
        }
        let out = fspath(&output)?;
        let model_hash = editor_model_fingerprint.call1((&model,))?;
        let draft = out.join("draft.xml");
        let (identity, full_xml) = if draft.exists() {
            let full = b(py, &std::fs::read(&draft)?);
            let saved = doc(py, "parse_document")?.call1((&full, "application/xml"))?;
            let identity = saved.getattr("local_id")?;
            let saved = py.import("dataclasses")?.getattr("replace")?.call((&saved,), Some(&kw(py, &[("commit_id", s(py, "local-draft-validation"))])?))?;
            let projected = editor_projection.call1((&saved,))?;
            if saved.getattr("title")?.ne(title)?
                || !projected.get_item("capabilities")?.get_item("textEdit")?.is_truthy()?
                || editor_model_fingerprint.call1((projected.get_item("model")?,))?.ne(&model_hash)?
                || (!local_id.is_none() && identity.ne(&local_id)?)
            {
                return Err(api_error(py, "operation_mismatch", "Hazırlanmış editör notu farklı içerik taşıyor."));
            }
            (identity, full)
        } else {
            let now = millis(&this.call_method0("_clock")?)?;
            match new_editor_document.call((title, &model), Some(&kw(py, &[("local_id", local_id.clone()), ("now_ms", int(py, now))])?)) {
                Ok(r) => tuple2(&r)?,
                Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
                Err(e) => return Err(e),
            }
        };
        let text_sha = util::sha256_hex(&utf8_of(&model.get_item("text")?)?);
        let expected = dict(py, &[
            ("title", title.clone()),
            ("local_id", identity),
            ("body_sha256", s(py, &text_sha)),
            ("model_sha256", model_hash),
            ("content_fingerprint_version", int(py, 2)),
            ("content_sha256", crate::areas::editor_content_fingerprint(py, &full_xml)?),
        ])?;
        json_once(py, &out.join("expected.json"), expected.as_any())?;
        save_once(py, &draft, &bytes_of(&full_xml).ok_or_else(|| PyTypeError::new_err("bytes expected"))?)?;
        let (payload, ctype) = crate::areas::multipart(py, &full_xml)?;
        let url = format!("{}/samsungnotes/v3/notes", str_of(&this.getattr("base")?)?);
        this.call_method1("_publish", (output, url, payload, ctype))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, title, model, baseline_commit = None, *, operation_id))]
    fn update_editor<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, title: &Bound<'py, PyAny>, model: &Bound<'py, PyAny>, baseline_commit: Option<&Bound<'py, PyAny>>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let normalize_model = editor(py, "normalize_model")?;
        let prepare_editor_edit = editor(py, "prepare_editor_edit")?;
        let editor_model_fingerprint = editor(py, "editor_model_fingerprint")?;
        let snapshot = coerce_snapshot(py, snapshot, server_id)?;
        let baseline = match baseline_commit {
            Some(b) if !b.is_none() => b.clone(),
            _ => snapshot.getattr("commit_id")?,
        };
        if normalize_commit(py, &baseline)? != normalize_commit(py, &snapshot.getattr("commit_id")?)? {
            return Err(conflict(py, None));
        }
        crate::areas::text_value(py, title)?;
        let model = match normalize_model.call1((model,)) {
            Ok(m) => m,
            Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
            Err(e) => return Err(e),
        };
        this.call_method0("_validate_identity")?;
        let opaque = this.call_method1("_opaque_content_fingerprint", (&snapshot,))?;
        let intent = dict(py, &[
            ("kind", s(py, "update_editor")),
            ("server_id", server_id.clone()),
            ("local_id", snapshot.getattr("local_id")?),
            ("baseline_commit", baseline.clone()),
            ("title", title.clone()),
            ("model", model.clone()),
            ("snapshot_sha256", s(py, &resource_sha(&snapshot)?)),
            ("opaque_sha256", opaque.clone()),
        ])?;
        let (output, replay) = tuple2(&this.call_method1("_operation", (operation_id, intent))?)?;
        if !replay.is_none() {
            return Ok(replay);
        }
        let out = fspath(&output)?;
        let fresh = fresh_matches(py, this, server_id, &snapshot, &baseline, Some(&opaque))?;
        let prepared = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyDict>)> {
            let mut at = int(py, millis(&this.call_method0("_clock")?)?);
            if out.join("draft.xml").exists() {
                at = draft_time(py, &out.join("draft.xml"))?;
            }
            let edit = prepare_editor_edit.call((&fresh, title, &model), Some(&kw(py, &[("now_ms", at)])?))?;
            let expected = dict(py, &[
                ("title", title.clone()),
                ("local_id", snapshot.getattr("local_id")?),
                ("server_id", server_id.clone()),
                ("body_sha256", s(py, &util::sha256_hex(&utf8_of(&edit.getattr("body")?)?))),
                ("model_sha256", editor_model_fingerprint.call1((&model,))?),
                ("content_fingerprint_version", int(py, 2)),
                ("content_sha256", crate::areas::editor_content_fingerprint(py, &edit.getattr("full_xml")?)?),
                ("opaque_sha256", opaque.clone()),
            ])?;
            Ok((edit, expected))
        })();
        let (edit, expected) = match prepared {
            Ok(v) => v,
            Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
            Err(e) => return Err(e),
        };
        let tree = save_receipts(py, &out, expected.as_any(), &edit)?;
        let raw = compare_upsync(this, &out, server_id, &tree, None)?;
        guarded(py, this.call_method1("_compare_guard", (&raw, &baseline, edit.getattr("requested_ids")?)), "Sunucunun istediği içerik düzenlemeyle uyuşmuyor.")?;
        let (payload, ctype) = crate::areas::multipart(py, &edit.getattr("upload_xml")?)?;
        let url = str_of(&this.call_method1("_url", (server_id,))?)? + "/update";
        this.call_method1("_publish", (output, url, payload, ctype))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, color_argb, *, operation_id, now_ms = None))]
    fn update_page_background<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, color_argb: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
        let snapshot = coerce_snapshot(py, snapshot, server_id)?;
        let color_ok = color_argb.get_type().is(py.get_type::<PyInt>())
            && color_argb.extract::<i128>().map(|c| (0..=0xFFFF_FFFF).contains(&c) && c >> 24 == 0xFF).unwrap_or(false);
        if !color_ok {
            return Err(unsupported(py, Some("Sayfa rengi opak bir renk olmalı.")));
        }
        check_now_ms(py, &now_ms, "Kayıt zamanı geçersiz.")?;
        this.call_method0("_validate_identity")?;
        let opaque = this.call_method1("_opaque_content_fingerprint", (&snapshot,))?;
        let commit = snapshot.getattr("commit_id")?;
        let intent = dict(py, &[
            ("kind", s(py, "update_page_background")),
            ("server_id", server_id.clone()),
            ("local_id", snapshot.getattr("local_id")?),
            ("baseline_commit", commit.clone()),
            ("color_argb", color_argb.clone()),
            ("now_ms", now_ms.clone()),
            ("snapshot_sha256", s(py, &resource_sha(&snapshot)?)),
            ("opaque_sha256", opaque.clone()),
        ])?;
        let (output, replay) = tuple2(&this.call_method1("_operation", (operation_id, intent))?)?;
        if !replay.is_none() {
            return Ok(replay);
        }
        let out = fspath(&output)?;
        let fresh = fresh_matches(py, this, server_id, &snapshot, &commit, Some(&opaque))?;
        let at = prepared_at(py, this, &out, &now_ms)?;
        let edit = match crate::areas::prepare_page_background(py, &fresh, color_argb, &at) {
            Ok(e) => e,
            Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
            Err(e) => return Err(e),
        };
        let page_ids = PyList::new(py, edit.getattr("page_ids")?.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
        let expected = dict(py, &[
            ("title", snapshot.getattr("title")?),
            ("local_id", snapshot.getattr("local_id")?),
            ("server_id", server_id.clone()),
            ("body_sha256", s(py, &body_sha(&snapshot)?)),
            ("content_fingerprint_version", int(py, 2)),
            ("content_sha256", crate::areas::editor_content_fingerprint(py, &edit.getattr("full_xml")?)?),
            ("opaque_sha256", opaque.clone()),
            ("page_ids", page_ids.into_any()),
        ])?;
        let tree = save_receipts(py, &out, expected.as_any(), &edit)?;
        let raw = compare_upsync(this, &out, server_id, &tree, None)?;
        guarded(py, this.call_method1("_compare_page_guard", (&raw, &commit, edit.getattr("page_ids")?)), "Sunucunun istediği sayfalar düzenlemeyle uyuşmuyor.")?;
        let (payload, ctype) = crate::areas::multipart(py, &edit.getattr("upload_xml")?)?;
        let url = str_of(&this.call_method1("_url", (server_id,))?)? + "/update";
        this.call_method1("_publish", (output, url, payload, ctype))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, pdf, *, file_name, page_sizes, page_indices = None, template_type = None, operation_id, now_ms = None))]
    fn add_pdf_pages<'py>(
        slf: &Bound<'py, Self>,
        server_id: &Bound<'py, PyAny>,
        snapshot: &Bound<'py, PyAny>,
        pdf: &Bound<'py, PyAny>,
        file_name: &Bound<'py, PyAny>,
        page_sizes: &Bound<'py, PyAny>,
        page_indices: Option<&Bound<'py, PyAny>>,
        template_type: Option<&Bound<'py, PyAny>>,
        operation_id: &Bound<'py, PyAny>,
        now_ms: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
        let template_type = template_type.cloned().unwrap_or_else(|| none(py));
        let snapshot = coerce_snapshot(py, snapshot, server_id)?;
        let pdf_bytes = match pdf.cast::<PyBytes>() {
            Ok(b) if b.as_bytes().starts_with(b"%PDF-") => b.as_bytes().to_vec(),
            _ => return Err(unsupported(py, Some("Dosya PDF değil."))),
        };
        check_now_ms(py, &now_ms, "Kayıt zamanı geçersiz.")?;
        let float = py.import("builtins")?.getattr("float")?;
        let sizes = PyList::empty(py);
        for pair in page_sizes.try_iter()? {
            let (w, h) = tuple2(&pair?)?;
            sizes.append(PyList::new(py, [float.call1((w,))?, float.call1((h,))?])?)?;
        }
        let indices: Bound<'py, PyAny> = match page_indices {
            Some(p) if !p.is_none() => PyList::new(py, p.try_iter()?.collect::<PyResult<Vec<_>>>()?)?.into_any(),
            _ => PyList::new(py, (0..sizes.len()).map(|i| i as i64))?.into_any(),
        };
        this.call_method0("_validate_identity")?;
        let opaque = this.call_method1("_opaque_content_fingerprint", (&snapshot,))?;
        let file_sha = util::sha256_hex(&pdf_bytes);
        let commit = snapshot.getattr("commit_id")?;
        let intent = dict(py, &[
            ("kind", s(py, "add_pdf_pages")),
            ("server_id", server_id.clone()),
            ("local_id", snapshot.getattr("local_id")?),
            ("baseline_commit", commit.clone()),
            ("file_sha256", s(py, &file_sha)),
            ("file_name", file_name.clone()),
            ("page_sizes", sizes.clone().into_any()),
            ("page_indices", indices.clone()),
            ("template_type", template_type.clone()),
            ("now_ms", now_ms.clone()),
            ("snapshot_sha256", s(py, &resource_sha(&snapshot)?)),
            ("opaque_sha256", opaque.clone()),
        ])?;
        let (output, replay) = tuple2(&this.call_method1("_operation", (operation_id, intent))?)?;
        if !replay.is_none() {
            return Ok(replay);
        }
        let out = fspath(&output)?;
        let fresh = fresh_matches(py, this, server_id, &snapshot, &commit, Some(&opaque))?;
        let prepared = out.join("prepared.json");
        let plan = if prepared.exists() {
            load_json(py, &prepared, DEFAULT_LIMIT)?
        } else {
            let at = if now_ms.is_none() { int(py, millis(&this.call_method0("_clock")?)?) } else { now_ms.clone() };
            let n = indices.len()?;
            let ids = PyList::new(py, (0..2 * n).map(|_| util::uuid4()))?;
            let plan = dict(py, &[("now_ms", at), ("ids", ids.into_any())])?.into_any();
            json_once(py, &prepared, &plan)?;
            plan
        };
        let upload_file = out.join("upload.json");
        let upload = if upload_file.exists() {
            load_json(py, &upload_file, DEFAULT_LIMIT)?
        } else {
            let result = this.call_method1("upload_content_file", (pdf,))?;
            let upload = dict(py, &[("sha256", result.get_item("sha256")?), ("uploadToken", result.get_item("uploadToken")?), ("status", result.get_item("status")?)])?.into_any();
            json_once(py, &upload_file, &upload)?;
            upload
        };
        if upload.get_item("sha256")?.ne(&file_sha)? {
            return Err(api_error(py, "operation_mismatch", "Yüklenen dosya işlemle uyuşmuyor."));
        }
        let edit = crate::areas::prepare_pdf_pages(
            py,
            &fresh,
            &file_sha,
            &upload.get_item("uploadToken")?,
            file_name,
            sizes.as_any(),
            &indices,
            &template_type,
            &plan.get_item("ids")?,
            &plan.get_item("now_ms")?,
        );
        let edit = match edit {
            Ok(e) => e,
            Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
            Err(e) => return Err(e),
        };
        let page_ids = PyList::new(py, edit.getattr("page_ids")?.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
        let layer_ids = PyList::new(py, edit.getattr("layer_ids")?.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
        let expected = dict(py, &[
            ("title", snapshot.getattr("title")?),
            ("local_id", snapshot.getattr("local_id")?),
            ("server_id", server_id.clone()),
            ("body_sha256", s(py, &body_sha(&snapshot)?)),
            ("content_fingerprint_version", int(py, 2)),
            ("content_sha256", crate::areas::editor_content_fingerprint(py, &edit.getattr("full_xml")?)?),
            ("opaque_sha256", opaque.clone()),
            ("page_ids", page_ids.clone().into_any()),
        ])?;
        let tree = save_receipts(py, &out, expected.as_any(), &edit)?;
        let raw = compare_upsync(this, &out, server_id, &tree, None)?;
        guarded(py, this.call_method1("_compare_new_pages_guard", (&raw, &commit, page_ids, layer_ids, &file_sha)), "Sunucunun istediği öğeler yeni sayfalarla uyuşmuyor.")?;
        let (payload, ctype) = crate::areas::multipart(py, &edit.getattr("upload_xml")?)?;
        let url = str_of(&this.call_method1("_url", (server_id,))?)? + "/update";
        this.call_method1("_publish", (output, url, payload, ctype))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, source_stroke_id, *, new_stroke_id, dx, dy, operation_id, now_ms = None))]
    fn update_ink_clone<'py>(
        slf: &Bound<'py, Self>,
        server_id: &Bound<'py, PyAny>,
        snapshot: &Bound<'py, PyAny>,
        source_stroke_id: &Bound<'py, PyAny>,
        new_stroke_id: &Bound<'py, PyAny>,
        dx: &Bound<'py, PyAny>,
        dy: &Bound<'py, PyAny>,
        operation_id: &Bound<'py, PyAny>,
        now_ms: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let prepare_ink_clone = crate::areas::live(py, "samsung_notes.ink_editor", "prepare_ink_clone")?;
        let isfinite = py.import("math")?.getattr("isfinite")?;
        for v in [dx, dy] {
            let ok = exact_number(py, v) && isfinite.call1((v,))?.is_truthy()? && v.abs()?.le(100000)?;
            if !ok {
                return Err(unsupported(py, Some("Çizgi konumu geçersiz.")));
            }
        }
        let float = py.import("builtins")?.getattr("float")?;
        let parameters = dict(py, &[("dx", float.call1((dx,))?), ("dy", float.call1((dy,))?)])?;
        let (src, new, dxo, dyo) = (source_stroke_id.clone().unbind(), new_stroke_id.clone().unbind(), dx.clone().unbind(), dy.clone().unbind());
        let prepare_fn = prepare_ink_clone.unbind();
        let prepare = PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let fresh = args.get_item(0)?;
            let tick = args.get_item(1)?;
            let k = dict(py, &[("new_stroke_id", new.bind(py).clone()), ("dx", dxo.bind(py).clone()), ("dy", dyo.bind(py).clone()), ("now_ms", tick)])?;
            Ok(prepare_fn.bind(py).call((fresh, src.bind(py)), Some(&k))?.unbind())
        })?;
        let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
        this.call_method(
            "_update_ink_document",
            (server_id, snapshot, source_stroke_id),
            Some(&kw(py, &[
                ("new_stroke_id", new_stroke_id.clone()),
                ("operation_id", operation_id.clone()),
                ("now_ms", now_ms),
                ("kind", s(py, "update_ink_clone")),
                ("parameters", parameters.into_any()),
                ("prepare", prepare.into_any()),
            ])?),
        )
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, source_stroke_id, *, samples, new_stroke_id, operation_id, now_ms = None))]
    fn append_ink_stroke<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, source_stroke_id: &Bound<'py, PyAny>, samples: &Bound<'py, PyAny>, new_stroke_id: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        append_stroke(slf.as_any(), server_id, snapshot, source_stroke_id, samples, new_stroke_id, operation_id, now_ms, 1, None)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, source_stroke_id, *, samples, new_stroke_id, operation_id, now_ms = None))]
    fn append_ink_stroke_v2<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, source_stroke_id: &Bound<'py, PyAny>, samples: &Bound<'py, PyAny>, new_stroke_id: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        append_stroke(slf.as_any(), server_id, snapshot, source_stroke_id, samples, new_stroke_id, operation_id, now_ms, 2, None)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, source_stroke_id, *, samples, new_stroke_id, operation_id, color_argb, pen_size, now_ms = None))]
    fn append_ink_stroke_v3<'py>(
        slf: &Bound<'py, Self>,
        server_id: &Bound<'py, PyAny>,
        snapshot: &Bound<'py, PyAny>,
        source_stroke_id: &Bound<'py, PyAny>,
        samples: &Bound<'py, PyAny>,
        new_stroke_id: &Bound<'py, PyAny>,
        operation_id: &Bound<'py, PyAny>,
        color_argb: &Bound<'py, PyAny>,
        pen_size: &Bound<'py, PyAny>,
        now_ms: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        append_stroke(slf.as_any(), server_id, snapshot, source_stroke_id, samples, new_stroke_id, operation_id, now_ms, 3, Some((color_argb, pen_size)))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, source_stroke_id, *, samples, new_stroke_id, operation_id, now_ms, recipe_version, style = None))]
    fn _append_ink_stroke<'py>(
        slf: &Bound<'py, Self>,
        server_id: &Bound<'py, PyAny>,
        snapshot: &Bound<'py, PyAny>,
        source_stroke_id: &Bound<'py, PyAny>,
        samples: &Bound<'py, PyAny>,
        new_stroke_id: &Bound<'py, PyAny>,
        operation_id: &Bound<'py, PyAny>,
        now_ms: &Bound<'py, PyAny>,
        recipe_version: i64,
        style: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let style_pair = match style {
            Some(st) if !st.is_none() => Some((st.get_item("colorARGB")?, st.get_item("penSize")?)),
            _ => None,
        };
        append_stroke(slf.as_any(), server_id, snapshot, source_stroke_id, samples, new_stroke_id, operation_id, Some(now_ms), recipe_version, style_pair.as_ref().map(|(a, b)| (a, b)))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, source_stroke_id, *, new_stroke_id, operation_id, now_ms, kind, parameters, prepare))]
    fn _update_ink_document<'py>(
        slf: &Bound<'py, Self>,
        server_id: &Bound<'py, PyAny>,
        snapshot: &Bound<'py, PyAny>,
        source_stroke_id: &Bound<'py, PyAny>,
        new_stroke_id: &Bound<'py, PyAny>,
        operation_id: &Bound<'py, PyAny>,
        now_ms: &Bound<'py, PyAny>,
        kind: &str,
        parameters: &Bound<'py, PyAny>,
        prepare: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        update_ink_document(slf.as_any(), server_id, snapshot, source_stroke_id, new_stroke_id, operation_id, now_ms, kind, parameters, prepare)
    }

    #[cfg(feature = "python")]
    fn _publish<'py>(slf: &Bound<'py, Self>, output: &Bound<'py, PyAny>, url: &str, payload: &Bound<'py, PyAny>, content_type: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let this = slf.as_any();
        publish(this, output, url, payload, content_type, "Sunucu kaydı kabul etmedi; yerel metin korunuyor.", &[], |id| this.call_method1("reconcile", (id,)))
    }

    #[cfg(feature = "python")]
    fn reconcile<'py>(slf: &Bound<'py, Self>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        reconcile_generic(slf.as_any(), operation_id)
    }
}

#[cfg(feature = "python")]
fn run_export(py: Python<'_>, binary: &Path, source: &Path, target: &Path) -> PyResult<()> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary).arg("export-changes").arg(source).arg(target).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let cls = py.import("subprocess")?.getattr("CalledProcessError")?;
            return Err(PyErr::from_value(cls.call1((status.code().unwrap_or(-1), binary.to_path_buf()))?));
        }
        if started.elapsed() > std::time::Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            let cls = py.import("subprocess")?.getattr("TimeoutExpired")?;
            return Err(PyErr::from_value(cls.call1((binary.to_path_buf(), 30))?));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(feature = "python")]
/// notes_cloud.changes_request -> the header items its Request would carry.
fn changes_request_headers<'py>(py: Python<'py>, cloud: &Bound<'py, PyAny>, endpoints: &Bound<'py, PyAny>, profile: &Bound<'py, PyAny>, cursor: &Bound<'py, PyAny>, now: &Bound<'py, PyAny>) -> PyResult<Vec<(String, String)>> {
    let get = |o: &Bound<'py, PyAny>, k: &str| o.call_method1("get", (k,));
    let token_invalid = get(cloud, "credentialKind")?.ne("CloudUser")?
        || get(cloud, "schemaVersion")?.ne(1)?
        || {
            let received = cloud.get_item("receivedUnixSeconds")?;
            !(received.le(now)? && now.lt(received.add(cloud.get_item("expiresIn")?)?)?)
        }
        || !safe_text(&get(cloud, "accessToken")?, 65536);
    if token_invalid {
        return Err(PyValueError::new_err("valid Cloud user token required"));
    }
    match opt_str(cursor) {
        Some(c) if c.chars().count() <= 65536 && !c.chars().any(|ch| (ch as u32) < 32) => {}
        _ => return Err(PyValueError::new_err("invalid opaque cursor")),
    }
    let headers = py.import("builtins")?.getattr("dict")?.call1((profile.call_method1("get", ("notesHeaders", PyDict::new(py)))?,))?;
    let allowed = ["x-sn-sdk-ver", "x-sn-app-ver", "x-sn-xml-ver", "x-sn-model-name", "x-sc-network", "x-sc-e2ee-state"];
    let required = ["x-sn-sdk-ver", "x-sn-app-ver", "x-sn-xml-ver", "x-sn-model-name"];
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut ok = true;
    for (k, v) in headers.cast::<PyDict>()?.iter() {
        match opt_str(&k) {
            Some(key) if allowed.contains(&key.as_str()) => {
                if !safe_text(&v, 1024) {
                    ok = false;
                }
                keys.push(key.clone());
                pairs.push((capitalize(&key), opt_str(&v).unwrap_or_default()));
            }
            _ => ok = false,
        }
    }
    if !ok || !required.iter().all(|r| keys.iter().any(|k| k == r)) {
        return Err(PyValueError::new_err("explicit Notes protocol headers required"));
    }
    let token = str_of(&cloud.get_item("accessToken")?)?;
    endpoint_of(endpoints, "default")?;
    pairs.push(("Authorization".into(), format!("Bearer {token}")));
    pairs.push(("X-sn-transaction-id".into(), format!("ND-{}", util::random_choice("abcdefghijklmnopqrstuvwxyz", 10))));
    pairs.push(("Accept-encoding".into(), "gzip".into()));
    pairs.push(("User-agent".into(), "SamsungNotesLinuxResearch/0.1".into()));
    Ok(pairs)
}

/// Python `str.capitalize()` for ASCII header names.
fn capitalize(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

#[cfg(feature = "python")]
pub fn ink_error(py: Python<'_>, message: &str) -> PyErr {
    match crate::areas::live(py, "samsung_notes.ink", "InkError").and_then(|c| c.call1((message,))) {
        Ok(i) => PyErr::from_value(i),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
/// Freeze caller-owned samples and encode the capture intent
/// (`StrokeSamples(tuple(...))`, `normalize_capture(frozen, stylus=False)`).
pub fn freeze_capture<'py>(py: Python<'py>, samples: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyDict>)> {
    let stroke_samples = crate::areas::live(py, "samsung_notes.ink_editor", "StrokeSamples")?;
    let max_points: usize = crate::areas::live(py, "samsung_notes.ink_editor", "MAX_POINTS")?.extract()?;
    let normalize_capture = crate::areas::live(py, "samsung_notes.ink_capture", "normalize_capture")?;
    if !samples.is_instance(&stroke_samples)? || !(1..=max_points).contains(&samples.getattr("points")?.len()?) {
        return Err(ink_error(py, "invalid capture"));
    }
    let tuple = py.import("builtins")?.getattr("tuple")?;
    let points = PyList::empty(py);
    for p in samples.getattr("points")?.try_iter()? {
        points.append(tuple.call1((p?,))?)?;
    }
    let frozen = stroke_samples.call1((
        tuple.call1((points,))?,
        tuple.call1((samples.getattr("pressures")?,))?,
        tuple.call1((samples.getattr("timestamps")?,))?,
        tuple.call1((samples.getattr("tilts")?,))?,
        tuple.call1((samples.getattr("orientations")?,))?,
    ))?;
    let capture = normalize_capture.call((&frozen,), Some(&kw(py, &[("stylus", boolean(py, false))])?))?;
    let channels = bytes_of(&capture.getattr("channels")?).ok_or_else(|| PyTypeError::new_err("bytes expected"))?;
    use base64::Engine as _;
    let samples_dict = dict(py, &[
        ("schemaVersion", int(py, 1)),
        ("encoding", s(py, "native-q5-q12-v1")),
        ("count", int(py, capture.getattr("samples")?.getattr("points")?.len()? as i64)),
        ("channelsBase64", s(py, &base64::engine::general_purpose::STANDARD.encode(&channels))),
    ])?;
    Ok((frozen, samples_dict))
}

#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn append_stroke<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    source_stroke_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: Option<&Bound<'py, PyAny>>,
    recipe_version: i64,
    style: Option<(&Bound<'py, PyAny>, &Bound<'py, PyAny>)>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let ink_editor = |n: &str| crate::areas::live(py, "samsung_notes.ink_editor", n);
    let ink_capture = |n: &str| crate::areas::live(py, "samsung_notes.ink_capture", n);
    let _ = ink_editor;
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    let built = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyDict>)> {
        let (frozen, samples_dict) = freeze_capture(py, samples)?;
        let parameters = dict(py, &[("samples", samples_dict.into_any())])?;
        if recipe_version >= 2 {
            parameters.set_item("recipeVersion", recipe_version)?;
        }
        if recipe_version == 3 {
            let (color, size) = style.ok_or_else(|| PyTypeError::new_err("style required"))?;
            let coords = |n: &str| crate::areas::live(py, "samsung_notes.ink_coordinates", n);
            let (min, max) = (coords("MIN_PEN_SIZE")?, coords("MAX_PEN_SIZE")?);
            let color_ok = color.get_type().is(py.get_type::<PyInt>()) && color.extract::<i128>().map(|c| (0..=0xffff_ffff).contains(&c) && c >> 24 == 0xff).unwrap_or(false);
            let isfinite = py.import("math")?.getattr("isfinite")?;
            let size_ok = exact_number(py, size) && isfinite.call1((size,))?.is_truthy()? && min.le(size)? && size.le(&max)?;
            if !color_ok || !size_ok {
                return Err(ink_error(py, "invalid style"));
            }
            let f: f64 = size.extract()?;
            let f32_size = (f as f32) as f64;
            if (f as f32).is_infinite() {
                return Err(pyo3::exceptions::PyOverflowError::new_err("float too large to pack with f format"));
            }
            parameters.set_item("style", dict(py, &[("colorARGB", color.clone()), ("penSize", PyFloat::new(py, f32_size).into_any())])?)?;
        }
        let encoded = pyjson::dumps(&pyjson::from_py(parameters.as_any())?, &pyjson::DEFAULT);
        if encoded.chars().count() > 1024 * 1024 {
            return Err(ink_error(py, "capture intent bound"));
        }
        Ok((frozen, parameters))
    })();
    let (frozen, parameters) = try_map(py, built, &[K::Ink, K::Type, K::Value, K::Overflow], || unsupported(py, Some("Çizgi noktaları desteklenen aralıkta değil.")))?;
    let kind = ["append_ink_stroke", "append_ink_stroke_v2", "append_ink_stroke_v3"][(recipe_version - 1).clamp(0, 2) as usize];
    let prepare_name = match recipe_version {
        1 => "prepare_ink_append",
        2 => "prepare_ink_append_v2",
        _ => "prepare_ink_append_v3",
    };
    let prepare_fn = ink_capture(prepare_name)?.unbind();
    let (src, new, frozen_u, params_u) = (source_stroke_id.clone().unbind(), new_stroke_id.clone().unbind(), frozen.unbind(), parameters.clone().unbind());
    let prepare = PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
        let py = args.py();
        let fresh = args.get_item(0)?;
        let tick = args.get_item(1)?;
        let k = dict(py, &[("new_stroke_id", new.bind(py).clone()), ("samples", frozen_u.bind(py).clone()), ("now_ms", tick)])?;
        if recipe_version == 3 {
            let style = params_u.bind(py).get_item("style")?.ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("style"))?;
            k.set_item("color_argb", style.get_item("colorARGB")?)?;
            k.set_item("pen_size", style.get_item("penSize")?)?;
        }
        Ok(prepare_fn.bind(py).call((fresh, src.bind(py)), Some(&k))?.unbind())
    })?;
    api.call_method(
        "_update_ink_document",
        (server_id, snapshot, source_stroke_id),
        Some(&kw(py, &[
            ("new_stroke_id", new_stroke_id.clone()),
            ("operation_id", operation_id.clone()),
            ("now_ms", now_ms),
            ("kind", s(py, kind)),
            ("parameters", parameters.into_any()),
            ("prepare", prepare.into_any()),
        ])?),
    )
}

#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn update_ink_document<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    source_stroke_id: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    kind: &str,
    parameters: &Bound<'py, PyAny>,
    prepare: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let snapshot = coerce_snapshot(py, snapshot, server_id)?;
    check_uuid(py, source_stroke_id)?;
    check_uuid(py, new_stroke_id)?;
    check_now_ms(py, now_ms, "Çizgi zamanı geçersiz.")?;
    api.call_method0("_validate_identity")?;
    let opaque = api.call_method1("_opaque_content_fingerprint", (&snapshot,))?;
    let commit = snapshot.getattr("commit_id")?;
    let intent = dict(py, &[
        ("kind", s(py, kind)),
        ("server_id", server_id.clone()),
        ("local_id", snapshot.getattr("local_id")?),
        ("baseline_commit", commit.clone()),
        ("source_stroke_id", source_stroke_id.clone()),
        ("new_stroke_id", new_stroke_id.clone()),
    ])?;
    for (k, v) in parameters.cast::<PyDict>()?.iter() {
        intent.set_item(k, v)?;
    }
    intent.set_item("now_ms", now_ms)?;
    intent.set_item("snapshot_sha256", resource_sha(&snapshot)?)?;
    intent.set_item("opaque_sha256", &opaque)?;
    let (output, replay) = tuple2(&api.call_method1("_operation", (operation_id, intent))?)?;
    if !replay.is_none() {
        return Ok(replay);
    }
    let out = fspath(&output)?;
    let fresh = fresh_matches(py, api, server_id, &snapshot, &commit, Some(&opaque))?;
    let at = prepared_at(py, api, &out, now_ms)?;
    let recipe: i64 = match kind {
        "append_ink_stroke_v2" => 2,
        "append_ink_stroke_v3" => 3,
        _ => 1,
    };
    let ink_error = |m: &str| ink_error(py, m);
    let built = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>, Bound<'py, PyAny>, Bound<'py, PyDict>)> {
        let edit = prepare.call1((&fresh, &at))?;
        let changed: Bound<'py, PyAny> = if recipe >= 2 { edit.getattr("content_file_ids")? } else { PyTuple::empty(py).into_any() };
        if edit.getattr("recipe_version")?.ne(recipe)? {
            return Err(ink_error("Çizgi sürümü uyuşmuyor."));
        }
        let mut wanted: Vec<Bound<'py, PyAny>> = vec![edit.getattr("page_id")?, edit.getattr("layer_id")?, new_stroke_id.clone()];
        wanted.extend(changed.try_iter()?.collect::<PyResult<Vec<_>>>()?);
        let wanted = pyo3::types::PyFrozenSet::new(py, &wanted)?;
        let parts = edit.getattr("binary_parts")?;
        if edit.getattr("requested_ids")?.ne(&wanted)? || parts.len()? != 1 {
            return Err(ink_error("Beklenmeyen çizgi değişikliği."));
        }
        let part = parts.get_item(0)?;
        let filename = str_of(new_stroke_id)? + "_" + &edit.getattr("stroke_modified_time")?.str()?.to_string();
        let body_ok = part.getattr("body")?.cast::<PyBytes>().map(|b| !b.as_bytes().is_empty() && b.as_bytes().len() <= max_body_bytes(py).unwrap_or(0)).unwrap_or(false);
        if part.getattr("name")?.ne("strokeResource")? || part.getattr("filename")?.ne(&filename)? || part.getattr("content_type")?.ne("application/octet-stream")? || !body_ok {
            return Err(ink_error("Geçersiz çizgi verisi."));
        }
        let appended = api.call_method("_opaque_content_fingerprint", (&snapshot,), Some(&kw(py, &[("appended", parts.clone())])?))?;
        let expected = dict(py, &[
            ("title", snapshot.getattr("title")?),
            ("local_id", snapshot.getattr("local_id")?),
            ("server_id", server_id.clone()),
            ("body_sha256", s(py, &body_sha(&snapshot)?)),
            ("content_fingerprint_version", int(py, 2)),
            ("content_sha256", crate::areas::editor_content_fingerprint(py, &edit.getattr("full_xml")?)?),
            ("opaque_sha256", appended),
        ])?;
        Ok((edit, part, changed, expected))
    })();
    let (edit, part, changed, expected) = match built {
        Ok(v) => v,
        Err(e) if caught(py, &e, &[K::Ink, K::Doc]) => return Err(unsupported_from(py, &e)),
        Err(e) => return Err(e),
    };
    json_once(py, &out.join("expected.json"), expected.as_any())?;
    save_once(py, &out.join("draft.xml"), &bytes_of(&edit.getattr("full_xml")?).ok_or_else(|| PyTypeError::new_err("bytes expected"))?)?;
    save_once(py, &out.join("stroke.bin"), &bytes_of(&part.getattr("body")?).unwrap_or_default())?;
    let tree = gz(py, &edit.getattr("tree_xml")?)?;
    save_once(py, &out.join("compare-request.gz"), &tree)?;
    let raw = compare_upsync(api, &out, server_id, &tree, None)?;
    let guard = api.call_method(
        "_compare_ink_guard",
        (&raw, &commit, edit.getattr("page_id")?, edit.getattr("layer_id")?, new_stroke_id),
        Some(&kw(py, &[("content_file_ids", changed)])?),
    );
    guarded(py, guard, "Sunucunun istediği çizim içeriği düzenlemeyle uyuşmuyor.")?;
    let (payload, ctype) = ink_payload(py, &edit)?;
    let url = str_of(&api.call_method1("_url", (server_id,))?)? + "/update";
    api.call_method1("_publish", (output, url, payload, ctype))
}

#[cfg(feature = "python")]
/// CloudAPI.reconcile
fn reconcile_generic<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let id = check_uuid(py, operation_id)?;
    let output = work_dir(api)?.join("operations").join(&id);
    if output.join("result.json").exists() {
        return load_json(py, &output.join("result.json"), RESULT_LIMIT);
    }
    let intent = load_json(py, &output.join("intent.json"), MIB2)?;
    if intent.call_method1("get", ("accountFingerprint",))?.ne(api.getattr("_identity")?)? {
        return Err(api_error(py, "identity_mismatch", "İşlem farklı bir hesaba ait."));
    }
    let page_ops = py.import("samsung_notes.cloud_page_ops")?;
    if page_ops.getattr("KINDS")?.contains(intent.call_method1("get", ("kind",))?)? {
        return page_ops.getattr("reconcile")?.call1((api, operation_id));
    }
    let shapes = py.import("samsung_notes.cloud_shapes")?;
    if shapes.getattr("KINDS")?.contains(intent.call_method1("get", ("kind",))?)? {
        return shapes.getattr("reconcile")?.call1((api, operation_id));
    }
    let kind = opt_str(&intent.call_method1("get", ("kind",))?);
    let checked = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let published = load_json(py, &output.join("published.json"), DEFAULT_LIMIT)?;
        let expected = load_json(py, &output.join("expected.json"), MIB2)?;
        let server = published.get_item("resourceId")?;
        if expected.call_method1("get", ("server_id", &server))?.ne(&server)? {
            return Err(value_error());
        }
        let snapshot = api.call_method1("get_note", (&server,))?;
        if snapshot.getattr("local_id")?.ne(expected.get_item("local_id")?)?
            || snapshot.getattr("title")?.ne(expected.get_item("title")?)?
            || normalize_commit(py, &snapshot.getattr("commit_id")?)? != normalize_commit(py, &published.get_item("commitId")?)?
        {
            return Err(value_error());
        }
        let body_hash = || -> PyResult<String> { body_sha(&snapshot) };
        let opaque_of = || api.call_method1("_opaque_content_fingerprint", (&snapshot,));
        match kind.as_deref() {
            Some("update_title") => {
                if expected.get_item("body_sha256")?.ne(body_hash()?)?
                    || crate::areas::title_content_fingerprint(py, &snapshot.getattr("resource_xml")?)?.ne(expected.get_item("content_sha256")?)?
                    || opaque_of()?.ne(expected.get_item("opaque_sha256")?)?
                {
                    return Err(value_error());
                }
            }
            Some("update_ink_clone" | "append_ink_stroke" | "append_ink_stroke_v2" | "append_ink_stroke_v3" | "update_page_background" | "add_pdf_pages") => {
                let draft = b(py, &std::fs::read(output.join("draft.xml"))?);
                if expected.call_method1("get", ("content_fingerprint_version",))?.ne(2)?
                    || crate::areas::editor_content_fingerprint(py, &draft)?.ne(expected.get_item("content_sha256")?)?
                    || crate::areas::editor_content_fingerprint(py, &snapshot.getattr("resource_xml")?)?.ne(expected.get_item("content_sha256")?)?
                    || expected.get_item("body_sha256")?.ne(body_hash()?)?
                    || opaque_of()?.ne(expected.get_item("opaque_sha256")?)?
                {
                    return Err(value_error());
                }
            }
            Some("create_editor" | "update_editor") => {
                let projection = editor(py, "editor_projection")?;
                let emf = editor(py, "editor_model_fingerprint")?;
                let projected = projection.call1((&snapshot,))?;
                let draft = b(py, &std::fs::read(output.join("draft.xml"))?);
                let version = expected.call_method1("get", ("content_fingerprint_version", 1))?;
                if version.eq(1)? {
                    if crate::areas::title_content_fingerprint(py, &draft)?.ne(expected.get_item("content_sha256")?)? {
                        return Err(value_error());
                    }
                } else if version.eq(2)? {
                    if crate::areas::editor_content_fingerprint(py, &draft)?.ne(expected.get_item("content_sha256")?)? {
                        return Err(value_error());
                    }
                } else {
                    return Err(value_error());
                }
                if !projected.get_item("capabilities")?.get_item("textEdit")?.is_truthy()?
                    || emf.call1((projected.get_item("model")?,))?.ne(expected.get_item("model_sha256")?)?
                    || expected.get_item("body_sha256")?.ne(body_hash()?)?
                    || crate::areas::editor_content_fingerprint(py, &snapshot.getattr("resource_xml")?)?.ne(crate::areas::editor_content_fingerprint(py, &draft)?)?
                    || (expected.contains("opaque_sha256")? && opaque_of()?.ne(expected.get_item("opaque_sha256")?)?)
                {
                    return Err(value_error());
                }
            }
            Some("erase_ink_strokes_v1" | "move_ink_strokes_v1") => {
                crate::cloud_ink_erase::verify_readback_impl(py, &py_path(py, &output)?, &snapshot, &expected)?;
            }
            _ => {
                if snapshot.getattr("body")?.ne(expected.get_item("body")?)? {
                    return Err(value_error());
                }
            }
        }
        if expected.contains("page_inventory")? && crate::areas::page_inventory(py, &snapshot)?.ne(expected.get_item("page_inventory")?)? {
            return Err(value_error());
        }
        Ok((server, snapshot))
    })();
    let (server, snapshot) = try_map(py, checked, &[K::OS, K::Value, K::Key, K::Type, K::Api], || {
        uncertain(py, "write_uncertain", "Kayıt yanıtı alındı; sunucudan geri okuyarak henüz doğrulanamadı.")
    })?;
    let result = dict(py, &[
        ("server_id", server),
        ("local_id", snapshot.getattr("local_id")?),
        ("commit_id", snapshot.getattr("commit_id")?),
        ("snapshot", snapshot.call_method0("to_dict")?),
        ("verified", boolean(py, true)),
    ])?;
    save_json(&output.join("result.json"), result.as_any())?;
    Ok(result.into_any())
}

#[cfg(feature = "python")]
/// Conformance probes (not exported in __all__): byte-exact helpers.
#[pyfunction]
fn _probe_dumps(obj: &Bound<'_, PyAny>, style: &str) -> PyResult<String> {
    let v = pyjson::from_py(obj)?;
    Ok(pyjson::dumps(&v, match style { "indent2" => &pyjson::INDENT2, "compact" => &pyjson::COMPACT, _ => &pyjson::DEFAULT }))
}
#[cfg(feature = "python")]
#[pyfunction]
fn _probe_loads_dumps(raw: &[u8]) -> PyResult<String> {
    let v = pyjson::loads(raw).map_err(|e| PyValueError::new_err(e.msg))?;
    Ok(pyjson::dumps(&v, &pyjson::INDENT2))
}
#[cfg(feature = "python")]
#[pyfunction]
fn _probe_gzip<'py>(py: Python<'py>, data: &[u8]) -> Bound<'py, PyBytes> {
    PyBytes::new(py, &util::gzip_mtime0(data))
}
#[cfg(feature = "python")]
#[pyfunction]
fn _probe_tostring<'py>(py: Python<'py>, raw: &[u8], mode: &str) -> PyResult<Bound<'py, PyBytes>> {
    let e = crate::etree::parse_bytes(raw, true).map_err(PyValueError::new_err)?;
    Ok(PyBytes::new(py, &match mode { "ascii" => crate::etree::tostring_ascii(&e), "utf8" => crate::etree::tostring_utf8_decl(&e), _ => crate::etree::tostring_unicode(&e).into_bytes() }))
}
#[cfg(feature = "python")]
#[pyfunction]
fn _probe_uuid5(ns: &str, name: &str) -> String {
    util::uuid5(ns, name)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(_probe_dumps, m)?)?;
    m.add_function(wrap_pyfunction!(_probe_loads_dumps, m)?)?;
    m.add_function(wrap_pyfunction!(_probe_gzip, m)?)?;
    m.add_function(wrap_pyfunction!(_probe_tostring, m)?)?;
    m.add_function(wrap_pyfunction!(_probe_uuid5, m)?)?;
    m.add_class::<APIError>()?;
    m.add_class::<ConflictError>()?;
    m.add_class::<UnsupportedDocumentError>()?;
    m.add_class::<CloudAPI>()?;
    rs_common::export(m, &["APIError", "ConflictError", "UnsupportedDocumentError", "CloudAPI"])
}
