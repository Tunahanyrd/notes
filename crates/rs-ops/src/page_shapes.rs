//! Replacement for the Python module `page_shapes`: one shape/line object
//! added to a page layer (inline XML, no binary MIME part) and the shapes DTO.
use crate::bridge;
#[cfg(feature = "python")]
use crate::bridge::PyDeps;
use crate::bridge::RPy;
use crate::deps::*;
use crate::err::*;
use crate::ink_projection::{verify, Verified};
use crate::util::*;
use crate::xml::{self, Element};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use std::any::Any;

pub const SHAPE_RECIPE: i64 = 1;
pub const KINDS: [&str; 7] = ["rectangle", "ellipse", "triangle", "rightTriangle", "diamond", "line", "arrow"];
pub const MIN_WIDTH: f64 = 0.5;
pub const MAX_WIDTH: f64 = 64.0;
pub const MIN_EXTENT: f64 = 1.0;
pub const MAX_SHAPES_PER_PAGE: usize = 256;
const ALLOWED_PAGE_CHILDREN: [&str; 4] = ["layerList", "drawnRect", "canvasCacheDataMap", "pdfDataList"];
const SECTIONS: [&str; 6] = ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];
const R2: f64 = 0.7071067811865476;
const C8: f64 = 0.9238795325112867;
const S8: f64 = 0.3826834323650898;
const K8: f64 = 1.082392200292394;
const STRUCTURE: &str = "Notun yapısı beklenenden farklı.";

fn shape_err<T>(msg: &str) -> R<T> {
    Err(Error::new(Kind::Shape, msg))
}

pub fn shape_type(kind: &str) -> Option<u32> {
    match kind {
        "ellipse" => Some(1),
        "triangle" => Some(2),
        "rightTriangle" => Some(3),
        "rectangle" => Some(4),
        "diamond" => Some(8),
        _ => None,
    }
}

fn is_line(kind: &str) -> bool {
    kind == "line" || kind == "arrow"
}

/// `math.floor(value * 4.0 + 0.5) / 4.0`
pub fn quantize(value: f64) -> f64 {
    (value * 4.0 + 0.5).floor() / 4.0
}

/// Java Float.toString of a quarter-unit value.
pub fn jfloat(value: f64) -> String {
    let v = quantize(value);
    if v == 0.0 {
        return "0.0".into();
    }
    if v == v.trunc() && v.abs() < 1e18 {
        return format!("{}.0", v as i64);
    }
    py_float_repr(v)
}

pub fn signed(argb: i128) -> i128 {
    if argb >= 1 << 31 {
        argb - (1 << 32)
    } else {
        argb
    }
}

/// `style.get('fillARGB')` as seen by `_color(..., optional=True)`.
#[derive(Clone, Debug)]
pub enum Fill {
    Absent,
    Int(i128),
    Bad,
}

/// The caller's style argument, classified like the Python checks read it.
#[derive(Clone, Debug)]
pub struct StyleIn {
    /// isinstance(style, dict)
    pub is_dict: bool,
    pub keys: Vec<String>,
    /// style['colorARGB'] when it is an exact int.
    pub color: Option<i128>,
    /// style['width'] when it is an int/float.
    pub width: Option<f64>,
    pub fill: Fill,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub kind: String,
    pub geometry: [f64; 4],
    pub color_argb: u32,
    pub width: f64,
    pub fill_argb: Option<u32>,
}

fn color(value: Option<i128>) -> R<u32> {
    match value {
        Some(v) if (0..=0xffff_ffff).contains(&v) && v >> 24 != 0 => Ok(v as u32),
        _ => shape_err("Şekil rengi geçersiz."),
    }
}

/// `normalize_shape(kind, geometry, style)`; geometry None = not list/tuple,
/// element None = not int/float.
pub fn normalize_shape(kind: Option<&str>, geometry: Option<&[Option<f64>]>, style: &StyleIn) -> R<Shape> {
    let kind = match kind {
        Some(k) if KINDS.contains(&k) => k,
        _ => return shape_err("Bu şekil türü desteklenmiyor."),
    };
    let g = match geometry {
        Some(g) if g.len() == 4 && g.iter().all(|v| v.is_some_and(|v| v.is_finite() && v.abs() <= 65536.0)) => {
            [g[0].unwrap(), g[1].unwrap(), g[2].unwrap(), g[3].unwrap()]
        }
        _ => return shape_err("Şekil konumu geçersiz."),
    };
    let allowed = ["colorARGB", "width", "fillARGB"];
    if !style.is_dict
        || style.keys.iter().any(|k| !allowed.contains(&k.as_str()))
        || !style.keys.iter().any(|k| k == "colorARGB")
        || !style.keys.iter().any(|k| k == "width")
    {
        return shape_err("Şekil biçimi geçersiz.");
    }
    let width = match style.width {
        Some(w) if w.is_finite() && (MIN_WIDTH..=MAX_WIDTH).contains(&w) => w,
        _ => return shape_err("Çizgi kalınlığı desteklenen aralıkta değil."),
    };
    let fill = match style.fill {
        Fill::Absent => None,
        Fill::Int(v) => Some(color(Some(v))?),
        Fill::Bad => return shape_err("Şekil rengi geçersiz."),
    };
    if is_line(kind) && fill.is_some() {
        return shape_err("Çizgilerin dolgusu olmaz.");
    }
    let [mut x1, mut y1, mut x2, mut y2] = g.map(quantize);
    if !is_line(kind) {
        let (a, b) = (x1.min(x2), x1.max(x2));
        x1 = a;
        x2 = b;
        let (a, b) = (y1.min(y2), y1.max(y2));
        y1 = a;
        y2 = b;
        if x2 - x1 < MIN_EXTENT || y2 - y1 < MIN_EXTENT {
            return shape_err("Şekil çok küçük.");
        }
    } else if (x2 - x1).hypot(y2 - y1) < MIN_EXTENT {
        return shape_err("Çizgi çok kısa.");
    }
    let color_argb = color(style.color)?;
    Ok(Shape { kind: kind.to_string(), geometry: [x1, y1, x2, y2], color_argb, width: MIN_WIDTH.max(quantize(width)), fill_argb: fill })
}

fn drawn(kind: &str, g: [f64; 4], width: f64) -> [f64; 4] {
    let pad = match kind {
        "rectangle" | "ellipse" | "line" => width / 2.0,
        "arrow" => width * 4.0,
        _ => width * 2.0,
    };
    let (l, t, r, b) = (g[0].min(g[2]) - pad, g[1].min(g[3]) - pad, g[0].max(g[2]) + pad, g[1].max(g[3]) + pad);
    [(l * 4.0).floor() / 4.0, (t * 4.0).floor() / 4.0, (r * 4.0).ceil() / 4.0, (b * 4.0).ceil() / 4.0]
}

/// One path segment: (type, coordinates...).
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub kind: i64,
    pub coords: Vec<f64>,
}

fn seg(kind: i64, coords: &[f64]) -> Segment {
    Segment { kind, coords: coords.to_vec() }
}

