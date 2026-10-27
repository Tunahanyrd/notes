//! Replacement for the Python module `ink_render`.
//!
//! Page admission and renderer input mirror the Python code exactly (the
//! stroke decoder, metadata binding, resource verification and rich-text
//! reader still belong to other areas and are called through PYFALLBACKs).
//! Speed comes from: (1) the verified resource and decoded strokes are
//! cached per exact snapshot, so inventory and every page share one decode;
//! (2) one preparation child per note emits the inventory, the requested
//! page first and then every other page body, which the parent caches;
//! (3) renderer/rasterizer run in the same bwrap sandbox and limits but are
//! spawned directly (no Python worker interpreters); (4) the rasterized PNG
//! is canonicalised in Rust instead of a per-byte Python loop.
use crate::pixels as px;
#[cfg(feature = "python")]
use crate::preview_worker::encode;
#[cfg(feature = "python")]
use crate::pyhelp::{self, kwargs};
use crate::sandbox;
use crate::xarea;
#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

const MODULE: &str = "samsung_notes.ink_render";
pub const MAX_INPUT: usize = 24 * 1024 * 1024;
pub const MAX_SVG: usize = 16 * 1024 * 1024;
pub const MAX_PNG: usize = 32 * 1024 * 1024;
const MAX_DISTANCE: f64 = 500_000.0;
const FOUNTAIN: &str = "com.samsung.android.sdk.pen.pen.preload.FountainPen";
const INKPEN2: &str = "com.samsung.android.sdk.pen.pen.preload.InkPen2";
const MARKER4: &str = "com.samsung.android.sdk.pen.pen.preload.Marker4";
const MARKER4_OFF: [&str; 9] =
    ["replay_only", "eraser", "fixed_width", "alpha_lock", "generated", "fixed_opacity", "rainbow_effect", "straighten", "reveal_mode"];
const PROPS: [&str; 15] = ["compressed", "replay_only", "stylus_channels", "eraser", "fixed_width", "millisecond_timestamps",
    "top_layer_pen", "alpha_lock", "binary_added", "unused9", "generated", "fixed_opacity", "rainbow_effect", "straighten", "reveal_mode"];
const INKPEN2_OFF: [&str; 11] = ["replay_only", "eraser", "fixed_width", "millisecond_timestamps", "top_layer_pen", "alpha_lock",
    "generated", "fixed_opacity", "rainbow_effect", "straighten", "reveal_mode"];
const STYLE: [(&str, &str); 23] = [("legacyPenNameId", "legacy_pen_name_id"), ("advancedSettingId", "advanced_pen_setting_id"),
    ("colorARGB", "color_argb"), ("penSize", "pen_size"), ("field4", "field_4_raw"), ("penNameId", "pen_name_id"),
    ("fixedWidth", "fixed_width"), ("sizeLevel", "size_level"), ("particleDensity", "particle_density"),
    ("renderingLevel", "rendering_level"), ("originalWidth", "original_width"), ("initialTolerance", "initial_tolerance"),
    ("lineType", "line_type_raw"), ("dashOffset", "dash_offset"), ("strokeType", "stroke_type_raw"),
    ("penRepeatDistance", "pen_repeat_distance"), ("particleSize", "particle_size"), ("patternIndex", "pattern_index"),
    ("patternScale", "pattern_scale"), ("particleLevel", "particle_level"), ("rainbowDistance", "rainbow_distance"),
    ("rainbowOffset", "rainbow_offset"), ("colorType", "color_type_raw")];
const PAGE_ATTRS: [&str; 19] = ["id", "hash", "isTextOnly", "orientation", "width", "height", "offsetX", "offsetY", "formatVersion",
    "bgImageMode", "bgImageId", "bgColor", "minFormatVersion", "currentLayerIndex", "createdTime", "modifiedTime", "bgWidth",
    "editableFormatVersion", "id"];
const BODY_ATTRS: [&str; 37] = ["id", "type", "shapeType", "originalDegree", "leftMargin", "topMargin", "rightMargin", "bottomMargin",
    "textGravity", "createdTime", "modifiedTime", "hash", "formatVersion", "haveBeenToAtt", "isHintTextVisible", "isHorizontalFlipped",
    "isReplayable", "isTemplateObject", "isTextEditable", "isTextReadOnly", "isTransparency", "isVerticalFlipped", "lockState",
    "movable", "removable", "resizable", "rotatable", "selectable", "visible", "clippable", "flippable", "ellipsisType",
    "textAutoFit", "timeStamp", "ownerPageWidth", "ownerPageHeight", "id"];
const FLOW_SPANS: [&str; 7] = ["bold", "italic", "underline", "strikethrough", "fontSize", "foregroundColor", "backgroundColor"];

// ---------------------------------------------------------------- errors / records

#[cfg(feature = "python")]
#[pyclass(extends=PyValueError, subclass, module="samsung_notes.ink_render", name="InkRenderError")]
pub struct InkRenderError {}

#[cfg(feature = "python")]
#[pymethods]
impl InkRenderError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        InkRenderError {}
    }
}

#[cfg(feature = "python")]
pub fn ire(py: Python<'_>, message: &str) -> PyErr {
    match py.get_type::<InkRenderError>().call1((message,)) {
        Ok(v) => PyErr::from_value(v),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
fn is_ire(py: Python<'_>, e: &PyErr) -> bool {
    e.is_instance(py, &py.get_type::<InkRenderError>())
}

#[cfg(feature = "python")]
#[pyclass(frozen, module="samsung_notes.ink_render", name="_PageInput", get_all)]
pub struct PageInput {
    index: i64,
    width: i64,
    height: i64,
    dark_mode: bool,
    body: Py<PyBytes>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PageInput {
    #[cfg(feature = "python")]
    #[new]
    fn new(index: i64, width: i64, height: i64, dark_mode: bool, body: Py<PyBytes>) -> Self {
        PageInput { index, width, height, dark_mode, body }
    }

    #[cfg(feature = "python")]
    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        let Ok(o) = other.cast::<PageInput>() else { return Ok(false) };
        let o = o.get();
        Ok(self.index == o.index && self.width == o.width && self.height == o.height && self.dark_mode == o.dark_mode
            && self.body.bind(py).as_bytes() == o.body.bind(py).as_bytes())
    }

    #[cfg(feature = "python")]
    fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
        (self.index, self.width, self.height, self.dark_mode, self.body.clone_ref(py)).into_pyobject(py)?.hash()
    }
}

#[cfg(feature = "python")]
impl PageInput {
    pub fn index(&self) -> i64 { self.index }
    pub fn width(&self) -> i64 { self.width }
    pub fn height(&self) -> i64 { self.height }
    pub fn dark_mode(&self) -> bool { self.dark_mode }
    #[cfg(feature = "python")]
    pub fn body_bytes<'py>(&self, py: Python<'py>) -> &Bound<'py, PyBytes> { self.body.bind(py) }
}

// ---------------------------------------------------------------- small helpers

#[cfg(feature = "python")]
fn g<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    pyhelp::global(py, MODULE, name)
}

#[cfg(feature = "python")]
fn tag(el: &Bound<'_, PyAny>) -> PyResult<String> {
    let t = el.getattr("tag")?;
    Ok(match t.extract::<String>() {
        Ok(s) => s,
        Err(_) => t.str()?.to_string(),
    })
}

#[cfg(feature = "python")]
fn get(el: &Bound<'_, PyAny>, name: &str) -> PyResult<Option<String>> {
    el.call_method1("get", (name,))?.extract()
}

#[cfg(feature = "python")]
fn kids<'py>(el: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    el.try_iter()?.collect()
}

#[cfg(feature = "python")]
fn iter_all<'py>(el: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    el.call_method0("iter")?.try_iter()?.collect()
}

#[cfg(feature = "python")]
fn iter_tag<'py>(el: &Bound<'py, PyAny>, t: &str) -> PyResult<Vec<Bound<'py, PyAny>>> {
    el.call_method1("iter", (t,))?.try_iter()?.collect()
}

#[cfg(feature = "python")]
fn attr_keys(el: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    el.getattr("attrib")?.try_iter()?.map(|k| k?.extract()).collect()
}

fn same_set(keys: &[String], wanted: &[&str]) -> bool {
    let mut a: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    a.sort();
    a.dedup();
    let mut b: Vec<&str> = wanted.to_vec();
    b.sort();
    b.dedup();
    a == b
}

#[cfg(feature = "python")]
fn pyfloat(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<f64> {
    py.import("builtins")?.getattr("float")?.call1((value,))?.extract()
}

#[cfg(feature = "python")]
fn pyint<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    py.import("builtins")?.getattr("int")?.call1((value,))
}

#[cfg(feature = "python")]
fn integer(py: Python<'_>, value: Option<String>) -> PyResult<i64> {
    xarea::integer(py, value.as_deref())
}

#[cfg(feature = "python")]
/// document.one: the single child whose local, lowercased tag is `name`.
fn one<'py>(py: Python<'py>, parent: &Bound<'py, PyAny>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    let mut found = Vec::new();
    for child in kids(parent)? {
        let t = tag(&child)?;
        if t.rsplit('}').next().unwrap_or("").to_lowercase() == name {
            found.push(child);
        }
    }
    if found.len() != 1 {
        return Err(xarea::document_error(py, "Belgenin yapısı güvenli düzenleme için uygun değil."));
    }
    Ok(found.pop().unwrap())
}

// ---------------------------------------------------------------- admission (native, rs-ink core)

use rs_ink::core::binary::{Record, StyleVal};
use rs_ink::core::num::py_repr;
use rs_ink::core::prepare::Binding;
use rs_ink::core::InkErr;

/// ink_render._props: property bits of the stroke frame (PROPS order).
fn props(record: &Record) -> Vec<(&'static str, bool)> {
    let raw = record.frames.get(1).map(|f| f.properties.as_slice()).unwrap_or(&[]);
    PROPS.iter().enumerate().filter(|(_, n)| **n != "unused9").map(|(i, name)| {
        let bit = raw.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 == 1);
        (*name, if *name == "binary_added" || *name == "generated" { !bit } else { bit })
    }).collect()
}

