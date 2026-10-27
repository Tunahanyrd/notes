//! Replacement for the Python module `cloud_ink_erase`: whole-stroke delete /
//! in-place move as one immutable, verified Cloud write.
#[cfg(feature = "python")]
use crate::cloud_api::{self as api_mod, check_uuid, coerce_snapshot, compare_upsync, fresh_matches, ink_payload, prepared_at};
use crate::glue::*;
use crate::pyjson::{self, Value};
use crate::util;
#[cfg(feature = "python")]
use pyo3::exceptions::PyTypeError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PyTuple};

pub const ERASE_KIND: &str = "erase_ink_strokes_v1";
pub const MOVE_KIND: &str = "move_ink_strokes_v1";
const MIB2: usize = 2 * 1024 * 1024;
const RESULT_LIMIT: usize = 96 * 1024 * 1024;

use crate::areas;

#[cfg(feature = "python")]
/// Fingerprint of every opaque MIME part that is not a strokeResource.
fn other_opaque(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<String> {
    let mut records: Vec<(String, String, String, String)> = Vec::new();
    for p in areas::snapshot_parts(py, snapshot)? {
        if p.content_type == "application/xml" || p.content_type == "text/xml" || p.name.as_deref() == Some("strokeResource") {
            continue;
        }
        records.push((p.name.unwrap_or_default(), p.filename.unwrap_or_default(), p.content_type, util::sha256_hex(&p.body)));
    }
    records.sort();
    let list = Value::List(records.into_iter().map(|(a, b, c, d)| Value::List(vec![Value::Str(a), Value::Str(b), Value::Str(c), Value::Str(d)])).collect());
    Ok(util::sha256_hex(pyjson::dumps(&list, &pyjson::COMPACT).as_bytes()))
}

#[cfg(feature = "python")]
/// sorted([filename, sha256]) of the strokeResource parts.
fn stroke_inventory<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let mut rows: Vec<(Option<String>, String)> = Vec::new();
    for p in areas::snapshot_parts(py, snapshot)? {
        if p.name.as_deref() == Some("strokeResource") {
            rows.push((p.filename, util::sha256_hex(&p.body)));
        }
    }
    if rows.len() > 1 && rows.iter().any(|(f, _)| f.is_none()) {
        return Err(PyTypeError::new_err("'<' not supported between instances of 'NoneType' and 'str'"));
    }
    rows.sort();
    let list = PyList::empty(py);
    for (f, h) in rows {
        list.append(PyList::new(py, [f.map(|x| s(py, &x)).unwrap_or_else(|| none(py)), s(py, &h)])?)?;
    }
    Ok(list.into_any())
}