pub fn shape_path(kind: &str, g: [f64; 4]) -> Vec<Segment> {
    let [l, t, r, b] = g;
    let (cx, cy) = (quantize((l + r) / 2.0), quantize((t + b) / 2.0));
    let points: Vec<(f64, f64)> = match kind {
        "rectangle" => vec![(l, t), (r, t), (r, b), (l, b)],
        "triangle" => vec![(cx, t), (r, b), (l, b)],
        "rightTriangle" => vec![(l, t), (r, b), (l, b)],
        "diamond" => vec![(cx, t), (r, cy), (cx, b), (l, cy)],
        "ellipse" => {
            let (ex, ey, rx, ry) = ((l + r) / 2.0, (t + b) / 2.0, (r - l) / 2.0, (b - t) / 2.0);
            let on = [(1.0, 0.0), (R2, R2), (0.0, 1.0), (-R2, R2), (-1.0, 0.0), (-R2, -R2), (0.0, -1.0), (R2, -R2)];
            let mid = [(C8, S8), (S8, C8), (-S8, C8), (-C8, S8), (-C8, -S8), (-S8, -C8), (S8, -C8), (C8, -S8)];
            let mut out = vec![seg(1, &[quantize(ex + rx), quantize(ey)])];
            for i in 0..8 {
                let (ux, uy) = on[(i + 1) % 8];
                let (mx, my) = mid[i];
                out.push(seg(3, &[quantize(ex + rx * mx * K8), quantize(ey + ry * my * K8), quantize(ex + rx * ux), quantize(ey + ry * uy)]));
            }
            out.push(seg(6, &[]));
            return out;
        }
        _ => return vec![seg(1, &[g[0], g[1]]), seg(2, &[g[2], g[3]])],
    };
    let mut out = vec![seg(1, &[points[0].0, points[0].1])];
    out.extend(points[1..].iter().map(|p| seg(2, &[p.0, p.1])));
    out.push(seg(6, &[]));
    out
}

fn rect_el(tag: &str, r: [f64; 4]) -> Element {
    Element::new(tag, &[("b", &jfloat(r[3])), ("l", &jfloat(r[0])), ("r", &jfloat(r[2])), ("t", &jfloat(r[1]))])
}

fn point_el(tag: &str, x: f64, y: f64) -> Element {
    Element::new(tag, &[("x", &jfloat(x)), ("y", &jfloat(y))])
}

fn path_el(segments: &[Segment]) -> Element {
    let mut path = Element::new("path", &[]);
    let list = path.sub_element("segmentList", &[]);
    for s in segments {
        let mut attrs: Vec<(&str, String)> = vec![("type", s.kind.to_string())];
        match s.kind {
            1 | 2 => {
                attrs.push(("x", jfloat(s.coords[0])));
                attrs.push(("y", jfloat(s.coords[1])));
            }
            3 => {
                attrs.push(("x", jfloat(s.coords[0])));
                attrs.push(("y", jfloat(s.coords[1])));
                attrs.push(("x2", jfloat(s.coords[2])));
                attrs.push(("y2", jfloat(s.coords[3])));
            }
            _ => {}
        }
        attrs.sort_by(|a, b| a.0.cmp(b.0));
        let pairs: Vec<(&str, &str)> = attrs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        list.sub_element("segment", &pairs);
    }
    path
}

fn sorted_element(tag: &str, mut attrs: Vec<(String, String)>) -> Element {
    attrs.sort_by(|a, b| a.0.cmp(&b.0));
    Element { tag: tag.into(), attrib: attrs, ..Default::default() }
}

/// `build_shape_object(kind, geometry, style, object_id=, tick=, owner=)` →
/// (element, drawn rectangle).
pub fn build_shape_object(kind: &str, g: [f64; 4], color_argb: u32, width: f64, fill_argb: Option<u32>, object_id: &str, tick: i64, owner: (i64, i64)) -> R<(Element, [f64; 4])> {
    let d = drawn(kind, g, width);
    let digest = hex(&object_hash(object_id, tick)?);
    let s = |k: &str, v: &str| (k.to_string(), v.to_string());
    let mut common = vec![
        s("clippable", "true"),
        s("flippable", "true"),
        s("formatVersion", "4000"),
        s("haveBeenToAtt", "true"),
        s("isReplayable", "true"),
        s("isTemplateObject", "false"),
        s("lockState", "false"),
        s("movable", "true"),
        s("removable", "true"),
        s("resizable", "0"),
        s("rotatable", "true"),
        s("selectable", "true"),
        s("timeStamp", "0"),
        s("visible", "true"),
        s("id", object_id),
        s("hash", &digest),
        s("createdTime", &(tick - 1).to_string()),
        s("appendTime", &(tick - 1).to_string()),
        s("modifiedTime", &tick.to_string()),
        s("ownerPageWidth", &owner.0.to_string()),
        s("ownerPageHeight", &owner.1.to_string()),
        s("originalDegree", "0.0"),
    ];
    let mut lce = Element::new(
        "lineColorEffect",
        &[("angle", "0"), ("colorType", "0"), ("gradientRotatable", "false"), ("gradientType", "0"), ("solidColor", &signed(color_argb as i128).to_string())],
    );
    lce.children.push(point_el("radialGradientPosition", 0.0, 0.0));
    let lse = Element::new(
        "lineStyleEffect",
        &[
            ("beginArrowSize", "0"),
            ("beginArrowType", "0"),
            ("capType", "0"),
            ("compoundType", "0"),
            ("dashType", "0"),
            ("endArrowSize", "0"),
            ("endArrowType", if kind == "arrow" { "1" } else { "0" }),
            ("joinType", "0"),
            ("width", &jfloat(width)),
        ],
    );
    if is_line(kind) {
        let [x1, y1, x2, y2] = g;
        common.extend([s("type", "line"), s("connectorType", "0"), s("startDirection", "0")]);
        let mut node = sorted_element("object", common);
        node.children.push(rect_el("rect", d));
        node.children.push(lce);
        node.children.push(lse);
        node.children.push(point_el("beginPos", x1, y1));
        node.children.push(point_el("endPos", x2, y2));
        node.children.push(rect_el("originalDrawnRect", d));
        node.children.push(rect_el("originalRect", [x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)]));
        node.children.push(path_el(&shape_path(kind, g)));
        return Ok((node, d));
    }
    let shape_type = match shape_type(kind) {
        Some(t) => t,
        None => return Err(Error::new(Kind::Key, format!("'{kind}'"))),
    };
    common.extend([
        s("type", "shape"),
        s("shapeType", &shape_type.to_string()),
        s("ellipsisType", "2"),
        s("isHintTextVisible", "false"),
        s("isHorizontalFlipped", "false"),
        s("isTextEditable", "false"),
        s("isTextReadOnly", "false"),
        s("isTransparency", "false"),
        s("isVerticalFlipped", "false"),
    ]);
    let mut node = sorted_element("object", common);
    let [l, t, r, b] = g;
    node.children.push(rect_el("rect", d));
    if matches!(kind, "rectangle" | "ellipse" | "diamond") {
        let mut points = Element::new("connectionPointList", &[]);
        let (cx, cy) = (quantize((l + r) / 2.0), quantize((t + b) / 2.0));
        for (x, y) in [(cx, t), (r, cy), (cx, b), (l, cy)] {
            points.children.push(point_el("point", x, y));
        }
        node.children.push(points);
    }
    node.children.push(lce);
    node.children.push(lse);
    node.children.push(rect_el("originalRect", g));
    node.children.push(path_el(&shape_path(kind, g)));
    node.children.push(rect_el("drawnRect", d));
    if let Some(fill) = fill_argb {
        let mut f = Element::new(
            "fillEffect",
            &[
                ("angle", "0"),
                ("colorType", "0"),
                ("colotCount", "0"),
                ("gradientRatatable", "false"),
                ("gradientType", "0"),
                ("solidColor", &signed(fill as i128).to_string()),
                ("type", "color"),
            ],
        );
        f.children.push(point_el("radialGradientPosition", 0.0, 0.0));
        node.children.push(f);
    }
    Ok((node, d))
}