fn prop(props: &[(&'static str, bool)], name: &str) -> bool {
    props.iter().any(|(n, v)| *n == name && *v)
}

fn style_num(v: Option<&StyleVal>) -> Option<f64> {
    match v {
        None => Some(0.0),
        Some(StyleVal::I(i)) => Some(*i as f64),
        Some(StyleVal::F(f)) => Some(*f),
        Some(StyleVal::G(_)) => None,
    }
}

/// ink_render._admitted_pen.
fn admitted(record: &Record, binding: &Binding) -> bool {
    let settings = binding.advanced_settings.as_deref();
    let tool = record.tool_type;
    if binding.pen_name == FOUNTAIN {
        return matches!(settings, Some("14;") | Some("18;0;100;")) && (1..=3).contains(&tool);
    }
    if binding.pen_name == MARKER4 {
        // Highlighter: upstream's Marker4 V7/V8 rounded-rectangle approximation
        // (translucent, top pass). Tool 3 takes the stylus path.
        let p = props(record);
        let style = &binding.style;
        let zero = |k: &str| style_num(style.get(k)) == Some(0.0);
        let size = style_num(style.get("penSize"));
        let count = record.samples.points.len();
        return matches!(settings, Some("7;") | Some("8;"))
            && matches!(tool, 0 | 2 | 3)
            && prop(&p, "binary_added")
            && !MARKER4_OFF.iter().any(|n| prop(&p, n))
            && zero("strokeType")
            && zero("lineType")
            && style.get("gradientColorsARGB").is_none()
            && size.is_some_and(|v| 0.0 < v && v <= 800.0)
            && (2..=65536).contains(&count);
    }
    if binding.pen_name != INKPEN2 || !matches!(settings, Some("8;") | Some("9;")) || tool != 2 {
        return false;
    }
    let p = props(record);
    if !prop(&p, "stylus_channels") || !prop(&p, "binary_added") {
        return false;
    }
    if INKPEN2_OFF.iter().any(|n| *n != "millisecond_timestamps" && prop(&p, n)) {
        return false;
    }
    let style = &binding.style;
    let is = |k: &str, options: &[f64]| style_num(style.get(k)).is_some_and(|v| options.contains(&v));
    let tolerance = style_num(style.get("initialTolerance"));
    let count = record.samples.points.len();
    is("strokeType", &[0.0]) && is("lineType", &[0.0]) && is("colorType", &[0.0, 1.0, 2.0])
        && tolerance.is_some_and(|t| (0.0..=1.0).contains(&t)) && (2..=65536).contains(&count)
}

fn json_str(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
        }
    }
    out.push('"');
}

enum JsonErr {
    Ink(InkErr),
    Render(&'static str),
    Value(String),
    Key(&'static str),
}

fn json_float(out: &mut String, v: f64) -> Result<(), JsonErr> {
    if !v.is_finite() {
        return Err(JsonErr::Value(format!("Out of range float values are not JSON compliant: {}", py_repr(v))));
    }
    out.push_str(&py_repr(v));
    Ok(())
}

fn json_style(out: &mut String, v: &StyleVal) -> Result<(), JsonErr> {
    match v {
        StyleVal::I(i) => out.push_str(&i.to_string()),
        StyleVal::F(f) => json_float(out, *f)?,
        StyleVal::G(g) => {
            out.push('[');
            for (i, c) in g.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&c.to_string());
            }
            out.push(']');
        }
    }
    Ok(())
}

fn json_floats(out: &mut String, values: &[f64]) -> Result<(), JsonErr> {
    out.push('[');
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_float(out, *v)?;
    }
    out.push(']');
    Ok(())
}

