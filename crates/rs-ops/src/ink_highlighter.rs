//! Replacement for the Python module `ink_highlighter`: a Marker4 top-layer
//! (highlighter) stroke on any supported page, encoded from scratch with the
//! first-stroke layout (see the Python module doc for the evidence).
use crate::bridge;
#[cfg(feature = "python")]
use crate::bridge::PyDeps;
use crate::bridge::RPy;
use crate::deps::*;
use crate::err::*;
#[cfg(feature = "python")]
use crate::ink_erase::exact_int;
#[cfg(feature = "python")]
use crate::ink_erase::int_or_float;
#[cfg(feature = "python")]
use crate::ink_erase::opt_str;
use crate::ink_erase::py_float;
use crate::ink_first_stroke::encode_stroke;
use crate::ink_first_stroke::ensure_strings;
use crate::ink_first_stroke::optional;
#[cfg(feature = "python")]
use crate::ink_first_stroke::style_from_py;
use crate::ink_first_stroke::style_get;
#[cfg(feature = "python")]
use crate::ink_first_stroke::style_to_py;
use crate::ink_first_stroke::EncodeProfile;
use crate::ink_first_stroke::FirstStroke;
use crate::ink_first_stroke::Style;
use crate::ink_projection::{verify, Verified};
use crate::page_style::advance_as;
use crate::util::*;
use crate::xml::{self, Element};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyTuple};
use std::any::Any;

pub const HIGHLIGHTER_RECIPE: i64 = 5;
pub const PEN_NAME: &str = "com.samsung.android.sdk.pen.pen.preload.Marker4";
pub const LEGACY_PEN_NAME: &str = "com.samsung.android.sdk.pen.pen.preload.Marker";
pub const ADVANCED_SETTING: &str = "8;";
pub const HIGHLIGHTER_ALPHA: i64 = 0x73;
pub const DEFAULT_COLOR: i64 = 0x73FFCB30;
pub const DEFAULT_SIZE: f64 = 40.0;
pub const TOP_LAYER: u16 = 0x40;
pub const STROKE_PROPERTIES: u16 = 0x0421 | TOP_LAYER;
pub const TOOL_TYPE: i64 = 3;
pub const PARTICLE_DENSITY: i64 = 1;
pub const TOLERANCE_RATIO: f64 = 0.17436965;
pub const MIN_SIZE: f64 = 1.0;
pub const MAX_SIZE: f64 = 128.0;
const ALLOWED_PAGE_CHILDREN: [&str; 4] = ["layerList", "drawnRect", "canvasCacheDataMap", "pdfDataList"];

const PROFILE: EncodeProfile = EncodeProfile { stroke_properties: STROKE_PROPERTIES, label: "highlighter", check_properties: true };

/// True for a Marker4 top-layer stroke (ours or Samsung's).
pub fn is_highlighter(binding: &Binding, record: &Record) -> bool {
    (binding.pen_name == PEN_NAME || binding.pen_name == LEGACY_PEN_NAME)
        && record.frames.get(1).is_some_and(|f| f.properties & TOP_LAYER as u64 != 0)
}

fn size_ok(size: Option<f64>) -> R<f64> {
    match size {
        Some(s) if s.is_finite() && (MIN_SIZE..=MAX_SIZE).contains(&s) => Ok(s),
        _ => ink("highlighter size out of range"),
    }
}

/// Style dict exactly as read_stroke_style returns it (ids filled later).
pub fn highlighter_style(color_argb: Option<i128>, size: Option<f64>) -> R<Style> {
    let color = match color_argb {
        Some(c) if (0..=0xffff_ffff).contains(&c) && c >> 24 == HIGHLIGHTER_ALPHA as i128 => c as i64,
        _ => return ink("highlighter colour must carry alpha 0x73"),
    };
    let size = f32r(size_ok(size)?)?;
    Ok(vec![
        ("colorARGB".into(), Num::I(color)),
        ("penSize".into(), Num::F(size)),
        ("fixedWidth".into(), Num::F(size)),
        ("particleDensity".into(), Num::I(PARTICLE_DENSITY)),
        ("initialTolerance".into(), Num::F(f32r(size * TOLERANCE_RATIO)?)),
    ])
}