fn children<'a>(node: &'a Element, tag: &str) -> Vec<&'a Element> {
    node.children.iter().filter(|n| n.tag == tag).collect()
}

fn one_tag<'a>(node: &'a Element, tag: &str) -> R<&'a Element> {
    let found = children(node, tag);
    if found.len() != 1 {
        return shape_err(STRUCTURE);
    }
    Ok(found[0])
}

fn one_tag_idx(node: &Element, tag: &str) -> R<usize> {
    let found: Vec<usize> = node.children.iter().enumerate().filter(|(_, n)| n.tag == tag).map(|(i, _)| i).collect();
    if found.len() != 1 {
        return shape_err(STRUCTURE);
    }
    Ok(found[0])
}

/// Python `float(text)` (whitespace-tolerant).
fn py_float(text: Option<&str>) -> Option<f64> {
    let t = text?.trim();
    if t.is_empty() {
        return None;
    }
    t.replace('_', "").parse::<f64>().ok().filter(|_| !t.starts_with('_') && !t.ends_with('_') && !t.contains("__"))
}

/// Python `int(text)`.
fn py_int(text: Option<&str>) -> Option<i128> {
    let t = text?.trim();
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean = digits.replace('_', "");
    if !clean.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = clean.parse().ok()?;
    Some(if neg { -v } else { v })
}

