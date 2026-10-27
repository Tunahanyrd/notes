//! `rs_ops::deps::Deps` on the pure rs-doc / rs-ink cores (no Python).
//!
//! Mirrors `rs_ops::bridge::PyDeps` with pure argument types. The `&dyn Any`
//! arguments the rs-ops cores hand back to [`PureDeps`] are:
//!
//! * snapshots (`snapshot`, `convert`, `verified`, `resource_xml`):
//!   - [`Val`] — a stored snapshot dict; a `Val::Dict` goes through
//!     `DocumentSnapshot.from_dict` (`snapshot::from_dict`), any other `Val`
//!     is "not a DocumentSnapshot" (`snapshot()` → `Ok(None)`);
//!   - [`Snapshot`] (`rs_doc::doc::DocumentSnapshot`) — used as is.
//!
//!   The `handle` of every `deps::Snapshot` returned is a boxed [`Snapshot`].
//! * samples (`normalize_capture`):
//!   - `rs_ink::core::binary::RawSamples` (what `samples_from_points` builds),
//!   - `rs_ink::core::binary::Samples` or `rs_ops::deps::Samples`,
//!   - [`Val`] — a list of `{x, y, pressure, timeMs}` dicts
//!     (`ink_store.samples_from_points`).
//!
//! `verified` always runs the rs-ops core `ink_projection::verified_resource`
//! (there is no module attribute to patch). MIME parsing uses rs-doc with the
//! default (unpatched) `tools.notes_payload` limits.
use std::any::Any;
use std::cell::RefCell;

use rs_doc::mime::Limits;
use rs_ink::core as inkcore;
use rs_ink::core::num::Val as IVal;
use rs_ops::deps::*;
use rs_ops::err::{Error, Kind, R};
use rs_ops::xml::Element;
use rs_store::val::Val;

use crate::err::{Fail, Kind as FKind};
use crate::snapshot::{self, Snapshot};

/// `tools.notes_payload.MAX_READONLY_PARTS` (rs-ops util).
const MAX_READONLY_PARTS: i64 = rs_ops::util::MAX_READONLY_PARTS as i64;

// ---------------------------------------------------------------- errors

/// Fail → rs-ops error with the same Python class; the Fail itself rides in
/// `ext` so [`fail_of_ops`] restores it unchanged.
pub fn ops_err(f: Fail) -> Error {
    let kind = match f.kind {
        FKind::Ink => Kind::Ink,
        FKind::Document => Kind::Doc,
        FKind::PageOps => Kind::PageOps,
        FKind::Payload => Kind::Payload,
        FKind::XmlParse => Kind::Parse,
        FKind::Value | FKind::Store | FKind::PdfBackground | FKind::InkRender => Kind::Value,
        FKind::Unicode => Kind::Unicode,
        FKind::Type => Kind::Type,
        FKind::Key => Kind::Key,
        FKind::Overflow => Kind::Overflow,
        FKind::Attribute => Kind::Attribute,
        _ => Kind::Other,
    };
    let msg = f.message.clone();
    Error { kind, msg, ext: Some(Box::new(f)) }
}

/// rs-ops error → Fail (a Fail carried in `ext` is returned unchanged).
pub fn fail_of_ops(e: Error) -> Fail {
    if let Some(ext) = e.ext {
        if let Ok(f) = ext.downcast::<Fail>() {
            return *f;
        }
    }
    let kind = match e.kind {
        Kind::Ink => FKind::Ink,
        Kind::Doc => FKind::Document,
        Kind::PageOps => FKind::PageOps,
        Kind::Shape => FKind::Document,
        Kind::Payload => FKind::Payload,
        Kind::Parse => FKind::XmlParse,
        Kind::Value => FKind::Value,
        Kind::Type => FKind::Type,
        Kind::Overflow => FKind::Overflow,
        Kind::Key => FKind::Key,
        Kind::Unicode => FKind::Unicode,
        Kind::Attribute => FKind::Attribute,
        Kind::Stop | Kind::ZeroDiv | Kind::Other => FKind::Other,
    };
    Fail::new(kind, e.msg)
}