/// ink_render._stroke_input, emitted straight to compact JSON (the bytes
/// `json.dumps(..., separators=(',',':'))` produces). Returns the path length.
fn stroke_json(out: &mut String, record: &Record, binding: &Binding, width: i64, height: i64) -> Result<f64, JsonErr> {
    use rs_ink::core::num::{f32r, Val};
    let f = |v: f64| f32r(v).map_err(JsonErr::Ink);
    let scale = rs_ink::core::coords::page_coordinate_scale(&record.frames[0], Val::Int(width as i128), Val::Int(height as i128), Val::Int(0))
        .map_err(JsonErr::Ink)?;
    let mut points = Vec::with_capacity(record.samples.points.len());
    for (x, y) in &record.samples.points {
        points.push((f(f(*x)? * scale)?, f(f(*y)? * scale)?));
    }
    if points.len() < 2 {
        return Err(JsonErr::Render("Tek noktalı çizginin görünümü henüz doğrulanmadı."));
    }
    let p = props(record);
    let mut style: Vec<(&'static str, StyleVal)> = Vec::new();
    for (k, v) in &binding.style.0 {
        let Some((_, mapped)) = STYLE.iter().find(|(a, _)| a == k) else {
            return Err(JsonErr::Render("Bilinmeyen kalem ayarı."));
        };
        style.push((mapped, v.clone()));
    }
    for field in ["pen_size", "fixed_width", "initial_tolerance"] {
        if let Some(entry) = style.iter_mut().find(|(k, _)| *k == field) {
            let value = match &entry.1 {
                StyleVal::I(i) => *i as f64,
                StyleVal::F(v) => *v,
                StyleVal::G(_) => return Err(JsonErr::Ink(InkErr::Ink(rs_ink::core::num::UNREPRESENTABLE.into()))),
            };
            entry.1 = StyleVal::F(f(f(value)? * scale)?);
        }
    }
    let get = |k: &str| style.iter().find(|(n, _)| *n == k).map(|(_, v)| v);
    if record.samples.timestamps.iter().any(|t| *t < 0 || *t >= 1 << 31) {
        return Err(JsonErr::Render("Çizgi zaman profili desteklenmiyor."));
    }
    if prop(&p, "fixed_width") && !style_num(get("fixed_width")).is_some_and(|v| get("fixed_width").is_some() && (0.0..=1024.0).contains(&v)) {
        return Err(JsonErr::Render("Sabit kalem genişliği desteklenmiyor."));
    }
    if !style_num(get("initial_tolerance")).is_some_and(|v| (0.0..=1000.0).contains(&v)) {
        return Err(JsonErr::Render("Kalem toleransı desteklenmiyor."));
    }
    let mut bounds = [0.0; 4];
    for (i, v) in binding.bounds.iter().enumerate() {
        bounds[i] = f(f(*v)? * scale)?;
    }
    let argb = match get("color_argb") {
        Some(StyleVal::I(v)) => *v,
        Some(StyleVal::F(v)) => *v as i64,
        _ => return Err(JsonErr::Key("color_argb")),
    };
    let pen_width = get("pen_size").cloned().ok_or(JsonErr::Key("pen_size"))?;
    out.push_str("{\"rendering\":{\"metadata\":null,\"pen_name\":");
    json_str(out, &binding.pen_name);
    out.push_str(",\"advanced_settings\":");
    match &binding.advanced_settings {
        Some(v) => json_str(out, v),
        None => out.push_str("null"),
    }
    out.push_str(&format!(",\"tool_type_raw\":{},\"properties\":{{", record.tool_type));
    for (i, (name, value)) in p.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{name}\":{value}"));
    }
    out.push_str("},\"style\":{");
    for (name, value) in &style {
        out.push_str(&format!("\"{name}\":"));
        json_style(out, value)?;
        out.push(',');
    }
    out.push_str("\"trailing_data\":[],\"first_unparsed_field\":null}},\"bbox\":{");
    for (i, name) in ["x_min", "y_min", "x_max", "y_max"].iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{name}\":"));
        json_float(out, bounds[i])?;
    }
    out.push_str("},\"points\":[");
    let mut distance = 0.0;
    for (i, (x, y)) in points.iter().enumerate() {
        if i > 0 {
            out.push(',');
            let (px_, py_) = points[i - 1];
            distance += (x - px_).hypot(y - py_);
        }
        out.push_str("{\"x\":");
        json_float(out, *x)?;
        out.push_str(",\"y\":");
        json_float(out, *y)?;
        out.push('}');
    }
    out.push_str("],\"pressures\":");
    json_floats(out, &record.samples.pressures)?;
    out.push_str(",\"timestamps\":[");
    out.push_str(&record.samples.timestamps.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(","));
    out.push_str("],\"tilts\":");
    json_floats(out, &record.samples.tilts)?;
    out.push_str(",\"orientations\":");
    json_floats(out, &record.samples.orientations)?;
    out.push_str(&format!(",\"color\":{{\"r\":{},\"g\":{},\"b\":{}}},\"pen_width\":", (argb >> 16) & 255, (argb >> 8) & 255, argb & 255));
    json_style(out, &pen_width)?;
    out.push('}');
    Ok(distance)
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _blank_text(py: Python<'_>, node: &Bound<'_, PyAny>) -> PyResult<bool> {
    let text = node.getattr("text")?;
    let text = if text.is_truthy()? { text } else { PyString::new(py, "").into_any() };
    let value = text.call_method0("strip")?;
    if !value.is_truthy()? {
        return Ok(true);
    }
    let base64 = py.import("base64")?;
    let decode = base64.getattr("b64decode")?;
    let validate = kwargs(py, &[("validate", true.into_pyobject(py)?.to_owned().into_any())])?;
    let attempt = (|| -> PyResult<bool> {
        let inner = decode.call((&value,), Some(&validate))?;
        let raw = decode.call((inner,), Some(&validate))?;
        Ok(!raw.call_method1("decode", ("utf-8",))?.call_method0("strip")?.is_truthy()?)
    })();
    match attempt {
        Ok(v) => Ok(v),
        Err(e) if e.is_instance_of::<PyValueError>(py) => Ok(false), // binascii.Error / UnicodeDecodeError
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
fn zero(py: Python<'_>, values: &Bound<'_, PyAny>) -> PyResult<bool> {
    for v in values.call_method0("values")?.try_iter()? {
        if pyfloat(py, &v?)? != 0.0 {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _empty_body_for_ink(py: Python<'_>, body: &Bound<'_, PyAny>) -> PyResult<bool> {
    const BASE: [&str; 7] = ["bodyText", "object", "rect", "originalRect", "text", "paragraph", "parsingState"];
    const SPANS: [&str; 4] = ["spanOrderDataList", "spanOrderData", "zeroLengthSpanList", "zeroLengthSpan"];
    const EFFECTS: [&str; 3] = ["lineColorEffect", "radialGradientPosition", "lineStyleEffect"];
    let all = iter_all(body)?;
    for n in &all {
        let t = tag(n)?;
        if !BASE.contains(&t.as_str()) && !SPANS.contains(&t.as_str()) && !EFFECTS.contains(&t.as_str()) {
            return Ok(false);
        }
    }
    for n in iter_tag(body, "text")? {
        if !_blank_text(py, &n)? {
            return Ok(false);
        }
        for c in kids(&n)? {
            if !SPANS.contains(&tag(&c)?.as_str()) {
                return Ok(false);
            }
        }
    }
    let present = all.iter().filter(|n| tag(n).is_ok_and(|t| EFFECTS.contains(&t.as_str()))).count();
    if present == 0 {
        return Ok(true);
    }
    let children = kids(body)?;
    if children.len() != 1 || tag(&children[0])? != "object" || get(&children[0], "type")?.as_deref() != Some("textBox") {
        return Ok(false);
    }
    let obj = &children[0];
    if get(obj, "shapeType")?.as_deref() != Some("4") || !matches!(get(obj, "originalDegree")?.as_deref(), Some("0") | Some("0.0")) {
        return Ok(false);
    }
    for t in ["rect", "originalRect"] {
        let nodes: Vec<Bound<PyAny>> = obj.call_method1("findall", (t,))?.try_iter()?.collect::<PyResult<_>>()?;
        if nodes.len() != 1 || !same_set(&attr_keys(&nodes[0])?, &["l", "t", "r", "b"]) || nodes[0].len()? != 0 {
            return Ok(false);
        }
        if !zero(py, &nodes[0].getattr("attrib")?)? {
            return Ok(false);
        }
    }
    let color: Vec<Bound<PyAny>> = obj.call_method1("findall", ("lineColorEffect",))?.try_iter()?.collect::<PyResult<_>>()?;
    let style: Vec<Bound<PyAny>> = obj.call_method1("findall", ("lineStyleEffect",))?.try_iter()?.collect::<PyResult<_>>()?;
    if color.len() != 1 || style.len() != 1 || present != 3 {
        return Ok(false);
    }
    let (color, style) = (&color[0], &style[0]);
    if !same_set(&attr_keys(color)?, &["angle", "colorType", "gradientRotatable", "gradientType", "solidColor"]) {
        return Ok(false);
    }
    if get(color, "angle")?.as_deref() != Some("0") || get(color, "colorType")?.as_deref() != Some("2")
        || get(color, "gradientType")?.as_deref() != Some("0")
        || color.call_method1("get", ("gradientRotatable", ""))?.call_method0("lower")?.ne("false")?
    {
        return Ok(false);
    }
    let value = pyint(py, &color.call_method1("get", ("solidColor",))?)?;
    if !(value.ge(-(1i64 << 31))? && value.lt(1i64 << 32)?) {
        return Ok(false);
    }
    let inner = kids(color)?;
    if inner.len() != 1 || tag(&inner[0])? != "radialGradientPosition" || !same_set(&attr_keys(&inner[0])?, &["x", "y"])
        || !zero(py, &inner[0].getattr("attrib")?)? || inner[0].len()? != 0
    {
        return Ok(false);
    }
    let fields = ["beginArrowSize", "beginArrowType", "capType", "compoundType", "dashType", "endArrowSize", "endArrowType", "joinType", "width"];
    if !same_set(&attr_keys(style)?, &fields) || !zero(py, &style.getattr("attrib")?)? || style.len()? != 0 {
        return Ok(false);
    }
    Ok(true)
}

// ---------------------------------------------------------------- body text flow

enum Item {
    Text(String),
    Raw(String),
    Tokens(i64, Vec<[Option<String>; 4]>),
}

#[cfg(feature = "python")]
/// Body text as renderer flow input; None when it paints nothing.
#[pyfunction]
pub fn _flow_body<'py>(py: Python<'py>, body: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
    let children = kids(body)?;
    if children.len() != 1 || tag(&children[0])? != "object" {
        return Err(ire(py, "Metin ve çizim birleşimi desteklenmiyor."));
    }
    let obj = &children[0];
    let keys = attr_keys(obj)?;
    if get(obj, "type")?.as_deref() != Some("textBox") || get(obj, "shapeType")?.as_deref() != Some("4")
        || !matches!(get(obj, "originalDegree")?.as_deref(), Some("0") | Some("0.0"))
        || obj.call_method1("get", ("textGravity", "0"))?.ne("0")? || keys.iter().any(|k| !BODY_ATTRS.contains(&k.as_str()))
    {
        return Err(ire(py, "Metin nesnesinin biçimi desteklenmiyor."));
    }
    let allowed = ["rect", "originalRect", "text", "lineColorEffect", "lineStyleEffect", "spanOrderDataList", "zeroLengthSpanList"];
    let obj_kids = kids(obj)?;
    for c in &obj_kids {
        if !allowed.contains(&tag(c)?.as_str()) {
            return Err(ire(py, "Metne gömülü nesne veya biçim desteklenmiyor."));
        }
    }
    let et = py.import("xml.etree.ElementTree")?;
    let shell = et.getattr("Element")?.call1(("bodyText",))?;
    let attrib = PyDict::new(py);
    attrib.update(obj.getattr("attrib")?.cast::<pyo3::types::PyMapping>()?)?;
    let copy = et.getattr("SubElement")?.call1((&shell, "object", attrib))?;
    for c in &obj_kids {
        let t = tag(c)?;
        if t != "text" && t != "spanOrderDataList" && t != "zeroLengthSpanList" {
            copy.call_method1("append", (c,))?;
        }
    }
    let count = |t: &str| -> PyResult<usize> { obj.call_method1("findall", (t,))?.len() };
    if !_empty_body_for_ink(py, &shell)? || count("rect")? != 1 || count("originalRect")? != 1 {
        return Err(ire(py, "Metin kutusu geometrisi desteklenmiyor."));
    }
    let texts: Vec<Bound<PyAny>> = obj.call_method1("findall", ("text",))?.try_iter()?.collect::<PyResult<_>>()?;
    if texts.len() != 1 || texts[0].len()? != 0 {
        return Err(ire(py, "Metin biçimi desteklenmiyor."));
    }
    let parsed = (|| -> PyResult<(Bound<'py, PyAny>, Vec<(i64, i64)>)> {
        let token_re = g(py, "_TOKEN")?;
        let raw_text = texts[0].getattr("text")?;
        let raw_text = if raw_text.is_truthy()? { raw_text } else { PyString::new(py, "").into_any() };
        let raw_text: String = raw_text.extract()?;
        let stream = xarea::editor_decode(py, &raw_text)?;
        let mut items: Vec<Item> = Vec::new();
        let (mut cursor, mut position) = (0usize, 0i64);
        while cursor < stream.len() {
            let marker = stream[cursor..].find("<?a").map(|i| i + cursor);
            let end = marker.unwrap_or(stream.len());
            let chunk = &stream[cursor..end];
            items.push(Item::Text(chunk.to_string()));
            position += xarea::utf16_length(&xarea::editor_decode(py, chunk)?) as i64;
            let Some(marker) = marker else { break };
            let Some(close) = stream[marker + 3..].find("?>").map(|i| i + marker + 3) else {
                return Err(ire(py, "Metin biçimi desteklenmiyor."));
            };
            let body_tokens = &stream[marker + 3..close];
            let mut tokens = Vec::new();
            let mut joined = String::new();
            for m in token_re.call_method1("finditer", (body_tokens,))?.try_iter()? {
                let m = m?;
                let group = |i: i64| -> PyResult<Option<String>> { m.call_method1("group", (i,))?.extract() };
                let whole = group(0)?.unwrap_or_default();
                joined.push_str(&whole);
                tokens.push([Some(whole), group(1)?, group(2)?, group(3)?]);
            }
            items.push(if joined != body_tokens { Item::Raw(body_tokens.to_string()) } else { Item::Tokens(position, tokens) });
            cursor = close + 2;
        }
        let mut opens: HashMap<(String, String), Vec<(usize, usize, i64)>> = HashMap::new();
        let mut sections: Vec<(i64, i64)> = Vec::new();
        for i in 0..items.len() {
            let Item::Tokens(at, ref tokens) = items[i] else { continue };
            let count = tokens.len();
            for j in 0..count {
                let (group, kind, value) = match &items[i] {
                    Item::Tokens(_, t) => (t[j][1].clone().unwrap_or_default(), t[j][2].clone().unwrap_or_default(), t[j][3].clone()),
                    _ => unreachable!(),
                };
                if value.is_some() {
                    opens.entry((group, kind)).or_default().push((i, j, at));
                    continue;
                }
                let Some(stack) = opens.get_mut(&(group.clone(), kind.clone())) else { continue };
                let Some((si, sj, start)) = stack.pop() else { continue };
                let clear = if group == "sectionData" && kind == "sectionData" {
                    sections.push((start, at));
                    true
                } else {
                    group == "paragraph" && (kind == "parsingState" || start == at)
                };
                if clear {
                    for (a, b) in [(i, j), (si, sj)] {
                        if let Item::Tokens(_, t) = &mut items[a] {
                            t[b][0] = Some(String::new());
                        }
                    }
                }
            }
        }
        let mut out = String::new();
        for item in &items {
            match item {
                Item::Text(s) => out.push_str(s),
                Item::Raw(s) => {
                    out.push_str("<?a");
                    out.push_str(s);
                    out.push_str("?>");
                }
                Item::Tokens(_, t) => {
                    out.push_str("<?a");
                    for token in t {
                        out.push_str(token[0].as_deref().unwrap_or(""));
                    }
                    out.push_str("?>");
                }
            }
        }
        let mut sidecars = Vec::new();
        for c in &obj_kids {
            let t = tag(c)?;
            if t == "spanOrderDataList" || t == "zeroLengthSpanList" {
                sidecars.push(c.clone());
            }
        }
        Ok((xarea::object_rich_model(py, xarea::editor_b64(&out), &sidecars)?, sections))
    })();
    let (model, sections) = match parsed {
        Ok(v) => v,
        Err(e) if is_ire(py, &e) => return Err(e),
        Err(e) if e.is_instance_of::<PyValueError>(py) || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
            || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py) || e.is_instance_of::<pyo3::exceptions::PyOverflowError>(py) => {
            let error = ire(py, "Metin biçimi desteklenmiyor.");
            error.set_cause(py, Some(e));
            return Err(error);
        }
        Err(e) => return Err(e),
    };
    let text = model.get_item("text")?;
    let spans_in = model.get_item("spans")?;
    let paragraphs_in = model.get_item("paragraphs")?;
    if !text.call_method1("strip", (" \t\r\n",))?.is_truthy()? {
        let mut quiet = true;
        for s in spans_in.try_iter()? {
            let kind: String = s?.get_item("kind")?.extract()?;
            if !["fontSize", "foregroundColor", "bold", "italic"].contains(&kind.as_str()) {
                quiet = false;
            }
        }
        for p in paragraphs_in.try_iter()? {
            for k in p?.try_iter()? {
                let k: String = k?.extract()?;
                if k != "index" && k != "alignment" {
                    quiet = false;
                }
            }
        }
        if quiet {
            return Ok(None);
        }
        return Err(ire(py, "Boş satırlardaki metin biçimi desteklenmiyor."));
    }
    let structs = py.import("struct")?;
    let pack = structs.getattr("pack")?;
    let span_types = xarea::class(py, "samsung_notes.editor_document", "SPAN_TYPES")?;
    let alignments = xarea::class(py, "samsung_notes.editor_document", "ALIGNMENTS")?;
    let as_list = |payload: Bound<'py, PyAny>| -> PyResult<Bound<'py, PyList>> { PyList::new(py, payload.try_iter()?.collect::<PyResult<Vec<_>>>()?) };
    let spans = PyList::empty(py);
    for s in spans_in.try_iter()? {
        let s = s?;
        let kind: String = s.get_item("kind")?.extract()?;
        if !FLOW_SPANS.contains(&kind.as_str()) {
            return Err(ire(py, "Metin biçimi desteklenmiyor."));
        }
        let value = s.get_item("value")?;
        let payload = if kind == "fontSize" {
            pack.call1(("<f", value))?
        } else if kind == "foregroundColor" || kind == "backgroundColor" {
            let hex = value.get_item(pyo3::types::PySlice::new(py, 1, isize::MAX, 1))?;
            pack.call1(("<I", py.import("builtins")?.getattr("int")?.call1((hex, 16))?))?
        } else {
            pack.call1(("<I", pyint(py, &value)?))?
        };
        let item = PyDict::new(py);
        item.set_item("kind", span_types.get_item(&kind)?)?;
        item.set_item("start", s.get_item("start")?)?;
        item.set_item("end", s.get_item("end")?)?;
        item.set_item("interval", s.get_item("interval")?)?;
        item.set_item("payload", as_list(payload)?)?;
        spans.append(item)?;
    }
    let paragraphs = PyList::empty(py);
    for p in paragraphs_in.try_iter()? {
        let p = p?;
        for k in p.try_iter()? {
            let k: String = k?.extract()?;
            if !["index", "alignment", "parsingState"].contains(&k.as_str()) {
                return Err(ire(py, "Paragraf biçimi desteklenmiyor."));
            }
        }
        let index = p.get_item("index")?;
        let make = |kind: i64, value: Bound<'py, PyAny>| -> PyResult<Bound<'py, PyDict>> {
            let item = PyDict::new(py);
            item.set_item("kind", kind)?;
            item.set_item("start", &index)?;
            item.set_item("end", index.add(1)?)?;
            item.set_item("payload", as_list(pack.call1(("<I", value))?)?)?;
            Ok(item)
        };
        if p.contains("alignment")? {
            paragraphs.append(make(3, alignments.get_item(p.get_item("alignment")?)?)?)?;
        }
        if p.contains("parsingState")? {
            paragraphs.append(make(6, pyint(py, &p.get_item("parsingState")?)?)?)?;
        }
    }
    let mut margins = Vec::new();
    for k in ["leftMargin", "topMargin", "rightMargin", "bottomMargin"] {
        margins.push(pyfloat(py, &obj.call_method1("get", (k, "0"))?)?);
    }
    if margins.iter().any(|v| !v.is_finite() || !(0.0..=1000.0).contains(v)) {
        return Err(ire(py, "Metin kutusu yerleşimi desteklenmiyor."));
    }
    let mut sections = sections;
    sections.sort();
    let sections: Vec<(i64, i64)> = sections.into_iter().map(|(s, e)| (s, e - s)).collect();
    let out = PyDict::new(py);
    out.set_item("text", text)?;
    out.set_item("spans", spans)?;
    out.set_item("paragraphs", paragraphs)?;
    out.set_item("sections", PyList::new(py, sections)?)?;
    out.set_item("margins", margins)?;
    out.set_item("gravity", 0)?;
    Ok(Some(out))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _page_color(py: Python<'_>, page: &Bound<'_, PyAny>) -> PyResult<u32> {
    let raw = page.call_method1("get", ("bgColor", ""))?;
    let text: String = raw.extract().unwrap_or_default();
    if text.is_empty() || text.chars().count() > 11 || raw.call_method0("strip")?.ne(&raw)? {
        return Err(ire(py, "Geçersiz sayfa rengi."));
    }
    let color = pyint(py, &raw)?;
    if !(color.ge(-(1i64 << 31))? && color.lt(1i64 << 32)?) {
        return Err(ire(py, "Geçersiz sayfa rengi."));
    }
    let color = (color.extract::<i64>()? & 0xffff_ffff) as u32;
    if color >> 24 != 255 {
        return Err(ire(py, "Sayfa arka planı opak değil."));
    }
    Ok(color)
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _flow_layout<'py>(py: Python<'py>, attrs: &Bound<'py, PyAny>, pages: &Bound<'py, PyAny>, body: &Bound<'py, PyDict>)
                         -> PyResult<(Bound<'py, PyDict>, Option<i64>)> {
    if get(attrs, "pageMode")?.as_deref() != Some("0") || attrs.call_method1("get", ("orientation", "0"))?.ne("0")? {
        return Err(ire(py, "Metin akış sayfa düzeni desteklenmiyor."));
    }
    let pad = (integer(py, get(attrs, "pageHorizontalPadding")?)?, integer(py, get(attrs, "pageVerticalPadding")?)?);
    let size = (integer(py, get(attrs, "width")?)?, integer(py, get(attrs, "height")?)?);
    let page_list = kids(pages)?;
    let mut geometry: Vec<(i64, i64, u32)> = Vec::new();
    for p in &page_list {
        geometry.push((integer(py, get(p, "width")?)?, integer(py, get(p, "height")?)?, _page_color(py, p)?));
    }
    let total: i64 = geometry.iter().map(|g| g.1).sum::<i64>() + (geometry.len() as i64 - 1) * pad.1;
    if geometry.is_empty() || geometry.len() > 4096 || pad.0.max(pad.1) > 32768 || total != size.1 {
        return Err(ire(py, "Metin akışı sayfa ölçüleriyle uyuşmuyor."));
    }
    let attrib = attrs.getattr("attrib")?;
    let default = if attrib.contains("pageDefaultWidth")? || attrib.contains("pageDefaultHeight")? {
        (integer(py, get(attrs, "pageDefaultWidth")?)?, integer(py, get(attrs, "pageDefaultHeight")?)?)
    } else {
        (size.0, geometry[0].1)
    };
    let raw_delta = attrs.call_method1("get", ("bodyTextFontSizeDelta",))?;
    let delta = if raw_delta.is_none() { None } else { Some(pyint(py, &raw_delta)?) };
    if let Some(d) = &delta {
        if !(d.ge(-100)? && d.le(100)?) {
            return Err(ire(py, "Metin boyutu ayarı desteklenmiyor."));
        }
    }
    let out = PyDict::new(py);
    out.update(body.as_mapping())?;
    let sections = body.get_item("sections")?.ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("sections"))?;
    if sections.is_truthy()? {
        let count = sections.len()?;
        if count > geometry.len() {
            return Err(ire(py, "Metin sayfa bölümleri sayfalarla uyuşmuyor."));
        }
        let padded = PyList::new(py, sections.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
        for _ in count..geometry.len() {
            padded.append((-1i64, 0i64))?;
        }
        out.set_item("sections", padded)?;
    }
    let trailing = geometry.len() > 1 && {
        let (last, previous) = (geometry[geometry.len() - 1], geometry[geometry.len() - 2]);
        iter_tag(&page_list[page_list.len() - 1], "object")?.is_empty() && last == previous && size.0 == last.0
    };
    let geometry_list = PyList::empty(py);
    for (w, h, c) in &geometry {
        let item = PyDict::new(py);
        item.set_item("width", w)?;
        item.set_item("height", h)?;
        item.set_item("backgroundArgb", c)?;
        geometry_list.append(item)?;
    }
    out.set_item("pages", geometry_list)?;
    out.set_item("padding", pad)?;
    out.set_item("fontSizeDelta", delta)?;
    out.set_item("defaultPage", default)?;
    out.set_item("flowSize", size)?;
    Ok((out, if trailing { Some(geometry.len() as i64 - 1) } else { None }))
}

// ---------------------------------------------------------------- page preparation

type SourceKey = ([u8; 32], [u8; 32], String, Option<String>, Option<String>);

#[cfg(feature = "python")]
/// Everything _prepare_pages derives before its page loop, per exact source.
struct Prepared {
    pages: Py<PyAny>,
    flow: Option<Py<PyDict>>,
    trailing: Option<i64>,
    /// stroke id -> Some((record, binding)) when admitted, None otherwise
    records: HashMap<String, Option<(Record, Binding)>>,
    /// Per page, per `iter('object')` position: a shape/line the renderer
    /// skips (`rs_ops::page_shapes::renderer_skippable_shape`).
    shapes: Vec<Vec<bool>>,
}

/// Per page (document order) and per object (pre-order): skippable shapes.
pub fn shape_flags(resource: &[u8]) -> Vec<Vec<bool>> {
    let Ok(root) = rs_ops::xml::parse_bytes(resource) else { return Vec::new() };
    let lists: Vec<&rs_ops::xml::Element> = root.children.iter().filter(|c| c.local().to_lowercase() == "pagelist").collect();
    if lists.len() != 1 {
        return Vec::new();
    }
    lists[0].children.iter()
        .map(|page| page.iter_tag("object").map(rs_ops::page_shapes::renderer_skippable_shape).collect())
        .collect()
}

/// `(width, height, backgroundArgb)` when `body` is exactly the renderer
/// input of a page whose only objects were skipped shapes (no strokes, no
/// flow). Such a page is a plain background drawn without the renderer.
pub fn background_only(body: &[u8]) -> Option<(i64, i64, u32)> {
    if body.len() > 256 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let width = value.get("width")?.as_i64()?;
    let height = value.get("height")?.as_i64()?;
    let color = u32::try_from(value.get("backgroundArgb")?.as_u64()?).ok()?;
    let exact = format!("{{\"schemaVersion\":1,\"width\":{width},\"height\":{height},\"backgroundArgb\":{color},\"strokes\":[]}}");
    (exact.as_bytes() == body && color >> 24 == 255 && width > 0 && height > 0 && width * height <= px::MAX_PIXELS).then_some((width, height, color))
}

/// The plain-background page as SVG (`render_ink_svg`).
pub fn background_svg(width: i64, height: i64, argb: u32) -> Vec<u8> {
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\
<rect width=\"{width}\" height=\"{height}\" fill=\"#{:06x}\"/></svg>", argb & 0xff_ffff).into_bytes()
}

/// The plain-background page as RGBA8 pixels.
pub fn background_pixels(width: i64, height: i64, argb: u32) -> Vec<u8> {
    let pixel = [(argb >> 16) as u8, (argb >> 8) as u8, argb as u8, 255];
    pixel.repeat((width * height) as usize)
}

fn sha(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

#[cfg(feature = "python")]
static PREPARED: OnceLock<Mutex<VecDeque<(SourceKey, Arc<Prepared>)>>> = OnceLock::new();
type PageKey = (SourceKey, Option<i64>, bool);
#[cfg(feature = "python")]
static PAGES: OnceLock<Mutex<VecDeque<(PageKey, Py<PyTuple>, usize)>>> = OnceLock::new();

#[cfg(feature = "python")]
fn source_key(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<SourceKey> {
    let raw: Vec<u8> = snapshot.getattr("raw_body")?.extract()?;
    let resource: Vec<u8> = snapshot.getattr("resource_xml")?.extract()?;
    let _ = py;
    Ok((sha(&raw), sha(&resource), snapshot.getattr("content_type")?.str()?.to_string(),
        snapshot.getattr("local_id")?.extract()?, snapshot.getattr("commit_id")?.extract()?))
}

enum Decode {
    Ink(InkErr),
    Mismatch,
}

/// Decode, bind and admit every stroke part (the expensive step, GIL-free).
fn decode_records(resource: &[u8], bodies: &[Vec<u8>]) -> Result<HashMap<String, Option<(Record, Binding)>>, Decode> {
    use rs_ink::core::binary::{decode_stroke_resource, i32_at, py_slice};
    use rs_ink::core::xml::Tree;
    let tree = Tree::parse(resource).map_err(Decode::Ink)?;
    let root = tree.root;
    let child = |name: &str| tree.children(root).iter().copied().find(|c| tree.local(*c).to_lowercase() == name);
    let mut objects: HashMap<String, usize> = HashMap::new();
    if let Some(list) = child("pagelist") {
        for page in tree.children(list) {
            for node in tree.iter(*page) {
                if tree.tag(node) == "object" {
                    if let Some(id) = tree.get(node, "id") {
                        objects.insert(id.to_string(), node);
                    }
                }
            }
        }
    }
    let table = child("attributes").and_then(|a| tree.find(a, "stringIdList")).map(|n| (&tree, n));
    let mut records = HashMap::new();
    for body in bodies {
        let record = decode_stroke_resource(body).map_err(Decode::Ink)?;
        let base = &record.frames[0];
        let identity = py_slice(&base.raw, base.fixed_start + 6, Some(base.fixed_start + 42));
        let identity = match std::str::from_utf8(identity) {
            Ok(s) if s.is_ascii() => s.to_string(),
            _ => return Err(Decode::Ink(InkErr::Unicode("ascii", identity.to_vec(), 0, identity.len(), "ordinal not in range(128)"))),
        };
        let Some(&node) = objects.get(&identity) else { return Err(Decode::Mismatch) };
        if records.contains_key(&identity) {
            return Err(Decode::Mismatch);
        }
        let binding = rs_ink::core::prepare::bind_stroke_metadata(&record.frames, (&tree, node), table).map_err(Decode::Ink)?;
        let rotation = i32_at(&base.raw, base.flexible_start - 5).map_err(Decode::Ink)?;
        let ok = admitted(&record, &binding) && rotation == 0;
        records.insert(identity, if ok { Some((record, binding)) } else { None });
    }
    Ok(records)
}

#[cfg(feature = "python")]
fn prepare_source(py: Python<'_>, snapshot: &Bound<'_, PyAny>, key: &SourceKey) -> PyResult<Arc<Prepared>> {
    let store = PREPARED.get_or_init(|| Mutex::new(VecDeque::new()));
    if let Some((_, hit)) = store.lock().unwrap().iter().find(|(k, _)| k == key) {
        return Ok(hit.clone());
    }
    // verified_resource (rs-ops core). Its root serialises to exactly
    // snapshot.resource_xml, so the admission walk parses that.
    let parts = match xarea::verified_resource(py, snapshot)? {
        xarea::Verified::Reason(reason) => return Err(ire(py, reason)),
        xarea::Verified::Parts(parts) => parts,
    };
    let resource: Vec<u8> = snapshot.getattr("resource_xml")?.extract()?;
    let root = py.import("xml.etree.ElementTree")?.getattr("fromstring")?.call1((PyBytes::new(py, &resource),))?;
    let tree = one(py, &root, "notetree")?;
    let attrs = one(py, &root, "attributes")?;
    if get(&attrs, "formatVersion")?.as_deref() != Some("4000") || get(&tree, "ver")?.as_deref() != Some("4000")
        || tree.call_method1("get", ("isStrokeGrouped", "false"))?.call_method0("lower")?.ne("false")?
    {
        return Err(ire(py, "Bu notun kalem biçimi henüz görüntülenemiyor."));
    }
    let allowed = ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];
    for n in kids(&root)? {
        if !allowed.contains(&tag(&n)?.as_str()) {
            return Err(ire(py, "Bu not başka içerikler de taşıyor."));
        }
    }
    let body = one(py, &root, "bodytext")?;
    let floating = one(py, &root, "floatingobjectlist")?;
    if floating.len()? != 0 {
        return Err(ire(py, "Bu not eklenmiş nesneler taşıyor."));
    }
    let pages = one(py, &root, "pagelist")?;
    let mut flow = None;
    let mut trailing = None;
    if !_empty_body_for_ink(py, &body)? {
        if let Some(parsed) = _flow_body(py, &body)? {
            let (f, t) = _flow_layout(py, &attrs, &pages, &parsed)?;
            flow = Some(f.unbind());
            trailing = t;
            let mut any = false;
            for p in kids(&pages)? {
                if !iter_tag(&p, "object")?.is_empty() {
                    any = true;
                    break;
                }
            }
            if !any {
                return Err(ire(py, "Bu notta çizim bulunmuyor."));
            }
        }
    }
    // Strokes are decoded and bound natively (rs-ink core) from the parts the
    // verification already parsed (no second MIME parse).
    let bodies: Vec<Vec<u8>> = parts.into_iter().filter(|(name, _)| name.as_deref() == Some("strokeResource")).map(|(_, b)| b).collect();
    let records = py.detach(|| decode_records(&resource, &bodies)).map_err(|e| match e {
        Decode::Ink(e) => xarea::ink_err(py, e),
        Decode::Mismatch => ire(py, "Çizgi kaynağı notla eşleşmiyor."),
    })?;
    let shapes = py.detach(|| shape_flags(&resource));
    let prepared = Arc::new(Prepared { pages: pages.unbind(), flow, trailing, records, shapes });
    let mut guard = store.lock().unwrap();
    guard.push_back((key.clone(), prepared.clone()));
    while guard.len() > 2 {
        guard.pop_front();
    }
    Ok(prepared)
}

#[cfg(feature = "python")]
fn page_inputs<'py>(py: Python<'py>, prepared: &Prepared, only_index: Option<i64>, build: bool) -> PyResult<Vec<Bound<'py, PageInput>>> {
    let pages = prepared.pages.bind(py);
    let records = &prepared.records;
    let flow = prepared.flow.as_ref().map(|f| f.bind(py));
    let page_attrs: Vec<&str> = PAGE_ATTRS.to_vec();
    let blocked = ["pdfDataList", "pdfData", "strokeGroup", "customObject", "shape", "image"];
    let max_pixels: i64 = px::MAX_PIXELS;
    let dumps = py.import("json")?.getattr("dumps")?;
    let mut output = Vec::new();
    for (index, page) in kids(pages)?.iter().enumerate() {
        let index = index as i64;
        if only_index.is_some_and(|i| i != index) {
            continue;
        }
        let width = integer(py, get(page, "width")?)?;
        let height = integer(py, get(page, "height")?)?;
        let all_objects = iter_tag(page, "object")?;
        // Recognised shapes/lines are drawn by the UI overlay, not here.
        let flags = prepared.shapes.get(index as usize).filter(|f| f.len() == all_objects.len());
        let has_shape = flags.is_some_and(|f| f.iter().any(|s| *s));
        let objects: Vec<_> = all_objects.iter().enumerate()
            .filter(|(i, _)| !flags.is_some_and(|f| f[*i])).map(|(_, o)| o.clone()).collect();
        let mut skip = false;
        for o in &objects {
            let id = o.call_method1("get", ("id",))?;
            let id: Option<String> = id.extract()?;
            let admitted = id.as_deref().and_then(|i| records.get(i)).is_some_and(|r| r.is_some());
            if get(o, "type")?.as_deref() != Some("stroke") || !admitted {
                skip = true;
                break;
            }
        }
        if skip {
            continue;
        }
        if iter_all(page)?.iter().any(|n| tag(n).is_ok_and(|t| blocked.contains(&t.as_str()))) {
            continue;
        }
        let keys = attr_keys(page)?;
        let children_ok = kids(page)?.iter().all(|n| tag(n).is_ok_and(|t| ["layerList", "drawnRect", "canvasCacheDataMap"].contains(&t.as_str())));
        if width * height > max_pixels || keys.iter().any(|k| !page_attrs.contains(&k.as_str()))
            || page.call_method1("get", ("bgImageId", "-1"))?.ne("-1")? || page.call_method1("get", ("bgImageMode", "2"))?.ne("2")?
            || page.call_method1("get", ("orientation", "0"))?.ne("0")? || !children_ok
        {
            continue;
        }
        if keys.iter().any(|k| k == "bgWidth") {
            let w = integer(py, get(page, "bgWidth")?)?;
            if !(0 < w && w <= 65535) {
                return Err(ire(py, "Bu sayfanın arka planı veya dönüşümü desteklenmiyor."));
            }
        }
        if keys.iter().any(|k| k == "editableFormatVersion") {
            let editable = get(page, "editableFormatVersion")?;
            if !(editable == get(page, "formatVersion")? && editable.as_deref() == Some("4000")) {
                return Err(ire(py, "Bu sayfanın sürümü henüz desteklenmiyor."));
            }
        }
        let color = _page_color(py, page)?;
        if Some(index) == prepared.trailing || (flow.is_none() && all_objects.is_empty()) {
            continue;
        }
        let dark = ((color >> 16) & 255) + ((color >> 8) & 255) + (color & 255) < 384;
        if !build {
            output.push(Bound::new(py, PageInput { index, width, height, dark_mode: dark, body: PyBytes::new(py, b"").unbind() })?);
            continue;
        }
        let mut text = format!("{{\"schemaVersion\":1,\"width\":{width},\"height\":{height},\"backgroundArgb\":{color},\"strokes\":[");
        let mut distance = 0.0f64;
        let mut inkpen2 = false;
        let mut marker4 = false;
        let mut count = 0usize;
        for o in &objects {
            let id: Option<String> = o.call_method1("get", ("id",))?.extract()?;
            let (record, binding) = records.get(id.as_deref().unwrap_or("")).and_then(|r| r.as_ref()).expect("admitted page");
            if count > 0 {
                text.push(',');
            }
            count += 1;
            inkpen2 |= binding.pen_name == INKPEN2;
            marker4 |= binding.pen_name == MARKER4;
            distance += stroke_json(&mut text, record, binding, width, height).map_err(|e| match e {
                JsonErr::Ink(e) => xarea::ink_err(py, e),
                JsonErr::Render(m) => ire(py, m),
                JsonErr::Value(m) => PyValueError::new_err(m),
                JsonErr::Key(k) => pyo3::exceptions::PyKeyError::new_err(k),
            })?;
            if distance > MAX_DISTANCE {
                break;
            }
        }
        // A page whose only objects are skipped shapes is a plain background.
        if (count == 0 && flow.is_none() && !has_shape) || distance > MAX_DISTANCE {
            continue;
        }
        text.push(']');
        let compact = kwargs(py, &[("allow_nan", false.into_pyobject(py)?.to_owned().into_any()),
            ("separators", (",", ":").into_pyobject(py)?.into_any())])?;
        if let Some(f) = flow {
            let copy = PyDict::new(py);
            copy.update(f.as_mapping())?;
            copy.set_item("pageIndex", index)?;
            text.push_str(",\"flow\":");
            text.push_str(&dumps.call((copy,), Some(&compact))?.extract::<String>()?);
        }
        if inkpen2 {
            text.push_str(",\"inkpen2Profile\":");
            text.push_str(&dumps.call((g(py, "INKPEN2_PROFILE")?,), Some(&compact))?.extract::<String>()?);
            text.push_str(",\"admit\":\"calibrated\"");
        } else if marker4 {
            text.push_str(",\"admit\":\"calibrated\"");
        }
        text.push('}');
        let text = PyString::new(py, &text);
        let raw = text.call_method1("encode", ())?;
        if raw.len()? > MAX_INPUT {
            return Err(ire(py, "Çizim verisi boyut sınırını aşıyor."));
        }
        output.push(Bound::new(py, PageInput { index, width, height, dark_mode: dark, body: raw.cast_into::<PyBytes>()?.unbind() })?);
    }
    Ok(output)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(snapshot, only_index=None, build=true))]
pub fn _prepare_pages<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, only_index: Option<i64>, build: bool) -> PyResult<Bound<'py, PyTuple>> {
    let document_snapshot = xarea::class(py, "samsung_notes.document", "DocumentSnapshot")?;
    let snapshot = if snapshot.is_instance_of::<PyDict>() {
        document_snapshot.call_method1("from_dict", (snapshot,))?
    } else {
        snapshot.clone()
    };
    if !snapshot.is_instance(&document_snapshot)? {
        return Err(ire(py, "Notun çizim kaynağı geçersiz."));
    }
    let source = source_key(py, &snapshot)?;
    let key: PageKey = (source.clone(), only_index, build);
    let cache = PAGES.get_or_init(|| Mutex::new(VecDeque::new()));
    {
        let mut guard = cache.lock().unwrap();
        if let Some(at) = guard.iter().position(|(k, _, _)| *k == key) {
            let entry = guard.remove(at).unwrap();
            let result = entry.1.clone_ref(py);
            guard.push_back(entry);
            return Ok(result.into_bound(py));
        }
    }
    let prepared = prepare_source(py, &snapshot, &source)?;
    let pages = page_inputs(py, &prepared, only_index, build)?;
    let size: usize = pages.iter().map(|p| p.get().body.bind(py).as_bytes().len()).sum();
    let result = PyTuple::new(py, pages)?;
    let mut guard = cache.lock().unwrap();
    guard.push_back((key, result.clone().unbind(), size));
    while guard.len() > 2 || guard.iter().map(|e| e.2).sum::<usize>() > 16 * 1024 * 1024 {
        guard.pop_front();
    }
    Ok(result)
}

