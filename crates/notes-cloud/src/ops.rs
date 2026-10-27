//! `samsung_notes.cloud_ink_erase` (whole-stroke delete/move) and
//! `samsung_notes.cloud_page_ops` (page add/delete/reorder, first stroke).
use crate::api::{
    conflict, ink_multipart, multipart, normalize_commit, opaque_fingerprint, resolve_for, result_val, snapshot_parts, unsupported, write_uncertain,
    Appended, CloudApi, SnapIn, MAX_BODY_BYTES,
};
use crate::editor;
use crate::etree::{self, Element};
use crate::guards::{self, GuardError};
use crate::json;
use crate::util::{self, check_uuid, dget, ditem, json_once, load_json, save_json, save_once, R};
use notes_host::deps::{fail_of_ops, PureDeps};
use notes_host::snapshot::Snapshot;
use notes_host::{Fail, Kind, Val};
use rs_ink::core::binary::{RawSamples, MAX_POINTS};
use rs_ops::deps::Num;
use rs_store::val::py_eq;
use std::collections::HashSet;
use std::path::Path;

/// `cloud_ink_erase.ERASE_KIND`
pub const ERASE_KIND: &str = "erase_ink_strokes_v1";
/// `cloud_ink_erase.MOVE_KIND`
pub const MOVE_KIND: &str = "move_ink_strokes_v1";
/// `cloud_ink_erase.KINDS`
pub const ERASE_KINDS: [&str; 2] = [ERASE_KIND, MOVE_KIND];
/// `cloud_page_ops.KINDS`
pub const PAGE_KINDS: [&str; 5] = ["add_page", "delete_pages", "reorder_pages", "append_first_stroke", "append_highlighter_stroke"];

fn guard_err(e: GuardError, message: &str) -> Fail {
    match e {
        GuardError::Conflict => conflict(),
        GuardError::Invalid => Fail::conflict(message),
    }
}

fn opt_ids(ids: &[String]) -> Vec<Option<String>> {
    ids.iter().cloned().map(Some).collect()
}

fn sha(data: &[u8]) -> Val {
    Val::s(util::sha256_hex(data))
}

fn list_str(items: &[String]) -> Val {
    Val::List(items.iter().map(Val::s).collect())
}

// ======================================================================
// cloud_ink_erase
// ======================================================================

/// `cloud_ink_erase._other_opaque(snapshot)`
pub fn other_opaque(snapshot: &Snapshot) -> R<String> {
    let mut records: Vec<(String, String, String, String)> = snapshot_parts(snapshot)?
        .into_iter()
        .filter(|(name, _, ctype, _)| ctype != "application/xml" && ctype != "text/xml" && name.as_deref() != Some("strokeResource"))
        .map(|(name, filename, ctype, body)| (name.unwrap_or_default(), filename.unwrap_or_default(), ctype, util::sha256_hex(&body)))
        .collect();
    records.sort();
    let value = Val::List(records.into_iter().map(|(a, b, c, d)| Val::List(vec![Val::Str(a), Val::Str(b), Val::Str(c), Val::Str(d)])).collect());
    Ok(util::sha256_hex(json::dumps(&value, &json::COMPACT).as_bytes()))
}

/// `cloud_ink_erase._stroke_inventory(snapshot)`
pub fn stroke_inventory(snapshot: &Snapshot) -> R<Val> {
    let mut items: Vec<(Option<String>, String)> = snapshot_parts(snapshot)?
        .into_iter()
        .filter(|(name, ..)| name.as_deref() == Some("strokeResource"))
        .map(|(_, filename, _, body)| (filename, util::sha256_hex(&body)))
        .collect();
    if items.iter().any(|(f, _)| f.is_none()) && items.iter().any(|(f, _)| f.is_some()) {
        return Err(Fail::type_("'<' not supported between instances of 'NoneType' and 'str'"));
    }
    items.sort();
    Ok(Val::List(items.into_iter().map(|(f, h)| Val::List(vec![Val::opt_str(f.as_deref()), Val::Str(h)])).collect()))
}

