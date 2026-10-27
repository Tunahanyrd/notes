//! Pure facades over the area cores, returning the same JSON values the
//! Python functions return (dict key order, int vs float).
//!
//! * `editor_document`: [`editor_projection`], [`editor_projection_val`],
//!   [`normalize_model`]
//! * `ink_projection`: [`project_ink`], [`project_page_geometry`],
//!   [`PROJECTION_RECIPE`]
//! * `ink_store`: [`samples_from_points`]
//! * `page_style` / `page_ops`: [`page_background_state`], [`page_ops_state`]
//! * `engine.templates`: [`templates`]
use std::any::Any;

use rs_editor::pyval::{parse_py_int, PyInt, Val as E};
use rs_editor::rich::Error as EdErr;
use rs_ink::core::binary::RawSamples;
use rs_store::val::Val;

use crate::deps::{fail_of_ops, PureDeps};
use crate::err::{Fail, Kind, R};
use crate::snapshot::{self, Snapshot};

/// `ink_projection.PROJECTION_RECIPE`
pub const PROJECTION_RECIPE: &str = rs_ops::ink_projection::PROJECTION_RECIPE;
/// `ink_projection.MAX_GEOMETRY_POINTS`
pub const MAX_GEOMETRY_POINTS: f64 = rs_ops::ink_projection::MAX_GEOMETRY_POINTS;

// ---------------------------------------------------------------- editor

fn to_editor(v: &Val) -> E {
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

/// rs-editor value → JSON value. `lenient` maps unrepresentable values to
/// None (the projection wrapper's `from_val`); strict mode raises TypeError
/// (PyHost's `from_editor`).
fn from_editor(v: &E, lenient: bool) -> R<Val> {
    Ok(match v {
        E::None => Val::Null,
        E::Bool(b) => Val::Bool(*b),
        E::Int(i, _) => match i.to_i64() {
            Some(x) => Val::Int(x),
            None => Val::BigInt(i.0.clone()),
        },
        E::Float(f) => Val::Float(*f),
        E::Str(s) => Val::Str(s.clone()),
        E::List(items) => Val::List(items.iter().map(|x| from_editor(x, lenient)).collect::<R<_>>()?),
        E::Dict(items) => Val::Dict(
            items
                .iter()
                .map(|(k, v)| {
                    let key = match k.as_str() {
                        Some(s) => s.to_string(),
                        None => return Err(Fail::type_("non-str key")),
                    };
                    Ok((key, from_editor(v, lenient)?))
                })
                .collect::<R<_>>()?,
        ),
        E::BadStr(_) | E::Other(_) if lenient => Val::Null,
        E::BadStr(_) | E::Other(_) => return Err(Fail::type_("unsupported value from rs-editor")),
    })
}

/// rs-editor error → Fail with the class PyHost raises for it.
fn editor_fail(e: EdErr) -> Fail {
    match e {
        EdErr::Document(m) => Fail::document(m),
        EdErr::Payload(m) => Fail::new(Kind::Payload, m),
        EdErr::Value(m) | EdErr::Parse(m) => Fail::value(m),
        EdErr::Type(m) => Fail::type_(m),
        EdErr::Overflow(m) => Fail::new(Kind::Overflow, m),
        EdErr::Attribute(m) => Fail::new(Kind::Attribute, m),
        EdErr::External { payload, message, .. } => match payload.downcast::<Fail>() {
            Ok(f) => *f,
            Err(_) => Fail::value(message),
        },
    }
}

/// `editor_document.normalize_model(model)`
pub fn normalize_model(model: &Val) -> R<Val> {
    match rs_editor::rich::normalize_model(&to_editor(model)) {
        Ok(v) => from_editor(&v, false),
        Err(e) => Err(editor_fail(e)),
    }
}

fn projection_val(p: &rs_editor::edit::Projection) -> R<Val> {
    Ok(Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        (
            "capabilities",
            Val::dict(vec![
                ("titleEdit", Val::Bool(p.title_edit)),
                ("textEdit", Val::Bool(p.text_edit)),
                ("richStyle", Val::Bool(p.text_edit)),
                ("inkEdit", Val::Bool(false)),
            ]),
        ),
        ("model", match &p.model {
            Some(m) => from_editor(m, true)?,
            None => Val::Null,
        }),
        ("reason", Val::opt_str(p.reason.as_deref())),
    ]))
}

