//! Replacement for the Python module `ink_projection`: bounded read-only
//! geometry projection and the conservative append/erase capability gate.
use crate::bridge;
#[cfg(feature = "python")]
use crate::bridge::PyDeps;
use crate::bridge::RPy;
use crate::deps::*;
use crate::err::*;
use crate::ink_erase;
use crate::util::*;
use crate::xml::{self, Element};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyList, PyTuple};
use std::any::Any;
use std::collections::{BTreeSet, HashMap, HashSet};

pub const PROJECTION_RECIPE: &str = "ink-projection-v7:carried-objects";
pub const MAX_PROJECTION_PAGES: usize = 2048;
pub const MAX_PROJECTION_STROKES: usize = 20_000;
pub const MAX_PROJECTION_POINTS: usize = 5_000_000;
pub const MAX_GEOMETRY_POINTS: f64 = 96.0;

const LOCKED: &str = "Kilitli veya şifreli notlarda kalem düzenleme desteklenmiyor.";
const EXTENDED: &str = "Bu notun genişletilmiş kalem biçimi henüz desteklenmiyor.";
const UNSAFE: &str = "Bu notun kalem biçimi güvenli biçimde açılamıyor.";
const NO_APPEND: &str = "Bu notun çizim verisi gösterilebilir; bu biçimde kalem ekleme henüz desteklenmiyor.";
const PAGE_BACKGROUND: &str = "Bu sayfada arka plan resmi var; kalem henüz desteklenmiyor.";
const PAGE_CONTENT: &str = "Bu sayfada desteklenmeyen içerik var; kalem henüz desteklenmiyor.";
const PAGE_PDF: &str = "Bu sayfanın PDF arka planı tanınmıyor; kalem henüz desteklenmiyor.";
const PAGE_LOCKED: &str = "Bu sayfanın katmanı kilitli; kalem kullanılamıyor.";
const PAGE_NO_TEMPLATE: &str = "Bu sayfadaki çizgilerin kalem türü henüz kopyalanamıyor; yeni çizgi eklenemiyor.";
const PAGE_LAYERS: &str = "Bu sayfanın katman yapısında ilk çizgi henüz desteklenmiyor.";

pub struct StrokeDto {
    pub id: String,
    pub template: bool,
    pub kind: &'static str,
    pub color: i64,
    pub size: f64,
    pub layer_id: String,
    pub erasable: bool,
    pub bounds: Option<[f64; 4]>,
    pub points: Option<Vec<(f64, f64, f64, i64)>>,
}

/// Per-page pen/eraser/first-stroke availability and its Turkish reason.
#[derive(Default)]
pub struct PageInk {
    pub append_ink: bool,
    pub erase_ink: bool,
    pub first_stroke: bool,
    pub highlighter: bool,
    pub reason: Option<String>,
}

pub struct PageDto {
    pub id: String,
    pub index: usize,
    pub width: i64,
    pub height: i64,
    pub pdf: bool,
    pub strokes: Vec<StrokeDto>,
    pub ink: PageInk,
}

pub struct Projection {
    pub pages: Vec<PageDto>,
    pub append_ink: bool,
    pub erase_ink: bool,
    pub first_stroke: bool,
    pub highlighter: bool,
    pub reason: Option<String>,
}

fn disabled(reason: &str) -> Projection {
    Projection { pages: vec![], append_ink: false, erase_ink: false, first_stroke: false, highlighter: false, reason: Some(reason.into()) }
}

/// ink_projection._note_ink_reason: whole-note refusals (media, groups,
/// shapes/text boxes, floating objects stay fail-closed).
pub fn note_ink_reason(root: &Element) -> Option<&'static str> {
    let tags: HashSet<&str> = root.iter().map(|n| n.tag.as_str()).collect();
    if tags.contains("voiceDataList") || tags.contains("attachedFileMap") {
        return Some("Bu notta ses kaydı veya ekli dosya var; kalem henüz desteklenmiyor.");
    }
    if tags.contains("strokeGroup") {
        return Some("Bu notta gruplanmış çizgiler var; kalem henüz desteklenmiyor.");
    }
    // Self-contained images, shapes, lines and text boxes on page layers are
    // carried through by every ink writer (util::passthrough_object); anything
    // else (nested/grouped objects, cache-referencing or unknown objects)
    // keeps the note fail-closed.
    let foreign: Vec<&Element> = root
        .children
        .iter()
        .filter(|c| c.tag == "pageList")
        .flat_map(|pages| pages.iter_tag("object"))
        .filter(|o| o.get("type") != Some("stroke"))
        .filter(|o| passthrough_object(o).is_none())
        .collect();
    if foreign.iter().any(|o| o.get("type") == Some("image")) || tags.contains("image") {
        return Some("Bu notta resim var; kalem henüz desteklenmiyor.");
    }
    if !foreign.is_empty() || tags.contains("customObject") || tags.contains("shape") {
        return Some("Bu notta şekil veya metin kutusu gibi nesneler var; kalem henüz desteklenmiyor.");
    }
    if root.children.iter().any(|n| n.tag == "floatingObjectList" && !n.is_empty()) {
        return Some("Bu notta serbest yerleşimli nesne var; kalem henüz desteklenmiyor.");
    }
    None
}

