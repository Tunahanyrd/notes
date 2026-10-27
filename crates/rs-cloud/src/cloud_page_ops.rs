//! Replacement for the Python module `cloud_page_ops`: page add/delete/
//! reorder and the first stroke on a page as immutable Cloud transactions.
#[cfg(feature = "python")]
use crate::cloud_api::{self as api_mod, check_uuid, coerce_snapshot, compare_upsync, fresh_matches, guarded, ink_error, ink_payload};
use crate::etree;
use crate::glue::*;
use crate::guards::{self, GuardError};
use crate::util;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyFrozenSet, PyList, PyString};

pub const KINDS: [&str; 5] = ["add_page", "delete_pages", "reorder_pages", "append_first_stroke", "append_highlighter_stroke"];
const MIB2: usize = 2 * 1024 * 1024;
const RESULT_LIMIT: usize = 96 * 1024 * 1024;

#[cfg(feature = "python")]
fn guard_err(py: Python<'_>, e: GuardError) -> PyErr {
    match e {
        GuardError::Conflict => conflict(py, None),
        GuardError::Invalid => PyValueError::new_err(()),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (raw, baseline, *, pages = None, layers = None, files = None))]
fn compare_page_ops_guard(py: Python<'_>, raw: &Bound<'_, PyAny>, baseline: &Bound<'_, PyAny>, pages: Option<&Bound<'_, PyAny>>, layers: Option<&Bound<'_, PyAny>>, files: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
    let pages = match pages { Some(p) => opt_strings(p)?, None => Vec::new() };
    let files = match files { Some(f) => opt_strings(f)?, None => Vec::new() };
    let mut layer_pairs = Vec::new();
    if let Some(l) = layers {
        if l.is_truthy()? {
            let d = py.import("builtins")?.getattr("dict")?.call1((l,))?;
            for (k, v) in d.cast::<PyDict>()?.iter() {
                let key = opt_strings(PyList::new(py, [k])?.as_any())?.remove(0);
                let value = opt_strings(PyList::new(py, [v])?.as_any())?.remove(0);
                layer_pairs.push((key, value));
            }
        }
    }
    guards::compare_page_ops_guard(bytes_of(raw).as_deref(), opt_str(baseline).as_deref(), &pages, &layer_pairs, &files).map_err(|e| guard_err(py, e))
}

#[cfg(feature = "python")]
/// `_opaque_without`: the opaque fingerprint with some stroke parts removed.
fn opaque_without(py: Python<'_>, snapshot: &Bound<'_, PyAny>, removed: &[String]) -> PyResult<String> {
    let mut present = std::collections::HashSet::new();
    for p in crate::areas::snapshot_parts(py, snapshot)? {
        if p.name.as_deref() == Some("strokeResource") {
            present.insert(p.filename);
        }
    }
    if !removed.iter().all(|r| present.contains(&Some(r.clone()))) {
        return Err(unsupported(py, Some("Silinecek çizgilerin verisi notta bulunamadı.")));
    }
    Ok(api_mod::opaque_digest(api_mod::opaque_records(py, snapshot, Some(removed))?))
}

#[cfg(feature = "python")]
pub(crate) struct Begun<'py> {
    pub(crate) fresh: Bound<'py, PyAny>,
    pub(crate) output: Bound<'py, PyAny>,
    pub(crate) opaque: Bound<'py, PyAny>,
}