fn run_editor_projection(snap: Option<&Snapshot>, resolved: rs_editor::rich::Result<rs_editor::edit::Snapshot>) -> R<Val> {
    let backend = rs_editor::edit::DocBackend {
        limits: rs_doc::doc::DocLimits::default(),
        raw_body: snap.map(|s| s.raw_body.clone()).unwrap_or_default(),
        content_type: snap.map(|s| s.content_type.clone()).unwrap_or_default(),
    };
    let p = match &resolved {
        Ok(s) => rs_editor::edit::editor_projection(Ok(s), &backend),
        Err(_) => rs_editor::edit::editor_projection(Err(resolved.err().unwrap()), &backend),
    }
    .map_err(editor_fail)?;
    projection_val(&p)
}

fn resolve(s: &Snapshot) -> rs_editor::rich::Result<rs_editor::edit::Snapshot> {
    if s.commit_id.as_deref().is_none_or(str::is_empty) {
        return rs_editor::rich::doc("Notun Cloud sürümü eksik.");
    }
    Ok(rs_editor::edit::Snapshot { title: Some(s.title.clone()), resource_xml: s.resource_xml.clone() })
}

/// `editor_document.editor_projection(snapshot)` for a parsed DocumentSnapshot.
pub fn editor_projection(snapshot: &Snapshot) -> R<Val> {
    run_editor_projection(Some(snapshot), resolve(snapshot))
}

/// `editor_projection(document_dict)`: `from_dict` failures of the format
/// classes become the disabled projection's reason, like Python.
pub fn editor_projection_val(document: &Val) -> R<Val> {
    if !matches!(document, Val::Dict(_)) {
        return run_editor_projection(None, rs_editor::rich::doc("Notun Cloud sürümü eksik."));
    }
    match snapshot::from_dict(document) {
        Ok(s) => editor_projection(&s),
        Err(f) => {
            let e = match f.kind {
                Kind::Document | Kind::PageOps => EdErr::Document(f.message),
                Kind::Payload => EdErr::Payload(f.message),
                Kind::Value | Kind::Unicode | Kind::Ink | Kind::Store => EdErr::Value(f.message),
                Kind::Type => EdErr::Type(f.message),
                Kind::XmlParse => EdErr::Parse(f.message),
                _ => return Err(f),
            };
            run_editor_projection(None, Err(e))
        }
    }
}

// ---------------------------------------------------------------- ink projection

/// `ink_projection.project_ink(snapshot, points=points)`; `snapshot` is a
/// stored snapshot dict or (any non-dict) "not a snapshot".
pub fn project_ink(document: &Val, points: bool) -> R<Val> {
    project_ink_any(&PureDeps::default(), document, points)
}

/// `project_ink` for a parsed DocumentSnapshot.
pub fn project_ink_snapshot(snapshot: &Snapshot, points: bool) -> R<Val> {
    project_ink_any(&PureDeps::default(), snapshot, points)
}