/// Conservative Marker4 drawn rectangle in page units. `samples` is None when
/// the argument is not a StrokeSamples.
pub fn highlighter_drawn_bounds(samples: Option<&Samples>, size: Option<f64>) -> R<[f64; 4]> {
    let size = size_ok(size)?;
    let samples = match samples {
        Some(s) if !s.points.is_empty() => s,
        _ => return ink("invalid bounds samples"),
    };
    let size = f32r(size)?;
    let extent = f32r(f32r(f32r(size * 0.5)? + 4.0)? + f32r(size.trunc() * 0.5 + 1.5)?)?;
    let mut xs = Vec::with_capacity(samples.points.len());
    let mut ys = Vec::with_capacity(samples.points.len());
    for p in &samples.points {
        xs.push(f32r(p.0)?);
    }
    for p in &samples.points {
        ys.push(f32r(p.1)?);
    }
    // Python min/max keep the first of equal values.
    let min = |v: &[f64]| v.iter().copied().reduce(|a, b| if b < a { b } else { a }).unwrap();
    let max = |v: &[f64]| v.iter().copied().reduce(|a, b| if b > a { b } else { a }).unwrap();
    Ok([
        f32r(min(&xs) - extent)?,
        f32r(min(&ys) - extent)?,
        f32r(max(&xs) + extent)?,
        f32r(max(&ys) + extent)?,
    ])
}

/// Encode one Marker4 top-layer stroke resource from scratch (no template).
#[allow(clippy::too_many_arguments)]
pub fn encode_highlighter_stroke(
    deps: &dyn Deps,
    stroke_id: Option<&str>,
    tick: Option<i128>,
    append_time: Option<i128>,
    bounds: [f64; 4],
    owner: (Option<i128>, Option<i128>),
    capture: Option<&Capture>,
    style: &Style,
    pen_name_id: i128,
    advanced_id: i128,
) -> R<(Vec<u8>, Record)> {
    encode_stroke(deps, &PROFILE, stroke_id, tick, append_time, bounds, owner, capture, style, pen_name_id, advanced_id)
}