fn floats(node: &Element, keys: &[&str]) -> R<Vec<f64>> {
    let mut out = Vec::new();
    for k in keys {
        match py_float(node.get(k)) {
            Some(v) if v.is_finite() => out.push(v),
            _ => return shape_err("Şekil verisi okunamadı."),
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapeInfo {
    pub id: Option<String>,
    pub kind: String,
    pub geometry: [f64; 4],
    pub color_argb: u32,
    pub width: f64,
    pub fill_argb: Option<u32>,
}

/// `read_shape_object(node)`: our own shape/line profile, or None.
pub fn read_shape_object(node: &Element) -> Option<ShapeInfo> {
    (|| -> R<Option<ShapeInfo>> {
        let kind_attr = node.get("type");
        if node.tag != "object" || !matches!(kind_attr, Some("shape") | Some("line")) {
            return Ok(None);
        }
        let color = one_tag(node, "lineColorEffect")?;
        let line = one_tag(node, "lineStyleEffect")?;
        if color.get("colorType") != Some("0") || line.get("beginArrowType") != Some("0") {
            return Ok(None);
        }
        let argb = match py_int(color.get("solidColor")) {
            Some(v) => (v & 0xffff_ffff) as u32,
            None => return Ok(None),
        };
        let width = floats(line, &["width"])?[0];
        let names: Vec<&str> = node.children.iter().map(|n| n.tag.as_str()).collect();
        if kind_attr == Some("line") {
            if names != ["rect", "lineColorEffect", "lineStyleEffect", "beginPos", "endPos", "originalDrawnRect", "originalRect", "path"] {
                return Ok(None);
            }
            if node.get("connectorType") != Some("0") || !matches!(line.get("endArrowType"), Some("0") | Some("1")) {
                return Ok(None);
            }
            let b = floats(one_tag(node, "beginPos")?, &["x", "y"])?;
            let e = floats(one_tag(node, "endPos")?, &["x", "y"])?;
            return Ok(Some(ShapeInfo {
                id: node.get("id").map(str::to_string),
                kind: if line.get("endArrowType") == Some("1") { "arrow" } else { "line" }.into(),
                geometry: [b[0], b[1], e[0], e[1]],
                color_argb: argb,
                width,
                fill_argb: None,
            }));
        }
        let kind = match node.get("shapeType") {
            Some("1") => "ellipse",
            Some("2") => "triangle",
            Some("3") => "rightTriangle",
            Some("4") => "rectangle",
            Some("8") => "diamond",
            _ => return Ok(None),
        };
        let names: Vec<&str> = names.into_iter().filter(|n| *n != "connectionPointList").collect();
        let a = ["rect", "lineColorEffect", "lineStyleEffect", "originalRect", "path", "drawnRect"];
        let b = ["rect", "lineColorEffect", "lineStyleEffect", "originalRect", "path", "drawnRect", "fillEffect"];
        if names != a && names != b {
            return Ok(None);
        }
        let mut fill = None;
        let fills = children(node, "fillEffect");
        if let Some(f) = fills.first() {
            if f.get("type") != Some("color") || f.get("colorType") != Some("0") {
                return Ok(None);
            }
            fill = match py_int(f.get("solidColor")) {
                Some(v) => Some((v & 0xffff_ffff) as u32),
                None => return Ok(None),
            };
        }
        let r = floats(one_tag(node, "originalRect")?, &["l", "t", "r", "b"])?;
        Ok(Some(ShapeInfo { id: node.get("id").map(str::to_string), kind: kind.into(), geometry: [r[0], r[1], r[2], r[3]], color_argb: argb, width, fill_argb: fill }))
    })()
    .unwrap_or(None)
}

fn known_object(node: &Element) -> bool {
    if node.tag != "object" {
        return false;
    }
    if node.get("type") == Some("stroke") {
        let wanted: std::collections::BTreeSet<&str> = ["type", "id", "modifiedTime", "hash"].into_iter().collect();
        return node.is_empty() && node.attr_key_set() == wanted;
    }
    read_shape_object(node).is_some()
}

/// Exact attribute names of one child element, plus whether it has children.
fn exact_leaf(node: &Element, keys: &[&str]) -> bool {
    node.children.is_empty() && node.attrib.len() == keys.len() && keys.iter().all(|k| node.has(k))
}

/// `<x radialGradientPosition/>` wrapper of the colour effects.
fn exact_effect(node: &Element, keys: &[&str]) -> bool {
    node.attrib.len() == keys.len() && keys.iter().all(|k| node.has(k))
        && node.children.len() == 1 && node.children[0].tag == "radialGradientPosition" && exact_leaf(&node.children[0], &["x", "y"])
}

/// A shape/line the local ink renderer may leave out of its page input.
///
/// Stricter than [`read_shape_object`]: besides that parser's own profile,
/// every attribute name of the object and of each descendant must be one
/// `build_shape_object` writes (only `appendTime`/`ownerPageWidth`/
/// `ownerPageHeight` may be absent), path segments carry exactly the
/// coordinates of their type (1, 2, 3, 6) and no element holds text. Such an
/// object paints nothing the renderer owns (the UI draws it as SVG geometry),
/// so skipping it cannot hide content; anything else keeps the page refused.
pub fn renderer_skippable_shape(node: &Element) -> bool {
    if read_shape_object(node).is_none() {
        return false;
    }
    if node.iter().any(|n| n.text.as_deref().is_some_and(|t| !t.trim().is_empty())) {
        return false;
    }
    const COMMON: [&str; 22] = ["clippable", "flippable", "formatVersion", "haveBeenToAtt", "isReplayable", "isTemplateObject",
        "lockState", "movable", "removable", "resizable", "rotatable", "selectable", "timeStamp", "visible", "id", "hash",
        "createdTime", "modifiedTime", "originalDegree", "appendTime", "ownerPageWidth", "ownerPageHeight"];
    const OPTIONAL: [&str; 3] = ["appendTime", "ownerPageWidth", "ownerPageHeight"];
    const LINE: [&str; 3] = ["type", "connectorType", "startDirection"];
    const SHAPE: [&str; 9] = ["type", "shapeType", "ellipsisType", "isHintTextVisible", "isHorizontalFlipped", "isTextEditable",
        "isTextReadOnly", "isTransparency", "isVerticalFlipped"];
    let line = node.get("type") == Some("line");
    let own: &[&str] = if line { &LINE } else { &SHAPE };
    let allowed = |k: &str| COMMON.contains(&k) || own.contains(&k);
    let mut seen = std::collections::BTreeSet::new();
    for (k, _) in &node.attrib {
        if !allowed(k) || !seen.insert(k.as_str()) {
            return false;
        }
    }
    if COMMON.iter().chain(own.iter()).any(|k| !OPTIONAL.contains(k) && !seen.contains(k)) {
        return false;
    }
    const RECT: [&str; 4] = ["b", "l", "r", "t"];
    const POINT: [&str; 2] = ["x", "y"];
    node.children.iter().all(|child| match child.tag.as_str() {
        "rect" | "originalRect" | "drawnRect" | "originalDrawnRect" => exact_leaf(child, &RECT),
        "beginPos" | "endPos" => exact_leaf(child, &POINT),
        "connectionPointList" => child.attrib.is_empty() && child.children.iter().all(|p| p.tag == "point" && exact_leaf(p, &POINT)),
        "lineColorEffect" => exact_effect(child, &["angle", "colorType", "gradientRotatable", "gradientType", "solidColor"]),
        "lineStyleEffect" => exact_leaf(child, &["beginArrowSize", "beginArrowType", "capType", "compoundType", "dashType",
            "endArrowSize", "endArrowType", "joinType", "width"]),
        "fillEffect" => exact_effect(child, &["angle", "colorType", "colotCount", "gradientRatatable", "gradientType", "solidColor", "type"]),
        "path" => {
            child.attrib.is_empty() && child.children.len() == 1 && child.children[0].tag == "segmentList"
                && child.children[0].attrib.is_empty()
                && child.children[0].children.iter().all(|s| {
                    s.tag == "segment" && match s.get("type") {
                        Some("1") | Some("2") => exact_leaf(s, &["type", "x", "y"]),
                        Some("3") => exact_leaf(s, &["type", "x", "y", "x2", "y2"]),
                        Some("6") => exact_leaf(s, &["type"]),
                        _ => false,
                    }
                })
        }
        _ => false,
    })
}

/// `_note_gate` after the snapshot was resolved: the parsed resource root.
fn note_gate(deps: &dyn Deps, snap: &Snapshot) -> R<Element> {
    if !snap.commit_id.as_deref().is_some_and(|c| !c.is_empty()) {
        return shape_err("Notun eşitlenmiş sürümü gerekli.");
    }
    let parsed = match verify(deps, snap) {
        Ok(Verified::Reason(r)) => return shape_err(r),
        Ok(Verified::Ok(p, _)) => p,
        Err(e) if e.is(Kind::Value) || e.is(Kind::Parse) => return shape_err("Notun yapısı şekil için güvenle okunamadı."),
        Err(e) => return Err(e),
    };
    let root = xml::parse_bytes(&snap.resource_xml).map_err(|m| e_parse(&m))?;
    if root.children.iter().any(|n| !SECTIONS.contains(&n.tag.as_str())) {
        return shape_err("Notta tanınmayan bir bölüm var; şekil eklenemiyor.");
    }
    let tree = one_tag(&root, "noteTree")?;
    let attrs = one_tag(&root, "attributes")?;
    let pages = one_tag(&root, "pageList")?;
    if attrs.get("formatVersion") != Some("4000") || tree.get("ver") != Some("4000") || tree.get_or("isStrokeGrouped", "false").to_lowercase() != "false" {
        return shape_err("Bu notun biçim sürümünde şekil desteklenmiyor.");
    }
    if attrs.get_or("orientation", "0") != "0" {
        return shape_err("Döndürülmüş notlarda şekil desteklenmiyor.");
    }
    if root.iter().any(|n| matches!(n.tag.as_str(), "strokeGroup" | "voiceDataList" | "attachedFileMap")) {
        return shape_err("Bu notta ses, ek veya gruplanmış çizgi var; şekil eklenemiyor.");
    }
    let floating = children(&root, "floatingObjectList");
    if floating.len() > 1 || floating.first().is_some_and(|f| !f.is_empty()) {
        return shape_err("Bu notta serbest yerleşimli nesne var; şekil eklenemiyor.");
    }
    let stroke_parts: Vec<&Part> = parsed.parts.iter().filter(|p| p.name.as_deref() == Some("strokeResource")).collect();
    if stroke_parts.len() >= MAX_READONLY_PARTS
        || parsed.parts.iter().any(|p| p.name.as_deref() != Some("strokeResource") && !matches!(p.content_type.as_str(), "text/xml" | "application/xml"))
    {
        return shape_err("Bu notta resim veya başka ekli veri var; şekil eklenemiyor.");
    }
    let objects: Vec<&Element> = pages.iter_tag("object").collect();
    if objects.iter().any(|o| !known_object(o)) {
        return shape_err("Bu notta resim, metin kutusu veya tanınmayan nesne var; şekil eklenemiyor.");
    }
    let mut a: Vec<String> = stroke_parts.iter().map(|p| p.filename.clone().unwrap_or_default()).collect();
    let mut b: Vec<String> = Vec::new();
    for o in objects.iter().filter(|o| o.get("type") == Some("stroke")) {
        b.push(format!("{}_{}", o.get("id").unwrap_or(""), o.get("modifiedTime").unwrap_or("")));
    }
    a.sort();
    b.sort();
    if a != b {
        return shape_err("Notun çizgi verisi XML ile uyuşmuyor; şekil eklenemiyor.");
    }
    let ids: Vec<Option<&str>> = objects.iter().map(|o| o.get("id")).collect();
    let unique: std::collections::BTreeSet<Option<&str>> = ids.iter().copied().collect();
    if unique.len() != ids.len() {
        return shape_err("Notta yinelenen nesne kimliği var.");
    }
    Ok(root)
}

struct PageGate {
    tree_page: usize,
    width: i64,
    height: i64,
}

/// `_page_gate(root, page)`.
fn page_gate(root: &Element, page: &Element) -> R<PageGate> {
    if page.get_or("orientation", "0") != "0" {
        return shape_err("Döndürülmüş sayfalarda şekil desteklenmiyor.");
    }
    if page.children.iter().any(|n| !ALLOWED_PAGE_CHILDREN.contains(&n.tag.as_str())) || page.get_or("bgImageId", "-1") != "-1" || !page.get_or("templateUri", "").is_empty() {
        return shape_err("Bu sayfada arka plan resmi veya tanınmayan içerik var; şekil eklenemiyor.");
    }
    if children(page, "drawnRect").len() > 1 {
        return shape_err("Sayfanın çizim alanı belirsiz.");
    }
    let layers = one_tag(page, "layerList")?;
    if layers.len() != 1 || layers.children[0].tag != "layer" {
        return shape_err("Şekil yalnız tek katmanlı sayfalara eklenebiliyor.");
    }
    let layer = &layers.children[0];
    if layer.get_or("lockState", "false").to_lowercase() != "false" || layer.get_or("thumbnail", "-1") != "-1" || layer.get_or("visible", "true").to_lowercase() != "true" {
        return shape_err("Sayfa katmanı kilitli veya gizli; şekil eklenemiyor.");
    }
    if layer.children.iter().any(|n| n.tag != "objectList") || layer.len() > 1 {
        return shape_err("Sayfa katmanının yapısı tanınmıyor.");
    }
    let objects: &[Element] = if layer.is_empty() { &[] } else { &layer.children[0].children };
    if objects.iter().filter(|o| o.get("type") != Some("stroke")).count() >= MAX_SHAPES_PER_PAGE {
        return shape_err("Bu sayfada çok fazla şekil var.");
    }
    let width = integer(page.get("width"))?;
    let height = integer(page.get("height"))?;
    if !(1..=32768).contains(&width) || !(1..=32768).contains(&height) {
        return shape_err("Sayfa boyutu geçersiz.");
    }
    let tree_pages = one_tag(one_tag(root, "noteTree")?, "pageList")?;
    let matches: Vec<usize> = tree_pages.children.iter().enumerate().filter(|(_, p)| p.get("id") == page.get("id")).map(|(i, _)| i).collect();
    if matches.len() != 1 {
        return shape_err("Sayfa not ağacında bulunamadı.");
    }
    let tree_page = &tree_pages.children[matches[0]];
    let tree_layers = one_tag(tree_page, "layerList")?;
    if tree_layers.len() != 1 || tree_layers.children[0].get("id") != layer.get("id") {
        return shape_err("Katman not ağacında bulunamadı.");
    }
    let tree_layer = &tree_layers.children[0];
    if tree_layer.children.iter().any(|n| n.tag != "objectList") || tree_layer.len() > 1 {
        return shape_err("Katman ağacı tanınmıyor.");
    }
    let tree_objects: &[Element] = if tree_layer.is_empty() { &[] } else { &tree_layer.children[0].children };
    let left: Vec<_> = objects.iter().map(|o| (o.get("id"), o.get("modifiedTime"), o.get("hash"))).collect();
    let right: Vec<_> = tree_objects.iter().map(|o| (o.get("id"), o.get("t"), o.get("h"))).collect();
    if left != right {
        return shape_err("Katman nesneleri not ağacıyla uyuşmuyor.");
    }
    let digests = (|| -> R<(String, String)> {
        let hs = objects.iter().map(|o| hash32(o.get("hash"))).collect::<R<Vec<_>>>()?;
        let layer_raw = parent_hash(layer.get("id"), integer(layer.get("modifiedTime"))?, &hs)?;
        let page_raw = parent_hash(page.get("id"), integer(page.get("modifiedTime"))?, &[layer_raw])?;
        Ok((hex(&layer_raw), hex(&page_raw)))
    })();
    let (layer_hash, page_hash) = match digests {
        Ok(v) => v,
        Err(e) if e.is(Kind::Value) || e.is(Kind::Type) => return shape_err("Katman özeti geçersiz."),
        Err(e) => return Err(e),
    };
    if Some(layer_hash.as_str()) != layer.get("hash") || Some(layer_hash.as_str()) != tree_layer.get("h") || tree_layer.get("t") != layer.get("modifiedTime") {
        return shape_err("Katman özeti uyuşmuyor.");
    }
    if Some(page_hash.as_str()) != page.get("hash") || Some(page_hash.as_str()) != tree_page.get("h") || tree_page.get("t") != page.get("modifiedTime") {
        return shape_err("Sayfa özeti uyuşmuyor.");
    }
    Ok(PageGate { tree_page: matches[0], width, height })
}

pub struct ShapeEdit {
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub requested_ids: Vec<String>,
    pub page_id: String,
    pub layer_id: String,
    pub object_id: String,
    pub modified_time: i64,
    pub content_file_ids: Vec<String>,
    pub kind: String,
    pub snapshot: Snapshot,
}

/// `prepare_add_shape(snapshot, page_id, kind, geometry, style, now_ms,
/// new_object_id=None)`; `new_object_id`: None = generate, Some(None) = not a str.
#[allow(clippy::too_many_arguments)]
pub fn prepare_add_shape(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    kind: Option<&str>,
    geometry: Option<&[Option<f64>]>,
    style: &StyleIn,
    now_ms: Option<i128>,
    new_object_id: Option<Option<&str>>,
) -> R<ShapeEdit> {
    let shape = normalize_shape(kind, geometry, style)?;
    let new_id = match new_object_id {
        None => Some(uuid4()),
        Some(v) => v.map(str::to_string),
    };
    let (page_id, new_id) = match (page_id, new_id) {
        (Some(p), Some(n)) if canonical_uuid(&n) && canonical_uuid(p) => (p.to_string(), n),
        _ => return shape_err("Şekil kimliği geçersiz."),
    };
    let snap = match deps.snapshot(snapshot)? {
        Some(s) => s,
        None => return shape_err("Notun eşitlenmiş sürümü gerekli."),
    };
    prepare_core(deps, snap, &page_id, &shape, now_ms, &new_id)
}

/// Body of prepare_add_shape after argument validation (normalized shape).
pub fn prepare_core(deps: &dyn Deps, snap: Snapshot, page_id: &str, shape: &Shape, now_ms: Option<i128>, new_id: &str) -> R<ShapeEdit> {
    let mut root = note_gate(deps, &snap)?;
    if root.iter().any(|n| n.get("id") == Some(new_id)) {
        return shape_err("Yeni şekil kimliği notta zaten var.");
    }
    let now_ms = match now_ms {
        None => return shape_err("Kayıt zamanı ilerlemeli."),
        Some(n) => {
            let sync = integer(root.get("syncModifiedTime"))? as i128;
            if !(sync < n && n < I63) {
                return shape_err("Kayıt zamanı ilerlemeli.");
            }
            n as i64
        }
    };
    let ti = one_tag_idx(&root, "noteTree")?;
    let ai = one_tag_idx(&root, "attributes")?;
    let pi = one_tag_idx(&root, "pageList")?;
    let matches: Vec<usize> = root.children[pi].children.iter().enumerate().filter(|(_, p)| p.get("id") == Some(page_id)).map(|(i, _)| i).collect();
    if matches.len() != 1 {
        return shape_err("Sayfa bulunamadı.");
    }
    let pg = matches[0];
    let gate = page_gate(&root, &root.children[pi].children[pg])?;
    let [x1, y1, x2, y2] = shape.geometry;
    let (w, h) = (gate.width as f64, gate.height as f64);
    if !(0.0 <= x1.min(x2) && x1.max(x2) <= w && 0.0 <= y1.min(y2) && y1.max(y2) <= h) {
        return shape_err("Şekil sayfanın dışına taşıyor.");
    }
    let mut max: Option<i64> = None;
    for n in root.iter() {
        if n.has("modifiedTime") {
            let v = integer(n.get("modifiedTime"))?;
            max = Some(max.map_or(v, |m| m.max(v)));
        }
    }
    let tick = match max {
        Some(m) => m as i128 + 1,
        None => return value("max() arg is an empty sequence"),
    };
    let revision = integer(root.children[ai].get("fileRevision"))? as i128 + 1;
    if tick >= I63 || revision >= 1 << 31 {
        return shape_err("Not zaman damgası taşıyor.");
    }
    let tick = tick as i64;
    let mut changed_files: Vec<String> = Vec::new();
    {
        let page = &root.children[pi].children[pg];
        if page.find("canvasCacheDataMap").is_some() || page.find("pdfDataList").is_some() {
            changed_files = deps
                .release_page_cache(&mut root, page_id, tick)
                .map_err(|e| if e.is(Kind::Ink) { Error::new(Kind::Shape, "Sayfa önbelleği güvenle bırakılamıyor; şekil eklenemiyor.") } else { e })?;
        }
    }
    let (node, d) = build_shape_object(&shape.kind, shape.geometry, shape.color_argb, shape.width, shape.fill_argb, new_id, tick, (gate.width, gate.height))?;
    let digest = node.get("hash").unwrap_or("").to_string();
    let ts = tick.to_string();
    let layer_id;
    let layer_hash;
    {
        let page = &mut root.children[pi].children[pg];
        let li = one_tag_idx(page, "layerList")?;
        let layer = &mut page.children[li].children[0];
        layer_id = layer.get("id").map(str::to_string);
        if layer.is_empty() {
            layer.sub_element("objectList", &[]);
        }
        layer.children[0].children.push(node);
        let hs = hashes(layer.children[0].children.iter())?;
        layer_hash = hex(&parent_hash(layer_id.as_deref(), tick, &hs)?);
        layer.set("modifiedTime", &ts);
        layer.set("hash", &layer_hash);
    }
    let page_hash = hex(&parent_hash(Some(page_id), tick, &[hash32(Some(&layer_hash))?])?);
    {
        let tpl = one_tag_idx(&root.children[ti], "pageList")?;
        let tree_page = &mut root.children[ti].children[tpl].children[gate.tree_page];
        let li = one_tag_idx(tree_page, "layerList")?;
        let tree_layer = &mut tree_page.children[li].children[0];
        if tree_layer.is_empty() {
            tree_layer.sub_element("objectList", &[]);
        }
        tree_layer.children[0].sub_element("object", &[("id", new_id), ("t", &ts), ("h", &digest)]);
        tree_layer.set("t", &ts);
        tree_layer.set("h", &layer_hash);
        tree_page.set("t", &ts);
        tree_page.set("h", &page_hash);
    }
    {
        let page = &mut root.children[pi].children[pg];
        page.set("modifiedTime", &ts);
        page.set("hash", &page_hash);
        match page.find_index("drawnRect") {
            None => {
                let r: Vec<String> = d.iter().map(|v| py_float_repr(*v)).collect();
                page.children.insert(0, Element::new("drawnRect", &[("l", &r[0]), ("t", &r[1]), ("r", &r[2]), ("b", &r[3])]));
            }
            Some(i) => {
                let old = floats(&page.children[i], &["l", "t", "r", "b"])?;
                let union = [old[0].min(d[0]), old[1].min(d[1]), old[2].max(d[2]), old[3].max(d[3])];
                let current = &mut page.children[i];
                for (k, v) in ["l", "t", "r", "b"].iter().zip(union) {
                    current.set(k, &py_float_repr(v));
                }
            }
        }
    }
    {
        let attrs = &mut root.children[ai];
        attrs.set("fileRevision", &revision.to_string());
        attrs.set("modifiedTime", &ts);
        attrs.set("displayModifiedTime", &now_ms.to_string());
    }
    root.set("syncModifiedTime", &now_ms.to_string());
    let full = xml::xml_bytes(&root);
    let mut upload = upload_copy(&root);
    if let Some(fi) = upload.children.iter().position(|n| n.tag == "contentFileList") {
        upload.children[fi].children.retain(|f| f.get("id").is_some_and(|i| changed_files.iter().any(|c| c == i)));
    }
    let upi = one_tag_idx(&upload, "pageList")?;
    upload.children[upi].children.retain(|p| p.get("id") == Some(page_id));
    for p in upload.children[upi].children.iter_mut() {
        if let Some(objects) = p.find_mut("layerList").and_then(|l| l.find_mut("layer")).and_then(|l| l.find_mut("objectList")) {
            objects.children.retain(|o| o.get("id") == Some(new_id));
        }
    }
    let layer_id = layer_id.unwrap_or_default();
    let mut requested = vec![page_id.to_string(), layer_id.clone(), new_id.to_string()];
    requested.extend(changed_files.iter().cloned());
    Ok(ShapeEdit {
        full_xml: full,
        tree_xml: xml::xml_bytes(&root.children[ti]),
        upload_xml: xml::xml_bytes(&upload),
        requested_ids: requested,
        page_id: page_id.to_string(),
        layer_id,
        object_id: new_id.to_string(),
        modified_time: tick,
        content_file_ids: changed_files,
        kind: shape.kind.clone(),
        snapshot: snap,
    })
}

pub struct PageShapes {
    pub id: Option<String>,
    pub index: usize,
    pub shapes: Vec<ShapeInfo>,
    pub size: Option<(i64, i64)>,
    pub add_shape: bool,
    pub reason: Option<String>,
}

pub struct ShapesState {
    pub pages: Vec<PageShapes>,
    pub reason: Option<String>,
}

const GENERIC_PAGE: &str = "Bu sayfaya şekil eklenemiyor.";

/// `project_shapes(snapshot)`.
pub fn project_shapes(deps: &dyn Deps, snapshot: &dyn Any) -> R<ShapesState> {
    let gated = (|| -> R<(Snapshot, Element)> {
        let snap = match deps.snapshot(snapshot)? {
            Some(s) => s,
            None => return shape_err("Notun eşitlenmiş sürümü gerekli."),
        };
        let root = note_gate(deps, &snap)?;
        Ok((snap, root))
    })();
    let (snap, root) = match gated {
        Ok(v) => v,
        Err(e) if e.kind == Kind::Shape => return Ok(ShapesState { pages: vec![], reason: Some(e.msg) }),
        Err(e) if e.any(&[Kind::Doc, Kind::Value, Kind::Type, Kind::Key, Kind::Parse, Kind::Unicode]) => {
            return Ok(ShapesState { pages: vec![], reason: Some("Bu notun yapısı şekil için güvenle okunamadı.".into()) })
        }
        Err(e) => return Err(e),
    };
    let now_ms = match integer(root.get("syncModifiedTime")) {
        Ok(v) => Some(v as i128 + 1),
        Err(e) if e.is(Kind::Doc) => None,
        Err(e) => return Err(e),
    };
    let mut probes: Vec<((bool, bool), Option<String>)> = Vec::new();
    let mut result = Vec::new();
    let pages = one_tag(&root, "pageList")?;
    for (index, page) in pages.children.iter().enumerate() {
        let mut entry = PageShapes { id: page.get("id").map(str::to_string), index, shapes: vec![], size: None, add_shape: false, reason: None };
        let outcome = (|| -> R<()> {
            let w = integer(page.get("width"))?;
            let h = integer(page.get("height"))?;
            entry.size = Some((w, h));
            page_gate(&root, page)?;
            let layer = &one_tag(page, "layerList")?.children[0];
            if !layer.is_empty() {
                entry.shapes = layer.children[0].children.iter().filter_map(read_shape_object).collect();
            }
            let category = (page.find("pdfDataList").is_some(), page.find("canvasCacheDataMap").is_some());
            let known = probes.iter().find(|(c, _)| *c == category).map(|(_, r)| r.clone());
            let reason = match known {
                Some(r) => r,
                None => {
                    let (wf, hf) = (w as f64, h as f64);
                    let probe = normalize_shape(
                        Some("rectangle"),
                        Some(&[Some(wf / 4.0), Some(hf / 4.0), Some(wf / 2.0), Some(hf / 2.0)]),
                        &StyleIn { is_dict: true, keys: vec!["colorARGB".into(), "width".into()], color: Some(0xff00_0000), width: Some(2.0), fill: Fill::Absent },
                    )
                    .and_then(|shape| {
                        let new_id = uuid4();
                        let page_id = page.get("id").map(str::to_string);
                        match page_id {
                            Some(p) if canonical_uuid(&p) => prepare_core(deps, clone_snapshot(deps, &snap)?, &p, &shape, now_ms, &new_id).map(|_| ()),
                            _ => shape_err("Şekil kimliği geçersiz."),
                        }
                    });
                    let r = match probe {
                        Ok(()) => None,
                        Err(e) if e.kind == Kind::Shape => Some(e.msg),
                        Err(e) if e.any(&[Kind::Doc, Kind::Value, Kind::Type]) => Some(GENERIC_PAGE.to_string()),
                        Err(e) => return Err(e),
                    };
                    probes.push((category, r.clone()));
                    r
                }
            };
            entry.add_shape = reason.is_none();
            entry.reason = reason;
            Ok(())
        })();
        match outcome {
            Ok(()) => {}
            Err(e) if e.kind == Kind::Shape => {
                entry.add_shape = false;
                entry.reason = Some(e.msg);
            }
            Err(e) if e.any(&[Kind::Doc, Kind::Value, Kind::Type]) => {
                entry.add_shape = false;
                entry.reason = Some(GENERIC_PAGE.into());
            }
            Err(e) => return Err(e),
        }
        result.push(entry);
    }
    let any_ok = result.iter().any(|p| p.add_shape);
    Ok(ShapesState { pages: result, reason: if any_ok { None } else { Some("Bu notta şekil eklenebilecek sayfa yok.".into()) } })
}

/// A second resolved copy of `snap` for the discarded dry run.
fn clone_snapshot(deps: &dyn Deps, snap: &Snapshot) -> R<Snapshot> {
    match snap.handle.as_ref() {
        Some(h) => match deps.snapshot(&**h)? {
            Some(s) => Ok(s),
            None => shape_err("Notun eşitlenmiş sürümü gerekli."),
        },
        None => Ok(Snapshot {
            raw_body: snap.raw_body.clone(),
            content_type: snap.content_type.clone(),
            resource_xml: snap.resource_xml.clone(),
            local_id: snap.local_id.clone(),
            commit_id: snap.commit_id.clone(),
            handle: None,
        }),
    }
}

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
crate::record_class!(PyShapeEdit, "ShapeEdit", "samsung_notes.page_shapes",
    [full_xml, tree_xml, upload_xml, requested_ids, page_id, layer_id, object_id, modified_time, title, body,
     content_file_ids, kind, recipe_version, binary_parts]);

#[cfg(feature = "python")]
/// Python argument → (is_str, value) for `kind in KINDS`.
fn py_kind(obj: &Bound<'_, PyAny>) -> Option<String> {
    if obj.is_instance_of::<pyo3::types::PyString>() {
        obj.extract::<String>().ok()
    } else {
        None
    }
}

#[cfg(feature = "python")]
fn py_num(obj: &Bound<'_, PyAny>) -> Option<f64> {
    crate::ink_erase::int_or_float(obj)
}

#[cfg(feature = "python")]
fn py_geometry(obj: &Bound<'_, PyAny>) -> PyResult<Option<Vec<Option<f64>>>> {
    if !(obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>()) {
        return Ok(None);
    }
    Ok(Some(obj.try_iter()?.map(|v| v.map(|v| py_num(&v))).collect::<PyResult<_>>()?))
}

#[cfg(feature = "python")]
fn py_style(obj: &Bound<'_, PyAny>) -> PyResult<StyleIn> {
    let Ok(d) = obj.cast::<PyDict>() else {
        return Ok(StyleIn { is_dict: false, keys: vec![], color: None, width: None, fill: Fill::Absent });
    };
    let mut keys = Vec::new();
    for k in d.keys() {
        keys.push(k.extract::<String>().unwrap_or_else(|_| "\u{0}<non-str>".into()));
    }
    let color = d.get_item("colorARGB")?.and_then(|v| crate::ink_erase::exact_int(&v));
    let width = d.get_item("width")?.and_then(|v| py_num(&v));
    let fill = match d.get_item("fillARGB")? {
        None => Fill::Absent,
        Some(v) if v.is_none() => Fill::Absent,
        Some(v) => match crate::ink_erase::exact_int(&v) {
            Some(i) => Fill::Int(i),
            None => Fill::Bad,
        },
    };
    Ok(StyleIn { is_dict: true, keys, color, width, fill })
}

#[cfg(feature = "python")]
fn shape_dict<'py>(py: Python<'py>, s: &ShapeInfo) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("id", &s.id)?;
    d.set_item("kind", &s.kind)?;
    d.set_item("geometry", PyList::new(py, s.geometry)?)?;
    d.set_item("colorARGB", s.color_argb)?;
    d.set_item("width", s.width)?;
    d.set_item("fillARGB", s.fill_argb)?;
    Ok(d)
}

#[cfg(feature = "python")]
fn style_dict<'py>(py: Python<'py>, s: &Shape) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("colorARGB", s.color_argb)?;
    d.set_item("width", s.width)?;
    d.set_item("fillARGB", s.fill_argb)?;
    Ok(d)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "quantize")]