fn project_ink_any(deps: &PureDeps, arg: &dyn Any, points: bool) -> R<Val> {
    let p = rs_ops::ink_projection::project_ink(deps, arg, points).map_err(fail_of_ops)?;
    let mut pages = Vec::new();
    for page in &p.pages {
        let mut strokes = Vec::new();
        for s in &page.strokes {
            let mut d: Vec<(&str, Val)> = vec![("id", Val::s(&s.id)), ("kind", Val::s(s.kind))];
            if s.template || s.kind == "highlighter" {
                d.push(("colorARGB", Val::Int(s.color)));
                d.push(("size", Val::Float(s.size)));
            }
            d.push(("template", Val::Bool(s.template)));
            d.push(("layerId", Val::s(&s.layer_id)));
            d.push(("erasable", Val::Bool(s.erasable)));
            if let Some(b) = s.bounds {
                d.push(("bounds", Val::List(b.iter().map(|x| Val::Float(*x)).collect())));
            }
            if let Some(pts) = &s.points {
                d.push((
                    "points",
                    Val::List(
                        pts.iter()
                            .map(|(x, y, pr, t)| {
                                Val::dict(vec![("x", Val::Float(*x)), ("y", Val::Float(*y)), ("pressure", Val::Float(*pr)), ("timeMs", Val::Int(*t))])
                            })
                            .collect(),
                    ),
                ));
            }
            strokes.push(Val::dict(d));
        }
        pages.push(Val::dict(vec![
            ("id", Val::s(&page.id)),
            ("index", Val::Int(page.index as i64)),
            ("width", Val::Int(page.width)),
            ("height", Val::Int(page.height)),
            ("background", if page.pdf { Val::s("pdf") } else { Val::Null }),
            ("strokes", Val::List(strokes)),
            (
                "ink",
                Val::dict(vec![
                    ("appendInk", Val::Bool(page.ink.append_ink)),
                    ("eraseInk", Val::Bool(page.ink.erase_ink)),
                    ("firstStroke", Val::Bool(page.ink.first_stroke)),
                    ("highlighter", Val::Bool(page.ink.highlighter)),
                    ("reason", Val::opt_str(page.ink.reason.as_deref())),
                ]),
            ),
        ]));
    }
    Ok(Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        ("pages", Val::List(pages)),
        (
            "capabilities",
            Val::dict(vec![
                ("appendInk", Val::Bool(p.append_ink)),
                ("eraseInk", Val::Bool(p.erase_ink)),
                ("firstStroke", Val::Bool(p.first_stroke)),
                ("highlighter", Val::Bool(p.highlighter)),
            ]),
        ),
        ("reason", Val::opt_str(p.reason.as_deref())),
        ("rendering", Val::dict(vec![("nativeEquivalent", Val::Bool(false)), ("mode", Val::s("geometry-only"))])),
    ]))
}

/// `ink_projection.project_page_geometry(snapshot, page_id)`.
pub fn project_page_geometry(document: &Val, page_id: &str) -> R<Val> {
    project_page_geometry_val(document, &Val::s(page_id), MAX_GEOMETRY_POINTS)
}

/// `project_page_geometry(snapshot, page_id, max_points=max_points)` with
/// an arbitrary JSON `page_id` (echoed back as `pageId`, like Python).
pub fn project_page_geometry_val(document: &Val, page_id: &Val, max_points: f64) -> R<Val> {
    let deps = PureDeps::default();
    let g = rs_ops::ink_projection::project_page_geometry(&deps, document as &dyn Any, page_id.as_str(), max_points).map_err(fail_of_ops)?;
    let mut d: Vec<(&str, Val)> = vec![("schemaVersion", Val::Int(1)), ("pageId", page_id.clone())];
    if let (Some(w), Some(h)) = (g.width, g.height) {
        d.push(("width", Val::Int(w)));
        d.push(("height", Val::Int(h)));
    }
    let strokes = g
        .strokes
        .iter()
        .map(|s| {
            Val::dict(vec![
                ("id", Val::s(&s.id)),
                ("layerId", Val::opt_str(s.layer_id.as_deref())),
                ("size", Val::Float(s.size)),
                ("points", Val::List(s.points.iter().map(|(x, y)| Val::List(vec![Val::Float(*x), Val::Float(*y)])).collect())),
            ])
        })
        .collect();
    d.push(("strokes", Val::List(strokes)));
    d.push(("reason", Val::opt_str(g.reason.as_deref())));
    Ok(Val::dict(d))
}

/// `ink_store.samples_from_points(points)` as rs-ink raw samples.
pub fn samples_from_points(points: &Val) -> R<RawSamples> {
    crate::deps::raw_samples_from_points(points)
}

// ---------------------------------------------------------------- page style / page ops

/// `page_style.page_background_state(snapshot)` (None: no base snapshot).
pub fn page_background_state(document: Option<&Val>) -> R<Val> {
    page_background_state_with(&PureDeps::default(), document)
}

pub fn page_background_state_with(deps: &PureDeps, document: Option<&Val>) -> R<Val> {
    // A JSON null is Python's None: no base snapshot.
    let arg: Option<&dyn Any> = document.filter(|d| !d.is_null()).map(|d| d as &dyn Any);
    let s = rs_ops::page_style::page_background_state(deps, arg).map_err(fail_of_ops)?;
    let mut d: Vec<(&str, Val)> = vec![
        ("backgroundArgb", s.background_argb.map(|c| Val::Int(c as i64)).unwrap_or(Val::Null)),
        ("editable", Val::Bool(s.editable)),
        ("reason", Val::opt_str(s.reason.as_deref())),
    ];
    if s.editable {
        d.push(("palette", palette()));
    }
    Ok(Val::dict(d))
}