#[cfg(feature = "python")]
/// `_begin`: Ok(Err(replay)) when the operation already has an answer.
pub(crate) fn begin<'py>(api: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, intent: &Bound<'py, PyDict>) -> PyResult<Result<Begun<'py>, Bound<'py, PyAny>>> {
    let py = api.py();
    api_mod::check_now_ms(py, now_ms, "Kayıt zamanı geçersiz.")?;
    api.call_method0("_validate_identity")?;
    let opaque = api.call_method1("_opaque_content_fingerprint", (snapshot,))?;
    let commit = snapshot.getattr("commit_id")?;
    let value = PyDict::new(py);
    for (k, v) in intent.iter() {
        value.set_item(k, v)?;
    }
    value.set_item("server_id", server_id)?;
    value.set_item("local_id", snapshot.getattr("local_id")?)?;
    value.set_item("baseline_commit", &commit)?;
    value.set_item("now_ms", now_ms)?;
    value.set_item("snapshot_sha256", api_mod::resource_sha(snapshot)?)?;
    value.set_item("opaque_sha256", &opaque)?;
    let result = api.call_method1("_operation", (operation_id, value))?;
    let (output, replay) = (result.get_item(0)?, result.get_item(1)?);
    if !replay.is_none() {
        return Ok(Err(replay));
    }
    let fresh = fresh_matches(py, api, server_id, snapshot, &commit, Some(&opaque))?;
    Ok(Ok(Begun { fresh, output, opaque }))
}

#[cfg(feature = "python")]
/// `_plan`: frozen time and generated ids in prepared.json.
pub(crate) fn plan<'py>(api: &Bound<'py, PyAny>, output: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>, id_count: usize) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let prepared = fspath(output)?.join("prepared.json");
    if prepared.exists() {
        return load_json(py, &prepared, DEFAULT_LIMIT);
    }
    let at = if now_ms.is_none() { int(py, millis(&api.call_method0("_clock")?)?) } else { now_ms.clone() };
    let ids = PyList::new(py, (0..id_count).map(|_| util::uuid4()))?;
    let plan = dict(py, &[("now_ms", at), ("ids", ids.into_any())])?.into_any();
    json_once(py, &prepared, &plan)?;
    Ok(plan)
}

#[cfg(feature = "python")]
pub(crate) fn expected<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, edit_xml: &Bound<'py, PyAny>, opaque: Bound<'py, PyAny>, order: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let order = PyList::new(py, order.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
    dict(py, &[
        ("title", snapshot.getattr("title")?),
        ("local_id", snapshot.getattr("local_id")?),
        ("server_id", server_id.clone()),
        ("body_sha256", s(py, &api_mod::body_sha(snapshot)?)),
        ("content_fingerprint_version", int(py, 2)),
        ("content_sha256", crate::areas::editor_content_fingerprint(py, &edit_xml)?),
        ("opaque_sha256", opaque),
        ("page_order", order.into_any()),
    ])
}

#[cfg(feature = "python")]
fn parse_xml(py: Python<'_>, xml: &Bound<'_, PyAny>) -> PyResult<etree::Element> {
    let raw = bytes_of(xml).ok_or_else(|| PyTypeError::new_err("a bytes-like object is required"))?;
    etree::parse_bytes(&raw, true).map_err(|e| api_mod::parse_error(py, &e))
}

#[cfg(feature = "python")]
/// `root.find('pageList')`; iterating None raises TypeError like Python.
fn page_list(root: &etree::Element) -> PyResult<&etree::Element> {
    root.children.iter().find(|c| c.tag == "pageList").ok_or_else(|| PyTypeError::new_err("'NoneType' object is not iterable"))
}

#[cfg(feature = "python")]
/// `_page_order(xml)`
pub(crate) fn page_order<'py>(py: Python<'py>, xml: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let root = parse_xml(py, xml)?;
    let ids: Vec<Bound<'py, PyAny>> = page_list(&root)?.children.iter().map(|p| p.get("id").map(|i| s(py, i)).unwrap_or_else(|| none(py))).collect();
    Ok(PyList::new(py, ids)?.into_any())
}

#[cfg(feature = "python")]
/// compare-request.gz receipt + compareForUpsync + typed guard.
pub(crate) fn compare<'py>(api: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, output: &Bound<'py, PyAny>, edit: &Bound<'py, PyAny>, guard: impl FnOnce(&Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>>, message: &str) -> PyResult<()> {
    let py = api.py();
    let out = fspath(output)?;
    let tree = util::gzip_mtime0(&bytes_of(&edit.getattr("tree_xml")?).ok_or_else(|| PyTypeError::new_err("bytes expected"))?);
    save_once(py, &out.join("compare-request.gz"), &tree)?;
    let raw = compare_upsync(api, &out, server_id, &tree, None)?;
    guarded(py, guard(&raw), message)
}