fn py_quantize(value: f64) -> f64 {
    quantize(value)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "jfloat")]
fn py_jfloat(value: f64) -> String {
    jfloat(value)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "signed")]
fn py_signed(argb: i128) -> i128 {
    signed(argb)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "normalize_shape")]
fn py_normalize_shape<'py>(kind: &Bound<'py, PyAny>, geometry: &Bound<'py, PyAny>, style: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = kind.py();
    let s = normalize_shape(py_kind(kind).as_deref(), py_geometry(geometry)?.as_deref(), &py_style(style)?).py(py)?;
    PyTuple::new(py, [s.kind.clone().into_pyobject(py)?.into_any(), PyTuple::new(py, s.geometry)?.into_any(), style_dict(py, &s)?.into_any()])
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "shape_path")]
fn py_shape_path<'py>(kind: &Bound<'py, PyAny>, geometry: Vec<f64>) -> PyResult<Bound<'py, PyList>> {
    let py = kind.py();
    let g: [f64; 4] = geometry.try_into().map_err(|_| pyo3::exceptions::PyValueError::new_err("expected 4 values to unpack"))?;
    let out = PyList::empty(py);
    for s in shape_path(&py_kind(kind).unwrap_or_default(), g) {
        let mut items: Vec<Bound<PyAny>> = vec![s.kind.into_pyobject(py)?.into_any()];
        for c in s.coords {
            items.push(c.into_pyobject(py)?.into_any());
        }
        out.append(PyTuple::new(py, items)?)?;
    }
    Ok(out)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "build_shape_object", signature = (kind, geometry, style, *, object_id, tick, owner))]