// ---------------------------------------------------------------- child process and renderer

#[cfg(feature = "python")]
#[pyfunction]
pub fn _snapshot_bytes<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let document_snapshot = xarea::class(py, "samsung_notes.document", "DocumentSnapshot")?;
    let mut value = snapshot.clone();
    if snapshot.is_instance(&document_snapshot)? {
        value = snapshot.call_method0("to_dict")?;
        let again = document_snapshot.call_method1("from_dict", (&value,))?;
        if again.getattr("resource_xml")?.ne(snapshot.getattr("resource_xml")?)? {
            return Err(ire(py, "Notun çizim kaynağı tutarsız."));
        }
    }
    let text = py.import("json")?.getattr("dumps")?.call((value,), Some(&kwargs(py, &[("separators", (",", ":").into_pyobject(py)?.into_any())])?))?;
    Ok(text.call_method0("encode")?.cast_into::<PyBytes>()?)
}

#[cfg(feature = "python")]
fn root_dir(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(PathBuf::from(py.import("os")?.getattr("fspath")?.call1((g(py, "ROOT")?,))?.extract::<String>()?))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: access() only reads the NUL-terminated path, which lives for the call.
    std::ffi::CString::new(path.as_os_str().as_bytes()).is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0)
}

#[cfg(feature = "python")]
fn renderer(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(PathBuf::from(py.import("os")?.getattr("fspath")?.call1((g(py, "RENDERER")?,))?.extract::<String>()?))
}