/// ink_projection._page_ink_reason: per-page refusal from page metadata.
pub fn page_ink_reason(page: &Element) -> R<Option<&'static str>> {
    if page.get_or("bgImageId", "-1") != "-1" || !page.get_or("templateUri", "").is_empty() {
        return Ok(Some(PAGE_BACKGROUND));
    }
    if page.children.iter().any(|n| !matches!(n.tag.as_str(), "layerList" | "drawnRect" | "canvasCacheDataMap" | "pdfDataList")) {
        return Ok(Some(PAGE_CONTENT));
    }
    if one(page, "layerlist")?.children.iter().any(|layer| layer.children.iter().any(|n| n.tag != "objectList")) {
        return Ok(Some(PAGE_CONTENT));
    }
    match bridge::pdf_background(page) {
        Ok(_) => Ok(None),
        Err(e) if e.is(Kind::Ink) => Ok(Some(PAGE_PDF)),
        Err(e) => Err(e),
    }
}

fn uuid_(v: Option<&str>) -> R<String> {
    match v {
        Some(s) if canonical_uuid(s) => Ok(s.into()),
        _ => ink("invalid projection identity"),
    }
}

fn digest_(v: Option<&str>) -> R<[u8; 32]> {
    match v {
        Some(s) if s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) => hash32(Some(s)),
        _ => ink("invalid projection digest"),
    }
}