/// rs-doc error → rs-ops error (bridge.rs `from_doc`).
pub fn from_doc(e: rs_doc::Error) -> Error {
    use rs_doc::Error as D;
    match e {
        D::Payload(m) => Error::new(Kind::Payload, m),
        D::Document(m) => Error::new(Kind::Doc, m.to_string()),
        D::Value(m) => Error::new(Kind::Value, m.to_string()),
        D::Type(m) => Error::new(Kind::Type, m.to_string()),
        D::XmlParse(m) => Error::new(Kind::Parse, m),
        D::Io(e) => Error::new(Kind::Other, e.to_string()),
        D::External(_) => Error::new(Kind::Other, "external error"),
    }
}

/// rs-ink error → rs-ops error (bridge.rs `from_ink`).
pub fn from_ink(e: inkcore::InkErr) -> Error {
    use inkcore::InkErr as I;
    match e {
        I::Ink(m) => Error::new(Kind::Ink, m),
        I::Document(m) => Error::new(Kind::Doc, m),
        I::Payload(m) => Error::new(Kind::Payload, m),
        I::Struct(m) => Error::new(Kind::Other, m),
        I::KeyError(m) => Error::new(Kind::Key, m),
        I::ValueError(m) => Error::new(Kind::Value, m),
        I::TypeError(m) => Error::new(Kind::Type, m),
        I::Overflow(m) => Error::new(Kind::Overflow, m),
        I::Parse(m) => Error::new(Kind::Parse, m),
        I::Unicode(enc, _, start, _, reason) => Error::new(Kind::Unicode, format!("'{enc}' codec can't decode byte in position {start}: {reason}")),
        I::Foreign => Error::new(Kind::Other, "foreign error"),
    }
}

/// rs-ink error → Fail with the Python class `rs_ink::pyapi::ink_error` raises.
pub fn fail_of_ink(e: inkcore::InkErr) -> Fail {
    use inkcore::InkErr as I;
    match e {
        I::Ink(m) => Fail::new(FKind::Ink, m),
        I::Document(m) => Fail::new(FKind::Document, m),
        I::Payload(m) => Fail::new(FKind::Payload, m),
        // struct.error is a plain Exception.
        I::Struct(m) => Fail::new(FKind::Other, m),
        I::KeyError(m) => Fail::new(FKind::Key, m),
        I::ValueError(m) => Fail::new(FKind::Value, m),
        I::TypeError(m) => Fail::new(FKind::Type, m),
        I::Overflow(m) => Fail::new(FKind::Overflow, m),
        I::Parse(m) => Fail::new(FKind::XmlParse, m),
        I::Unicode(enc, data, start, _, reason) => {
            let byte = data.get(start).map(|b| format!("0x{b:02x}")).unwrap_or_default();
            Fail::new(FKind::Unicode, format!("'{enc}' codec can't decode byte {byte} in position {start}: {reason}"))
        }
        I::Foreign => Fail::new(FKind::Value, "foreign error lost"),
    }
}

fn ink_r<T>(r: inkcore::R<T>) -> R<T> {
    r.map_err(from_ink)
}

// ---------------------------------------------------------------- payload

/// `parse_payload(raw, ct, max_parts=MAX_READONLY_PARTS)` with the given limits.
pub fn payload(raw: &[u8], content_type: &str, limits: &Limits) -> R<Payload> {
    let p = rs_doc::payload::parse_payload(raw, content_type, Some(MAX_READONLY_PARTS), limits, &mut rs_doc::mime::NoHooks).map_err(from_doc)?;
    let body = |part: &rs_doc::mime::Part| raw[part.body.clone()].to_vec();
    Ok(Payload {
        locked: p.locked,
        xml_documents: p.xml_parts.iter().map(|i| body(&p.parts[*i])).collect(),
        parts: p
            .parts
            .iter()
            .map(|part| Part { name: part.name.clone(), filename: part.filename.clone(), content_type: part.content_type.clone(), body: body(part) })
            .collect(),
        handle: None,
    })
}

// ---------------------------------------------------------------- conversions (bridge.rs)

fn mask(b: &[u8]) -> u64 {
    b.iter().enumerate().take(8).fold(0u64, |a, (i, x)| a | (*x as u64) << (8 * i))
}