#[cfg(feature = "python")]
enum Op<'a, 'py> {
    Add(&'a Bound<'py, PyAny>),
    Delete(&'a Bound<'py, PyList>),
    Reorder(&'a Bound<'py, PyList>),
}

#[cfg(feature = "python")]
fn page_transaction<'py>(api: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>, op: Op<'_, 'py>) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let (kind, parameters, id_count): (&str, Bound<'py, PyDict>, usize) = match &op {
        Op::Add(after) => ("add_page", dict(py, &[("after_index", (*after).clone())])?, 2),
        Op::Delete(t) => ("delete_pages", dict(py, &[("page_ids", (*t).clone().into_any())])?, 0),
        Op::Reorder(o) => ("reorder_pages", dict(py, &[("page_ids", (*o).clone().into_any())])?, 0),
    };
    let snapshot = coerce_snapshot(py, snapshot, server_id)?;
    let intent = dict(py, &[("kind", s(py, kind))])?;
    for (k, v) in parameters.iter() {
        intent.set_item(k, v)?;
    }
    let begun = match begin(api, server_id, &snapshot, now_ms, operation_id, &intent)? {
        Ok(b) => b,
        Err(replay) => return Ok(replay),
    };
    let plan = plan(api, &begun.output, now_ms, id_count)?;
    let at = plan.get_item("now_ms")?;
    let edit = match &op {
        Op::Add(after) => {
            let ids = plan.get_item("ids")?;
            crate::areas::prepare_add_page(py, &begun.fresh, after, &at, &ids.get_item(0)?, &ids.get_item(1)?)
        }
        Op::Delete(t) => crate::areas::prepare_delete_page(py, &begun.fresh, t.as_any(), &at),
        Op::Reorder(o) => crate::areas::prepare_reorder_pages(py, &begun.fresh, o.as_any(), &at),
    };
    let edit = match edit {
        Ok(e) => e,
        Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
        Err(e) => return Err(e),
    };
    let expected_opaque = match &op {
        Op::Delete(t) => {
            let root = parse_xml(py, &begun.fresh.getattr("resource_xml")?)?;
            let mut names = Vec::new();
            for page in &page_list(&root)?.children {
                let pid = page.get("id").map(|x| s(py, x)).unwrap_or_else(|| none(py));
                if t.contains(&pid)? {
                    for o in page.iter().into_iter().filter(|o| o.tag == "object") {
                        match (o.get("id"), o.get("modifiedTime")) {
                            (Some(i), Some(m)) => names.push(format!("{i}_{m}")),
                            _ => return Err(PyTypeError::new_err("unsupported operand type(s) for +: 'NoneType' and 'str'")),
                        }
                    }
                }
            }
            s(py, &opaque_without(py, &snapshot, &names)?)
        }
        _ => begun.opaque.clone(),
    };
    let out = fspath(&begun.output)?;
    let exp = expected(py, &snapshot, server_id, &edit.getattr("full_xml")?, expected_opaque, edit.getattr("page_order")?)?;
    json_once(py, &out.join("expected.json"), exp.as_any())?;
    save_once(py, &out.join("draft.xml"), &bytes_of(&edit.getattr("full_xml")?).ok_or_else(|| PyTypeError::new_err("bytes expected"))?)?;
    let commit = snapshot.getattr("commit_id")?;
    let guard = |raw: &Bound<'py, PyAny>| -> PyResult<Bound<'py, PyAny>> {
        let (pages, layers, files) = match &op {
            Op::Add(_) => {
                let pages = opt_strings(&edit.getattr("page_ids")?)?;
                let layers = opt_strings(&edit.getattr("layer_ids")?)?;
                (pages.clone(), layers.into_iter().zip(pages).collect::<Vec<_>>(), Vec::new())
            }
            Op::Delete(_) => (Vec::new(), Vec::new(), opt_strings(&edit.getattr("content_file_ids")?)?),
            Op::Reorder(_) => (Vec::new(), Vec::new(), Vec::new()),
        };
        guards::compare_page_ops_guard(bytes_of(raw).as_deref(), opt_str(&commit).as_deref(), &pages, &layers, &files).map_err(|e| guard_err(py, e))?;
        Ok(none(py))
    };
    compare(api, server_id, &begun.output, &edit, guard, "Sunucunun istediği öğeler sayfa değişikliğiyle uyuşmuyor.")?;
    let (payload, ctype) = crate::areas::multipart(py, &edit.getattr("upload_xml")?)?;
    let url = str_of(&api.call_method1("_url", (server_id,))?)? + "/update";
    api.call_method1("_publish", (&begun.output, url, payload, ctype))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, after_index, *, operation_id, now_ms = None))]