pub enum Verified {
    Reason(&'static str),
    Ok(Payload, Element),
}

/// `verified_resource` on a caller argument (dict/DocumentSnapshot).
pub fn verified_resource(deps: &dyn Deps, snapshot: &dyn Any) -> R<Verified> {
    match deps.snapshot(snapshot)? {
        Some(s) => verify(deps, &s),
        None => ink("snapshot required"),
    }
}

/// Parsed payload and its single noteResource bound to the snapshot.
pub fn verify(deps: &dyn Deps, snap: &Snapshot) -> R<Verified> {
    let parsed = deps.parse_payload(&snap.raw_body, &snap.content_type)?;
    if parsed.locked {
        return Ok(Verified::Reason(LOCKED));
    }
    let mut roots = Vec::new();
    for raw in &parsed.xml_documents {
        roots.push(xml::parse_bytes(raw).map_err(|_| Error::new(Kind::Payload, "malformed XML"))?);
    }
    let mut resources: Vec<&Element> = roots.iter().flat_map(|r| r.iter()).filter(|n| n.local() == "noteresource").collect();
    if resources.len() != 1 {
        return ink("single projection resource required");
    }
    let root = resources.pop().unwrap();
    if root.iter().any(|n| n.tag.contains('}')) {
        return Ok(Verified::Reason(EXTENDED));
    }
    if xml::xml_bytes(root) != snap.resource_xml || root.get("id") != snap.local_id.as_deref() {
        return ink("snapshot raw XML identity mismatch");
    }
    uuid_(snap.local_id.as_deref())?;
    let commits: Vec<&Element> = roots.iter().flat_map(|r| r.iter()).filter(|n| n.local() == "commitid").collect();
    if commits.len() != 1 || normalize_commit(commits[0].text.as_deref())? != normalize_commit(snap.commit_id.as_deref())? {
        return ink("snapshot commit mismatch");
    }
    if root.iter().any(locked_flags) {
        return Ok(Verified::Reason(LOCKED));
    }
    let root = root.clone();
    Ok(Verified::Ok(parsed, root))
}

fn objectlist(e: &Element) -> R<Vec<&Element>> {
    if e.children.iter().any(|n| n.tag.to_lowercase() == "objectlist") {
        Ok(one(e, "objectlist")?.children.iter().collect())
    } else {
        Ok(vec![])
    }
}

fn project(deps: &dyn Deps, snapshot: &dyn Any, include_points: bool) -> R<Projection> {
    let snap = match deps.snapshot(snapshot)? {
        Some(s) => s,
        None => return ink("snapshot required"),
    };
    let (parsed, root) = match verify(deps, &snap)? {
        Verified::Reason(r) => return Ok(disabled(r)),
        Verified::Ok(p, r) => (p, r),
    };
    let attrs = one(&root, "attributes")?;
    let tree = one(&root, "notetree")?;
    let pages = one(&root, "pagelist")?;
    let tree_pages = one(tree, "pagelist")?;
    if attrs.get("formatVersion") != Some("4000")
        || tree.get("ver") != Some("4000")
        || tree.get_or("isStrokeGrouped", "false").to_lowercase() != "false"
    {
        return Ok(disabled("Bu notun kalem biçimi henüz desteklenmiyor."));
    }
    if !(1..=MAX_PROJECTION_PAGES).contains(&pages.len()) || ids(&pages.children) != ids(&tree_pages.children) {
        return ink("projection page/tree inventory mismatch");
    }
    if let Some(reason) = note_ink_reason(&root) {
        return Ok(disabled(reason));
    }
    let (objects, _) = ink_objects(pages).remap(&[Kind::Ink], Kind::Ink, "mixed object inventory")?;
    let parts: Vec<&Part> = parsed.parts.iter().filter(|p| p.name.as_deref() == Some("strokeResource")).collect();
    // A note without strokes (e.g. a fresh PDF book) still lists its pages.
    if !(parts.len() == objects.len() && objects.len() <= MAX_PROJECTION_STROKES) {
        return Ok(disabled("Bu not için desteklenen bir kalem şablonu bulunamadı."));
    }
    let wanted: BTreeSet<&str> = ["id", "type", "hash", "modifiedTime"].into_iter().collect();
    if objects.iter().any(|o| o.tag != "object" || o.get("type") != Some("stroke") || !o.is_empty() || o.attr_key_set() != wanted) {
        return ink("mixed object inventory");
    }
    let object_ids: Vec<String> = objects.iter().map(|o| uuid_(o.get("id"))).collect::<R<_>>()?;
    if object_ids.iter().collect::<HashSet<_>>().len() != object_ids.len() {
        return ink("duplicate object identity");
    }
    let by_id: HashMap<&str, &Element> = objects.iter().map(|o| (o.get("id").unwrap(), *o)).collect();
    let basic = deps.basic_pens()?;
    let strings = attrs.find("stringIdList");
    let mut records: HashMap<String, (Record, Binding, bool)> = HashMap::new();
    let mut point_count = 0usize;
    for part in &parts {
        let record = deps.decode_stroke(&part.body)?;
        let identity = record.identity()?;
        if records.contains_key(&identity) || !by_id.contains_key(identity.as_str()) {
            return ink("binary/XML inventory mismatch");
        }
        let binding = deps.bind(&record, by_id[identity.as_str()], strings)?;
        if part.filename.as_deref() != Some(format!("{}_{}", identity, binding.modified_time).as_str()) {
            return ink("binary filename differs");
        }
        let size = binding.style_get("penSize");
        let color = binding.style_get("colorARGB");
        let n = record.samples.points.len();
        point_count += n;
        if n < 1 || point_count > MAX_PROJECTION_POINTS {
            return ink("projection point budget");
        }
        let s = &record.samples;
        let template = basic.contains(&binding.pen_name)
            && record.frames.len() == 2
            && record.frames[0].fields == 0x6000
            && record.frames[1].properties & !(1 | 4 | 16 | 32 | 256 | 1024 | 2048) == 0
            && size.is_some_and(|v| {
                let f = v.as_f64();
                f.is_finite() && 0.0 < f && f <= 256.0
            })
            && matches!(color, Some(Num::I(c)) if (0..=0xffff_ffff).contains(&c))
            && s.pressures.iter().all(|p| (0.0..=1.0).contains(p))
            && s.timestamps.iter().all(|t| *t >= 0);
        records.insert(identity, (record, binding, template));
    }
    let mut result = Vec::new();
    let mut seen: HashSet<String> = object_ids.iter().cloned().collect();
    if let Some(l) = &snap.local_id {
        seen.insert(l.clone());
    }
    let mut append_source: Option<(String, i64, i64)> = None;
    let mut erase_probe: Option<(String, String, String)> = None;
    // (page_reason, locked, strokes, objects, templates, erasable, first_shape, category, marker_shape, marker_category)
    #[allow(clippy::type_complexity)]
    let mut facts: Vec<(Option<&'static str>, bool, usize, usize, usize, usize, bool, (bool, bool, bool), bool, (bool, bool, bool, bool))> =
        Vec::new();
    let mut first_probes: Vec<((bool, bool, bool), (String, i64, i64))> = Vec::new();
    let mut marker_probes: Vec<((bool, bool, bool, bool), (String, i64, i64))> = Vec::new();
    for (index, (page, tp)) in pages.children.iter().zip(tree_pages.children.iter()).enumerate() {
        let pid = uuid_(page.get("id"))?;
        let width = integer(page.get("width"))?;
        let height = integer(page.get("height"))?;
        if seen.contains(&pid) || !(1..=32768).contains(&width) || !(1..=32768).contains(&height) {
            return ink("invalid projection page");
        }
        seen.insert(pid.clone());
        if page.get_or("orientation", "0") != "0" {
            return Ok(disabled("Bu sayfanın dönüşümü için özgün sayfa görüntüsünü kullanın."));
        }
        let layers = one(page, "layerlist")?;
        let tlayers = one(tp, "layerlist")?;
        if !(1..=64).contains(&layers.len()) || ids(&layers.children) != ids(&tlayers.children) {
            return ink("layer/tree mismatch");
        }
        let page_reason = page_ink_reason(page)?;
        let page_objects = page.iter_tag("object").count();
        let mut strokes = Vec::new();
        let (mut templates, mut erasable_count, mut locked) = (0usize, 0usize, false);
        for (layer, tl) in layers.children.iter().zip(tlayers.children.iter()) {
            let lid = uuid_(layer.get("id"))?;
            if seen.contains(&lid) {
                return ink("duplicate layer identity");
            }
            seen.insert(lid.clone());
            let layer_open = layer.get_or("lockState", "false").to_lowercase() == "false" && layer.get_or("thumbnail", "-1") == "-1";
            if !layer_open {
                locked = true;
            }
            let obj = objectlist(layer)?;
            let tobj = objectlist(tl)?;
            let left: Vec<_> = obj.iter().map(|o| (o.get("id"), o.get("modifiedTime"), o.get("hash"))).collect();
            let right: Vec<_> = tobj.iter().map(|o| (o.get("id"), o.get("t"), o.get("h"))).collect();
            if left != right {
                return ink("stroke/tree mismatch");
            }
            let tick = integer(layer.get("modifiedTime"))?;
            let children: Vec<[u8; 32]> = obj.iter().map(|o| digest_(o.get("hash"))).collect::<R<_>>()?;
            let digest = hex(&parent_hash(Some(&lid), tick, &children)?);
            if Some(digest.as_str()) != layer.get("hash") || Some(digest.as_str()) != tl.get("h") || Some(tick.to_string().as_str()) != tl.get("t") {
                return ink("layer/tree digest mismatch");
            }
            for o in obj {
                if o.get("type") != Some("stroke") {
                    continue; // carried image/shape/line/text box (never edited)
                }
                let key = o.get("id").unwrap_or("");
                let (record, binding, template) = records.get(key).ok_or_else(|| Error::new(Kind::Key, format!("'{key}'")))?;
                let mut template = *template;
                let scale = match deps.coordinate_scale(record, width, height) {
                    Ok(s) => Some(s),
                    Err(e) if e.is(Kind::Ink) => {
                        template = false;
                        None
                    }
                    Err(e) => return Err(e),
                };
                let scale = scale.filter(|s| *s != 0.0);
                let xy: Vec<(f64, f64)> = match scale {
                    Some(sc) => record.samples.points.iter().map(|(x, y)| Ok((f32r(x * sc)?, f32r(y * sc)?))).collect::<R<_>>()?,
                    None => vec![],
                };
                if xy.iter().any(|(x, y)| !(0.0 <= *x && *x <= width as f64) || !(0.0 <= *y && *y <= height as f64)) {
                    template = false;
                }
                let erasable = layer_open && page_reason.is_none() && ink_erase::stroke_erasable(record);
                erasable_count += erasable as usize;
                let bounds = match scale {
                    Some(sc) => {
                        let b = binding.bounds;
                        Some([f32r(b[0] * sc)?, f32r(b[1] * sc)?, f32r(b[2] * sc)?, f32r(b[3] * sc)?])
                    }
                    None => None,
                };
                if erasable && erase_probe.is_none() && page_objects >= 2 {
                    erase_probe = Some((pid.clone(), lid.clone(), binding.object_id.clone()));
                }
                if !template {
                    // Marker4 top-layer stroke: listed with its colour/size, never a pen template.
                    let size = binding.style_get("penSize").filter(|v| {
                        let f = v.as_f64();
                        f.is_finite() && 0.0 < f && f <= 256.0
                    });
                    let color = match binding.style_get("colorARGB") {
                        Some(Num::I(c)) if (0..=0xffff_ffff).contains(&c) => Some(c),
                        _ => None,
                    };
                    if let (Some(sc), true, Some(size), Some(color)) =
                        (scale, crate::ink_highlighter::is_highlighter(binding, record), size, color)
                    {
                        let size = f32r(size.as_f64() * sc)?;
                        let s = &record.samples;
                        let points = include_points.then(|| {
                            xy.iter()
                                .zip(s.pressures.iter())
                                .zip(s.timestamps.iter())
                                .map(|(((x, y), p), t)| (*x, *y, *p, *t))
                                .collect()
                        });
                        strokes.push(StrokeDto {
                            id: binding.object_id.clone(),
                            template: false,
                            kind: "highlighter",
                            color,
                            size,
                            layer_id: lid.clone(),
                            erasable,
                            bounds,
                            points,
                        });
                        continue;
                    }
                    strokes.push(StrokeDto {
                        id: binding.object_id.clone(),
                        template: false,
                        kind: "other",
                        color: 0,
                        size: 0.0,
                        layer_id: lid.clone(),
                        erasable,
                        bounds,
                        points: None,
                    });
                    continue;
                }
                let sc = scale.unwrap();
                let color = match binding.style_get("colorARGB") {
                    Some(Num::I(c)) => c,
                    _ => 0,
                };
                let size = f32r(binding.style_get("penSize").map(|v| v.as_f64()).unwrap_or(0.0) * sc)?;
                let s = &record.samples;
                let points = if include_points {
                    Some(
                        xy.iter()
                            .zip(s.pressures.iter())
                            .zip(s.timestamps.iter())
                            .map(|(((x, y), p), t)| (*x, *y, *p, *t))
                            .collect(),
                    )
                } else {
                    None
                };
                strokes.push(StrokeDto {
                    id: binding.object_id.clone(),
                    template: true,
                    kind: if binding.pen_name.ends_with(".FountainPen") { "fountain" } else { "pen" },
                    color,
                    size,
                    layer_id: lid.clone(),
                    erasable,
                    bounds,
                    points,
                });
                if layer_open && page_reason.is_none() {
                    templates += 1;
                    if append_source.is_none() {
                        append_source = Some((binding.object_id.clone(), width, height));
                    }
                }
            }
        }
        let tick = integer(page.get("modifiedTime"))?;
        let children: Vec<[u8; 32]> = layers.children.iter().map(|l| digest_(l.get("hash"))).collect::<R<_>>()?;
        let digest = hex(&parent_hash(Some(&pid), tick, &children)?);
        if Some(digest.as_str()) != page.get("hash") || Some(digest.as_str()) != tp.get("h") || Some(tick.to_string().as_str()) != tp.get("t") {
            return ink("page/tree digest mismatch");
        }
        // A drawn rectangle exists exactly when the page has objects (strokes
        // or carried images/shapes/lines/text boxes).
        let first_shape = strokes.is_empty()
            && page_reason.is_none()
            && layers.len() == 1
            && !locked
            && layers.children[0].get_or("visible", "true").to_lowercase() == "true"
            && page.find("drawnRect").is_some() == (page_objects > 0)
            && layers.children[0].children.iter().all(|n| n.tag == "objectList")
            && layers.children[0].len() <= 1;
        let category = (page.find("pdfDataList").is_some(), page.find("canvasCacheDataMap").is_some(), page_objects > strokes.len());
        if first_shape && !first_probes.iter().any(|(c, _)| *c == category) {
            first_probes.push((category, (pid.clone(), width, height)));
        }
        // Page shape the highlighter writer accepts (one open layer; a drawn
        // rectangle exactly when strokes exist); one dry run per category.
        let marker_shape = page_reason.is_none()
            && layers.len() == 1
            && !locked
            && layers.children[0].get_or("visible", "true").to_lowercase() == "true"
            && layers.children[0].children.iter().all(|n| n.tag == "objectList")
            && layers.children[0].len() <= 1
            && page.find("drawnRect").is_some() == (page_objects > 0);
        let marker_category = (!strokes.is_empty(), category.0, category.1, category.2);
        if marker_shape && !marker_probes.iter().any(|(c, _)| *c == marker_category) {
            marker_probes.push((marker_category, (pid.clone(), width, height)));
        }
        facts.push((page_reason, locked, strokes.len(), page_objects, templates, erasable_count, first_shape, category, marker_shape, marker_category));
        result.push(PageDto { id: pid, index, width, height, pdf: category.0, strokes, ink: PageInk::default() });
    }
    let probe_errors = [Kind::Ink, Kind::Doc, Kind::Value, Kind::Type, Kind::Overflow];
    let mut allowed = false;
    if let Some((identity, width, height)) = &append_source {
        let attempt = (|| -> R<()> {
            let now = integer(root.get("syncModifiedTime"))? as i128 + 1;
            let now = i64::try_from(now).map_err(|_| Error::new(Kind::Overflow, "now_ms overflow"))?;
            deps.append_probe(&snap, identity, &uuid4(), *width as f64 / 2.0, *height as f64 / 2.0, now)
        })();
        match attempt {
            Ok(()) => allowed = true,
            Err(e) if e.any(&probe_errors) => {}
            Err(e) => return Err(e),
        }
    }
    let mut erase = false;
    if let Some((pid, lid, oid)) = &erase_probe {
        let attempt = (|| -> R<()> {
            let now = integer(root.get("syncModifiedTime"))? as i128 + 1;
            let handle: &dyn Any = match snap.handle.as_deref() {
                Some(h) => h,
                None => &snap,
            };
            ink_erase::prepare_ink_delete(deps, handle, Some(pid), Some(lid), &Some(vec![Some(oid.clone())]), Some(now))?;
            Ok(())
        })();
        match attempt {
            Ok(()) => erase = true,
            Err(e) if e.any(&probe_errors) => {}
            Err(e) => return Err(e),
        }
    }
    let mut first_ok: Vec<((bool, bool, bool), bool)> = Vec::new();
    for (category, (pid, width, height)) in &first_probes {
        let attempt = (|| -> R<()> {
            let now = integer(root.get("syncModifiedTime"))? as i128 + 1;
            let now = i64::try_from(now).map_err(|_| Error::new(Kind::Overflow, "now_ms overflow"))?;
            deps.first_stroke_probe(&snap, pid, *width as f64 / 2.0, *height as f64 / 2.0, now)
        })();
        match attempt {
            Ok(()) => first_ok.push((*category, true)),
            Err(e) if e.any(&probe_errors) => first_ok.push((*category, false)),
            Err(e) => return Err(e),
        }
    }
    let mut marker_ok: Vec<((bool, bool, bool, bool), bool)> = Vec::new();
    for (category, (pid, width, height)) in &marker_probes {
        let attempt = (|| -> R<()> {
            let now = integer(root.get("syncModifiedTime"))? as i128 + 1;
            let now = i64::try_from(now).map_err(|_| Error::new(Kind::Overflow, "now_ms overflow"))?;
            deps.highlighter_probe(&snap, pid, *width as f64 / 2.0, *height as f64 / 2.0, now)
        })();
        match attempt {
            Ok(()) => marker_ok.push((*category, true)),
            Err(e) if e.any(&probe_errors) => marker_ok.push((*category, false)),
            Err(e) => return Err(e),
        }
    }
    let mut any_first = false;
    let mut any_marker = false;
    for (page, (page_reason, locked, count, object_count, templates, erasable_count, first_shape, category, marker_shape, marker_category)) in
        result.iter_mut().zip(facts)
    {
        let page_append = allowed && templates > 0;
        // The delete writer keeps at least one object on the page; a carried
        // image/shape/line/text box counts.
        let page_erase = erase && object_count >= 2 && erasable_count > 0;
        let page_first = first_shape && first_ok.iter().any(|(c, ok)| *c == category && *ok);
        let page_marker = marker_shape && marker_ok.iter().any(|(c, ok)| *c == marker_category && *ok);
        any_first |= page_first;
        any_marker |= page_marker;
        if !page_erase {
            for s in page.strokes.iter_mut() {
                s.erasable = false;
            }
        }
        let reason = if page_append || page_erase || page_first || page_marker {
            None
        } else if let Some(r) = page_reason {
            Some(r)
        } else if locked {
            Some(PAGE_LOCKED)
        } else if count > 0 && templates == 0 {
            Some(PAGE_NO_TEMPLATE)
        } else if count == 0 && !first_shape {
            Some(PAGE_LAYERS)
        } else {
            Some(NO_APPEND)
        };
        page.ink = PageInk {
            append_ink: page_append,
            erase_ink: page_erase,
            first_stroke: page_first,
            highlighter: page_marker,
            reason: reason.map(str::to_string),
        };
    }
    Ok(Projection {
        pages: result,
        append_ink: allowed,
        erase_ink: erase,
        first_stroke: any_first,
        highlighter: any_marker,
        reason: if allowed || any_first || any_marker { None } else { Some(NO_APPEND.into()) },
    })
}

/// `project_ink`: any inventory/format failure yields a disabled projection.
pub fn project_ink(deps: &dyn Deps, snapshot: &dyn Any, points: bool) -> R<Projection> {
    match project(deps, snapshot, points) {
        Err(e)
            if e.any(&[
                Kind::Ink,
                Kind::Doc,
                Kind::Payload,
                Kind::Parse,
                Kind::Value,
                Kind::Type,
                Kind::Overflow,
                Kind::Key,
                Kind::Unicode,
            ]) =>
        {
            Ok(disabled(UNSAFE))
        }
        other => other,
    }
}

pub struct GeometryStroke {
    pub id: String,
    pub layer_id: Option<String>,
    pub size: f64,
    pub points: Vec<(f64, f64)>,
}

pub struct Geometry {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub strokes: Vec<GeometryStroke>,
    pub reason: Option<String>,
}

fn page_geometry(deps: &dyn Deps, snapshot: &dyn Any, page_id: Option<&str>, max_points: f64) -> R<Geometry> {
    let page_id = uuid_(page_id)?;
    let (parsed, root) = match verified_resource(deps, snapshot)? {
        Verified::Reason(r) => return Ok(Geometry { width: None, height: None, strokes: vec![], reason: Some(r.into()) }),
        Verified::Ok(p, r) => (p, r),
    };
    let found: Vec<&Element> = one(&root, "pagelist")?.children.iter().filter(|p| p.get("id") == Some(page_id.as_str())).collect();
    if found.len() != 1 {
        return ink("page absent");
    }
    let page = found[0];
    let width = integer(page.get("width"))?;
    let height = integer(page.get("height"))?;
    let mut wanted: HashMap<Option<String>, Option<String>> = HashMap::new();
    for layer in &one(page, "layerlist")?.children {
        for o in layer.iter_tag("object") {
            wanted.insert(o.get("id").map(str::to_string), layer.get("id").map(str::to_string));
        }
    }
    if wanted.len() > MAX_PROJECTION_STROKES {
        return ink("page stroke budget");
    }
    let mut strokes = Vec::new();
    for part in &parsed.parts {
        let filename = match (&part.name, &part.filename) {
            (Some(n), Some(f)) if n == "strokeResource" && !f.is_empty() => f,
            _ => continue,
        };
        let prefix: String = filename.chars().take(36).collect();
        if !wanted.contains_key(&Some(prefix.clone())) {
            continue;
        }
        let record = deps.decode_stroke(&part.body)?;
        let identity = record.identity()?;
        if identity != prefix {
            return ink("binary identity differs");
        }
        let scale = match deps.coordinate_scale(&record, width, height) {
            Ok(s) => s,
            Err(e) if e.is(Kind::Ink) => continue,
            Err(e) => return Err(e),
        };
        let points = &record.samples.points;
        if max_points == 0.0 {
            return Err(Error::new(Kind::ZeroDiv, "division by zero"));
        }
        let step = ((points.len() as f64 / max_points).ceil() as i64).max(1) as usize;
        let mut kept: Vec<(f64, f64)> = points.iter().step_by(step).copied().collect();
        if !points.is_empty() && kept.last() != points.last() {
            kept.push(*points.last().unwrap());
        }
        let object = page.iter_tag("object").find(|o| o.get("id") == Some(identity.as_str()));
        let object = object.ok_or_else(|| Error::new(Kind::Stop, ""))?;
        let attrs = one(&root, "attributes")?;
        let binding = deps.bind(&record, object, attrs.find("stringIdList"))?;
        let size = f32r(binding.style_get("penSize").map(|v| v.as_f64()).unwrap_or(1.0) * scale)?;
        let pts = kept.iter().map(|(x, y)| Ok((f32r(x * scale)?, f32r(y * scale)?))).collect::<R<_>>()?;
        strokes.push(GeometryStroke { id: identity.clone(), layer_id: wanted[&Some(identity)].clone(), size, points: pts });
    }
    Ok(Geometry { width: Some(width), height: Some(height), strokes, reason: None })
}

/// `project_page_geometry`: simplified page-unit stroke paths for hit tests.
pub fn project_page_geometry(deps: &dyn Deps, snapshot: &dyn Any, page_id: Option<&str>, max_points: f64) -> R<Geometry> {
    match page_geometry(deps, snapshot, page_id, max_points) {
        Err(e)
            if e.any(&[
                Kind::Ink,
                Kind::Doc,
                Kind::Payload,
                Kind::Parse,
                Kind::Value,
                Kind::Type,
                Kind::Overflow,
                Kind::Key,
                Kind::Unicode,
                Kind::Stop,
            ]) =>
        {
            Ok(Geometry {
                width: None,
                height: None,
                strokes: vec![],
                reason: Some("Bu sayfanın çizgi geometrisi güvenle okunamadı.".into()),
            })
        }
        other => other,
    }
}

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
fn rendering(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("nativeEquivalent", false)?;
    d.set_item("mode", "geometry-only")?;
    Ok(d)
}

#[cfg(feature = "python")]
pub fn projection_to_py<'py>(py: Python<'py>, p: &Projection) -> PyResult<Bound<'py, PyDict>> {
    let pages = PyList::empty(py);
    for page in &p.pages {
        let strokes = PyList::empty(py);
        for s in &page.strokes {
            let d = PyDict::new(py);
            d.set_item("id", &s.id)?;
            d.set_item("kind", s.kind)?;
            if s.template || s.kind == "highlighter" {
                d.set_item("colorARGB", s.color)?;
                d.set_item("size", s.size)?;
            }
            d.set_item("template", s.template)?;
            d.set_item("layerId", &s.layer_id)?;
            d.set_item("erasable", s.erasable)?;
            if let Some(b) = s.bounds {
                d.set_item("bounds", PyList::new(py, b)?)?;
            }
            if let Some(points) = &s.points {
                let list = PyList::empty(py);
                for (x, y, pr, t) in points {
                    let pd = PyDict::new(py);
                    pd.set_item("x", x)?;
                    pd.set_item("y", y)?;
                    pd.set_item("pressure", pr)?;
                    pd.set_item("timeMs", t)?;
                    list.append(pd)?;
                }
                d.set_item("points", list)?;
            }
            strokes.append(d)?;
        }
        let pd = PyDict::new(py);
        pd.set_item("id", &page.id)?;
        pd.set_item("index", page.index)?;
        pd.set_item("width", page.width)?;
        pd.set_item("height", page.height)?;
        pd.set_item("background", if page.pdf { Some("pdf") } else { None })?;
        pd.set_item("strokes", strokes)?;
        let ink = PyDict::new(py);
        ink.set_item("appendInk", page.ink.append_ink)?;
        ink.set_item("eraseInk", page.ink.erase_ink)?;
        ink.set_item("firstStroke", page.ink.first_stroke)?;
        ink.set_item("highlighter", page.ink.highlighter)?;
        ink.set_item("reason", &page.ink.reason)?;
        pd.set_item("ink", ink)?;
        pages.append(pd)?;
    }
    let caps = PyDict::new(py);
    caps.set_item("appendInk", p.append_ink)?;
    caps.set_item("eraseInk", p.erase_ink)?;
    caps.set_item("firstStroke", p.first_stroke)?;
    caps.set_item("highlighter", p.highlighter)?;
    let d = PyDict::new(py);
    d.set_item("schemaVersion", 1)?;
    d.set_item("pages", pages)?;
    d.set_item("capabilities", caps)?;
    d.set_item("reason", &p.reason)?;
    d.set_item("rendering", rendering(py)?)?;
    Ok(d)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "project_ink", signature = (snapshot, *, points=None))]