fn samples_of(s: &inkcore::binary::Samples) -> Samples {
    Samples {
        points: s.points.clone(),
        pressures: s.pressures.clone(),
        timestamps: s.timestamps.clone(),
        tilts: s.tilts.clone(),
        orientations: s.orientations.clone(),
    }
}

fn record_of(r: inkcore::binary::Record) -> Record {
    Record {
        raw: r.raw.clone(),
        frames: r
            .frames
            .iter()
            .map(|f| Frame {
                raw: f.raw.clone(),
                kind: f.kind,
                properties: mask(&f.properties),
                fields: mask(&f.fields),
                fixed_start: f.fixed_start.max(0) as usize,
                flexible_start: f.flexible_start.max(0) as usize,
            })
            .collect(),
        samples: samples_of(&r.samples),
        compressed: r.compressed,
        stylus: r.stylus,
        tool_type: r.tool_type as i64,
        handle: Some(Box::new(r)),
    }
}

fn ink_record(r: &Record) -> Option<&inkcore::binary::Record> {
    r.handle.as_ref().and_then(|h| h.downcast_ref::<inkcore::binary::Record>())
}

fn core_record(r: &Record) -> R<&inkcore::binary::Record> {
    ink_record(r).ok_or_else(|| Error::new(Kind::Ink, "validated stroke record required"))
}

fn style_of(s: &inkcore::binary::Style) -> Vec<(String, Num)> {
    s.0.iter()
        .filter_map(|(k, v)| match v {
            inkcore::binary::StyleVal::I(i) => Some((k.to_string(), Num::I(*i))),
            inkcore::binary::StyleVal::F(f) => Some((k.to_string(), Num::F(*f))),
            inkcore::binary::StyleVal::G(_) => None,
        })
        .collect()
}

fn to_tree(tree: &mut inkcore::xml::Tree, e: &Element, parent: Option<usize>) -> usize {
    let id = tree.add(&e.tag, e.attrib.clone(), parent);
    tree.nodes[id].text = e.text.clone();
    tree.nodes[id].tail = e.tail.clone();
    for c in &e.children {
        to_tree(tree, c, Some(id));
    }
    id
}

fn from_tree(tree: &inkcore::xml::Tree, id: usize) -> Element {
    let n = &tree.nodes[id];
    Element {
        tag: n.tag.clone(),
        attrib: n.attrs.clone(),
        text: n.text.clone(),
        tail: n.tail.clone(),
        children: n.children.iter().map(|c| from_tree(tree, *c)).collect(),
    }
}

/// A JSON number as rs-ink sees a Python object (`type(v) is int/float`).
pub fn ink_val(v: &Val) -> IVal {
    match v {
        Val::Int(i) => IVal::Int(*i as i128),
        Val::BigInt(s) => match s.trim().parse::<i128>() {
            Ok(i) => IVal::Int(i),
            Err(_) => IVal::Big(s.trim_start().starts_with('-')),
        },
        Val::Float(f) => IVal::Float(*f),
        _ => IVal::Bad,
    }
}

fn key_err(k: &str) -> Fail {
    Fail::key(format!("'{k}'"))
}

/// `p[k]` on a point dict (KeyError / TypeError like Python).
fn item<'a>(p: &'a Val, k: &str) -> crate::err::R<&'a Val> {
    match p {
        Val::Dict(_) => p.get(k).ok_or_else(|| key_err(k)),
        Val::List(_) | Val::Str(_) => Err(Fail::type_(format!("{} indices must be integers or slices, not str", p.type_name()))),
        other => Err(Fail::type_(format!("'{}' object is not subscriptable", other.type_name()))),
    }
}

/// `ink_store.samples_from_points(points)` as the rs-ink core reads the
/// resulting `StrokeSamples` (points, pressures, timestamps; no axes).
pub fn raw_samples_from_points(points: &Val) -> crate::err::R<inkcore::binary::RawSamples> {
    let items: Vec<Val> = match points {
        Val::List(items) => items.clone(),
        Val::Dict(items) => items.iter().map(|(k, _)| Val::s(k)).collect(),
        Val::Str(s) => s.chars().map(|c| Val::s(c.to_string())).collect(),
        other => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
    };
    let mut raw = inkcore::binary::RawSamples::default();
    // Python builds the three tuples in three passes, so the first failing key
    // of the first pass wins.
    for p in &items {
        raw.points.push(Some([ink_val(item(p, "x")?), ink_val(item(p, "y")?)]));
    }
    for p in &items {
        raw.pressures.push(ink_val(item(p, "pressure")?));
    }
    for p in &items {
        raw.timestamps.push(ink_val(item(p, "timeMs")?));
    }
    Ok(raw)
}