/// Python `min(a, b)` / `max(a, b)` on floats (first argument wins ties).
fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Offline edit adding one highlighter stroke to a page (empty or not).
/// `samples`: the caller's StrokeSamples object, or None when it is not one.
#[allow(clippy::too_many_arguments)]
pub fn prepare_highlighter_stroke(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    samples: Option<&dyn Any>,
    color_argb: Option<i128>,
    size: Option<f64>,
    now_ms: Option<i128>,
    new_stroke_id: Option<Option<&str>>,
) -> R<FirstStroke> {
    let snap = match deps.snapshot(snapshot)? {
        Some(s) if s.commit_id.as_deref().is_some_and(|c| !c.is_empty()) => s,
        _ => return ink("committed snapshot required"),
    };
    let new_id = match new_stroke_id {
        None => uuid4(),
        Some(Some(s)) => s.to_string(),
        Some(None) => String::new(),
    };
    let page_id = match page_id {
        Some(p) if canonical_uuid(&new_id) && canonical_uuid(p) => p.to_string(),
        _ => return ink("canonical UUID required"),
    };
    let samples = match samples {
        Some(s) => s,
        None => return ink("stroke samples required"),
    };
    let style = highlighter_style(color_argb, size)?;
    let parsed = match verify(deps, &snap)? {
        Verified::Reason(r) => return ink(r),
        Verified::Ok(p, _) => p,
    };
    let mut root = xml::parse_bytes(&snap.resource_xml).map_err(|m| e_parse(&m))?;
    if root.iter().any(|n| n.get("id") == Some(new_id.as_str())) {
        return ink("new stroke identity already exists");
    }
    let now_ms = advance_as(&root, now_ms, Kind::Ink, "sync timestamp must advance in wall-clock milliseconds")?;
    let known = ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];
    if root.children.iter().any(|n| !known.contains(&n.tag.as_str())) {
        return ink("unknown resource section");
    }
    let ti = one_idx(&root, "notetree")?;
    let ai = one_idx(&root, "attributes")?;
    let pi = one_idx(&root, "pagelist")?;
    {
        let (tree, attrs) = (&root.children[ti], &root.children[ai]);
        if attrs.get("formatVersion") != Some("4000")
            || tree.get("ver") != Some("4000")
            || tree.get_or("isStrokeGrouped", "false").to_lowercase() != "false"
        {
            return ink("unsupported ink document profile");
        }
    }
    if root.iter().any(|n| matches!(n.tag.as_str(), "strokeGroup" | "voiceDataList" | "attachedFileMap")) {
        return ink("grouped/media ink is unsupported");
    }
    if let Some(f) = optional(&root, "floatingObjectList")? {
        if !root.children[f].is_empty() {
            return ink("floating objects are unsupported");
        }
    }
    let stroke_parts: Vec<&Part> = parsed.parts.iter().filter(|p| p.name.as_deref() == Some("strokeResource")).collect();
    if stroke_parts.len() >= MAX_READONLY_PARTS
        || parsed.parts.iter().any(|p| {
            p.name.as_deref() != Some("strokeResource") && !matches!(p.content_type.as_str(), "text/xml" | "application/xml")
        })
    {
        return ink("unsupported ink part inventory");
    }
    {
        let pages = &root.children[pi];
        // Images, shapes, lines and text boxes are carried through untouched.
        let (objects, _) = ink_objects(pages)?;
        let wanted: std::collections::BTreeSet<&str> = ["type", "id", "modifiedTime", "hash"].into_iter().collect();
        if objects.iter().any(|o| o.get("type") != Some("stroke") || !o.is_empty() || o.attr_key_set() != wanted) {
            return ink("unsupported or ambiguous object inventory");
        }
        let mut a: Vec<String> = stroke_parts.iter().map(|p| p.filename.clone().unwrap_or_default()).collect();
        let mut b: Vec<String> =
            objects.iter().map(|o| format!("{}_{}", o.get("id").unwrap(), o.get("modifiedTime").unwrap())).collect();
        a.sort();
        b.sort();
        if a != b {
            return ink("binary/XML stroke inventory differs");
        }
    }
    let matches: Vec<usize> = root.children[pi]
        .children
        .iter()
        .enumerate()
        .filter(|(_, p)| p.get("id") == Some(page_id.as_str()))
        .map(|(i, _)| i)
        .collect();
    if matches.len() != 1 {
        return ink("target page absent or duplicated");
    }
    let pg = matches[0];
    let (width, height, layer_id, tpl, tp, child_hashes, original) = {
        let page = &root.children[pi].children[pg];
        if page.get_or("orientation", "0") != "0" || root.children[ai].get_or("orientation", "0") != "0" {
            return ink("page orientation transform is unsupported");
        }
        if page.children.iter().any(|n| !ALLOWED_PAGE_CHILDREN.contains(&n.tag.as_str()))
            || page.get_or("bgImageId", "-1") != "-1"
            || !page.get_or("templateUri", "").is_empty()
        {
            return ink("page has unsupported background or media metadata");
        }
        let layers = one(page, "layerlist")?;
        if layers.len() != 1 || layers.children[0].tag != "layer" {
            return ink("exactly one page layer required");
        }
        let layer = &layers.children[0];
        if layer.get_or("lockState", "false").to_lowercase() != "false"
            || layer.get_or("thumbnail", "-1") != "-1"
            || layer.get_or("visible", "true").to_lowercase() != "true"
        {
            return ink("locked, hidden or cached layer");
        }
        if layer.children.iter().any(|n| n.tag != "objectList") || layer.len() > 1 {
            return ink("unknown layer metadata");
        }
        let existing: &[Element] = if layer.is_empty() { &[] } else { &layer.children[0].children };
        let drawn = page.find("drawnRect");
        if page.findall("drawnRect").len() > 1 {
            return ink("duplicate drawn rectangle");
        }
        if !existing.is_empty() && drawn.is_none() {
            return ink("stroked page without a drawn rectangle");
        }
        if existing.is_empty() && drawn.is_some() {
            return ink("stroke-free page with a drawn rectangle is unverified");
        }
        let width = integer(page.get("width"))?;
        let height = integer(page.get("height"))?;
        if !(1..=32768).contains(&width) || !(1..=32768).contains(&height) {
            return ink("invalid page extent");
        }
        let tree = &root.children[ti];
        let tpl = one_idx(tree, "pagelist")?;
        let tp = match tree.children[tpl].children.iter().position(|p| p.get("id") == Some(page_id.as_str())) {
            Some(i) => i,
            None => return ink("page absent from tree"),
        };
        let tree_page = &tree.children[tpl].children[tp];
        let tree_layers = one(tree_page, "layerlist")?;
        if tree_layers.len() != 1 || tree_layers.children[0].get("id") != layer.get("id") {
            return ink("layer absent from tree");
        }
        let tree_layer = &tree_layers.children[0];
        if tree_layer.children.iter().any(|n| n.tag != "objectList") || tree_layer.len() > 1 {
            return ink("unknown layer tree metadata");
        }
        let tree_existing: &[Element] = if tree_layer.is_empty() { &[] } else { &tree_layer.children[0].children };
        let a: Vec<_> = existing.iter().map(|o| (o.get("id"), o.get("modifiedTime"), o.get("hash"))).collect();
        let b: Vec<_> = tree_existing.iter().map(|o| (o.get("id"), o.get("t"), o.get("h"))).collect();
        if a != b {
            return ink("stroke tree differs from objects");
        }
        // bytes.fromhex for every child first, then integer(), then parent_hash's length check.
        let raw_hashes = existing.iter().map(|o| fromhex(o.get("hash"))).collect::<R<Vec<_>>>()?;
        let layer_time = integer(layer.get("modifiedTime"))?;
        let child_hashes = raw_hashes
            .into_iter()
            .map(|h| <[u8; 32]>::try_from(h).map_err(|_| Error::new(Kind::Value, "child hashes must be raw SHA256 bytes")))
            .collect::<R<Vec<_>>>()?;
        let layer_raw = parent_hash(layer.get("id"), layer_time, &child_hashes)?;
        let layer_hash = hex(&layer_raw);
        if Some(layer_hash.as_str()) != layer.get("hash")
            || Some(layer_hash.as_str()) != tree_layer.get("h")
            || tree_layer.get("t") != layer.get("modifiedTime")
        {
            return ink("layer hash mismatch");
        }
        let page_hash = hex(&parent_hash(Some(&page_id), integer(page.get("modifiedTime"))?, &[layer_raw])?);
        if Some(page_hash.as_str()) != page.get("hash")
            || Some(page_hash.as_str()) != tree_page.get("h")
            || tree_page.get("t") != page.get("modifiedTime")
        {
            return ink("page hash mismatch");
        }
        let mut original = None;
        if let Some(d) = drawn {
            let wanted: std::collections::BTreeSet<&str> = ["l", "t", "r", "b"].into_iter().collect();
            if d.attr_key_set() != wanted || !d.is_empty() {
                return ink("invalid page drawn rectangle");
            }
            let mut v = [0f64; 4];
            for (slot, key) in v.iter_mut().zip(["l", "t", "r", "b"]) {
                *slot = match py_float(d.get(key).unwrap_or("")) {
                    Some(f) => f,
                    None => return ink("invalid page drawn rectangle"),
                };
            }
            if v.iter().any(|x| !x.is_finite()) {
                return ink("invalid page drawn rectangle");
            }
            original = Some(v);
        }
        (width, height, layer.get("id").unwrap_or("").to_string(), tpl, tp, child_hashes, original)
    };
    let mut max = i64::MIN;
    for n in root.iter() {
        if n.has("modifiedTime") {
            max = max.max(integer(n.get("modifiedTime"))?);
        }
    }
    let tick = max as i128 + 1;
    let revision = integer(root.children[ai].get("fileRevision"))? as i128 + 1;
    if tick >= I63 || revision >= 1 << 31 {
        return ink("ink timestamp/revision overflow");
    }
    let tick = tick as i64;
    let mut changed_files = Vec::new();
    let target = &root.children[pi].children[pg];
    if target.find("canvasCacheDataMap").is_some() || target.find("pdfDataList").is_some() {
        changed_files = deps.release_page_cache(&mut root, &page_id, tick)?;
    }
    let capture = deps.normalize_capture(samples)?;
    let bounds = highlighter_drawn_bounds(Some(&capture.samples), Some(style_get(&style, "penSize")?.as_f64()))?;
    let (w, h) = (width as f64, height as f64);
    if !(0.0 <= bounds[0] && bounds[0] <= bounds[2] && bounds[2] <= w) || !(0.0 <= bounds[1] && bounds[1] <= bounds[3] && bounds[3] <= h) {
        return ink("captured drawn bounds leave the page");
    }
    let (table_i, pen_id, advanced_id) = ensure_strings(&mut root.children[ai], [PEN_NAME, ADVANCED_SETTING])?;
    let (raw, record) = encode_highlighter_stroke(
        deps,
        Some(&new_id),
        Some(tick as i128),
        Some(tick as i128 - 1),
        bounds,
        (Some(width as i128), Some(height as i128)),
        Some(&capture),
        &style,
        pen_id as i128,
        advanced_id as i128,
    )?;
    let digest_raw = object_hash(&new_id, tick)?;
    let digest = hex(&digest_raw);
    let ts = tick.to_string();
    let new_object = Element::new("object", &[("type", "stroke"), ("id", &new_id), ("hash", &digest), ("modifiedTime", &ts)]);
    let binding = deps.bind(&record, &new_object, Some(&root.children[ai].children[table_i]))?;
    if binding.pen_name != PEN_NAME
        || binding.advanced_settings.as_deref() != Some(ADVANCED_SETTING)
        || !is_highlighter(&binding, &record)
    {
        return ink("brush binding mismatch");
    }
    let mut all_hashes = child_hashes;
    all_hashes.push(digest_raw);
    let layer_raw = parent_hash(Some(&layer_id), tick, &all_hashes)?;
    let layer_hash = hex(&layer_raw);
    let page_hash = hex(&parent_hash(Some(&page_id), tick, &[layer_raw])?);
    {
        let page = &mut root.children[pi].children[pg];
        let li = one_idx(page, "layerlist")?;
        let layer = &mut page.children[li].children[0];
        if layer.is_empty() {
            layer.sub_element("objectList", &[]);
        }
        layer.children[0].children.push(new_object);
        layer.set("modifiedTime", &ts);
        layer.set("hash", &layer_hash);
        page.set("modifiedTime", &ts);
        page.set("hash", &page_hash);
        match original {
            None => {
                let rect: Vec<String> = bounds.iter().map(|v| py_float_repr(*v)).collect();
                page.children.insert(
                    0,
                    Element::new("drawnRect", &[("l", &rect[0]), ("t", &rect[1]), ("r", &rect[2]), ("b", &rect[3])]),
                );
            }
            Some(o) => {
                let union = [py_min(o[0], bounds[0]), py_min(o[1], bounds[1]), py_max(o[2], bounds[2]), py_max(o[3], bounds[3])];
                let di = page.children.iter().position(|n| n.tag == "drawnRect").unwrap();
                let drawn = &mut page.children[di];
                for (key, value) in ["l", "t", "r", "b"].into_iter().zip(union) {
                    drawn.set(key, &py_float_repr(value));
                }
            }
        }
    }
    {
        let tree_page = &mut root.children[ti].children[tpl].children[tp];
        let li = one_idx(tree_page, "layerlist")?;
        let tree_layer = &mut tree_page.children[li].children[0];
        if tree_layer.is_empty() {
            tree_layer.sub_element("objectList", &[]);
        }
        tree_layer.children[0].sub_element("object", &[("id", &new_id), ("t", &ts), ("h", &digest)]);
        tree_layer.set("t", &ts);
        tree_layer.set("h", &layer_hash);
        tree_page.set("t", &ts);
        tree_page.set("h", &page_hash);
    }
    let attrs = &mut root.children[ai];
    attrs.set("fileRevision", &revision.to_string());
    attrs.set("modifiedTime", &ts);
    attrs.set("displayModifiedTime", &now_ms.to_string());
    root.set("syncModifiedTime", &now_ms.to_string());
    let full = xml::xml_bytes(&root);
    let mut upload = upload_copy(&root);
    if let Some(f) = optional(&upload, "contentFileList")? {
        upload.children[f].children.retain(|n| n.get("id").is_some_and(|i| changed_files.iter().any(|c| c == i)));
    }
    {
        let upload_pages = one_mut(&mut upload, "pagelist")?;
        upload_pages.children.retain(|p| p.get("id") == Some(page_id.as_str()));
        for p in upload_pages.children.iter_mut() {
            retain_new_objects(p, &new_id);
        }
    }
    let mut requested = vec![page_id.clone(), layer_id.clone(), new_id.clone()];
    requested.extend(changed_files.iter().cloned());
    Ok(FirstStroke {
        full_xml: full,
        tree_xml: xml::xml_bytes(&root.children[ti]),
        upload_xml: xml::xml_bytes(&upload),
        requested_ids: requested,
        page_id,
        layer_id,
        stroke_id: new_id.clone(),
        tick,
        part: BinaryPart { name: "strokeResource".into(), filename: format!("{new_id}_{tick}"), body: raw },
        content_file_ids: changed_files,
        snapshot: snap,
    })
}