fn add_page<'py>(api: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, after_index: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    if !after_index.get_type().is(py.get_type::<pyo3::types::PyInt>()) {
        return Err(unsupported(py, Some("Sayfa konumu geçersiz.")));
    }
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    page_transaction(api, server_id, snapshot, operation_id, &now_ms, Op::Add(after_index))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, page_ids, *, operation_id, now_ms = None))]
fn delete_pages<'py>(api: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, page_ids: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let targets = if page_ids.is_instance_of::<PyString>() { PyList::new(py, [page_ids])? } else { PyList::new(py, page_ids.try_iter()?.collect::<PyResult<Vec<_>>>()?)? };
    for t in targets.iter() {
        check_uuid(py, &t)?;
    }
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    page_transaction(api, server_id, snapshot, operation_id, &now_ms, Op::Delete(&targets))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, page_ids_in_order, *, operation_id, now_ms = None))]
fn reorder_pages<'py>(api: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, page_ids_in_order: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let order = PyList::new(py, page_ids_in_order.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
    for t in order.iter() {
        check_uuid(py, &t)?;
    }
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    page_transaction(api, server_id, snapshot, operation_id, &now_ms, Op::Reorder(&order))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, page_id, samples, *, color_argb, size, new_stroke_id, operation_id, now_ms = None))]
#[allow(clippy::too_many_arguments)]
fn append_first_stroke<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    color_argb: &Bound<'py, PyAny>,
    size: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    append_scratch_stroke(api, server_id, snapshot, page_id, samples, color_argb, size, new_stroke_id, operation_id, now_ms, Scratch::First)
}

#[cfg(feature = "python")]
/// Recipe 5: one highlighter (Marker4, top layer) stroke on a page; same
/// transaction as the first stroke (one compare, one publish, full readback).
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, page_id, samples, *, color_argb, size, new_stroke_id, operation_id, now_ms = None))]
#[allow(clippy::too_many_arguments)]
fn append_highlighter_stroke<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    color_argb: &Bound<'py, PyAny>,
    size: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    append_scratch_stroke(api, server_id, snapshot, page_id, samples, color_argb, size, new_stroke_id, operation_id, now_ms, Scratch::Highlighter)
}

#[derive(Clone, Copy)]
enum Scratch {
    First,
    Highlighter,
}