fn raw_samples_any(samples: &dyn Any) -> R<inkcore::binary::RawSamples> {
    if let Some(r) = samples.downcast_ref::<inkcore::binary::RawSamples>() {
        return Ok(r.clone());
    }
    if let Some(s) = samples.downcast_ref::<inkcore::binary::Samples>() {
        return Ok(inkcore::binary::RawSamples::from_samples(s));
    }
    if let Some(s) = samples.downcast_ref::<Samples>() {
        let points = s.points.iter().map(|(x, y)| Some([IVal::Float(*x), IVal::Float(*y)])).collect();
        return Ok(inkcore::binary::RawSamples {
            points,
            pressures: s.pressures.iter().map(|v| IVal::Float(*v)).collect(),
            timestamps: s.timestamps.iter().map(|v| IVal::Int(*v as i128)).collect(),
            tilts: s.tilts.iter().map(|v| IVal::Float(*v)).collect(),
            orientations: s.orientations.iter().map(|v| IVal::Float(*v)).collect(),
        });
    }
    if let Some(v) = samples.downcast_ref::<Val>() {
        return raw_samples_from_points(v).map_err(ops_err);
    }
    Err(Error::new(Kind::Attribute, "samples object has no attribute 'points'"))
}

// ---------------------------------------------------------------- snapshots

fn doc_snapshot(s: &Snapshot) -> rs_ops::deps::Snapshot {
    rs_ops::deps::Snapshot {
        raw_body: s.raw_body.clone(),
        content_type: s.content_type.clone(),
        resource_xml: s.resource_xml.clone(),
        local_id: s.local_id.clone(),
        commit_id: s.commit_id.clone(),
        handle: Some(Box::new(s.clone())),
    }
}

/// The pure `Deps`: rs-doc for MIME, rs-ink for strokes, no Python.
#[derive(Clone, Copy, Debug, Default)]
pub struct PureDeps {
    /// `tools.notes_payload` limits (defaults unless a caller overrides them).
    pub limits: Limits,
}

impl PureDeps {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a snapshot argument to an owned `DocumentSnapshot`
    /// (`Ok(None)`: not a DocumentSnapshot / dict).
    pub fn document(&self, arg: &dyn Any) -> R<Option<Snapshot>> {
        if let Some(s) = arg.downcast_ref::<Snapshot>() {
            return Ok(Some(s.clone()));
        }
        if let Some(v) = arg.downcast_ref::<Val>() {
            return match v {
                Val::Dict(_) => snapshot::from_dict(v).map(Some).map_err(ops_err),
                _ => Ok(None),
            };
        }
        if let Some(s) = arg.downcast_ref::<Box<Snapshot>>() {
            return Ok(Some((**s).clone()));
        }
        Ok(None)
    }
}

impl Deps for PureDeps {
    fn snapshot(&self, arg: &dyn Any) -> R<Option<rs_ops::deps::Snapshot>> {
        if let Some(s) = arg.downcast_ref::<Snapshot>() {
            return Ok(Some(doc_snapshot(s)));
        }
        Ok(self.document(arg)?.map(|s| doc_snapshot(&s)))
    }

    fn parse_payload(&self, raw: &[u8], content_type: &str) -> R<Payload> {
        payload(raw, content_type, &self.limits)
    }

    fn convert(&self, arg: &dyn Any, _module: &str) -> R<Handle> {
        if let Some(v) = arg.downcast_ref::<Val>() {
            return match v {
                Val::Dict(_) => Ok(Box::new(snapshot::from_dict(v).map_err(ops_err)?)),
                other => Ok(Box::new(other.clone())),
            };
        }
        if let Some(s) = arg.downcast_ref::<Snapshot>() {
            return Ok(Box::new(s.clone()));
        }
        Err(Error::new(Kind::Type, "unsupported snapshot argument"))
    }