/// `for obj_list in p.iter('objectList'): drop every object but the new one`.
fn retain_new_objects(e: &mut Element, new_id: &str) {
    if e.tag == "objectList" {
        e.children.retain(|o| o.get("id") == Some(new_id));
    }
    for c in e.children.iter_mut() {
        retain_new_objects(c, new_id);
    }
}

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "is_highlighter")]
fn py_is_highlighter(binding: &Bound<'_, PyAny>, record: &Bound<'_, PyAny>) -> PyResult<bool> {
    let pen: Option<String> = binding.getattr("pen_name")?.extract().ok();
    if !pen.is_some_and(|p| p == PEN_NAME || p == LEGACY_PEN_NAME) {
        return Ok(false);
    }
    let props: Vec<u8> = record.getattr("frames")?.get_item(1)?.getattr("properties")?.extract()?;
    let mut value: u128 = 0;
    for (i, b) in props.iter().enumerate().take(16) {
        value |= (*b as u128) << (8 * i);
    }
    Ok(value & TOP_LAYER as u128 != 0)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "highlighter_style")]
fn py_highlighter_style<'py>(color_argb: &Bound<'py, PyAny>, size: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = color_argb.py();
    let style = highlighter_style(exact_int(color_argb), int_or_float(size)).py(py)?;
    style_to_py(py, &style)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "highlighter_drawn_bounds")]