#[cfg(feature = "python")]
pub fn verify_readback_impl<'py>(py: Python<'py>, output: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, expected: &Bound<'py, PyAny>) -> PyResult<()> {
    let draft = b(py, &std::fs::read(fspath(output)?.join("draft.xml"))?);
    let content = expected.get_item("content_sha256")?;
    let bad = expected.call_method1("get", ("content_fingerprint_version",))?.ne(2)?
        || areas::editor_content_fingerprint(py, &draft)?.ne(&content)?
        || areas::editor_content_fingerprint(py, &snapshot.getattr("resource_xml")?)?.ne(&content)?
        || s(py, &api_mod::body_sha(snapshot)?).ne(expected.get_item("body_sha256")?)?
        || stroke_inventory(py, snapshot)?.ne(expected.get_item("stroke_parts")?)?
        || s(py, &other_opaque(py, snapshot)?).ne(expected.get_item("other_opaque_sha256")?)?;
    if bad {
        return Err(value_error());
    }
    let gone = expected.call_method1("get", ("removed_ids", PyTuple::empty(py)))?;
    let gone: Vec<Bound<'py, PyAny>> = gone.try_iter()?.collect::<PyResult<_>>()?;
    if !gone.is_empty() {
        let raw = bytes_of(&snapshot.getattr("resource_xml")?).ok_or_else(|| PyTypeError::new_err("bytes expected"))?;
        let root = crate::etree::parse_bytes(&raw, true).map_err(|e| api_mod::parse_error(py, &e))?;
        for n in root.iter() {
            if let Some(id) = n.get("id") {
                for g in &gone {
                    if g.eq(id)? {
                        return Err(value_error());
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(feature = "python")]
#[pyfunction]
fn verify_readback(py: Python<'_>, output: &Bound<'_, PyAny>, snapshot: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) -> PyResult<()> {
    verify_readback_impl(py, output, snapshot, expected)
}

#[cfg(feature = "python")]
fn operation<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, intent: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    api_mod::operation(api, operation_id, intent, |id| reconcile_impl(api, id))
}

#[cfg(feature = "python")]
pub fn reconcile_impl<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let id = check_uuid(py, operation_id)?;
    let output = api_mod::work_dir(api)?.join("operations").join(&id);
    if output.join("result.json").exists() {
        return load_json(py, &output.join("result.json"), RESULT_LIMIT);
    }
    let intent = load_json(py, &output.join("intent.json"), MIB2)?;
    if intent.call_method1("get", ("accountFingerprint",))?.ne(api.getattr("_identity")?)? {
        return Err(api_error(py, "identity_mismatch", "İşlem farklı bir hesaba ait."));
    }
    let kind = intent.call_method1("get", ("kind",))?;
    if !(kind.eq(ERASE_KIND)? || kind.eq(MOVE_KIND)?) {
        return Err(api_error(py, "operation_mismatch", "İşlem türü bu doğrulayıcıya ait değil."));
    }
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
            || api_mod::normalize_commit(py, &snapshot.getattr("commit_id")?)? != api_mod::normalize_commit(py, &published.get_item("commitId")?)?
        {
            return Err(value_error());
        }
        verify_readback_impl(py, &py_path(py, &output)?, &snapshot, &expected)?;
        Ok((server, snapshot))
    })();
    let (server, snapshot) = try_map(py, checked, &[K::OS, K::Value, K::Key, K::Type, K::Api, K::Doc, K::Parse], || {
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
#[pyfunction]
fn reconcile<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    reconcile_impl(api, operation_id)
}

#[cfg(feature = "python")]
fn publish<'py>(api: &Bound<'py, PyAny>, output: &Bound<'py, PyAny>, url: &str, payload: &Bound<'py, PyAny>, content_type: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    api_mod::publish(api, output, url, payload, content_type, "Sunucu kaydı kabul etmedi; yerel çizim korunuyor.", &[K::Key, K::Doc], |id| reconcile_impl(api, id))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Erase,
    Move,
}

#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn run<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    kind: Kind,
    parameters: &Bound<'py, PyDict>,
    page_id: &Bound<'py, PyAny>,
    layer_id: &Bound<'py, PyAny>,
    ids: &Bound<'py, PyList>,
    offset: Option<(&Bound<'py, PyAny>, &Bound<'py, PyAny>)>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let kind_name = if kind == Kind::Erase { ERASE_KIND } else { MOVE_KIND };
    let snapshot = coerce_snapshot(py, snapshot, server_id)?;
    api_mod::check_now_ms(py, now_ms, "Çizim zamanı geçersiz.")?;
    api.call_method0("_validate_identity")?;
    let opaque = api.call_method1("_opaque_content_fingerprint", (&snapshot,))?;
    let commit = snapshot.getattr("commit_id")?;
    let intent = dict(py, &[
        ("kind", s(py, kind_name)),
        ("server_id", server_id.clone()),
        ("local_id", snapshot.getattr("local_id")?),
        ("baseline_commit", commit.clone()),
    ])?;
    for (k, v) in parameters.iter() {
        intent.set_item(k, v)?;
    }
    intent.set_item("now_ms", now_ms)?;
    intent.set_item("snapshot_sha256", api_mod::resource_sha(&snapshot)?)?;
    intent.set_item("opaque_sha256", &opaque)?;
    let result = operation(api, operation_id, intent.as_any())?;
    let (output, replay) = (result.get_item(0)?, result.get_item(1)?);
    if !replay.is_none() {
        return Ok(replay);
    }
    let out = fspath(&output)?;
    let fresh = fresh_matches(py, api, server_id, &snapshot, &commit, Some(&opaque))?;
    let at = prepared_at(py, api, &out, now_ms)?;
    let max_body = api_mod::max_body_bytes(py)?;
    let built = (|| -> PyResult<(Bound<'py, PyAny>, rs_ops::ink_erase::StrokeEdit, Bound<'py, PyDict>)> {
        if kind == Kind::Move && offset.is_none() {
            return Err(PyTypeError::new_err("move offset required"));
        }
        let (edit, core) = areas::prepare_stroke_edit(py, &fresh, page_id, layer_id, ids.as_any(), if kind == Kind::Move { offset } else { None }, &at)?;
        let wanted_kind = if kind == Kind::Erase { "delete" } else { "move" };
        if edit.getattr("kind")?.ne(wanted_kind)? {
            return Err(api_mod::ink_error(py, "Beklenmeyen çizim değişikliği."));
        }
        let id_tuple = PyTuple::new(py, ids.iter())?;
        let mut req: Vec<Bound<'py, PyAny>> = vec![edit.getattr("page_id")?, edit.getattr("layer_id")?];
        if kind == Kind::Move {
            req.extend(id_tuple.iter());
        }
        req.extend(edit.getattr("content_file_ids")?.try_iter()?.collect::<PyResult<Vec<_>>>()?);
        if edit.getattr("stroke_ids")?.ne(&id_tuple)?
            || edit.getattr("page_id")?.ne(parameters.get_item("page_id")?)?
            || edit.getattr("layer_id")?.ne(parameters.get_item("layer_id")?)?
            || edit.getattr("requested_ids")?.ne(PyFrozenSet::new(py, &req)?)?
        {
            return Err(api_mod::ink_error(py, "Beklenmeyen çizim değişikliği."));
        }
        let binary = edit.getattr("binary_parts")?;
        if kind == Kind::Erase && binary.is_truthy()? {
            return Err(api_mod::ink_error(py, "Silme işlemi çizgi verisi göndermez."));
        }
        if kind == Kind::Move {
            let names = PyList::empty(py);
            for p in binary.try_iter()? {
                names.append(p?.getattr("filename")?)?;
            }
            let modified = edit.getattr("modified_time")?.str()?.to_string();
            let wanted = PyList::empty(py);
            for i in ids.iter() {
                wanted.append(i.add("_")?.add(&modified)?)?;
            }
            if names.ne(&wanted)? {
                return Err(api_mod::ink_error(py, "Geçersiz çizgi verisi."));
            }
        }
        for p in binary.try_iter()? {
            let p = p?;
            let body_ok = p.getattr("body")?.len().map(|n| 0 < n && n <= max_body).unwrap_or(false);
            if p.getattr("name")?.ne("strokeResource")? || p.getattr("content_type")?.ne("application/octet-stream")? || !body_ok {
                return Err(api_mod::ink_error(py, "Geçersiz çizgi verisi."));
            }
        }
        let stroke_parts = areas::expected_stroke_parts(py, &fresh, &core)?;
        let removed: Bound<'py, PyAny> = if kind == Kind::Erase { PyList::new(py, ids.iter())?.into_any() } else { PyList::empty(py).into_any() };
        let expected = dict(py, &[
            ("title", snapshot.getattr("title")?),
            ("local_id", snapshot.getattr("local_id")?),
            ("server_id", server_id.clone()),
            ("body_sha256", s(py, &api_mod::body_sha(&snapshot)?)),
            ("content_fingerprint_version", int(py, 2)),
            ("content_sha256", areas::editor_content_fingerprint(py, &edit.getattr("full_xml")?)?),
            ("stroke_parts", stroke_parts),
            ("other_opaque_sha256", s(py, &other_opaque(py, &fresh)?)),
            ("removed_ids", removed),
        ])?;
        Ok((edit, core, expected))
    })();
    let (edit, core, expected) = match built {
        Ok(v) => v,
        Err(e) if caught(py, &e, &[K::Ink, K::Doc, K::Value, K::Type, K::Key]) => {
            let text = e.value(py).str()?.to_string();
            return Err(unsupported(py, Some(if text.is_empty() { "Bu çizim değişikliği desteklenmiyor." } else { &text })));
        }
        Err(e) => return Err(e),
    };
    let tree = api_mod::save_receipts(py, &out, expected.as_any(), &edit)?;
    let raw = compare_upsync(api, &out, server_id, &tree, None)?;
    let check = areas::check_compare_response(py, &raw, &commit, &core);
    try_map(py, check, &[K::Ink, K::Value, K::Type, K::Parse, K::Unicode], || conflict(py, Some("Sunucunun istediği çizim içeriği düzenlemeyle uyuşmuyor.")))?;
    let (payload, ctype) = ink_payload(py, &edit)?;
    let url = str_of(&api.call_method1("_url", (server_id,))?)? + "/update";
    publish(api, &output, &url, &payload, &ctype)
}