/// `cloud_ink_erase.verify_readback(output, snapshot, expected)`
pub fn verify_erase_readback(output: &Path, snapshot: &Snapshot, expected: &Val) -> R<()> {
    let mismatch = || Fail::value("readback mismatch");
    let draft = util::read_bytes(&output.join("draft.xml"))?;
    let neq = |a: String, key: &str| -> R<bool> { Ok(!py_eq(&Val::s(a), ditem(expected, key)?)) };
    if !py_eq(dget(expected, "content_fingerprint_version")?.unwrap_or(&Val::Null), &Val::Int(2))
        || neq(editor::editor_content_fingerprint(&draft)?, "content_sha256")?
        || neq(editor::editor_content_fingerprint(&snapshot.resource_xml)?, "content_sha256")?
        || neq(util::sha256_hex(snapshot.body.as_bytes()), "body_sha256")?
        || !py_eq(&stroke_inventory(snapshot)?, ditem(expected, "stroke_parts")?)
        || neq(other_opaque(snapshot)?, "other_opaque_sha256")?
    {
        return Err(mismatch());
    }
    let gone: Vec<Val> = match dget(expected, "removed_ids")? {
        None => vec![],
        Some(Val::List(items)) => items.clone(),
        Some(Val::Str(s)) => s.chars().map(|c| Val::s(c.to_string())).collect(),
        Some(other) => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
    };
    if !gone.is_empty() {
        let root = etree::parse_bytes(&snapshot.resource_xml, false).map_err(|m| Fail::new(Kind::XmlParse, m))?;
        if root.iter().iter().any(|n| n.get("id").is_some_and(|id| gone.iter().any(|g| py_eq(g, &Val::s(id))))) {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// `cloud_ink_erase.reconcile(api, operation_id)`
pub fn reconcile_erase(api: &CloudApi, operation_id: &str) -> R<Val> {
    let output = api.work_dir.join("operations").join(check_uuid(operation_id)?);
    if output.join("result.json").exists() {
        return load_json(&output.join("result.json"), 96 * 1024 * 1024);
    }
    let intent = load_json(&output.join("intent.json"), 2 * 1024 * 1024)?;
    if !py_eq(dget(&intent, "accountFingerprint")?.unwrap_or(&Val::Null), &Val::s(api.identity())) {
        return Err(Fail::api("identity_mismatch", "İşlem farklı bir hesaba ait."));
    }
    if !matches!(dget(&intent, "kind")?, Some(Val::Str(k)) if ERASE_KINDS.contains(&k.as_str())) {
        return Err(Fail::api("operation_mismatch", "İşlem türü bu doğrulayıcıya ait değil."));
    }
    let checked = (|| -> R<(String, Snapshot)> {
        let (server, snapshot, expected) = readback_header(api, &output)?;
        verify_erase_readback(&output, &snapshot, &expected)?;
        Ok((server, snapshot))
    })();
    let (server, snapshot) = match checked {
        Ok(v) => v,
        Err(e) if e.is_os() || e.is_value() || e.is_key() || e.is_type() || e.is_api() || e.kind == Kind::XmlParse => {
            return Err(write_uncertain("Kayıt yanıtı alındı; sunucudan geri okuyarak henüz doğrulanamadı."))
        }
        Err(e) => return Err(e),
    };
    let result = result_val(&server, &snapshot);
    save_json(&output.join("result.json"), &result)?;
    Ok(result)
}

/// The shared reconcile header: published/expected receipts, readback of the
/// published server note, local id / title / commit checks.
fn readback_header(api: &CloudApi, output: &Path) -> R<(String, Snapshot, Val)> {
    let published = load_json(&output.join("published.json"), util::DEFAULT_LIMIT)?;
    let expected = load_json(&output.join("expected.json"), 2 * 1024 * 1024)?;
    let server_val = ditem(&published, "resourceId")?.clone();
    if !py_eq(dget(&expected, "server_id")?.unwrap_or(&server_val), &server_val) {
        return Err(Fail::value("server"));
    }
    let server = match &server_val {
        Val::Str(s) => s.clone(),
        _ => return Err(Fail::api("invalid_id", "Cloud not kimliği geçersiz.")),
    };
    let snapshot = api.get_note(&server)?;
    if !py_eq(&Val::opt_str(snapshot.local_id.as_deref()), ditem(&expected, "local_id")?)
        || !py_eq(&Val::s(&snapshot.title), ditem(&expected, "title")?)
    {
        return Err(Fail::value("readback"));
    }
    let published_commit = match ditem(&published, "commitId")? {
        Val::Str(s) => normalize_commit(Some(s))?,
        _ => return Err(Fail::document("Senkronizasyon sürümü eksik veya geçersiz.")),
    };
    if normalize_commit(snapshot.commit_id.as_deref())? != published_commit {
        return Err(Fail::value("commit"));
    }
    Ok((server, snapshot, expected))
}

/// `cloud_ink_erase._ids(page_id, layer_id, stroke_ids)`
fn erase_ids(page_id: &str, layer_id: &str, stroke_ids: &[String]) -> R<Vec<String>> {
    let invalid = || unsupported("Çizgi kimlikleri geçersiz.");
    check_uuid(page_id).map_err(|_| invalid())?;
    check_uuid(layer_id).map_err(|_| invalid())?;
    let ids: Vec<String> = stroke_ids.iter().map(|s| check_uuid(s).map_err(|_| invalid())).collect::<R<_>>()?;
    let unique: HashSet<&String> = ids.iter().collect();
    if !(1..=64).contains(&ids.len()) || unique.len() != ids.len() {
        return Err(invalid());
    }
    Ok(ids)
}

/// `cloud_ink_erase.erase_ink_strokes(api, server_id, snapshot, page_id, layer_id, stroke_ids, operation_id=..., now_ms=None)`
#[allow(clippy::too_many_arguments)]
pub fn erase_ink_strokes<'a>(
    api: &CloudApi,
    server_id: &str,
    snapshot: impl Into<SnapIn<'a>>,
    page_id: &str,
    layer_id: &str,
    stroke_ids: &[String],
    operation_id: &str,
    now_ms: Option<i64>,
) -> R<Val> {
    let ids = erase_ids(page_id, layer_id, stroke_ids)?;
    let parameters = vec![("page_id", Val::s(page_id)), ("layer_id", Val::s(layer_id)), ("stroke_ids", list_str(&ids))];
    run_erase(api, server_id, snapshot.into(), ERASE_KIND, parameters, page_id, layer_id, &ids, None, operation_id, now_ms)
}

/// `cloud_ink_erase.move_ink_strokes(api, server_id, snapshot, page_id, layer_id, stroke_ids, dx, dy, operation_id=..., now_ms=None)`;
/// `dx`/`dy` are the Python values (`Val::Int` or `Val::Float`), kept in the intent as given.
#[allow(clippy::too_many_arguments)]
pub fn move_ink_strokes<'a>(
    api: &CloudApi,
    server_id: &str,
    snapshot: impl Into<SnapIn<'a>>,
    page_id: &str,
    layer_id: &str,
    stroke_ids: &[String],
    dx: &Val,
    dy: &Val,
    operation_id: &str,
    now_ms: Option<i64>,
) -> R<Val> {
    let ids = erase_ids(page_id, layer_id, stroke_ids)?;
    let mut offsets = [0.0f64; 2];
    for (slot, v) in offsets.iter_mut().zip([dx, dy]) {
        let x = match v {
            Val::Int(i) => *i as f64,
            Val::Float(f) => *f,
            _ => return Err(unsupported("Taşıma miktarı geçersiz.")),
        };
        if x.is_nan() || x.abs() > 32768.0 {
            return Err(unsupported("Taşıma miktarı geçersiz."));
        }
        *slot = x;
    }
    let parameters = vec![
        ("page_id", Val::s(page_id)),
        ("layer_id", Val::s(layer_id)),
        ("stroke_ids", list_str(&ids)),
        ("dx", dx.clone()),
        ("dy", dy.clone()),
    ];
    run_erase(api, server_id, snapshot.into(), MOVE_KIND, parameters, page_id, layer_id, &ids, Some((offsets[0], offsets[1])), operation_id, now_ms)
}

#[allow(clippy::too_many_arguments)]
fn run_erase(
    api: &CloudApi,
    server_id: &str,
    snapshot: SnapIn<'_>,
    kind: &str,
    parameters: Vec<(&str, Val)>,
    page_id: &str,
    layer_id: &str,
    ids: &[String],
    offset: Option<(f64, f64)>,
    operation_id: &str,
    now_ms: Option<i64>,
) -> R<Val> {
    let snapshot = resolve_for(snapshot, server_id)?;
    if matches!(now_ms, Some(n) if n <= 0) {
        return Err(unsupported("Çizim zamanı geçersiz."));
    }
    api.validate_identity()?;
    let opaque = opaque_fingerprint(&snapshot, &[], None)?;
    let mut intent = vec![
        ("kind", Val::s(kind)),
        ("server_id", Val::s(server_id)),
        ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
        ("baseline_commit", Val::opt_str(snapshot.commit_id.as_deref())),
    ];
    intent.extend(parameters);
    intent.push(("now_ms", now_ms.map(Val::Int).unwrap_or(Val::Null)));
    intent.push(("snapshot_sha256", sha(&snapshot.resource_xml)));
    intent.push(("opaque_sha256", Val::s(&opaque)));
    let (output, replay) = api.operation_with(operation_id, intent, &reconcile_erase)?;
    if let Some(r) = replay {
        return Ok(r);
    }
    let fresh = api.fresh_matching(server_id, &snapshot, snapshot.commit_id.as_deref(), Some(&opaque))?;
    let prepared_at = api.prepared_time(&output, now_ms)?;
    let deps = PureDeps::default();
    let checked = (|| -> R<(rs_ops::ink_erase::StrokeEdit, Val)> {
        let stroke_ids: rs_ops::ink_erase::StrokeIds = Some(opt_ids(ids));
        let edit = match offset {
            None => rs_ops::ink_erase::prepare_ink_delete(&deps, &fresh, Some(page_id), Some(layer_id), &stroke_ids, Some(prepared_at as i128)),
            Some((dx, dy)) => {
                rs_ops::ink_erase::prepare_ink_move(&deps, &fresh, Some(page_id), Some(layer_id), &stroke_ids, Some(dx), Some(dy), Some(prepared_at as i128))
            }
        }
        .map_err(fail_of_ops)?;
        let bad = || Fail::new(Kind::Ink, "Beklenmeyen çizim değişikliği.");
        let wanted_kind = if kind == MOVE_KIND { "move" } else { "delete" };
        if edit.kind != wanted_kind {
            return Err(bad());
        }
        let objects: &[String] = if kind == MOVE_KIND { ids } else { &[] };
        let requested: HashSet<&String> = edit.requested_ids.iter().collect();
        let mut wanted: HashSet<&String> = [&edit.page_id, &edit.layer_id].into_iter().collect();
        wanted.extend(objects.iter());
        wanted.extend(edit.content_file_ids.iter());
        if edit.stroke_ids != ids || edit.page_id != page_id || edit.layer_id != layer_id || requested != wanted {
            return Err(bad());
        }
        if kind == ERASE_KIND && !edit.binary_parts.is_empty() {
            return Err(Fail::new(Kind::Ink, "Silme işlemi çizgi verisi göndermez."));
        }
        if kind == MOVE_KIND {
            let names: Vec<String> = edit.binary_parts.iter().map(|p| p.filename.clone()).collect();
            let want: Vec<String> = ids.iter().map(|i| format!("{}_{}", i, edit.modified_time)).collect();
            if names != want {
                return Err(Fail::new(Kind::Ink, "Geçersiz çizgi verisi."));
            }
        }
        if edit.binary_parts.iter().any(|p| p.name != "strokeResource" || p.body.is_empty() || p.body.len() > MAX_BODY_BYTES) {
            return Err(Fail::new(Kind::Ink, "Geçersiz çizgi verisi."));
        }
        let payload = notes_host::deps::payload(&fresh.raw_body, &fresh.content_type, &deps.limits).map_err(fail_of_ops)?;
        let removed: Vec<Option<String>> = edit.removed_part_filenames.iter().cloned().map(Some).collect();
        let added: Vec<(Option<String>, Vec<u8>)> = edit.binary_parts.iter().map(|p| (Some(p.filename.clone()), p.body.clone())).collect();
        let parts = rs_ops::ink_erase::expected_stroke_parts(&payload.parts, &removed, &added).map_err(fail_of_ops)?;
        let stroke_parts = Val::List(parts.into_iter().map(|(f, h)| Val::List(vec![Val::opt_str(f.as_deref()), Val::Str(h)])).collect());
        let expected = Val::dict(vec![
            ("title", Val::s(&snapshot.title)),
            ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
            ("server_id", Val::s(server_id)),
            ("body_sha256", sha(snapshot.body.as_bytes())),
            ("content_fingerprint_version", Val::Int(2)),
            ("content_sha256", Val::s(editor::editor_content_fingerprint(&edit.full_xml)?)),
            ("stroke_parts", stroke_parts),
            ("other_opaque_sha256", Val::s(other_opaque(&fresh)?)),
            ("removed_ids", if kind == ERASE_KIND { list_str(ids) } else { Val::List(vec![]) }),
        ]);
        Ok((edit, expected))
    })();
    let (edit, expected) = match checked {
        Ok(v) => v,
        Err(e) if e.kind == Kind::Ink || e.is_document() || e.is_value() || e.is_type() || e.is_key() => {
            let message = if e.message.is_empty() { "Bu çizim değişikliği desteklenmiyor." } else { &e.message };
            return Err(unsupported(message));
        }
        Err(e) => return Err(e),
    };
    json_once(&output.join("expected.json"), &expected)?;
    save_once(&output.join("draft.xml"), &edit.full_xml)?;
    let response = api.compare_request(&output, server_id, &edit.tree_xml)?;
    let view = rs_ops::ink_erase::EditView {
        kind: edit.kind.to_string(),
        page_id: Some(edit.page_id.clone()),
        layer_id: Some(edit.layer_id.clone()),
        stroke_ids: opt_ids(&edit.stroke_ids),
        content_file_ids: opt_ids(&edit.content_file_ids),
    };
    if let Err(e) = rs_ops::ink_erase::check_compare_response(response.body.as_deref().unwrap_or_default(), snapshot.commit_id.as_deref(), &view) {
        let f = fail_of_ops(e);
        if f.kind == Kind::Ink || f.is_value() || f.is_type() || f.kind == Kind::XmlParse {
            return Err(Fail::conflict("Sunucunun istediği çizim içeriği düzenlemeyle uyuşmuyor."));
        }
        return Err(f);
    }
    let parts: Vec<Appended> = edit.binary_parts.iter().map(|p| (p.name.clone(), p.filename.clone(), p.body.clone())).collect();
    let (payload, content_type) = ink_multipart(api.random(), &edit.upload_xml, &parts)?;
    api.publish_with(
        &output,
        &format!("{}/update", api.url(server_id)?),
        payload,
        &content_type,
        "Sunucu kaydı kabul etmedi; yerel çizim korunuyor.",
        &reconcile_erase,
    )
}

// ======================================================================
// cloud_page_ops
// ======================================================================

/// `cloud_page_ops._opaque_without(snapshot, removed_filenames, appended=())`
pub fn opaque_without(snapshot: &Snapshot, removed: &[String], appended: &[Appended]) -> R<String> {
    let present: HashSet<Option<String>> =
        snapshot_parts(snapshot)?.into_iter().filter(|(name, ..)| name.as_deref() == Some("strokeResource")).map(|(_, f, ..)| f).collect();
    if !removed.iter().all(|r| present.contains(&Some(r.clone()))) {
        return Err(unsupported("Silinecek çizgilerin verisi notta bulunamadı."));
    }
    opaque_fingerprint(snapshot, appended, Some(removed))
}

/// `cloud_page_ops.compare_page_ops_guard(raw, baseline, pages=, layers=, files=)`
pub fn compare_page_ops_guard(
    raw: Option<&[u8]>,
    baseline: Option<&str>,
    pages: &[String],
    layers: &[(String, String)],
    files: &[String],
) -> Result<(), GuardError> {
    let layers: Vec<(Option<String>, Option<String>)> = layers.iter().map(|(l, p)| (Some(l.clone()), Some(p.clone()))).collect();
    guards::compare_page_ops_guard(raw, baseline, &opt_ids(pages), &layers, &opt_ids(files))
}

/// `[p.get('id') for p in ET.fromstring(xml).find('pageList')]`
fn page_order(xml: &[u8]) -> R<Val> {
    let root = etree::parse_bytes(xml, false).map_err(|m| Fail::new(Kind::XmlParse, m))?;
    let list = root.children.iter().find(|c| c.tag == "pageList").ok_or_else(|| Fail::type_("'NoneType' object is not iterable"))?;
    Ok(Val::List(list.children.iter().map(|p| Val::opt_str(p.get("id"))).collect()))
}

fn page_expected(snapshot: &Snapshot, server_id: &str, edit_xml: &[u8], opaque: &str, order: Val) -> R<Val> {
    Ok(Val::dict(vec![
        ("title", Val::s(&snapshot.title)),
        ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
        ("server_id", Val::s(server_id)),
        ("body_sha256", sha(snapshot.body.as_bytes())),
        ("content_fingerprint_version", Val::Int(2)),
        ("content_sha256", Val::s(editor::editor_content_fingerprint(edit_xml)?)),
        ("opaque_sha256", Val::s(opaque)),
        ("page_order", order),
    ]))
}

struct Begun {
    snapshot: Snapshot,
    fresh: Snapshot,
    output: std::path::PathBuf,
    opaque: String,
}

/// `cloud_page_ops._begin`; `Err(Ok(replay))` style is avoided by returning
/// `Ok(Err(replay))` for an already completed operation.
fn begin(
    api: &CloudApi,
    server_id: &str,
    snapshot: SnapIn<'_>,
    now_ms: Option<i64>,
    operation_id: &str,
    kind: &str,
    parameters: Vec<(&str, Val)>,
) -> R<Result<Begun, Val>> {
    let snapshot = resolve_for(snapshot, server_id)?;
    if matches!(now_ms, Some(n) if n <= 0) {
        return Err(unsupported("Kayıt zamanı geçersiz."));
    }
    api.validate_identity()?;
    let opaque = opaque_fingerprint(&snapshot, &[], None)?;
    let mut intent = vec![("kind", Val::s(kind))];
    intent.extend(parameters);
    intent.push(("server_id", Val::s(server_id)));
    intent.push(("local_id", Val::opt_str(snapshot.local_id.as_deref())));
    intent.push(("baseline_commit", Val::opt_str(snapshot.commit_id.as_deref())));
    intent.push(("now_ms", now_ms.map(Val::Int).unwrap_or(Val::Null)));
    intent.push(("snapshot_sha256", sha(&snapshot.resource_xml)));
    intent.push(("opaque_sha256", Val::s(&opaque)));
    let (output, replay) = api.operation(operation_id, intent)?;
    if let Some(r) = replay {
        return Ok(Err(r));
    }
    let fresh = api.fresh_matching(server_id, &snapshot, snapshot.commit_id.as_deref(), Some(&opaque))?;
    Ok(Ok(Begun { snapshot, fresh, output, opaque }))
}

/// `cloud_page_ops._plan(api, output, now_ms, id_count)` → (now_ms, ids)
fn plan(api: &CloudApi, output: &Path, now_ms: Option<i64>, id_count: usize) -> R<(Option<i128>, Vec<Option<String>>)> {
    let prepared = output.join("prepared.json");
    let plan = if prepared.exists() {
        load_json(&prepared, util::DEFAULT_LIMIT)?
    } else {
        let ids: Vec<Val> = (0..id_count).map(|_| Val::s(api.random().uuid4())).collect();
        let plan = Val::dict(vec![("now_ms", Val::Int(now_ms.unwrap_or_else(|| api.now_ms()))), ("ids", Val::List(ids))]);
        json_once(&prepared, &plan)?;
        plan
    };
    let now = match ditem(&plan, "now_ms")? {
        Val::Int(v) => Some(*v as i128),
        _ => None,
    };
    let ids = match dget(&plan, "ids")? {
        Some(Val::List(items)) => items.iter().map(|v| v.as_str().map(str::to_string)).collect(),
        _ => vec![],
    };
    Ok((now, ids))
}

type Guard<'a> = &'a dyn Fn(Option<&[u8]>, Option<&str>, &rs_ops::page_ops::PageOps) -> Result<(), GuardError>;

#[allow(clippy::too_many_arguments)]
fn page_transaction(
    api: &CloudApi,
    server_id: &str,
    snapshot: SnapIn<'_>,
    operation_id: &str,
    now_ms: Option<i64>,
    kind: &str,
    parameters: Vec<(&str, Val)>,
    id_count: usize,
    prepare: &dyn Fn(&Snapshot, Option<i128>, &[Option<String>]) -> Result<rs_ops::page_ops::PageOps, rs_ops::err::Error>,
    guard: Guard<'_>,
    removed_parts: Option<&dyn Fn(&Snapshot) -> R<Vec<String>>>,
) -> R<Val> {
    let b = match begin(api, server_id, snapshot, now_ms, operation_id, kind, parameters)? {
        Ok(b) => b,
        Err(replay) => return Ok(replay),
    };
    let (now, ids) = plan(api, &b.output, now_ms, id_count)?;
    let edit = prepare(&b.fresh, now, &ids).map_err(|e| {
        let f = fail_of_ops(e);
        if f.is_document() {
            unsupported(&f.message)
        } else {
            f
        }
    })?;
    let expected_opaque = match removed_parts {
        Some(removed) => opaque_without(&b.snapshot, &removed(&b.fresh)?, &[])?,
        None => b.opaque.clone(),
    };
    json_once(&b.output.join("expected.json"), &page_expected(&b.snapshot, server_id, &edit.full_xml, &expected_opaque, list_str(&edit.page_order))?)?;
    save_once(&b.output.join("draft.xml"), &edit.full_xml)?;
    let response = api.compare_request(&b.output, server_id, &edit.tree_xml)?;
    guard(response.body.as_deref(), b.snapshot.commit_id.as_deref(), &edit)
        .map_err(|e| guard_err(e, "Sunucunun istediği öğeler sayfa değişikliğiyle uyuşmuyor."))?;
    let (payload, content_type) = multipart(api.random(), &edit.upload_xml);
    api.publish(&b.output, &format!("{}/update", api.url(server_id)?), payload, &content_type)
}

/// `cloud_page_ops.add_page(api, server_id, snapshot, after_index, operation_id=..., now_ms=None)`
pub fn add_page<'a>(api: &CloudApi, server_id: &str, snapshot: impl Into<SnapIn<'a>>, after_index: i64, operation_id: &str, now_ms: Option<i64>) -> R<Val> {
    let deps = PureDeps::default();
    page_transaction(
        api,
        server_id,
        snapshot.into(),
        operation_id,
        now_ms,
        "add_page",
        vec![("after_index", Val::Int(after_index))],
        2,
        &|fresh, now, ids| {
            let page = ids.first().cloned().flatten();
            let layer = ids.get(1).cloned().flatten();
            rs_ops::page_ops::prepare_add_page(&deps, fresh, Some(after_index as i128), now, Some(page.as_deref()), Some(layer.as_deref()), None, None)
        },
        &|raw, baseline, edit| {
            let layers: Vec<(String, String)> = edit.layer_ids.iter().cloned().zip(edit.page_ids.iter().cloned()).collect();
            compare_page_ops_guard(raw, baseline, &edit.page_ids, &layers, &[])
        },
        None,
    )
}