fn py_build_shape_object<'py>(kind: &Bound<'py, PyAny>, geometry: Vec<f64>, style: &Bound<'py, PyAny>, object_id: String, tick: i64, owner: (i64, i64)) -> PyResult<Bound<'py, PyTuple>> {
    let py = kind.py();
    let g: [f64; 4] = geometry.try_into().map_err(|_| pyo3::exceptions::PyValueError::new_err("expected 4 values to unpack"))?;
    let width: f64 = style.get_item("width")?.extract()?;
    let color: i128 = style.get_item("colorARGB")?.extract()?;
    let fill: Option<i128> = style.call_method1("get", ("fillARGB",))?.extract()?;
    let (node, d) = build_shape_object(&py_kind(kind).unwrap_or_default(), g, color as u32, width, fill.map(|f| f as u32), &object_id, tick, owner).py(py)?;
    PyTuple::new(py, [bridge::to_et(py, &node)?, PyTuple::new(py, d)?.into_any()])
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "read_shape_object")]
fn py_read_shape_object<'py>(node: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
    let py = node.py();
    let element = match bridge::from_et(node) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    read_shape_object(&element).map(|s| shape_dict(py, &s)).transpose()
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "prepare_add_shape", signature = (snapshot, page_id, kind, geometry, style, now_ms, *, new_object_id=None))]
#[allow(clippy::too_many_arguments)]
fn py_prepare_add_shape<'py>(
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    kind: &Bound<'py, PyAny>,
    geometry: &Bound<'py, PyAny>,
    style: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    new_object_id: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let new_id = match &new_object_id {
        Some(o) if !o.is_none() => Some(py_kind(o)),
        _ => None,
    };
    let page = py_kind(page_id);
    let e = prepare_add_shape(
        &deps,
        &bridge::opaque(snapshot),
        page.as_deref(),
        py_kind(kind).as_deref(),
        py_geometry(geometry)?.as_deref(),
        &py_style(style)?,
        crate::ink_erase::exact_int(now_ms),
        new_id.as_ref().map(|x| x.as_deref()),
    )
    .py(py)?;
    let snap = bridge::handle(py, &e.snapshot.handle)?;
    let obj = PyShapeEdit {
        full_xml: PyBytes::new(py, &e.full_xml).into_any().unbind(),
        tree_xml: PyBytes::new(py, &e.tree_xml).into_any().unbind(),
        upload_xml: PyBytes::new(py, &e.upload_xml).into_any().unbind(),
        requested_ids: bridge::frozenset_str(py, &e.requested_ids)?.unbind(),
        page_id: e.page_id.into_pyobject(py)?.into_any().unbind(),
        layer_id: e.layer_id.into_pyobject(py)?.into_any().unbind(),
        object_id: e.object_id.into_pyobject(py)?.into_any().unbind(),
        modified_time: e.modified_time.into_pyobject(py)?.into_any().unbind(),
        title: snap.getattr("title")?.unbind(),
        body: snap.getattr("body")?.unbind(),
        content_file_ids: bridge::tuple_str(py, &e.content_file_ids)?.into_any().unbind(),
        kind: e.kind.into_pyobject(py)?.into_any().unbind(),
        recipe_version: SHAPE_RECIPE.into_pyobject(py)?.into_any().unbind(),
        binary_parts: PyTuple::empty(py).into_any().unbind(),
    };
    Ok(Bound::new(py, obj)?.into_any())
}