    fn verified(&self, snapshot: &dyn Any) -> R<Option<String>> {
        match rs_ops::ink_projection::verified_resource(self, snapshot)? {
            rs_ops::ink_projection::Verified::Reason(r) => Ok(Some(r.to_string())),
            _ => Ok(None),
        }
    }

    fn resource_xml(&self, snapshot: &dyn Any) -> R<Vec<u8>> {
        if let Some(s) = snapshot.downcast_ref::<Snapshot>() {
            return Ok(s.resource_xml.clone());
        }
        let name = snapshot.downcast_ref::<Val>().map(|v| v.type_name()).unwrap_or("object");
        Err(Error::new(Kind::Attribute, format!("'{name}' object has no attribute 'resource_xml'")))
    }

    fn decode_stroke(&self, body: &[u8]) -> R<Record> {
        ink_r(inkcore::binary::decode_stroke_resource(body)).map(record_of)
    }

    fn bind(&self, record: &Record, object: &Element, strings: Option<&Element>) -> R<Binding> {
        let core = core_record(record)?;
        let mut tree = inkcore::xml::Tree::default();
        let obj = to_tree(&mut tree, object, None);
        let table = strings.map(|s| to_tree(&mut tree, s, None));
        let b = ink_r(inkcore::prepare::bind_stroke_metadata(&core.frames, (&tree, obj), table.map(|t| (&tree, t))))?;
        Ok(Binding {
            object_id: b.object_id,
            modified_time: b.modified_time,
            bounds: b.bounds,
            pen_name: b.pen_name,
            advanced_settings: b.advanced_settings,
            style: style_of(&b.style),
            handle: None,
        })
    }

    fn coordinate_scale(&self, record: &Record, width: i64, height: i64) -> R<f64> {
        let core = core_record(record)?;
        ink_r(inkcore::coords::page_coordinate_scale(&core.frames[0], IVal::Int(width as i128), IVal::Int(height as i128), IVal::Int(0)))
    }

    fn owner_extent(&self, record: &Record) -> R<(i64, i64)> {
        ink_r(inkcore::coords::stroke_owner_extent(&core_record(record)?.frames[0]))
    }

    fn read_style(&self, record: &Record) -> R<Vec<(String, Num)>> {
        ink_r(inkcore::binary::read_stroke_style(&core_record(record)?.frames[1])).map(|s| style_of(&s))
    }

    fn decode_point_channels(&self, data: &[u8], count: usize, compressed: bool, stylus: bool) -> R<Samples> {
        ink_r(inkcore::binary::decode_point_channels(data, count, compressed, stylus)).map(|s| samples_of(&s))
    }

    fn release_page_cache(&self, root: &mut Element, page_id: &str, tick: i64) -> R<Vec<String>> {
        let mut tree = inkcore::xml::Tree::default();
        let r = to_tree(&mut tree, root, None);
        tree.root = r;
        let page = tree.nodes[r]
            .children
            .iter()
            .find(|c| rs_ops::xml::local_name(&tree.nodes[**c].tag) == "pagelist")
            .and_then(|pl| tree.nodes[*pl].children.iter().find(|p| tree.get(**p, "id") == Some(page_id)).copied());
        let result = inkcore::cache::release_page_cache(&mut tree, page, tick as i128);
        *root = from_tree(&tree, r);
        ink_r(result)
    }

    fn append_probe(&self, snapshot: &rs_ops::deps::Snapshot, source: &str, new_id: &str, x: f64, y: f64, now_ms: i64) -> R<()> {
        let raw = inkcore::binary::RawSamples {
            points: vec![Some([IVal::Float(x), IVal::Float(y)])],
            pressures: vec![IVal::Float(0.5)],
            timestamps: vec![IVal::Int(0)],
            tilts: vec![],
            orientations: vec![],
        };
        let capture = ink_r(inkcore::capture::normalize_capture(&raw, false))?;
        let foreign: RefCell<Option<Error>> = RefCell::new(None);
        let parse = |body: &[u8], ct: &str| -> inkcore::R<Vec<inkcore::prepare::PartIn>> {
            let parsed = payload(body, ct, &self.limits).map_err(|e| {
                *foreign.borrow_mut() = Some(e);
                inkcore::InkErr::Foreign
            })?;
            Ok(parsed
                .parts
                .into_iter()
                .map(|p| inkcore::prepare::PartIn { name: p.name, filename: p.filename, content_type: p.content_type, body: p.body })
                .collect())
        };
        let snap = inkcore::prepare::SnapshotIn { resource_xml: &snapshot.resource_xml, raw_body: &snapshot.raw_body, content_type: &snapshot.content_type };
        if snapshot.commit_id.as_deref().is_none_or(|c| c.is_empty()) {
            return Err(Error::new(Kind::Ink, "committed snapshot required"));
        }
        match inkcore::prepare::prepare_ink_document(
            &snap,
            Some(source),
            Some(new_id),
            inkcore::prepare::Geometry::Capture(&capture),
            IVal::Int(now_ms as i128),
            2,
            None,
            &parse,
        ) {
            Ok(_) => Ok(()),
            Err(inkcore::InkErr::Foreign) => Err(foreign.borrow_mut().take().unwrap_or_else(|| Error::new(Kind::Other, "foreign error"))),
            Err(e) => Err(from_ink(e)),
        }
    }