pub fn renderer_available(binary: &Path) -> bool {
    binary.is_file() && executable(binary) && sandbox::ink_sandbox_available()
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn provider_available(py: Python<'_>) -> PyResult<bool> {
    let binary = renderer(py)?;
    Ok(renderer_available(&binary))
}

const JOB_LIMITS: sandbox::Limits = sandbox::Limits { address_space: 3 * 1024 * 1024 * 1024, cpu_seconds: 170, file_size: None };
const JOB_BOOT: &str = "import sys\nfrom samsung_notes._rust import _extension\nsys.exit(_extension('render').ink_render_job._serve())";

#[cfg(feature = "python")]
fn python_executable(py: Python<'_>) -> PyResult<String> {
    py.import("sys")?.getattr("executable")?.extract()
}

#[cfg(feature = "python")]
/// Same contract as Python `_job`: one bounded preparation child.
fn job_output(py: Python<'_>, mode: &str, data: Vec<u8>, index: Option<i64>) -> PyResult<Vec<u8>> {
    let pid = std::process::id().to_string();
    let mut args = vec![python_executable(py)?, "-E".into(), "-m".into(), "samsung_notes.ink_render_job".into(), mode.into(), pid];
    if let Some(i) = index {
        args.push(i.to_string());
    }
    let root = root_dir(py)?;
    let outcome = py.detach(|| run_in(&root, &args, data, Duration::from_secs(180)));
    match outcome {
        Err(sandbox::RunError::Timeout) => Err(ire(py, "Çizim hazırlama süresi doldu.")),
        Err(sandbox::RunError::Spawn) => Err(pyo3::exceptions::PyOSError::new_err("ink preparation could not start")),
        Ok(o) if o.code != Some(0) || o.stdout.is_empty() => Err(ire(py, "Bu sayfanın çizimi hazırlanamadı.")),
        Ok(o) => Ok(o.stdout),
    }
}

fn run_in(root: &Path, args: &[String], data: Vec<u8>, timeout: Duration) -> Result<sandbox::Outcome, sandbox::RunError> {
    sandbox::run_with_cwd(args, Some(data), JOB_LIMITS, true, timeout, Some(root), false)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(mode, data, index=None))]