fn py_highlighter_drawn_bounds<'py>(samples: &Bound<'py, PyAny>, size: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = samples.py();
    let size = int_or_float(size);
    size_ok(size).py(py)?;
    let parsed = if samples.is_instance(&bridge::stroke_samples_class(py)?)? {
        let mut s = Samples::default();
        for p in samples.getattr("points")?.try_iter()? {
            let p = p?;
            let coord = |i: usize| -> PyResult<f64> {
                match p.get_item(i)?.extract::<f64>() {
                    Ok(v) => Ok(v),
                    Err(_) => Err(bridge::to_py(py, e_ink("non-finite or unrepresentable stroke value"))),
                }
            };
            s.points.push((coord(0)?, coord(1)?));
        }
        Some(s)
    } else {
        None
    };
    let b = highlighter_drawn_bounds(parsed.as_ref(), size).py(py)?;
    PyTuple::new(py, b)
}

#[pyfunction]
#[pyo3(name = "encode_highlighter_stroke",
       signature = (*, stroke_id, tick, append_time, bounds, owner, capture, style, pen_name_id, advanced_id))]
#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn py_encode_highlighter_stroke<'py>(
    stroke_id: &Bound<'py, PyAny>,
    tick: &Bound<'py, PyAny>,
    append_time: &Bound<'py, PyAny>,
    bounds: Vec<f64>,
    owner: &Bound<'py, PyAny>,
    capture: &Bound<'py, PyAny>,
    style: &Bound<'py, PyAny>,
    pen_name_id: i128,
    advanced_id: i128,
) -> PyResult<Bound<'py, PyTuple>> {
    let py = stroke_id.py();
    let deps = PyDeps { py };
    let cap = if capture.is_instance(&bridge::normalized_capture_class(py)?)? { Some(bridge::capture_from(capture)?) } else { None };
    let pair: Vec<Bound<PyAny>> = owner.try_iter()?.collect::<PyResult<_>>()?;
    if pair.len() != 2 {
        return Err(pyo3::exceptions::PyValueError::new_err(format!("expected 2 values to unpack (got {})", pair.len())));
    }
    let (w, h) = (&pair[0], &pair[1]);
    let style = style_from_py(style)?;
    let (raw, record) = encode_highlighter_stroke(
        &deps,
        opt_str(stroke_id).as_deref(),
        exact_int(tick),
        exact_int(append_time),
        <[f64; 4]>::try_from(bounds).map_err(|_| pyo3::exceptions::PyValueError::new_err("pack expected 5 items for packing"))?,
        (exact_int(w), exact_int(h)),
        cap.as_ref(),
        &style,
        pen_name_id,
        advanced_id,
    )
    .py(py)?;
    PyTuple::new(py, [PyBytes::new(py, &raw).into_any(), bridge::record_py(py, &record)?])
}