/// `page_style.PAGE_PALETTE` as the state dict lists it.
pub fn palette() -> Val {
    Val::List(
        rs_ops::page_style::PAGE_PALETTE
            .iter()
            .map(|row| Val::List(row.iter().map(|(c, n)| Val::dict(vec![("argb", Val::Int(*c as i64)), ("name", Val::s(*n))])).collect()))
            .collect(),
    )
}

/// `page_ops.page_ops_state(snapshot)` (None: no base snapshot).
pub fn page_ops_state(document: Option<&Val>) -> R<Val> {
    page_ops_state_with(&PureDeps::default(), document)
}

pub fn page_ops_state_with(deps: &PureDeps, document: Option<&Val>) -> R<Val> {
    // A JSON null is Python's None: no base snapshot.
    let arg: Option<&dyn Any> = document.filter(|d| !d.is_null()).map(|d| d as &dyn Any);
    let s = rs_ops::page_ops::page_ops_state(deps, arg).map_err(fail_of_ops)?;
    let pages = s
        .pages
        .iter()
        .map(|r| {
            Val::dict(vec![
                ("id", Val::opt_str(r.id.as_deref())),
                ("index", Val::Int(r.index as i64)),
                ("width", Val::Int(r.width)),
                ("height", Val::Int(r.height)),
                ("hasStrokes", Val::Bool(r.has_strokes)),
                ("deletable", Val::Bool(r.deletable)),
                ("reason", Val::opt_str(r.reason.as_deref())),
            ])
        })
        .collect();
    let mut d: Vec<(&str, Val)> = vec![("editable", Val::Bool(s.editable)), ("reason", Val::opt_str(s.reason.as_deref())), ("pages", Val::List(pages))];
    if s.editable {
        d.push(("canReorder", Val::Bool(s.can_reorder)));
        d.push(("canAdd", Val::Bool(s.can_add)));
    }
    Ok(Val::dict(d))
}

// ---------------------------------------------------------------- templates

/// `Engine.templates()` over `(note id, base_document text)` rows in query
/// order: PDF-page templates (pages with `templateType`) already used in the
/// library, sorted by name. A row whose snapshot/XML does not load is skipped;
/// a non-integer `pageIndex` fails the whole call (ValueError), as in Python.
pub fn templates(rows: &[(String, String)]) -> R<Val> {
    use rs_doc::xml::Element as X;
    let mut found: Vec<(Option<String>, Val)> = Vec::new();
    for (note_id, base) in rows {
        let root = (|| -> Option<X> {
            let document = rs_store::val::loads(base).ok()?;
            let snap = snapshot::from_dict(&document).ok()?;
            rs_doc::xml::fromstring(&snap.resource_xml).ok()
        })();
        let Some(root) = root else { continue };
        let children = |e: &X, tag: &str| -> Vec<X> { e.children.iter().filter(|c| c.tag == tag).cloned().collect() };
        let mut names: Vec<(Option<String>, String)> = Vec::new();
        for list in children(&root, "contentFileList") {
            for f in children(&list, "contentFile") {
                let Some(decoded) = b64decode(f.get("name").unwrap_or("")) else { continue };
                let Ok(text) = String::from_utf8(decoded) else { continue };
                let name = match text.split_once('@') {
                    Some((_, rest)) => rest.to_string(),
                    None => text,
                };
                let id = f.get("id").map(String::from);
                match names.iter_mut().find(|(k, _)| *k == id) {
                    Some(slot) => slot.1 = name,
                    None => names.push((id, name)),
                }
            }
        }
        // groups: sha -> (templateType, indices), in first-seen order.
        let mut groups: Vec<(Option<String>, String, Vec<i64>)> = Vec::new();
        for list in children(&root, "pageList") {
            for page in children(&list, "page") {
                let data: Vec<X> = children(&page, "pdfDataList").iter().flat_map(|l| children(l, "pdfData")).collect();
                let Some(template_type) = page.get("templateType") else { continue };
                if data.len() != 1 {
                    continue;
                }
                let sha = data[0].get("hash").map(String::from);
                let index_text = data[0].get("pageIndex").unwrap_or("0");
                let index = python_int(index_text).ok_or_else(|| Fail::value(format!("invalid literal for int() with base 10: {}", py_repr(index_text))))?;
                match groups.iter_mut().find(|(k, _, _)| *k == sha) {
                    Some(g) => g.2.push(index),
                    None => groups.push((sha, template_type.to_string(), vec![index])),
                }
            }
        }
        for (sha, template_type, indices) in groups {
            if found.iter().any(|(k, _)| *k == sha) {
                continue;
            }
            let Some((_, file_name)) = names.iter().find(|(k, _)| *k == sha) else { continue };
            if template_type.is_empty() || !template_type.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let stem = match file_name.rsplit_once('.') {
                Some((stem, _)) => stem,
                None => file_name.as_str(),
            };
            let ttype = match template_type.parse::<i64>() {
                Ok(v) => Val::Int(v),
                Err(_) => Val::BigInt(template_type.trim_start_matches('0').to_string()),
            };
            let count = indices.len() as i64;
            found.push((
                sha.clone(),
                Val::dict(vec![
                    ("id", Val::opt_str(sha.as_deref())),
                    ("name", Val::s(stem.replace('_', " "))),
                    ("fileName", Val::s(file_name)),
                    ("templateType", ttype),
                    ("pageIndices", Val::List(indices.into_iter().map(Val::Int).collect())),
                    ("pageCount", Val::Int(count)),
                    ("sourceNoteId", Val::s(note_id)),
                ]),
            ));
        }
    }
    // sorted(key=name): stable, by code point (UTF-8 byte order is the same).
    found.sort_by(|a, b| a.1.get("name").and_then(Val::as_str).cmp(&b.1.get("name").and_then(Val::as_str)));
    Ok(Val::dict(vec![("templates", Val::List(found.into_iter().map(|(_, v)| v).collect()))]))
}