fn py_project_ink<'py>(snapshot: &Bound<'py, PyAny>, points: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyDict>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let include = match points {
        Some(p) => p.is_truthy()?,
        None => true,
    };
    let p = project_ink(&deps, &bridge::opaque(snapshot), include).py(py)?;
    projection_to_py(py, &p)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "verified_resource")]
fn py_verified_resource<'py>(snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let arg = bridge::opaque(snapshot);
    match verified_resource(&deps, &arg).py(py)? {
        Verified::Reason(r) => Ok(r.into_pyobject(py)?.into_any()),
        Verified::Ok(_, root) => {
            let snap = deps.snapshot(&arg).py(py)?.ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("snapshot vanished"))?;
            let parsed = bridge::python_payload(py, &snap.raw_body, &snap.content_type)?;
            Ok(PyTuple::new(py, [parsed, bridge::to_et(py, &root)?])?.into_any())
        }
    }
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "project_page_geometry", signature = (snapshot, page_id, *, max_points=None))]
fn py_project_page_geometry<'py>(
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    max_points: Option<f64>,
) -> PyResult<Bound<'py, PyDict>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let g = project_page_geometry(
        &deps,
        &bridge::opaque(snapshot),
        ink_erase::opt_str(page_id).as_deref(),
        max_points.unwrap_or(MAX_GEOMETRY_POINTS),
    )
    .py(py)?;
    let d = PyDict::new(py);
    d.set_item("schemaVersion", 1)?;
    d.set_item("pageId", page_id)?;
    if let (Some(w), Some(h)) = (g.width, g.height) {
        d.set_item("width", w)?;
        d.set_item("height", h)?;
    }
    let strokes = PyList::empty(py);
    for s in &g.strokes {
        let sd = PyDict::new(py);
        sd.set_item("id", &s.id)?;
        sd.set_item("layerId", &s.layer_id)?;
        sd.set_item("size", s.size)?;
        let pts = PyList::empty(py);
        for (x, y) in &s.points {
            pts.append(PyList::new(py, [*x, *y])?)?;
        }
        sd.set_item("points", pts)?;
        strokes.append(sd)?;
    }
    d.set_item("strokes", strokes)?;
    d.set_item("reason", &g.reason)?;
    Ok(d)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_project_ink, m)?)?;
    m.add_function(wrap_pyfunction!(py_verified_resource, m)?)?;
    let _ = bridge::VERIFIED_FN.set(m.py(), m.getattr("verified_resource")?.unbind());
    m.add_function(wrap_pyfunction!(py_project_page_geometry, m)?)?;
    rs_common::export(m, &["project_ink", "verified_resource", "project_page_geometry"])
}