#[cfg(feature = "python")]
/// `_append_scratch_stroke`: shared by recipe 4 (first stroke) and recipe 5 (highlighter).
#[allow(clippy::too_many_arguments)]
fn append_scratch_stroke<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    color_argb: &Bound<'py, PyAny>,
    size: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: Option<&Bound<'py, PyAny>>,
    which: Scratch,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let (kind, recipe) = match which {
        Scratch::First => ("append_first_stroke", int(py, crate::areas::FIRST_STROKE_RECIPE)),
        Scratch::Highlighter => ("append_highlighter_stroke", int(py, crate::areas::HIGHLIGHTER_RECIPE)),
    };
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    check_uuid(py, page_id)?;
    check_uuid(py, new_stroke_id)?;
    let built = (|| -> PyResult<(Bound<'py, PyAny>, (Bound<'py, PyAny>, Bound<'py, PyAny>), Bound<'py, PyDict>)> {
        let (frozen, samples_dict) = api_mod::freeze_capture(py, samples)?;
        let (style_color, style_size) = match which {
            Scratch::First => crate::areas::first_stroke_style(py, color_argb, size)?,
            Scratch::Highlighter => crate::areas::highlighter_style(py, color_argb, size)?,
        };
        let parameters = dict(py, &[
            ("page_id", page_id.clone()),
            ("new_stroke_id", new_stroke_id.clone()),
            ("recipeVersion", recipe.clone()),
            ("samples", samples_dict.into_any()),
            ("style", dict(py, &[("colorARGB", style_color.clone()), ("penSize", style_size.clone())])?.into_any()),
        ])?;
        let encoded = crate::pyjson::dumps(&crate::pyjson::from_py(parameters.as_any())?, &crate::pyjson::DEFAULT);
        if encoded.chars().count() > 1024 * 1024 {
            return Err(ink_error(py, "capture intent bound"));
        }
        Ok((frozen, (style_color, style_size), parameters))
    })();
    let (frozen, style, parameters) = try_map(py, built, &[K::Ink, K::Type, K::Value, K::Overflow], || {
        unsupported(py, Some("Çizgi noktaları veya kalem ayarı desteklenen aralıkta değil."))
    })?;
    let snapshot = coerce_snapshot(py, snapshot, server_id)?;
    let intent = dict(py, &[("kind", s(py, kind))])?;
    for (k, v) in parameters.iter() {
        intent.set_item(k, v)?;
    }
    let begun = match begin(api, server_id, &snapshot, &now_ms, operation_id, &intent)? {
        Ok(b) => b,
        Err(replay) => return Ok(replay),
    };
    let plan = plan(api, &begun.output, &now_ms, 0)?;
    let max_body = api_mod::max_body_bytes(py)?;
    let prepared = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let prepare = match which {
            Scratch::First => crate::areas::prepare_first_stroke,
            Scratch::Highlighter => crate::areas::prepare_highlighter_stroke,
        };
        let edit = prepare(py, &begun.fresh, page_id, &frozen, &style.0, &style.1, &plan.get_item("now_ms")?, new_stroke_id)?;
        let parts = edit.getattr("binary_parts")?;
        if edit.getattr("recipe_version")?.ne(&recipe)? || parts.len()? != 1 {
            return Err(ink_error(py, "Beklenmeyen çizgi değişikliği."));
        }
        let mut req = vec![edit.getattr("page_id")?, edit.getattr("layer_id")?, new_stroke_id.clone()];
        req.extend(edit.getattr("content_file_ids")?.try_iter()?.collect::<PyResult<Vec<_>>>()?);
        if edit.getattr("requested_ids")?.ne(PyFrozenSet::new(py, &req)?)? {
            return Err(ink_error(py, "Beklenmeyen çizgi değişikliği."));
        }
        let part = parts.get_item(0)?;
        let filename = new_stroke_id.add("_")?.add(edit.getattr("stroke_modified_time")?.str()?)?;
        let body_ok = part.getattr("body")?.len().map(|n| 0 < n && n <= max_body).unwrap_or(false);
        if part.getattr("name")?.ne("strokeResource")? || part.getattr("filename")?.ne(&filename)? || !body_ok {
            return Err(ink_error(py, "Geçersiz çizgi verisi."));
        }
        Ok((edit, part))
    })();
    let (edit, part) = match prepared {
        Ok(v) => v,
        Err(e) if caught(py, &e, &[K::Ink, K::Doc]) => return Err(unsupported_from(py, &e)),
        Err(e) => return Err(e),
    };
    let appended = api.call_method("_opaque_content_fingerprint", (&snapshot,), Some(&dict(py, &[("appended", edit.getattr("binary_parts")?)])?))?;
    let order = page_order(py, &edit.getattr("full_xml")?)?;
    let exp = expected(py, &snapshot, server_id, &edit.getattr("full_xml")?, appended, order)?;
    let out = fspath(&begun.output)?;
    json_once(py, &out.join("expected.json"), exp.as_any())?;
    save_once(py, &out.join("draft.xml"), &bytes_of(&edit.getattr("full_xml")?).ok_or_else(|| PyTypeError::new_err("bytes expected"))?)?;
    save_once(py, &out.join("stroke.bin"), &bytes_of(&part.getattr("body")?).unwrap_or_default())?;
    let commit = snapshot.getattr("commit_id")?;
    let guard = |raw: &Bound<'py, PyAny>| {
        api.call_method(
            "_compare_ink_guard",
            (raw, &commit, edit.getattr("page_id")?, edit.getattr("layer_id")?, new_stroke_id),
            Some(&dict(py, &[("content_file_ids", edit.getattr("content_file_ids")?)])?),
        )
    };
    compare(api, server_id, &begun.output, &edit, guard, "Sunucunun istediği çizim içeriği düzenlemeyle uyuşmuyor.")?;
    let (payload, ctype) = ink_payload(py, &edit)?;
    let url = str_of(&api.call_method1("_url", (server_id,))?)? + "/update";
    api.call_method1("_publish", (&begun.output, url, payload, ctype))
}