#[cfg(feature = "python")]
/// The `project_shapes` dict (key order as the Python module builds it).
pub fn state_to_py<'py>(py: Python<'py>, s: &ShapesState) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("schemaVersion", 1)?;
    d.set_item("kinds", PyList::new(py, KINDS)?)?;
    let pages = PyList::empty(py);
    for p in &s.pages {
        let e = PyDict::new(py);
        e.set_item("id", &p.id)?;
        e.set_item("index", p.index)?;
        let shapes = PyList::empty(py);
        for sh in &p.shapes {
            shapes.append(shape_dict(py, sh)?)?;
        }
        e.set_item("shapes", shapes)?;
        if let Some((w, h)) = p.size {
            e.set_item("width", w)?;
            e.set_item("height", h)?;
        }
        e.set_item("addShape", p.add_shape)?;
        e.set_item("reason", &p.reason)?;
        pages.append(e)?;
    }
    d.set_item("pages", pages)?;
    d.set_item("reason", &s.reason)?;
    let rendering = PyDict::new(py);
    rendering.set_item("nativeEquivalent", false)?;
    rendering.set_item("mode", "geometry-only")?;
    d.set_item("rendering", rendering)?;
    Ok(d)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "project_shapes")]
fn py_project_shapes<'py>(snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let s = project_shapes(&deps, &bridge::opaque(snapshot)).py(py)?;
    state_to_py(py, &s)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("ShapeError", bridge::shape_error(m.py())?)?;
    m.add_class::<PyShapeEdit>()?;
    m.add_function(wrap_pyfunction!(py_quantize, m)?)?;
    m.add_function(wrap_pyfunction!(py_jfloat, m)?)?;
    m.add_function(wrap_pyfunction!(py_signed, m)?)?;
    m.add_function(wrap_pyfunction!(py_normalize_shape, m)?)?;
    m.add_function(wrap_pyfunction!(py_shape_path, m)?)?;
    m.add_function(wrap_pyfunction!(py_build_shape_object, m)?)?;
    m.add_function(wrap_pyfunction!(py_read_shape_object, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_add_shape, m)?)?;
    m.add_function(wrap_pyfunction!(py_project_shapes, m)?)?;
    rs_common::export(
        m,
        &[
            "ShapeError",
            "ShapeEdit",
            "quantize",
            "jfloat",
            "signed",
            "normalize_shape",
            "shape_path",
            "build_shape_object",
            "read_shape_object",
            "prepare_add_shape",
            "project_shapes",
        ],
    )
}