#[cfg(feature = "python")]
/// cloud_ink_erase._ids
fn stroke_ids<'py>(py: Python<'py>, page_id: &Bound<'py, PyAny>, layer_id: &Bound<'py, PyAny>, stroke_ids: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let collected = (|| -> PyResult<Bound<'py, PyList>> {
        check_uuid(py, page_id)?;
        check_uuid(py, layer_id)?;
        let list = PyList::empty(py);
        for item in stroke_ids.try_iter()? {
            list.append(check_uuid(py, &item?)?)?;
        }
        Ok(list)
    })();
    let list = try_map(py, collected, &[K::Api, K::Value, K::Type, K::Attr], || unsupported(py, Some("Çizgi kimlikleri geçersiz.")))?;
    let mut seen = std::collections::HashSet::new();
    let unique = list.iter().all(|i| seen.insert(i.extract::<String>().unwrap_or_default()));
    if !(1..=64).contains(&list.len()) || !unique {
        return Err(unsupported(py, Some("Çizgi kimlikleri geçersiz.")));
    }
    Ok(list)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, page_id, layer_id, stroke_ids, *, operation_id, now_ms = None))]
#[allow(clippy::too_many_arguments)]
fn erase_ink_strokes<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    layer_id: &Bound<'py, PyAny>,
    stroke_ids: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let ids = self::stroke_ids(py, page_id, layer_id, stroke_ids)?;
    let parameters = dict(py, &[("page_id", page_id.clone()), ("layer_id", layer_id.clone()), ("stroke_ids", ids.clone().into_any())])?;
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    run(api, server_id, snapshot, Kind::Erase, &parameters, page_id, layer_id, &ids, None, operation_id, &now_ms)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, page_id, layer_id, stroke_ids, dx, dy, *, operation_id, now_ms = None))]
#[allow(clippy::too_many_arguments)]
fn move_ink_strokes<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    layer_id: &Bound<'py, PyAny>,
    stroke_ids: &Bound<'py, PyAny>,
    dx: &Bound<'py, PyAny>,
    dy: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let ids = self::stroke_ids(py, page_id, layer_id, stroke_ids)?;
    for v in [dx, dy] {
        let exact = v.get_type().is(py.get_type::<PyInt>()) || v.get_type().is(py.get_type::<PyFloat>());
        if !exact || v.ne(v)? || v.abs()?.gt(32768)? {
            return Err(unsupported(py, Some("Taşıma miktarı geçersiz.")));
        }
    }
    let parameters = dict(py, &[
        ("page_id", page_id.clone()),
        ("layer_id", layer_id.clone()),
        ("stroke_ids", ids.clone().into_any()),
        ("dx", dx.clone()),
        ("dy", dy.clone()),
    ])?;
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    run(api, server_id, snapshot, Kind::Move, &parameters, page_id, layer_id, &ids, Some((dx, dy)), operation_id, &now_ms)
}


#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(verify_readback, m)?)?;
    m.add_function(wrap_pyfunction!(reconcile, m)?)?;
    m.add_function(wrap_pyfunction!(erase_ink_strokes, m)?)?;
    m.add_function(wrap_pyfunction!(move_ink_strokes, m)?)?;
    rs_common::export(m, &["verify_readback", "reconcile", "erase_ink_strokes", "move_ink_strokes"])
}