/// `cloud_page_ops.delete_pages(api, server_id, snapshot, page_ids, operation_id=..., now_ms=None)`
pub fn delete_pages<'a>(api: &CloudApi, server_id: &str, snapshot: impl Into<SnapIn<'a>>, page_ids: &[String], operation_id: &str, now_ms: Option<i64>) -> R<Val> {
    for identity in page_ids {
        check_uuid(identity)?;
    }
    let targets = page_ids.to_vec();
    let deps = PureDeps::default();
    let removed = |fresh: &Snapshot| -> R<Vec<String>> {
        let root: Element = etree::parse_bytes(&fresh.resource_xml, false).map_err(|m| Fail::new(Kind::XmlParse, m))?;
        let list = root.children.iter().find(|c| c.tag == "pageList").ok_or_else(|| Fail::type_("'NoneType' object is not iterable"))?;
        let mut names = Vec::new();
        for page in &list.children {
            if page.get("id").is_some_and(|id| targets.iter().any(|t| t == id)) {
                for object in page.iter().into_iter().filter(|o| o.tag == "object") {
                    let id = object.get("id").ok_or_else(|| Fail::type_("unsupported operand type(s) for +: 'NoneType' and 'str'"))?;
                    let time = object.get("modifiedTime").ok_or_else(|| Fail::type_("can only concatenate str (not \"NoneType\") to str"))?;
                    names.push(format!("{id}_{time}"));
                }
            }
        }
        Ok(names)
    };
    page_transaction(
        api,
        server_id,
        snapshot.into(),
        operation_id,
        now_ms,
        "delete_pages",
        vec![("page_ids", list_str(page_ids))],
        0,
        &|fresh, now, _| rs_ops::page_ops::prepare_delete_page(&deps, fresh, opt_ids(&targets), now),
        &|raw, baseline, edit| compare_page_ops_guard(raw, baseline, &[], &[], &edit.content_file_ids),
        Some(&removed),
    )
}