#[cfg(feature = "python")]
pub fn reconcile_impl<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    reconcile_kinds(api, operation_id, &KINDS)
}

#[cfg(feature = "python")]
/// The readback shared with `cloud_shapes.reconcile` (same contract, own KINDS).
pub(crate) fn reconcile_kinds<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, kinds: &[&str]) -> PyResult<Bound<'py, PyAny>> {
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
    let checked = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let kind = intent.call_method1("get", ("kind",))?;
        if !kinds.iter().any(|k| kind.eq(*k).unwrap_or(false)) {
            return Err(value_error());
        }
        let published = load_json(py, &output.join("published.json"), DEFAULT_LIMIT)?;
        let expected = load_json(py, &output.join("expected.json"), MIB2)?;
        let server = published.get_item("resourceId")?;
        if expected.call_method1("get", ("server_id", &server))?.ne(&server)? {
            return Err(value_error());
        }
        let snapshot = api.call_method1("get_note", (&server,))?;
        let draft = b(py, &std::fs::read(output.join("draft.xml"))?);
        let content = expected.get_item("content_sha256")?;
        if snapshot.getattr("local_id")?.ne(expected.get_item("local_id")?)?
            || snapshot.getattr("title")?.ne(expected.get_item("title")?)?
            || api_mod::normalize_commit(py, &snapshot.getattr("commit_id")?)? != api_mod::normalize_commit(py, &published.get_item("commitId")?)?
            || expected.call_method1("get", ("content_fingerprint_version",))?.ne(2)?
            || crate::areas::editor_content_fingerprint(py, &draft)?.ne(&content)?
            || crate::areas::editor_content_fingerprint(py, &snapshot.getattr("resource_xml")?)?.ne(&content)?
            || s(py, &api_mod::body_sha(&snapshot)?).ne(expected.get_item("body_sha256")?)?
            || api.call_method1("_opaque_content_fingerprint", (&snapshot,))?.ne(expected.get_item("opaque_sha256")?)?
            || page_order(py, &snapshot.getattr("resource_xml")?)?.ne(expected.get_item("page_order")?)?
        {
            return Err(value_error());
        }
        Ok((server, snapshot))
    })();
    let (server, snapshot) = try_map(py, checked, &[K::OS, K::Value, K::Key, K::Type, K::Attr, K::Parse, K::Api], || {
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
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(compare_page_ops_guard, m)?)?;
    m.add_function(wrap_pyfunction!(add_page, m)?)?;
    m.add_function(wrap_pyfunction!(delete_pages, m)?)?;
    m.add_function(wrap_pyfunction!(reorder_pages, m)?)?;
    m.add_function(wrap_pyfunction!(append_first_stroke, m)?)?;
    m.add_function(wrap_pyfunction!(append_highlighter_stroke, m)?)?;
    m.add_function(wrap_pyfunction!(reconcile, m)?)?;
    rs_common::export(m, &["compare_page_ops_guard", "add_page", "delete_pages", "reorder_pages", "append_first_stroke", "append_highlighter_stroke", "reconcile"])
}