#[pyfunction]
#[pyo3(name = "prepare_highlighter_stroke",
       signature = (snapshot, page_id, samples, *, color_argb, size, now_ms, new_stroke_id=None))]
#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn py_prepare_highlighter_stroke<'py>(
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    color_argb: &Bound<'py, PyAny>,
    size: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    new_stroke_id: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let samples_obj = bridge::opaque(samples);
    let is_samples = samples.is_instance(&bridge::stroke_samples_class(py)?)?;
    let new_id = match &new_stroke_id {
        Some(o) if !o.is_none() => Some(opt_str(o)),
        _ => None,
    };
    let e = prepare_highlighter_stroke(
        &deps,
        &bridge::opaque(snapshot),
        opt_str(page_id).as_deref(),
        if is_samples { Some(&samples_obj as &dyn Any) } else { None },
        exact_int(color_argb),
        int_or_float(size),
        exact_int(now_ms),
        new_id.as_ref().map(|x| x.as_deref()),
    )
    .py(py)?;
    let snap = bridge::handle(py, &e.snapshot.handle)?;
    let part = bridge::ink_binary_part(py, &e.part)?;
    let args = PyTuple::new(
        py,
        [
            PyBytes::new(py, &e.full_xml).into_any(),
            PyBytes::new(py, &e.tree_xml).into_any(),
            PyBytes::new(py, &e.upload_xml).into_any(),
            bridge::frozenset_str(py, &e.requested_ids)?,
            e.page_id.into_pyobject(py)?.into_any(),
            e.layer_id.into_pyobject(py)?.into_any(),
            e.stroke_id.into_pyobject(py)?.into_any(),
            e.tick.into_pyobject(py)?.into_any(),
            PyTuple::new(py, [part])?.into_any(),
            snap.getattr("title")?,
            snap.getattr("body")?,
            bridge::tuple_str(py, &e.content_file_ids)?.into_any(),
            HIGHLIGHTER_RECIPE.into_pyobject(py)?.into_any(),
        ],
    )?;
    bridge::ink_append_edit(py, args)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_is_highlighter, m)?)?;
    m.add_function(wrap_pyfunction!(py_highlighter_style, m)?)?;
    m.add_function(wrap_pyfunction!(py_highlighter_drawn_bounds, m)?)?;
    m.add_function(wrap_pyfunction!(py_encode_highlighter_stroke, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_highlighter_stroke, m)?)?;
    rs_common::export(
        m,
        &["is_highlighter", "highlighter_style", "highlighter_drawn_bounds", "encode_highlighter_stroke", "prepare_highlighter_stroke"],
    )
}