pub fn _job<'py>(py: Python<'py>, mode: &str, data: &[u8], index: Option<i64>) -> PyResult<Bound<'py, PyBytes>> {
    Ok(PyBytes::new(py, &job_output(py, mode, data.to_vec(), index)?))
}

/// Renderer/rasterizer in the ink_render_worker sandbox, spawned directly.
pub fn run_renderer(binary: &Path, png: bool, data: Vec<u8>, limit: usize) -> Result<Vec<u8>, &'static str> {
    let Ok(resolved) = std::fs::canonicalize(binary) else { return Err("Bu çizginin görünümü güvenle oluşturulamadı.") };
    if !resolved.is_file() || (png && Some(&resolved) != std::fs::canonicalize(sandbox::rsvg_path()).ok().as_ref()) {
        return Err("Bu çizginin görünümü güvenle oluşturulamadı.");
    }
    let args = sandbox::ink_args(&resolved.to_string_lossy(), png);
    match sandbox::run(&args, Some(data), sandbox::INK_LIMITS, true, Duration::from_secs(35)) {
        Err(sandbox::RunError::Timeout) => Err("Çizim görüntüleme süresi doldu."),
        Err(sandbox::RunError::Spawn) => Err("Bu çizginin görünümü güvenle oluşturulamadı."),
        Ok(o) if o.code != Some(0) || o.stdout.is_empty() || o.stdout.len() > limit => Err("Bu çizginin görünümü güvenle oluşturulamadı."),
        Ok(o) => Ok(o.stdout),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _run<'py>(py: Python<'py>, mode: &str, data: &[u8], binary: &Bound<'py, PyAny>, limit: usize) -> PyResult<Bound<'py, PyBytes>> {
    if !provider_available(py)? {
        return Err(ire(py, "Çizim görüntüleyicisi bu cihazda hazır değil."));
    }
    let binary = PathBuf::from(py.import("os")?.getattr("fspath")?.call1((binary,))?.extract::<String>()?);
    if mode != "svg" && mode != "png" {
        return Err(ire(py, "Bu çizginin görünümü güvenle oluşturulamadı."));
    }
    let data = data.to_vec();
    let out = py.detach(|| run_renderer(&binary, mode == "png", data, limit)).map_err(|m| ire(py, m))?;
    Ok(PyBytes::new(py, &out))
}

// ---- per-note preparation cache filled by one streaming child

#[derive(Default)]
struct NoteBodies {
    inventory: Option<Vec<u8>>,
    pages: HashMap<i64, Option<Vec<u8>>>,
    done: bool,
    bytes: usize,
}

type Bodies = Mutex<VecDeque<([u8; 32], Arc<(Mutex<NoteBodies>, Condvar)>)>>;
static BODIES: OnceLock<Bodies> = OnceLock::new();
const BODY_BUDGET: usize = 96 * 1024 * 1024;

fn note_slot(key: [u8; 32]) -> (Arc<(Mutex<NoteBodies>, Condvar)>, bool) {
    let store = BODIES.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut guard = store.lock().unwrap();
    if let Some(at) = guard.iter().position(|(k, _)| *k == key) {
        let entry = guard.remove(at).unwrap();
        let slot = entry.1.clone();
        guard.push_back(entry);
        return (slot, false);
    }
    let slot = Arc::new((Mutex::new(NoteBodies::default()), Condvar::new()));
    guard.push_back((key, slot.clone()));
    while guard.len() > 4 || guard.iter().map(|(_, s)| s.0.lock().map(|b| b.bytes).unwrap_or(0)).sum::<usize>() > BODY_BUDGET {
        if guard.len() <= 1 {
            break;
        }
        guard.pop_front();
    }
    (slot, true)
}

fn forget_note(key: [u8; 32]) {
    if let Some(store) = BODIES.get() {
        store.lock().unwrap().retain(|(k, _)| *k != key);
    }
}

fn read_exact(stream: &mut impl Read, n: usize) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; n];
    stream.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