    fn first_stroke_probe(&self, snapshot: &rs_ops::deps::Snapshot, page_id: &str, x: f64, y: f64, now_ms: i64) -> R<()> {
        // Discarded `prepare_first_stroke` dry run (one centre sample, black, size 1),
        // like rs_ops::bridge::PyDeps::first_stroke_probe.
        let samples = inkcore::binary::RawSamples {
            points: vec![Some([IVal::Float(x), IVal::Float(y)])],
            pressures: vec![IVal::Float(0.5)],
            timestamps: vec![IVal::Int(0)],
            tilts: vec![],
            orientations: vec![],
        };
        let handle: &dyn Any = match snapshot.handle.as_deref() {
            Some(h) => h,
            None => return Err(Error::new(Kind::Ink, "snapshot required")),
        };
        rs_ops::ink_first_stroke::prepare_first_stroke(
            self,
            handle,
            Some(page_id),
            Some(&samples as &dyn Any),
            Some(0xff00_0000),
            Some(1.0),
            Some(now_ms as i128),
            None,
        )
        .map(|_| ())
    }

    fn highlighter_probe(&self, snapshot: &rs_ops::deps::Snapshot, page_id: &str, x: f64, y: f64, now_ms: i64) -> R<()> {
        // Discarded `prepare_highlighter_stroke` dry run (one centre sample,
        // DEFAULT_COLOR, size 1), like rs_ops::bridge::PyDeps::highlighter_probe.
        let samples = inkcore::binary::RawSamples {
            points: vec![Some([IVal::Float(x), IVal::Float(y)])],
            pressures: vec![IVal::Float(0.5)],
            timestamps: vec![IVal::Int(0)],
            tilts: vec![],
            orientations: vec![],
        };
        let handle: &dyn Any = match snapshot.handle.as_deref() {
            Some(h) => h,
            None => return Err(Error::new(Kind::Ink, "snapshot required")),
        };
        rs_ops::ink_highlighter::prepare_highlighter_stroke(
            self,
            handle,
            Some(page_id),
            Some(&samples as &dyn Any),
            Some(rs_ops::ink_highlighter::DEFAULT_COLOR as i128),
            Some(1.0),
            Some(now_ms as i128),
            None,
        )
        .map(|_| ())
    }

    fn basic_pens(&self) -> R<Vec<String>> {
        Ok(inkcore::capture::BASIC_PENS.iter().map(|s| s.to_string()).collect())
    }

    fn normalize_capture(&self, samples: &dyn Any) -> R<Capture> {
        let raw = raw_samples_any(samples)?;
        let c = ink_r(inkcore::capture::normalize_capture(&raw, false))?;
        Ok(Capture { samples: samples_of(&c.samples), channels: c.channels, stylus: c.stylus, handle: None })
    }

    fn drawn_bounds(&self, samples: &Samples, pen_name: &str, pen_size: f64) -> R<[f64; 4]> {
        let points: Vec<Option<[IVal; 2]>> = samples.points.iter().map(|(x, y)| Some([IVal::Float(*x), IVal::Float(*y)])).collect();
        ink_r(inkcore::capture::basic_pen_drawn_bounds(Some(&points), Some(pen_name), IVal::Float(pen_size), IVal::Int(0)))
    }
}