/// `cloud_page_ops.reorder_pages(api, server_id, snapshot, page_ids_in_order, operation_id=..., now_ms=None)`
pub fn reorder_pages<'a>(api: &CloudApi, server_id: &str, snapshot: impl Into<SnapIn<'a>>, order: &[String], operation_id: &str, now_ms: Option<i64>) -> R<Val> {
    for identity in order {
        check_uuid(identity)?;
    }
    let deps = PureDeps::default();
    page_transaction(
        api,
        server_id,
        snapshot.into(),
        operation_id,
        now_ms,
        "reorder_pages",
        vec![("page_ids", list_str(order))],
        0,
        &|fresh, now, _| rs_ops::page_ops::prepare_reorder_pages(&deps, fresh, opt_ids(order), now),
        &|raw, baseline, _| compare_page_ops_guard(raw, baseline, &[], &[], &[]),
        None,
    )
}

/// `cloud_page_ops.append_first_stroke(api, server_id, snapshot, page_id, samples,
/// color_argb=..., size=..., new_stroke_id=..., operation_id=..., now_ms=None)`;
/// `color_argb` / `size` are the Python values.
#[allow(clippy::too_many_arguments)]
pub fn append_first_stroke<'a>(
    api: &CloudApi,
    server_id: &str,
    snapshot: impl Into<SnapIn<'a>>,
    page_id: &str,
    samples: &RawSamples,
    color_argb: &Val,
    size: &Val,
    new_stroke_id: &str,
    operation_id: &str,
    now_ms: Option<i64>,
) -> R<Val> {
    append_scratch_stroke(api, server_id, snapshot, page_id, samples, color_argb, size, new_stroke_id, operation_id, now_ms, false)
}