#[cfg(feature = "python")]
/// Start the streaming child for one note: frame 0 = inventory JSON, then
/// (index, body) frames, the wanted page first. Runs in the background and
/// fills `slot`; waiters are woken on every frame.
fn start_note_child(py: Python<'_>, data: Vec<u8>, wanted: i64, slot: Arc<(Mutex<NoteBodies>, Condvar)>, key: [u8; 32]) -> PyResult<()> {
    let pid = std::process::id().to_string();
    let args = vec![python_executable(py)?, "-E".into(), "-c".into(), JOB_BOOT.into(), "render".into(), pid, wanted.to_string()];
    let root = root_dir(py)?;
    let spawned = sandbox::spawn_streaming(&args, data, JOB_LIMITS, true, Some(&root));
    let (mut child, mut stdout) = match spawned {
        Ok(v) => v,
        Err(_) => {
            forget_note(key);
            return Err(pyo3::exceptions::PyOSError::new_err("ink preparation could not start"));
        }
    };
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        let killer_pid = child.id() as i32;
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            if stop_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).is_err() {
                unsafe { libc::kill(-killer_pid, libc::SIGKILL) };
            }
        });
        let (lock, wake) = &*slot;
        let header = read_exact(&mut stdout, 4).and_then(|n| read_exact(&mut stdout, u32::from_le_bytes(n.try_into().unwrap()) as usize));
        {
            let mut state = lock.lock().unwrap();
            state.inventory = header;
            wake.notify_all();
        }
        loop {
            let Some(head) = read_exact(&mut stdout, 8) else { break };
            let index = i32::from_le_bytes(head[..4].try_into().unwrap()) as i64;
            let size = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
            if size > MAX_INPUT + 1 {
                break;
            }
            let body = if size == 0 { None } else { read_exact(&mut stdout, size) };
            if size != 0 && body.is_none() {
                break;
            }
            let mut state = lock.lock().unwrap();
            if state.bytes + size > BODY_BUDGET {
                state.pages.insert(index, None);
                drop(state);
                break;
            }
            state.bytes += size;
            state.pages.insert(index, body);
            wake.notify_all();
        }
        let _ = child.wait();
        let _ = stop_tx.send(());
        let _ = watchdog.join();
        let mut state = lock.lock().unwrap();
        state.done = true;
        wake.notify_all();
    });
    Ok(())
}

enum Lookup {
    Inventory(Option<Vec<u8>>),
    Body(Option<Vec<u8>>),
}

/// Wait for the inventory frame or a page body of a note's child.
fn wait_for(slot: &Arc<(Mutex<NoteBodies>, Condvar)>, page: Option<i64>, timeout: Duration) -> Option<Lookup> {
    let (lock, wake) = &**slot;
    let deadline = std::time::Instant::now() + timeout;
    let mut state = lock.lock().unwrap();
    loop {
        match page {
            None => {
                if state.inventory.is_some() || state.done {
                    return Some(Lookup::Inventory(state.inventory.clone()));
                }
            }
            Some(i) => {
                if let Some(body) = state.pages.get(&i) {
                    return Some(Lookup::Body(body.clone()));
                }
                if state.done {
                    return Some(Lookup::Body(None));
                }
            }
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        state = wake.wait_timeout(state, deadline - now).unwrap().0;
    }
}

#[cfg(feature = "python")]
static INVENTORY: OnceLock<Mutex<VecDeque<([u8; 32], Py<PyTuple>)>>> = OnceLock::new();

#[cfg(feature = "python")]
fn inventory_get(py: Python<'_>, key: &[u8; 32]) -> Option<Py<PyTuple>> {
    let store = INVENTORY.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut guard = store.lock().unwrap();
    let at = guard.iter().position(|(k, _)| k == key)?;
    let entry = guard.remove(at).unwrap();
    let value = entry.1.clone_ref(py);
    guard.push_back(entry);
    Some(value)
}

#[cfg(feature = "python")]
fn inventory_put(key: [u8; 32], pages: Py<PyTuple>) {
    let store = INVENTORY.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut guard = store.lock().unwrap();
    guard.retain(|(k, _)| *k != key);
    guard.push_back((key, pages));
    while guard.len() > 512 {
        guard.pop_front();
    }
}

#[cfg(feature = "python")]
fn parse_inventory<'py>(py: Python<'py>, raw: &[u8]) -> PyResult<Bound<'py, PyTuple>> {
    let loaded = py.import("json")?.getattr("loads")?.call1((PyBytes::new(py, raw),))?;
    PyTuple::new(py, loaded.try_iter()?.collect::<PyResult<Vec<_>>>()?)
}

