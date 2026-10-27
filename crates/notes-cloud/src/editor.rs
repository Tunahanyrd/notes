//! `samsung_notes.editor_document` calls of the cloud layer over the pure
//! rs-editor core (the PyO3 wrappers' argument handling, without Python).
use notes_host::snapshot::Snapshot;
use notes_host::{Fail, Kind, Val};
use rs_editor::edit::{self, DocBackend, EditOut, Now};
use rs_editor::pyval::{parse_py_int, PyInt, Val as E};
use rs_editor::rich::Error as EdErr;

use crate::util::R;

pub fn to_editor(v: &Val) -> E {
    match v {
        Val::Null => E::None,
        Val::Bool(b) => E::Bool(*b),
        Val::Int(i) => E::Int(PyInt::from_i128(*i as i128), true),
        Val::BigInt(s) => E::Int(parse_py_int(s).unwrap_or_else(|| PyInt(s.clone())), true),
        Val::Float(f) => E::Float(*f),
        Val::Str(s) => E::Str(s.clone()),
        Val::Bytes(_) => E::Other(None),
        Val::List(items) => E::List(items.iter().map(to_editor).collect()),
        Val::Dict(items) => E::Dict(items.iter().map(|(k, v)| (E::Str(k.clone()), to_editor(v))).collect()),
    }
}

pub fn from_editor(v: &E) -> R<Val> {
    Ok(match v {
        E::None => Val::Null,
        E::Bool(b) => Val::Bool(*b),
        E::Int(i, _) => match i.to_i64() {
            Some(x) => Val::Int(x),
            None => Val::BigInt(i.0.clone()),
        },
        E::Float(f) => Val::Float(*f),
        E::Str(s) => Val::Str(s.clone()),
        E::List(items) => Val::List(items.iter().map(from_editor).collect::<R<_>>()?),
        E::Dict(items) => Val::Dict(
            items
                .iter()
                .map(|(k, v)| match k.as_str() {
                    Some(s) => Ok((s.to_string(), from_editor(v)?)),
                    None => Err(Fail::type_("non-str key")),
                })
                .collect::<R<_>>()?,
        ),
        E::BadStr(_) | E::Other(_) => return Err(Fail::type_("unsupported value from rs-editor")),
    })
}

/// rs-editor error → the Python exception class.
pub fn editor_fail(e: EdErr) -> Fail {
    match e {
        EdErr::Document(m) => Fail::document(m),
        EdErr::Payload(m) => Fail::new(Kind::Payload, m),
        EdErr::Value(m) => Fail::value(m),
        EdErr::Parse(m) => Fail::new(Kind::XmlParse, m),
        EdErr::Type(m) => Fail::type_(m),
        EdErr::Overflow(m) => Fail::new(Kind::Overflow, m),
        EdErr::Attribute(m) => Fail::new(Kind::Attribute, m),
        EdErr::External { payload, message, .. } => match payload.downcast::<Fail>() {
            Ok(f) => *f,
            Err(_) => Fail::value(message),
        },
    }
}

fn backend(snapshot: &Snapshot) -> DocBackend {
    DocBackend { limits: rs_doc::doc::DocLimits::default(), raw_body: snapshot.raw_body.clone(), content_type: snapshot.content_type.clone() }
}

/// `resolve()` of the editor wrappers: a committed snapshot is required.
fn resolve(snapshot: &Snapshot) -> R<edit::Snapshot> {
    if snapshot.commit_id.as_deref().is_none_or(str::is_empty) {
        return Err(Fail::document("Notun Cloud sürümü eksik."));
    }
    Ok(edit::Snapshot { title: Some(snapshot.title.clone()), resource_xml: snapshot.resource_xml.clone() })
}

/// `editor_document.normalize_model(model)`
pub fn normalize_model(model: &Val) -> R<Val> {
    notes_host::areas::normalize_model(model)
}

/// `editor_document.editor_model_fingerprint(model)`
pub fn editor_model_fingerprint(model: &Val) -> R<String> {
    rs_editor::rich::editor_model_fingerprint(&to_editor(model)).map_err(editor_fail)
}

/// `editor_document.editor_content_fingerprint(xml)`
pub fn editor_content_fingerprint(xml: &[u8]) -> R<String> {
    edit::editor_content_fingerprint(xml).map_err(editor_fail)
}

/// `editor_document.title_content_fingerprint(xml)`
pub fn title_content_fingerprint(xml: &[u8]) -> R<String> {
    edit::title_content_fingerprint(xml).map_err(editor_fail)
}

/// `editor_document.editor_projection(snapshot)` → (textEdit, model).
pub fn projection(snapshot: &Snapshot) -> R<(bool, Val)> {
    let p = notes_host::areas::editor_projection(snapshot)?;
    let text_edit = p.get("capabilities").and_then(|c| c.get("textEdit")).map(|v| v.truthy()).unwrap_or(false);
    Ok((text_edit, p.get("model").cloned().unwrap_or(Val::Null)))
}

/// `editor_document.new_editor_document(title, model, local_id=..., now_ms=...)`
pub fn new_editor_document(title: &str, model: &Val, local_id: Option<&str>, now_ms: i64) -> R<(String, Vec<u8>)> {
    let max_text = rs_doc::doc::DocLimits::default().max_text;
    edit::new_editor_document(&to_editor(model), |text| {
        rs_doc::doc::new_document(title, text, local_id, rs_doc::doc::NowArg::Int(now_ms as i128), max_text).map_err(EdErr::from)
    })
    .map_err(editor_fail)
}

/// `editor_document.prepare_title_edit(snapshot, title, now_ms=...)`
pub fn prepare_title_edit(snapshot: &Snapshot, title: &str, now_ms: i64) -> R<EditOut> {
    let snap = resolve(snapshot)?;
    edit::prepare_title_edit(&snap, &E::Str(title.to_string()), &Now::Int(PyInt::from_i128(now_ms as i128)), &backend(snapshot)).map_err(editor_fail)
}

/// `editor_document.prepare_editor_edit(snapshot, title, model, now_ms=...)`
pub fn prepare_editor_edit(snapshot: &Snapshot, title: &str, model: &Val, now_ms: i64) -> R<EditOut> {
    let snap = resolve(snapshot)?;
    edit::prepare_editor_edit(&snap, &E::Str(title.to_string()), &to_editor(model), &Now::Int(PyInt::from_i128(now_ms as i128)), &backend(snapshot))
        .map_err(editor_fail)
}