/// `cloud_page_ops.append_highlighter_stroke(...)`: recipe 5, one highlighter
/// (Marker4, top layer) stroke; same transaction as the first stroke.
#[allow(clippy::too_many_arguments)]
pub fn append_highlighter_stroke<'a>(
    api: &CloudApi,
    server_id: &str,
    snapshot: impl Into<SnapIn<'a>>,
    page_id: &str,
    samples: &RawSamples,
    color_argb: &Val,
    size: &Val,
    new_stroke_id: &str,
    operation_id: &str,
    now_ms: Option<i64>,
) -> R<Val> {
    append_scratch_stroke(api, server_id, snapshot, page_id, samples, color_argb, size, new_stroke_id, operation_id, now_ms, true)
}

/// `cloud_page_ops._append_scratch_stroke`: recipe 4 (first stroke) or 5 (highlighter).
#[allow(clippy::too_many_arguments)]
fn append_scratch_stroke<'a>(
    api: &CloudApi,
    server_id: &str,
    snapshot: impl Into<SnapIn<'a>>,
    page_id: &str,
    samples: &RawSamples,
    color_argb: &Val,
    size: &Val,
    new_stroke_id: &str,
    operation_id: &str,
    now_ms: Option<i64>,
    highlighter: bool,
) -> R<Val> {
    let (kind, recipe) = if highlighter {
        ("append_highlighter_stroke", rs_ops::ink_highlighter::HIGHLIGHTER_RECIPE)
    } else {
        ("append_first_stroke", rs_ops::ink_first_stroke::FIRST_STROKE_RECIPE)
    };
    check_uuid(page_id)?;
    check_uuid(new_stroke_id)?;
    let frozen = samples.clone();
    let built = (|| -> R<(Vec<(&'static str, Val)>, i128, f64)> {
        if !(1..=MAX_POINTS).contains(&frozen.points.len()) {
            return Err(Fail::new(Kind::Ink, "invalid capture"));
        }
        let capture = rs_ink::core::capture::normalize_capture(&frozen, false).map_err(notes_host::deps::fail_of_ink)?;
        let color = match color_argb {
            Val::Int(c) => Some(*c as i128),
            Val::BigInt(s) => Some(if s.starts_with('-') { i128::MIN } else { i128::MAX }),
            _ => None,
        };
        let pen = match size {
            Val::Int(i) => Some(*i as f64),
            Val::Float(f) => Some(*f),
            _ => None,
        };
        let style = if highlighter {
            rs_ops::ink_highlighter::highlighter_style(color, pen)
        } else {
            rs_ops::ink_first_stroke::first_stroke_style(color, pen)
        }
        .map_err(fail_of_ops)?;
        let get = |k: &str| style.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
        let (color, pen) = match (get("colorARGB"), get("penSize")) {
            (Some(Num::I(c)), Some(p)) => (c as i128, p.as_f64()),
            _ => return Err(Fail::key("'colorARGB'")),
        };
        let parameters = vec![
            ("page_id", Val::s(page_id)),
            ("new_stroke_id", Val::s(new_stroke_id)),
            ("recipeVersion", Val::Int(recipe)),
            (
                "samples",
                Val::dict(vec![
                    ("schemaVersion", Val::Int(1)),
                    ("encoding", Val::s("native-q5-q12-v1")),
                    ("count", Val::Int(capture.samples.points.len() as i64)),
                    ("channelsBase64", Val::s(util::b64encode(&capture.channels))),
                ]),
            ),
            ("style", Val::dict(vec![("colorARGB", Val::Int(color as i64)), ("penSize", Val::Float(pen))])),
        ];
        if json::dumps(&Val::dict(parameters.clone()), &json::DEFAULT).chars().count() > 1024 * 1024 {
            return Err(Fail::new(Kind::Ink, "capture intent bound"));
        }
        Ok((parameters, color, pen))
    })();
    let (parameters, color, pen) = match built {
        Ok(v) => v,
        Err(e) if e.kind == Kind::Ink || e.is_type() || e.is_value() || e.kind == Kind::Overflow => {
            return Err(unsupported("Çizgi noktaları veya kalem ayarı desteklenen aralıkta değil."))
        }
        Err(e) => return Err(e),
    };
    let b = match begin(api, server_id, snapshot.into(), now_ms, operation_id, kind, parameters)? {
        Ok(b) => b,
        Err(replay) => return Ok(replay),
    };
    let (now, _) = plan(api, &b.output, now_ms, 0)?;
    let deps = PureDeps::default();
    let checked = (|| -> R<(rs_ops::ink_first_stroke::FirstStroke, Appended)> {
        let prepare = if highlighter {
            rs_ops::ink_highlighter::prepare_highlighter_stroke
        } else {
            rs_ops::ink_first_stroke::prepare_first_stroke
        };
        let edit = prepare(
            &deps,
            &b.fresh,
            Some(page_id),
            Some(&frozen as &dyn std::any::Any),
            Some(color),
            Some(pen),
            now,
            Some(Some(new_stroke_id)),
        )
        .map_err(fail_of_ops)?;
        let requested: HashSet<&String> = edit.requested_ids.iter().collect();
        let new_owned = new_stroke_id.to_string();
        let mut wanted: HashSet<&String> = [&edit.page_id, &edit.layer_id, &new_owned].into_iter().collect();
        wanted.extend(edit.content_file_ids.iter());
        if requested != wanted {
            return Err(Fail::new(Kind::Ink, "Beklenmeyen çizgi değişikliği."));
        }
        let part = &edit.part;
        if part.name != "strokeResource" || part.filename != format!("{}_{}", new_stroke_id, edit.tick) || part.body.is_empty() || part.body.len() > MAX_BODY_BYTES {
            return Err(Fail::new(Kind::Ink, "Geçersiz çizgi verisi."));
        }
        let appended: Appended = (part.name.clone(), part.filename.clone(), part.body.clone());
        Ok((edit, appended))
    })();
    let (edit, part) = match checked {
        Ok(v) => v,
        Err(e) if e.kind == Kind::Ink || e.is_document() => return Err(unsupported(&e.message)),
        Err(e) => return Err(e),
    };
    let expected = page_expected(
        &b.snapshot,
        server_id,
        &edit.full_xml,
        &opaque_fingerprint(&b.snapshot, std::slice::from_ref(&part), None)?,
        page_order(&edit.full_xml)?,
    )?;
    json_once(&b.output.join("expected.json"), &expected)?;
    save_once(&b.output.join("draft.xml"), &edit.full_xml)?;
    save_once(&b.output.join("stroke.bin"), &part.2)?;
    let response = api.compare_request(&b.output, server_id, &edit.tree_xml)?;
    guards::compare_ink_guard(
        response.body.as_deref(),
        b.snapshot.commit_id.as_deref(),
        Some(&edit.page_id),
        Some(&edit.layer_id),
        Some(new_stroke_id),
        &opt_ids(&edit.content_file_ids),
    )
    .map_err(|e| guard_err(e, "Sunucunun istediği çizim içeriği düzenlemeyle uyuşmuyor."))?;
    let (payload, content_type) = ink_multipart(api.random(), &edit.upload_xml, std::slice::from_ref(&part))?;
    api.publish(&b.output, &format!("{}/update", api.url(server_id)?), payload, &content_type)
}

/// `cloud_page_ops.reconcile(api, operation_id)`
pub fn reconcile_page(api: &CloudApi, operation_id: &str) -> R<Val> {
    reconcile_kinds(api, operation_id, &PAGE_KINDS)
}

/// `cloud_shapes.reconcile(api, operation_id)`: the page-ops readback for SHAPE_KINDS.
pub fn reconcile_shape(api: &CloudApi, operation_id: &str) -> R<Val> {
    reconcile_kinds(api, operation_id, &SHAPE_KINDS)
}

fn reconcile_kinds(api: &CloudApi, operation_id: &str, kinds: &[&str]) -> R<Val> {
    let output = api.work_dir.join("operations").join(check_uuid(operation_id)?);
    if output.join("result.json").exists() {
        return load_json(&output.join("result.json"), 96 * 1024 * 1024);
    }
    let intent = load_json(&output.join("intent.json"), 2 * 1024 * 1024)?;
    if !py_eq(dget(&intent, "accountFingerprint")?.unwrap_or(&Val::Null), &Val::s(api.identity())) {
        return Err(Fail::api("identity_mismatch", "İşlem farklı bir hesaba ait."));
    }
    let checked = (|| -> R<(String, Snapshot)> {
        if !matches!(dget(&intent, "kind")?, Some(Val::Str(k)) if kinds.contains(&k.as_str())) {
            return Err(Fail::value("kind"));
        }
        let (server, snapshot, expected) = readback_header(api, &output)?;
        let draft = util::read_bytes(&output.join("draft.xml"))?;
        let neq = |a: String, key: &str| -> R<bool> { Ok(!py_eq(&Val::s(a), ditem(&expected, key)?)) };
        if !py_eq(dget(&expected, "content_fingerprint_version")?.unwrap_or(&Val::Null), &Val::Int(2))
            || neq(editor::editor_content_fingerprint(&draft)?, "content_sha256")?
            || neq(editor::editor_content_fingerprint(&snapshot.resource_xml)?, "content_sha256")?
            || neq(util::sha256_hex(snapshot.body.as_bytes()), "body_sha256")?
            || neq(opaque_fingerprint(&snapshot, &[], None)?, "opaque_sha256")?
            || !py_eq(&page_order(&snapshot.resource_xml)?, ditem(&expected, "page_order")?)
        {
            return Err(Fail::value("readback mismatch"));
        }
        Ok((server, snapshot))
    })();
    let (server, snapshot) = match checked {
        Ok(v) => v,
        Err(e) if e.is_os() || e.is_value() || e.is_key() || e.is_type() || e.is_api() || e.kind == Kind::Attribute || e.kind == Kind::XmlParse => {
            return Err(write_uncertain("Kayıt yanıtı alındı; sunucudan geri okuyarak henüz doğrulanamadı."))
        }
        Err(e) => return Err(e),
    };
    let result = result_val(&server, &snapshot);
    save_json(&output.join("result.json"), &result)?;
    Ok(result)
}

// ======================================================================
// cloud_shapes
// ======================================================================

/// `cloud_shapes.ADD_SHAPE_KIND`
pub const ADD_SHAPE_KIND: &str = "add_shape_v1";
/// `cloud_shapes.KINDS`
pub const SHAPE_KINDS: [&str; 1] = [ADD_SHAPE_KIND];

/// `cloud_shapes.shape_intent(shape)` → normalized shape (UnsupportedDocumentError).
pub fn shape_intent(shape: &Val) -> R<rs_ops::page_shapes::Shape> {
    rs_store::shapes::shape_intent(shape).map_err(|m| unsupported(&m))
}

/// `cloud_shapes.add_shape(api, server_id, snapshot, page_id, shape, operation_id=..., now_ms=None)`
pub fn add_shape<'a>(
    api: &CloudApi,
    server_id: &str,
    snapshot: impl Into<SnapIn<'a>>,
    page_id: &str,
    shape: &Val,
    operation_id: &str,
    now_ms: Option<i64>,
) -> R<Val> {
    use rs_ops::page_shapes::{Fill, StyleIn, SHAPE_RECIPE};
    check_uuid(page_id)?;
    let spec = shape_intent(shape)?;
    let parameters = vec![
        ("page_id", Val::s(page_id)),
        ("recipeVersion", Val::Int(SHAPE_RECIPE)),
        ("shape", rs_store::shapes::shape_val(&spec)),
    ];
    let b = match begin(api, server_id, snapshot.into(), now_ms, operation_id, ADD_SHAPE_KIND, parameters)? {
        Ok(b) => b,
        Err(replay) => return Ok(replay),
    };
    let (now, ids) = plan(api, &b.output, now_ms, 1)?;
    let object_id = ids.first().cloned().flatten();
    let deps = PureDeps::default();
    let style = StyleIn {
        is_dict: true,
        keys: vec!["colorARGB".into(), "width".into(), "fillARGB".into()],
        color: Some(spec.color_argb as i128),
        width: Some(spec.width),
        fill: spec.fill_argb.map(|f| Fill::Int(f as i128)).unwrap_or(Fill::Absent),
    };
    let checked = (|| -> R<rs_ops::page_shapes::ShapeEdit> {
        let edit = rs_ops::page_shapes::prepare_add_shape(
            &deps,
            &b.fresh,
            Some(page_id),
            Some(spec.kind.as_str()),
            Some(&spec.geometry.map(Some)),
            &style,
            now,
            object_id.as_deref().map(Some),
        )
        .map_err(fail_of_ops)?;
        let requested: HashSet<&String> = edit.requested_ids.iter().collect();
        let mut wanted: HashSet<&String> = [&edit.page_id, &edit.layer_id, &edit.object_id].into_iter().collect();
        wanted.extend(edit.content_file_ids.iter());
        if Some(edit.object_id.as_str()) != object_id.as_deref() || edit.page_id != page_id || requested != wanted {
            return Err(Fail::document("Beklenmeyen şekil değişikliği."));
        }
        Ok(edit)
    })();
    let edit = match checked {
        Ok(v) => v,
        Err(e) if e.is_document() => return Err(unsupported(&e.message)),
        Err(e) => return Err(e),
    };
    json_once(&b.output.join("expected.json"), &page_expected(&b.snapshot, server_id, &edit.full_xml, &b.opaque, page_order(&edit.full_xml)?)?)?;
    save_once(&b.output.join("draft.xml"), &edit.full_xml)?;
    let response = api.compare_request(&b.output, server_id, &edit.tree_xml)?;
    guards::compare_ink_guard(
        response.body.as_deref(),
        b.snapshot.commit_id.as_deref(),
        Some(page_id),
        Some(&edit.layer_id),
        Some(&edit.object_id),
        &opt_ids(&edit.content_file_ids),
    )
    .map_err(|e| guard_err(e, "Sunucunun istediği öğeler şekil eklemesiyle uyuşmuyor."))?;
    let (payload, content_type) = multipart(api.random(), &edit.upload_xml);
    api.publish(&b.output, &format!("{}/update", api.url(server_id)?), payload, &content_type)
}