#[cfg(feature = "python")]
fn tolerated(py: Python<'_>, e: &PyErr) -> bool {
    e.is_instance_of::<PyValueError>(py) || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py) || e.is_instance_of::<pyo3::exceptions::PyOverflowError>(py)
        || py.import("xml.etree.ElementTree").and_then(|m| m.getattr("ParseError")).is_ok_and(|c| e.is_instance(py, &c))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn inspect_render_pages<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let result = (|| -> PyResult<Bound<'py, PyTuple>> {
        let data = _snapshot_bytes(py, snapshot)?;
        let key = sha(data.as_bytes());
        if let Some(hit) = inventory_get(py, &key) {
            return Ok(hit.into_bound(py));
        }
        let pages = parse_inventory(py, &job_output(py, "inventory", data.as_bytes().to_vec(), None)?)?;
        inventory_put(key, pages.clone().unbind());
        Ok(pages)
    })();
    match result {
        Ok(v) => Ok(v),
        Err(e) if tolerated(py, &e) => Ok(PyTuple::empty(py)),
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
/// Inventory + body for `index`, sharing one streaming preparation child per note.
fn page_body<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, index: i64) -> PyResult<(Bound<'py, PyAny>, Vec<u8>)> {
    let not_found = || ire(py, "Bu sayfada desteklenen çizim bulunmuyor.");
    let data = match _snapshot_bytes(py, snapshot) {
        Ok(d) => d,
        Err(e) if tolerated(py, &e) => return Err(not_found()),
        Err(e) => return Err(e),
    };
    let key = sha(data.as_bytes());
    let (slot, fresh) = note_slot(key);
    if fresh {
        start_note_child(py, data.as_bytes().to_vec(), index, slot.clone(), key)?;
    } else {
        let known = slot.0.lock().unwrap();
        let missing = !known.pages.contains_key(&index) && known.done;
        drop(known);
        if missing {
            // Evicted or failed earlier: start over for this page.
            forget_note(key);
            let (slot2, _) = note_slot(key);
            start_note_child(py, data.as_bytes().to_vec(), index, slot2.clone(), key)?;
            return finish_page(py, snapshot, key, slot2, index);
        }
    }
    finish_page(py, snapshot, key, slot, index)
}

#[cfg(feature = "python")]
fn finish_page<'py>(py: Python<'py>, _snapshot: &Bound<'py, PyAny>, key: [u8; 32], slot: Arc<(Mutex<NoteBodies>, Condvar)>, index: i64)
                    -> PyResult<(Bound<'py, PyAny>, Vec<u8>)> {
    let timeout = Duration::from_secs(185);
    let inventory = match inventory_get(py, &key) {
        Some(hit) => hit.into_bound(py),
        None => {
            let slot2 = slot.clone();
            let found = py.detach(move || wait_for(&slot2, None, timeout));
            let raw = match found {
                Some(Lookup::Inventory(Some(raw))) => raw,
                None => {
                    forget_note(key);
                    return Err(ire(py, "Çizim hazırlama süresi doldu."));
                }
                _ => {
                    forget_note(key);
                    return Err(ire(py, "Bu sayfada desteklenen çizim bulunmuyor."));
                }
            };
            let pages = match parse_inventory(py, &raw) {
                Ok(p) => p,
                Err(e) if tolerated(py, &e) => PyTuple::empty(py),
                Err(e) => return Err(e),
            };
            inventory_put(key, pages.clone().unbind());
            pages
        }
    };
    let mut page = None;
    for p in inventory.iter() {
        if p.get_item("index")?.eq(index)? {
            page = Some(p);
            break;
        }
    }
    let Some(page) = page else { return Err(ire(py, "Bu sayfada desteklenen çizim bulunmuyor.")) };
    let found = py.detach(move || wait_for(&slot, Some(index), timeout));
    match found {
        Some(Lookup::Body(Some(body))) => Ok((page, body)),
        None => {
            forget_note(key);
            Err(ire(py, "Çizim hazırlama süresi doldu."))
        }
        _ => {
            forget_note(key);
            Err(ire(py, "Bu sayfanın çizimi hazırlanamadı."))
        }
    }
}

/// SAMSUNG_NOTES_INK_RASTER=rsvg keeps the sandboxed rsvg-convert rasterizer.
fn rsvg_requested() -> bool {
    std::env::var("SAMSUNG_NOTES_INK_RASTER").is_ok_and(|v| v == "rsvg")
}

#[cfg(feature = "python")]
fn exact_index(py: Python<'_>, page_index: &Bound<'_, PyAny>) -> PyResult<i64> {
    pyhelp::exact_int(page_index).ok_or_else(|| ire(py, "Sayfa seçimi geçersiz."))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn render_ink_svg<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, page_index: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let index = exact_index(py, page_index)?;
    let (_, body) = page_body(py, snapshot, index)?;
    if let Some((width, height, argb)) = background_only(&body) {
        return Ok(PyBytes::new(py, &background_svg(width, height, argb)));
    }
    if !provider_available(py)? {
        return Err(ire(py, "Çizim görüntüleyicisi bu cihazda hazır değil."));
    }
    let binary = renderer(py)?;
    let svg = py.detach(|| run_renderer(&binary, false, body, MAX_SVG)).map_err(|m| ire(py, m))?;
    Ok(PyBytes::new(py, &svg))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _rgba_png<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>, width: i64, height: i64) -> PyResult<Bound<'py, PyBytes>> {
    let Ok(bytes) = raw.cast::<PyBytes>() else { return Err(ire(py, px::FilteredError::Invalid.message())) };
    let data = bytes.as_bytes();
    let pixels = py.detach(|| px::unfilter_png(data, width, height, MAX_PNG)).map_err(|e| match e {
        px::FilteredError::Corrupt => match py.import("zlib").and_then(|z| z.getattr("error")).and_then(|c| c.call1((e.message(),))) {
            Ok(v) => PyErr::from_value(v),
            Err(err) => err,
        },
        other => ire(py, other.message()),
    })?;
    encode(py, width, height, &pixels)
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn render_ink_page<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, page_index: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let index = exact_index(py, page_index)?;
    let (page, body) = page_body(py, snapshot, index)?;
    let (width, height): (i64, i64) = (page.get_item("width")?.extract()?, page.get_item("height")?.extract()?);
    if let Some((w, h, argb)) = background_only(&body) {
        if (w, h) != (width, height) {
            return Err(ire(py, "Bu çizginin görünümü güvenle oluşturulamadı."));
        }
        let png = py.detach(|| px::rgba_png(width, height, &background_pixels(width, height, argb))).map_err(|_| ire(py, px::FilteredError::Format.message()))?;
        return Ok(PyBytes::new(py, &png));
    }
    if !provider_available(py)? {
        return Err(ire(py, "Çizim görüntüleyicisi bu cihazda hazır değil."));
    }
    let binary = renderer(py)?;
    let result = py.detach(|| -> Result<Vec<u8>, px::FilteredError> {
        let svg = run_renderer(&binary, false, body, MAX_SVG).map_err(|_| px::FilteredError::Invalid)?;
        let pixels = if rsvg_requested() {
            let png = run_renderer(&sandbox::rsvg_path(), true, svg, MAX_PNG).map_err(|_| px::FilteredError::Invalid)?;
            px::unfilter_png(&png, width, height, MAX_PNG)?
        } else {
            // Default: resvg in-process (≈5x faster than rsvg-convert on stamp-heavy
            // pages; antialiasing-level pixel differences). The SVG is our own
            // renderer's bounded output (<=16 MB, no <image>/<script>).
            crate::raster::rasterize(&svg, width as u32, height as u32).map_err(|_| px::FilteredError::Invalid)?
        };
        px::rgba_png(width, height, &pixels).map_err(|_| px::FilteredError::Format)
    });
    match result {
        Ok(png) => Ok(PyBytes::new(py, &png)),
        Err(px::FilteredError::Invalid) => Err(ire(py, "Bu çizginin görünümü güvenle oluşturulamadı.")),
        Err(e) => Err(ire(py, e.message())),
    }
}

#[cfg(feature = "python")]
/// Benchmark/diagnostic hook: in-process rasterization of renderer SVG.
#[pyfunction]
pub fn _rasterize<'py>(py: Python<'py>, svg: &[u8], width: u32, height: u32) -> PyResult<Bound<'py, PyBytes>> {
    let pixels = py.detach(|| crate::raster::rasterize(svg, width, height)).map_err(|m| ire(py, m))?;
    encode(py, width as i64, height as i64, &pixels)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(_rasterize, m)?)?;
    m.add_class::<InkRenderError>()?;
    m.add("RenderError", m.py().get_type::<InkRenderError>())?;
    m.add_class::<PageInput>()?;
    for f in [wrap_pyfunction!(_blank_text, m)?, wrap_pyfunction!(_empty_body_for_ink, m)?, wrap_pyfunction!(_flow_body, m)?,
              wrap_pyfunction!(_page_color, m)?, wrap_pyfunction!(_flow_layout, m)?, wrap_pyfunction!(_prepare_pages, m)?,
              wrap_pyfunction!(_snapshot_bytes, m)?, wrap_pyfunction!(provider_available, m)?, wrap_pyfunction!(_job, m)?,
              wrap_pyfunction!(_run, m)?, wrap_pyfunction!(inspect_render_pages, m)?, wrap_pyfunction!(render_ink_svg, m)?,
              wrap_pyfunction!(_rgba_png, m)?, wrap_pyfunction!(render_ink_page, m)?] {
        m.add_function(f)?;
    }
    rs_common::export(m, &["InkRenderError", "RenderError", "_PageInput", "_blank_text",
        "_empty_body_for_ink", "_flow_body", "_page_color", "_flow_layout", "_prepare_pages", "_snapshot_bytes",
        "provider_available", "_job", "_run", "inspect_render_pages", "render_ink_svg", "_rgba_png", "render_ink_page", "_rasterize"])
}