fn py_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// `int(text)` for decimal ASCII text (whitespace, sign, single underscores).
fn python_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") || !body.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let v: i64 = body.replace('_', "").parse().ok()?;
    Some(if neg { -v } else { v })
}

/// `base64.b64decode(s)` (binascii non-strict mode): non-alphabet characters
/// are skipped, decoding ends at the first complete padding; leftover data
/// bits are an error.
fn b64decode(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() {
        return None;
    }
    let mut out = Vec::new();
    let (mut quad, mut pads, mut acc, mut bits) = (0u32, 0u32, 0u32, 0u32);
    for c in s.bytes() {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Some(out);
                }
            }
            continue;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        pads = 0;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
        quad = (quad + 1) % 4;
    }
    if quad != 0 {
        return None;
    }
    Some(out)
}

/// `page_shapes.project_shapes(document)` (the dict the Python module builds).
pub fn project_shapes(document: &Val) -> R<Val> {
    let s = rs_ops::page_shapes::project_shapes(&PureDeps::default(), document as &dyn Any).map_err(fail_of_ops)?;
    let shape = |sh: &rs_ops::page_shapes::ShapeInfo| {
        Val::dict(vec![
            ("id", Val::opt_str(sh.id.as_deref())),
            ("kind", Val::s(sh.kind.as_str())),
            ("geometry", Val::List(sh.geometry.iter().map(|v| Val::Float(*v)).collect())),
            ("colorARGB", Val::Int(sh.color_argb as i64)),
            ("width", Val::Float(sh.width)),
            ("fillARGB", sh.fill_argb.map(|f| Val::Int(f as i64)).unwrap_or(Val::Null)),
        ])
    };
    let mut pages = Vec::new();
    for p in &s.pages {
        let mut d: Vec<(&str, Val)> =
            vec![("id", Val::opt_str(p.id.as_deref())), ("index", Val::Int(p.index as i64)), ("shapes", Val::List(p.shapes.iter().map(shape).collect()))];
        if let Some((w, h)) = p.size {
            d.push(("width", Val::Int(w)));
            d.push(("height", Val::Int(h)));
        }
        d.push(("addShape", Val::Bool(p.add_shape)));
        d.push(("reason", Val::opt_str(p.reason.as_deref())));
        pages.push(Val::dict(d));
    }
    Ok(Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        ("kinds", Val::List(rs_ops::page_shapes::KINDS.iter().map(|k| Val::s(*k)).collect())),
        ("pages", Val::List(pages)),
        ("reason", Val::opt_str(s.reason.as_deref())),
        ("rendering", Val::dict(vec![("nativeEquivalent", Val::Bool(false)), ("mode", Val::s("geometry-only"))])),
    ]))
}
