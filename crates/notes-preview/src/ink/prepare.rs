//! `ink_render._prepare_pages` in pure Rust: page admission and the renderer
//! input bodies (byte-identical compact JSON), over the verified resource.
//!
//! Every failure is a [`PrepErr`]: the Python callers collapse all of them
//! (the inventory becomes `()`, a page body becomes "hazırlanamadı"), so only
//! the decision "admitted / skipped / failed" has to match, not the class.
use crate::util::{json_str, sha256, J};
use rs_editor::pyval::{self, Val as PVal};
use rs_ink::core::binary::{Record, StyleVal};
use rs_ink::core::num::py_repr;
use rs_ink::core::prepare::Binding;
use rs_ink::core::InkErr;
use rs_ops::xml::Element;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub const MAX_INPUT: usize = 24 * 1024 * 1024;
const MAX_PIXELS: i64 = 4 * 1024 * 1024;
const MAX_DISTANCE: f64 = 500_000.0;
pub const FOUNTAIN: &str = "com.samsung.android.sdk.pen.pen.preload.FountainPen";
pub const INKPEN2: &str = "com.samsung.android.sdk.pen.pen.preload.InkPen2";
pub const MARKER4: &str = "com.samsung.android.sdk.pen.pen.preload.Marker4";
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
const PAGE_ATTRS: [&str; 18] = ["id", "hash", "isTextOnly", "orientation", "width", "height", "offsetX", "offsetY", "formatVersion",
    "bgImageMode", "bgImageId", "bgColor", "minFormatVersion", "currentLayerIndex", "createdTime", "modifiedTime", "bgWidth",
    "editableFormatVersion"];
const BODY_ATTRS: [&str; 36] = ["id", "type", "shapeType", "originalDegree", "leftMargin", "topMargin", "rightMargin", "bottomMargin",
    "textGravity", "createdTime", "modifiedTime", "hash", "formatVersion", "haveBeenToAtt", "isHintTextVisible", "isHorizontalFlipped",
    "isReplayable", "isTemplateObject", "isTextEditable", "isTextReadOnly", "isTransparency", "isVerticalFlipped", "lockState",
    "movable", "removable", "resizable", "rotatable", "selectable", "visible", "clippable", "flippable", "ellipsisType",
    "textAutoFit", "timeStamp", "ownerPageWidth", "ownerPageHeight"];
const FLOW_SPANS: [&str; 7] = ["bold", "italic", "underline", "strikethrough", "fontSize", "foregroundColor", "backgroundColor"];
/// `INKPEN2_PROFILE` as `json.dumps(..., separators=(',',':'))`.
const INKPEN2_PROFILE: &str = "{\"screenWidth\":1440,\"screenHeight\":2304,\"enhancedAntialias\":false}";

/// Any `_prepare_pages` failure (message kept for diagnostics only).
#[derive(Debug, Clone, PartialEq)]
pub struct PrepErr(pub String);

type P<T> = Result<T, PrepErr>;

fn fail<T>(message: impl Into<String>) -> P<T> {
    Err(PrepErr(message.into()))
}

impl From<InkErr> for PrepErr {
    fn from(e: InkErr) -> Self {
        PrepErr(format!("{e:?}"))
    }
}

/// `ink_render._PageInput`
#[derive(Clone, Debug, PartialEq)]
pub struct PageInput {
    pub index: i64,
    pub width: i64,
    pub height: i64,
    pub dark_mode: bool,
    pub body: Vec<u8>,
    /// A `page-reconstructed` composite (images, shapes, text boxes, blank
    /// sheet, approximated ink) rather than a renderer-only ink page.
    pub composite: bool,
    /// Why the composite is approximate/incomplete (None: complete).
    pub notice: Option<String>,
    /// A PDF page's own objects, drawn over its Poppler page image.
    pub over_pdf: bool,
}

// ---------------------------------------------------------------- element helpers

fn iter_tag<'a>(e: &'a Element, tag: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
    e.iter().filter(move |n| n.tag == tag)
}

fn same_set(e: &Element, wanted: &[&str]) -> bool {
    let mut a: Vec<&str> = e.attrib.iter().map(|(k, _)| k.as_str()).collect();
    a.sort_unstable();
    a.dedup();
    let mut b: Vec<&str> = wanted.to_vec();
    b.sort_unstable();
    b.dedup();
    a == b
}

/// Python `float(text)`.
fn pyfloat(text: &str) -> P<f64> {
    pyval::parse_py_float(text).map_or_else(|| fail(format!("could not convert string to float: {text:?}")), Ok)
}

/// Python `int(text)` (base 10).
fn pyint(text: Option<&str>) -> P<i128> {
    let Some(text) = text else { return fail("int() argument must be a string, a bytes-like object or a real number, not 'NoneType'") };
    match pyval::parse_py_int(text).and_then(|v| v.to_i128()) {
        Some(v) => Ok(v),
        None => fail(format!("invalid literal for int() with base 10: {text:?}")),
    }
}

/// `document.integer`
fn integer(value: Option<&str>) -> P<i64> {
    Ok(rs_ink::core::doc_integer(value)?)
}

/// `document.one`
fn one<'a>(parent: &'a Element, name: &str) -> P<&'a Element> {
    let found: Vec<&Element> = parent.children.iter().filter(|c| c.local() == name).collect();
    if found.len() != 1 {
        return fail("Belgenin yapısı güvenli düzenleme için uygun değil.");
    }
    Ok(found[0])
}

/// Every attribute value parses as a float equal to zero (`any(float(v)!=0)`
/// short-circuits on the first nonzero; a parse error raises).
fn all_zero(e: &Element) -> P<bool> {
    for (_, v) in &e.attrib {
        if pyfloat(v)? != 0.0 {
            return Ok(false);
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------- admission (rs-ink core)

/// `_props`: property bits of the stroke frame (PROPS order).
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

/// `_admitted_pen`
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
    // Stylus axes are optional. Our writer (rs-ink recipes 2/3) copies the
    // template's tool 2 but stores no tilt/orientation and clears this bit;
    // Samsung's V9 redraw reads only XY, time and tool, so the device draws such
    // a stroke. The renderer takes upstream's documented InkPen2 approximation
    // for it (its V9 route requires the axes), exactly the path the live pen
    // preview (crates/notes-ink-live) draws. Refusing it refused the whole page.
    if !prop(&p, "binary_added") {
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

fn json_float(out: &mut String, v: f64) -> P<()> {
    if !v.is_finite() {
        return fail(format!("Out of range float values are not JSON compliant: {}", py_repr(v)));
    }
    out.push_str(&py_repr(v));
    Ok(())
}

fn json_style(out: &mut String, v: &StyleVal) -> P<()> {
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

fn json_floats(out: &mut String, values: &[f64]) -> P<()> {
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

/// `_stroke_input`, emitted straight to compact JSON. Returns the path length.
fn stroke_json(out: &mut String, record: &Record, binding: &Binding, width: i64, height: i64) -> P<f64> {
    use rs_ink::core::num::{f32r, Val};
    let f = |v: f64| f32r(v).map_err(PrepErr::from);
    let scale = rs_ink::core::coords::page_coordinate_scale(&record.frames[0], Val::Int(width as i128), Val::Int(height as i128), Val::Int(0))?;
    let mut points = Vec::with_capacity(record.samples.points.len());
    for (x, y) in &record.samples.points {
        points.push((f(f(*x)? * scale)?, f(f(*y)? * scale)?));
    }
    if points.len() < 2 {
        return fail("Tek noktalı çizginin görünümü henüz doğrulanmadı.");
    }
    let p = props(record);
    let mut style: Vec<(&'static str, StyleVal)> = Vec::new();
    for (k, v) in &binding.style.0 {
        let Some((_, mapped)) = STYLE.iter().find(|(a, _)| a == k) else {
            return fail("Bilinmeyen kalem ayarı.");
        };
        style.push((mapped, v.clone()));
    }
    for field in ["pen_size", "fixed_width", "initial_tolerance"] {
        if let Some(entry) = style.iter_mut().find(|(k, _)| *k == field) {
            let value = match &entry.1 {
                StyleVal::I(i) => *i as f64,
                StyleVal::F(v) => *v,
                StyleVal::G(_) => return fail(rs_ink::core::num::UNREPRESENTABLE),
            };
            entry.1 = StyleVal::F(f(f(value)? * scale)?);
        }
    }
    let get = |k: &str| style.iter().find(|(n, _)| *n == k).map(|(_, v)| v);
    if record.samples.timestamps.iter().any(|t| *t < 0 || *t >= 1 << 31) {
        return fail("Çizgi zaman profili desteklenmiyor.");
    }
    if prop(&p, "fixed_width") && !style_num(get("fixed_width")).is_some_and(|v| get("fixed_width").is_some() && (0.0..=1024.0).contains(&v)) {
        return fail("Sabit kalem genişliği desteklenmiyor.");
    }
    if !style_num(get("initial_tolerance")).is_some_and(|v| (0.0..=1000.0).contains(&v)) {
        return fail("Kalem toleransı desteklenmiyor.");
    }
    let mut bounds = [0.0; 4];
    for (i, v) in binding.bounds.iter().enumerate() {
        bounds[i] = f(f(*v)? * scale)?;
    }
    let argb = match get("color_argb") {
        Some(StyleVal::I(v)) => *v,
        Some(StyleVal::F(v)) => *v as i64,
        _ => return fail("KeyError: 'color_argb'"),
    };
    let Some(pen_width) = get("pen_size").cloned() else { return fail("KeyError: 'pen_size'") };
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

// ---------------------------------------------------------------- body text

/// `_blank_text`: body text is base64(base64(UTF-8)); only whitespace is blank.
fn blank_text(node: &Element) -> bool {
    let value = pyval::py_strip(node.text.as_deref().unwrap_or(""));
    if value.is_empty() {
        return true;
    }
    let Some(inner) = pyval::b64decode_strict(value) else { return false };
    let Ok(inner) = String::from_utf8(inner) else { return false };
    let Some(raw) = pyval::b64decode_strict(&inner) else { return false };
    match String::from_utf8(raw) {
        Ok(text) => pyval::py_strip(&text).is_empty(),
        Err(_) => false,
    }
}

/// `_empty_body_for_ink`: empty text plus the zero-area SDK decoration shell.
pub fn empty_body_for_ink(body: &Element) -> P<bool> {
    const BASE: [&str; 7] = ["bodyText", "object", "rect", "originalRect", "text", "paragraph", "parsingState"];
    const SPANS: [&str; 4] = ["spanOrderDataList", "spanOrderData", "zeroLengthSpanList", "zeroLengthSpan"];
    const EFFECTS: [&str; 3] = ["lineColorEffect", "radialGradientPosition", "lineStyleEffect"];
    let known = |t: &str| BASE.contains(&t) || SPANS.contains(&t) || EFFECTS.contains(&t);
    if body.iter().any(|n| !known(&n.tag)) {
        return Ok(false);
    }
    for n in iter_tag(body, "text") {
        if !blank_text(n) || n.children.iter().any(|c| !SPANS.contains(&c.tag.as_str())) {
            return Ok(false);
        }
    }
    let present = body.iter().filter(|n| EFFECTS.contains(&n.tag.as_str())).count();
    if present == 0 {
        return Ok(true);
    }
    if body.children.len() != 1 || body.children[0].tag != "object" || body.children[0].get("type") != Some("textBox") {
        return Ok(false);
    }
    let obj = &body.children[0];
    if obj.get("shapeType") != Some("4") || !matches!(obj.get("originalDegree"), Some("0") | Some("0.0")) {
        return Ok(false);
    }
    for tag in ["rect", "originalRect"] {
        let nodes = obj.findall(tag);
        if nodes.len() != 1 || !same_set(nodes[0], &["l", "t", "r", "b"]) || !nodes[0].children.is_empty() {
            return Ok(false);
        }
        if !all_zero(nodes[0])? {
            return Ok(false);
        }
    }
    let color = obj.findall("lineColorEffect");
    let style = obj.findall("lineStyleEffect");
    if color.len() != 1 || style.len() != 1 || present != 3 {
        return Ok(false);
    }
    let (color, style) = (color[0], style[0]);
    if !same_set(color, &["angle", "colorType", "gradientRotatable", "gradientType", "solidColor"]) {
        return Ok(false);
    }
    if color.get("angle") != Some("0") || color.get("colorType") != Some("2") || color.get("gradientType") != Some("0")
        || color.get_or("gradientRotatable", "").to_lowercase() != "false"
    {
        return Ok(false);
    }
    let value = pyint(color.get("solidColor"))?;
    if !(-(1i128 << 31) <= value && value < 1i128 << 32) {
        return Ok(false);
    }
    if color.children.len() != 1 || color.children[0].tag != "radialGradientPosition" || !same_set(&color.children[0], &["x", "y"])
        || !all_zero(&color.children[0])? || !color.children[0].children.is_empty()
    {
        return Ok(false);
    }
    let fields = ["beginArrowSize", "beginArrowType", "capType", "compoundType", "dashType", "endArrowSize", "endArrowType", "joinType", "width"];
    if !same_set(style, &fields) || !all_zero(style)? || !style.children.is_empty() {
        return Ok(false);
    }
    Ok(true)
}

/// `_TOKEN.finditer(body)` for `\s+'([A-Za-z]+)/([A-Za-z]+)'(?:='([^']*)')?`
/// (`\s` = `str.isspace`). Returns (whole, group, kind, value) matches.
fn tokens(body: &str) -> Vec<[Option<String>; 4]> {
    let chars: Vec<char> = body.chars().collect();
    let alpha = |c: char| c.is_ascii_alphabetic();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let attempt = (|| -> Option<(usize, [Option<String>; 4])> {
            let mut j = i;
            while j < chars.len() && pyval::py_isspace(chars[j]) {
                j += 1;
            }
            if j == i || chars.get(j) != Some(&'\'') {
                return None;
            }
            j += 1;
            let g1 = j;
            while j < chars.len() && alpha(chars[j]) {
                j += 1;
            }
            if j == g1 || chars.get(j) != Some(&'/') {
                return None;
            }
            let group: String = chars[g1..j].iter().collect();
            j += 1;
            let g2 = j;
            while j < chars.len() && alpha(chars[j]) {
                j += 1;
            }
            if j == g2 || chars.get(j) != Some(&'\'') {
                return None;
            }
            let kind: String = chars[g2..j].iter().collect();
            j += 1;
            let mut value = None;
            if chars.get(j) == Some(&'=') && chars.get(j + 1) == Some(&'\'') {
                let start = j + 2;
                let mut k = start;
                while k < chars.len() && chars[k] != '\'' {
                    k += 1;
                }
                if k < chars.len() {
                    value = Some(chars[start..k].iter().collect::<String>());
                    j = k + 1;
                }
            }
            Some((j, [Some(chars[i..j].iter().collect()), Some(group), Some(kind), value]))
        })();
        match attempt {
            Some((end, m)) => {
                out.push(m);
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

enum Item {
    Text(String),
    Raw(String),
    Tokens(i64, Vec<[Option<String>; 4]>),
}

/// The body flow as renderer input (`_flow_body` result).
pub struct Flow {
    pub text: String,
    pub spans: Vec<J>,
    pub paragraphs: Vec<J>,
    /// (start, end) pairs of saved per-page text sections, unsorted.
    pub sections: Vec<(i64, i64)>,
    pub margins: [f64; 4],
}

fn to_etree(e: &Element) -> rs_editor::etree::Element {
    rs_editor::etree::Element {
        tag: e.tag.clone(),
        attrib: e.attrib.clone(),
        text: e.text.clone(),
        tail: e.tail.clone(),
        children: e.children.iter().map(to_etree).collect(),
    }
}

fn editor_fail(e: rs_editor::rich::Error) -> PrepErr {
    use rs_editor::rich::Error as E;
    PrepErr(match e {
        E::Document(m) | E::Value(m) | E::Type(m) | E::Overflow(m) | E::Attribute(m) | E::Parse(m) | E::Payload(m) => m,
        E::External { message, .. } => message,
    })
}

fn model_json(v: &PVal) -> P<J> {
    Ok(match v {
        PVal::None => J::Null,
        PVal::Bool(b) => J::Bool(*b),
        PVal::Int(i, _) => J::RawInt(i.0.clone()),
        PVal::Float(f) => J::Float(*f),
        PVal::Str(s) => J::Str(s.clone()),
        PVal::List(items) => J::List(items.iter().map(model_json).collect::<P<_>>()?),
        PVal::Dict(items) => {
            let mut out = Vec::new();
            for (k, v) in items {
                let Some(k) = k.as_str() else { return fail("keys must be str") };
                out.push((k.to_string(), model_json(v)?));
            }
            J::Obj(out)
        }
        PVal::BadStr(_) | PVal::Other(_) => return fail("unrepresentable rich-text value"),
    })
}

/// Python `int(value)` for a model value.
fn model_int(v: &PVal) -> P<i128> {
    match v {
        PVal::Bool(b) => Ok(*b as i128),
        PVal::Int(i, _) => i.to_i128().map_or_else(|| fail("int too large"), Ok),
        PVal::Float(f) if f.is_finite() => Ok(f.trunc() as i128),
        PVal::Float(_) => fail("cannot convert float to integer"),
        PVal::Str(s) => pyint(Some(s)),
        _ => fail("int() argument must be a string or a real number"),
    }
}

/// Python `int(text, 16)`.
fn hex_int(text: &str) -> P<i128> {
    let s = pyval::py_strip(text);
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (mut body, prefixed) = match rest.get(..2) {
        Some("0x") | Some("0X") => (&rest[2..], true),
        _ => (rest, false),
    };
    if prefixed {
        body = body.strip_prefix('_').unwrap_or(body);
    }
    let bad = || fail(format!("invalid literal for int() with base 16: {text:?}"));
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return bad();
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return bad();
    }
    let trimmed = digits.trim_start_matches('0');
    if trimmed.len() > 30 {
        return fail("int too large");
    }
    let value = if trimmed.is_empty() { 0 } else { i128::from_str_radix(trimmed, 16).map_err(|_| PrepErr("int".into()))? };
    Ok(if neg { -value } else { value })
}

/// `list(struct.pack('<I', value))`
fn pack_u32(value: i128) -> P<J> {
    if !(0..=u32::MAX as i128).contains(&value) {
        return fail("argument out of range");
    }
    Ok(J::List((value as u32).to_le_bytes().iter().map(|b| J::Int(*b as i128)).collect()))
}

/// `list(struct.pack('<f', value))`
fn pack_f32(v: &PVal) -> P<J> {
    let x = match v {
        PVal::Float(f) => *f,
        PVal::Int(i, _) => i.to_f64(),
        PVal::Bool(b) => *b as i64 as f64,
        _ => return fail("required argument is not a float"),
    };
    let y = x as f32;
    if y.is_infinite() && x.is_finite() {
        return fail("float too large to pack with f format");
    }
    Ok(J::List(y.to_le_bytes().iter().map(|b| J::Int(*b as i128)).collect()))
}

fn lookup(table: &[(&str, i64)], key: &str) -> Option<i64> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// `_flow_body(body)`: None when the body paints nothing.
pub fn flow_body(body: &Element) -> P<Option<Flow>> {
    if body.children.len() != 1 || body.children[0].tag != "object" {
        return fail("Metin ve çizim birleşimi desteklenmiyor.");
    }
    text_object(&body.children[0], false, false)
}

/// `flow_body` keeping the text when some of its formatting is not taken by
/// the renderer: unsupported spans/paragraph fields are dropped.
pub fn flow_body_lenient(body: &Element) -> P<Option<Flow>> {
    if body.children.len() != 1 || body.children[0].tag != "object" {
        return fail("Metin ve çizim birleşimi desteklenmiyor.");
    }
    text_object(&body.children[0], false, true)
}

/// A page text box as renderer input (`textBoxes` item): Ok(None) when it
/// paints no text; the flag tells whether formatting was dropped.
fn text_box_input(obj: &Element, at: usize) -> P<Option<(J, bool)>> {
    let rect = obj.findall("rect");
    if rect.len() != 1 {
        return fail("Metin kutusu geometrisi desteklenmiyor.");
    }
    let bbox: Vec<f64> = ["l", "t", "r", "b"].iter().map(|k| pyfloat(rect[0].get_or(k, "nan"))).collect::<P<_>>()?;
    if bbox.iter().any(|v| !v.is_finite()) || bbox[2] <= bbox[0] || bbox[3] <= bbox[1] {
        return fail("Metin kutusu geometrisi desteklenmiyor.");
    }
    let (flow, approximate) = match text_object(obj, true, false) {
        Ok(f) => (f, false),
        Err(_) => (text_object(obj, true, true)?, true),
    };
    let Some(flow) = flow else { return Ok(None) };
    let gravity = pyint(Some(obj.get_or("textGravity", "0"))).unwrap_or(0).clamp(0, 2);
    Ok(Some((J::obj(vec![
        ("at", J::Int(at as i128)),
        ("bbox", J::List(bbox.iter().map(|v| J::Float(*v)).collect())),
        ("text", J::Str(flow.text)),
        ("spans", J::List(flow.spans)),
        ("paragraphs", J::List(flow.paragraphs)),
        ("margins", J::List(flow.margins.iter().map(|m| J::Float(*m)).collect())),
        ("gravity", J::Int(gravity)),
    ]), approximate || obj.get("originalDegree").is_some_and(|d| pyfloat(d).map_or(true, |d| d != 0.0)))))
}

/// The text object reader behind `flow_body` (`page_box`: a text box on a
/// page, whose frame is real and whose attributes vary; `lenient`: drop
/// formatting the renderer does not take instead of failing).
fn text_object(obj: &Element, page_box: bool, lenient: bool) -> P<Option<Flow>> {
    if page_box {
        if obj.get("type") != Some("textBox") {
            return fail("Metin nesnesinin biçimi desteklenmiyor.");
        }
    } else if obj.get("type") != Some("textBox") || obj.get("shapeType") != Some("4")
        || !matches!(obj.get("originalDegree"), Some("0") | Some("0.0")) || obj.get_or("textGravity", "0") != "0"
        || obj.attrib.iter().any(|(k, _)| !BODY_ATTRS.contains(&k.as_str()))
    {
        return fail("Metin nesnesinin biçimi desteklenmiyor.");
    }
    if !page_box {
        let allowed = ["rect", "originalRect", "text", "lineColorEffect", "lineStyleEffect", "spanOrderDataList", "zeroLengthSpanList"];
        if obj.children.iter().any(|c| !allowed.contains(&c.tag.as_str())) {
            return fail("Metne gömülü nesne veya biçim desteklenmiyor.");
        }
        let mut copy = Element { tag: "object".into(), attrib: obj.attrib.clone(), ..Default::default() };
        for c in &obj.children {
            if !matches!(c.tag.as_str(), "text" | "spanOrderDataList" | "zeroLengthSpanList") {
                copy.children.push(c.clone());
            }
        }
        let shell = Element { tag: "bodyText".into(), children: vec![copy], ..Default::default() };
        if !empty_body_for_ink(&shell)? || obj.findall("rect").len() != 1 || obj.findall("originalRect").len() != 1 {
            return fail("Metin kutusu geometrisi desteklenmiyor.");
        }
    }
    let texts = obj.findall("text");
    if texts.len() != 1 || !texts[0].children.is_empty() {
        return fail("Metin biçimi desteklenmiyor.");
    }
    let parsed = (|| -> P<(PVal, Vec<(i64, i64)>)> {
        let decode = |s: &str| rs_editor::rich::decode(s).map_err(editor_fail);
        let stream = decode(texts[0].text.as_deref().unwrap_or(""))?;
        let mut items: Vec<Item> = Vec::new();
        let (mut cursor, mut position) = (0usize, 0i64);
        while cursor < stream.len() {
            let marker = stream[cursor..].find("<?a").map(|i| i + cursor);
            let end = marker.unwrap_or(stream.len());
            let chunk = &stream[cursor..end];
            items.push(Item::Text(chunk.to_string()));
            position += rs_editor::rich::utf16_length(&decode(chunk)?) as i64;
            let Some(marker) = marker else { break };
            let Some(close) = stream[marker + 3..].find("?>").map(|i| i + marker + 3) else {
                return fail("Metin biçimi desteklenmiyor.");
            };
            let body_tokens = &stream[marker + 3..close];
            let found = tokens(body_tokens);
            let joined: String = found.iter().map(|t| t[0].as_deref().unwrap_or("")).collect();
            items.push(if joined != body_tokens { Item::Raw(body_tokens.to_string()) } else { Item::Tokens(position, found) });
            cursor = close + 2;
        }
        let mut opens: HashMap<(String, String), Vec<(usize, usize, i64)>> = HashMap::new();
        let mut sections: Vec<(i64, i64)> = Vec::new();
        for i in 0..items.len() {
            let (at, count) = match &items[i] {
                Item::Tokens(at, t) => (*at, t.len()),
                _ => continue,
            };
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
        let mut clean = rs_editor::etree::Element::new("object");
        let mut text = rs_editor::etree::Element::new("text");
        text.text = Some(rs_editor::rich::b64(&out));
        clean.children.push(text);
        for c in &obj.children {
            if c.tag == "spanOrderDataList" || c.tag == "zeroLengthSpanList" {
                clean.children.push(to_etree(c));
            }
        }
        let rich = rs_editor::edit::object_rich(&clean).map_err(editor_fail)?;
        Ok((rich.model, sections))
    })();
    let (model, sections) = parsed.map_err(|e| PrepErr(format!("Metin biçimi desteklenmiyor. ({})", e.0)))?;
    let Some(model) = model.dict() else { return fail("model") };
    let text = match pyval::dget(model, "text") {
        Some(PVal::Str(s)) => s.clone(),
        _ => return fail("model text"),
    };
    let empty: Vec<PVal> = Vec::new();
    let spans_in = pyval::dget(model, "spans").and_then(|v| v.list()).unwrap_or(&empty);
    let paragraphs_in = pyval::dget(model, "paragraphs").and_then(|v| v.list()).unwrap_or(&empty);
    let kind_of = |s: &PVal| -> P<String> {
        match s.dict().and_then(|d| pyval::dget(d, "kind")) {
            Some(PVal::Str(k)) => Ok(k.clone()),
            _ => fail("span kind"),
        }
    };
    if text.trim_matches([' ', '\t', '\r', '\n']).is_empty() {
        let mut quiet = true;
        for s in spans_in {
            if !["fontSize", "foregroundColor", "bold", "italic"].contains(&kind_of(s)?.as_str()) {
                quiet = false;
            }
        }
        for p in paragraphs_in {
            let Some(d) = p.dict() else { return fail("paragraph") };
            if d.iter().any(|(k, _)| !matches!(k.as_str(), Some("index") | Some("alignment"))) {
                quiet = false;
            }
        }
        if quiet || lenient {
            return Ok(None);
        }
        return fail("Boş satırlardaki metin biçimi desteklenmiyor.");
    }
    let mut spans = Vec::new();
    for s in spans_in {
        let span = (|| -> P<J> {
        let kind = kind_of(s)?;
        if !FLOW_SPANS.contains(&kind.as_str()) {
            return fail("Metin biçimi desteklenmiyor.");
        }
        let d = s.dict().unwrap();
        let field = |k: &str| pyval::dget(d, k).ok_or_else(|| PrepErr(format!("KeyError: {k:?}")));
        let value = field("value")?;
        let payload = if kind == "fontSize" {
            pack_f32(value)?
        } else if kind == "foregroundColor" || kind == "backgroundColor" {
            let PVal::Str(hex) = value else { return fail("color value") };
            pack_u32(hex_int(&hex.chars().skip(1).collect::<String>())?)?
        } else {
            pack_u32(model_int(value)?)?
        };
        let Some(id) = lookup(rs_editor::rich::SPAN_TYPES, &kind) else { return fail("span type") };
        Ok(J::obj(vec![("kind", J::Int(id as i128)), ("start", model_json(field("start")?)?),
            ("end", model_json(field("end")?)?), ("interval", model_json(field("interval")?)?), ("payload", payload)]))
        })();
        match span {
            Ok(span) => spans.push(span),
            Err(_) if lenient => {}
            Err(e) => return Err(e),
        }
    }
    let mut paragraphs = Vec::new();
    for p in paragraphs_in {
        let Some(d) = p.dict() else { return fail("paragraph") };
        if !lenient && d.iter().any(|(k, _)| !matches!(k.as_str(), Some("index") | Some("alignment") | Some("parsingState"))) {
            return fail("Paragraf biçimi desteklenmiyor.");
        }
        let index = || -> P<i128> {
            match pyval::dget(d, "index") {
                Some(PVal::Int(i, _)) => i.to_i128().map_or_else(|| fail("index"), Ok),
                Some(PVal::Bool(b)) => Ok(*b as i128),
                _ => fail("KeyError: 'index'"),
            }
        };
        if let Some(alignment) = pyval::dget(d, "alignment") {
            match alignment.as_str().and_then(|a| lookup(rs_editor::rich::ALIGNMENTS, a)) {
                Some(value) => {
                    let i = index()?;
                    paragraphs.push(J::obj(vec![("kind", J::Int(3)), ("start", J::Int(i)), ("end", J::Int(i + 1)), ("payload", pack_u32(value as i128)?)]));
                }
                None if lenient => {}
                None => return fail("KeyError: alignment"),
            }
        }
        if let Some(state) = pyval::dget(d, "parsingState") {
            let value = model_int(state)?;
            let i = index()?;
            paragraphs.push(J::obj(vec![("kind", J::Int(6)), ("start", J::Int(i)), ("end", J::Int(i + 1)), ("payload", pack_u32(value)?)]));
        }
    }
    let mut margins = [0.0; 4];
    for (i, k) in ["leftMargin", "topMargin", "rightMargin", "bottomMargin"].iter().enumerate() {
        margins[i] = match pyfloat(obj.get_or(k, "0")) {
            Ok(v) if lenient && !(v.is_finite() && (0.0..=1000.0).contains(&v)) => 0.0,
            Ok(v) => v,
            Err(_) if lenient => 0.0,
            Err(e) => return Err(e),
        };
    }
    if margins.iter().any(|v| !v.is_finite() || !(0.0..=1000.0).contains(v)) {
        return fail("Metin kutusu yerleşimi desteklenmiyor.");
    }
    Ok(Some(Flow { text, spans, paragraphs, sections, margins }))
}

/// `_page_color(page)`
fn page_color(page: &Element) -> P<u32> {
    let raw = page.get_or("bgColor", "");
    if raw.is_empty() || raw.chars().count() > 11 || pyval::py_strip(raw) != raw {
        return fail("Geçersiz sayfa rengi.");
    }
    let color = pyint(Some(raw))?;
    if !(-(1i128 << 31) <= color && color < 1i128 << 32) {
        return fail("Geçersiz sayfa rengi.");
    }
    let color = (color & 0xffff_ffff) as u32;
    if color >> 24 != 255 {
        return fail("Sayfa arka planı opak değil.");
    }
    Ok(color)
}

/// `_flow_layout(attrs, pages, body)`: the flow object (without pageIndex)
/// and the trailing list-compatibility page to omit.
fn flow_layout(attrs: &Element, pages: &Element, body: Flow) -> P<(Vec<(String, J)>, Option<i64>)> {
    if attrs.get("pageMode") != Some("0") || attrs.get_or("orientation", "0") != "0" {
        return fail("Metin akış sayfa düzeni desteklenmiyor.");
    }
    let pad = (integer(attrs.get("pageHorizontalPadding"))?, integer(attrs.get("pageVerticalPadding"))?);
    let size = (integer(attrs.get("width"))?, integer(attrs.get("height"))?);
    let mut geometry: Vec<(i64, i64, u32)> = Vec::new();
    for p in &pages.children {
        geometry.push((integer(p.get("width"))?, integer(p.get("height"))?, page_color(p)?));
    }
    let total = geometry.iter().map(|g| g.1 as i128).sum::<i128>() + (geometry.len() as i128 - 1) * pad.1 as i128;
    if geometry.is_empty() || geometry.len() > 4096 || pad.0.max(pad.1) > 32768 || total != size.1 as i128 {
        return fail("Metin akışı sayfa ölçüleriyle uyuşmuyor.");
    }
    let default = if attrs.has("pageDefaultWidth") || attrs.has("pageDefaultHeight") {
        (integer(attrs.get("pageDefaultWidth"))?, integer(attrs.get("pageDefaultHeight"))?)
    } else {
        (size.0, geometry[0].1)
    };
    let delta = match attrs.get("bodyTextFontSizeDelta") {
        None => None,
        Some(raw) => Some(pyint(Some(raw))?),
    };
    if delta.is_some_and(|d| !(-100..=100).contains(&d)) {
        return fail("Metin boyutu ayarı desteklenmiyor.");
    }
    let mut sections = body.sections.clone();
    sections.sort();
    let mut sections: Vec<J> = sections.into_iter().map(|(s, e)| J::List(vec![J::Int(s as i128), J::Int((e - s) as i128)])).collect();
    if !sections.is_empty() {
        if sections.len() > geometry.len() {
            return fail("Metin sayfa bölümleri sayfalarla uyuşmuyor.");
        }
        while sections.len() < geometry.len() {
            sections.push(J::List(vec![J::Int(-1), J::Int(0)]));
        }
    }
    let trailing = geometry.len() > 1 && {
        let (last, previous) = (geometry[geometry.len() - 1], geometry[geometry.len() - 2]);
        iter_tag(pages.children.last().unwrap(), "object").next().is_none() && last == previous && size.0 == last.0
    };
    let pair = |a: i64, b: i64| J::List(vec![J::Int(a as i128), J::Int(b as i128)]);
    let flow = vec![
        ("text".to_string(), J::Str(body.text)),
        ("spans".into(), J::List(body.spans)),
        ("paragraphs".into(), J::List(body.paragraphs)),
        ("sections".into(), J::List(sections)),
        ("margins".into(), J::List(body.margins.iter().map(|m| J::Float(*m)).collect())),
        ("gravity".into(), J::Int(0)),
        ("pages".into(), J::List(geometry.iter().map(|(w, h, c)| J::obj(vec![("width", J::Int(*w as i128)),
            ("height", J::Int(*h as i128)), ("backgroundArgb", J::Int(*c as i128))])).collect())),
        ("padding".into(), pair(pad.0, pad.1)),
        ("fontSizeDelta".into(), delta.map_or(J::Null, J::Int)),
        ("defaultPage".into(), pair(default.0, default.1)),
        ("flowSize".into(), pair(size.0, size.1)),
    ];
    Ok((flow, if trailing { Some(geometry.len() as i64 - 1) } else { None }))
}

// ---------------------------------------------------------------- verification and records

/// `ink_projection.verified_resource(snapshot)`: rs-ops `verify` over the
/// pure host deps. The verified payload's parts and its single noteResource
/// root, or the disabled reason.
enum Verified {
    Reason(&'static str),
    Ok(Vec<(Option<String>, Vec<u8>)>, Element),
}

fn verify(snapshot: &rs_doc::doc::DocumentSnapshot) -> P<Verified> {
    let snap = rs_ops::deps::Snapshot {
        raw_body: snapshot.raw_body.clone(), content_type: snapshot.content_type.clone(),
        resource_xml: snapshot.resource_xml.clone(), local_id: snapshot.local_id.clone(),
        commit_id: snapshot.commit_id.clone(), handle: None,
    };
    match rs_ops::ink_projection::verify(&notes_host::PureDeps::new(), &snap) {
        Err(e) => fail(format!("{e:?}")),
        Ok(rs_ops::ink_projection::Verified::Reason(reason)) => Ok(Verified::Reason(reason)),
        Ok(rs_ops::ink_projection::Verified::Ok(payload, root)) => {
            Ok(Verified::Ok(payload.parts.into_iter().map(|p| (p.name, p.body)).collect(), root))
        }
    }
}

/// A decoded stroke: its record, bound metadata and whether the renderer
/// admits its pen (the others are approximated by the page composite).
pub struct Rec {
    pub record: Record,
    pub binding: Binding,
    pub admitted: bool,
}

type Records = HashMap<String, Option<Rec>>;

/// Decode, bind and admit every stroke part.
fn decode_records(resource: &[u8], bodies: &[Vec<u8>]) -> P<Records> {
    use rs_ink::core::binary::{decode_stroke_resource, i32_at, py_slice};
    use rs_ink::core::xml::Tree;
    let tree = Tree::parse(resource)?;
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
        let record = decode_stroke_resource(body)?;
        let base = &record.frames[0];
        let identity = py_slice(&base.raw, base.fixed_start + 6, Some(base.fixed_start + 42));
        let identity = match std::str::from_utf8(identity) {
            Ok(s) if s.is_ascii() => s.to_string(),
            _ => return fail("'ascii' codec can't decode bytes"),
        };
        let Some(&node) = objects.get(&identity) else { return fail("Çizgi kaynağı notla eşleşmiyor.") };
        if records.contains_key(&identity) {
            return fail("Çizgi kaynağı notla eşleşmiyor.");
        }
        let binding = rs_ink::core::prepare::bind_stroke_metadata(&record.frames, (&tree, node), table)?;
        let rotation = i32_at(&base.raw, base.flexible_start - 5)?;
        let ok = admitted(&record, &binding) && rotation == 0;
        records.insert(identity, Some(Rec { record, binding, admitted: ok }));
    }
    Ok(records)
}

type SourceKey = ([u8; 32], [u8; 32], String, Option<String>, Option<String>);

/// Everything `_prepare_pages` derives before its page loop.
struct Prepared {
    pages: Element,
    flow: Option<Vec<(String, J)>>,
    /// The body text needed the lenient (formatting-dropping) reading.
    flow_approximate: bool,
    trailing: Option<i64>,
    records: Records,
    /// contentFileList bindNumber -> SHA256 (images).
    files: HashMap<String, String>,
    /// Some page carries objects, a PDF or a canvas cache: a handwritten /
    /// page-based note whose empty pages are shown as blank sheets.
    page_based: bool,
}

static PREPARED: Mutex<VecDeque<(SourceKey, Arc<Prepared>)>> = Mutex::new(VecDeque::new());
type PageKey = (SourceKey, Option<i64>, bool);
static PAGES: Mutex<VecDeque<(PageKey, Arc<Vec<PageInput>>, usize)>> = Mutex::new(VecDeque::new());

fn prepare_source(snapshot: &rs_doc::doc::DocumentSnapshot, key: &SourceKey) -> P<Arc<Prepared>> {
    if let Some((_, hit)) = PREPARED.lock().unwrap().iter().find(|(k, _)| k == key) {
        return Ok(hit.clone());
    }
    let (parts, root) = match verify(snapshot)? {
        Verified::Reason(reason) => return fail(reason),
        Verified::Ok(parts, root) => (parts, root),
    };
    let tree = one(&root, "notetree")?;
    let attrs = one(&root, "attributes")?;
    if attrs.get("formatVersion") != Some("4000") || tree.get("ver") != Some("4000")
        || tree.get_or("isStrokeGrouped", "false").to_lowercase() != "false"
    {
        return fail("Bu notun kalem biçimi henüz görüntülenemiyor.");
    }
    let allowed = ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];
    if root.children.iter().any(|n| !allowed.contains(&n.tag.as_str())) {
        return fail("Bu not başka içerikler de taşıyor.");
    }
    let body = one(&root, "bodytext")?;
    let floating = one(&root, "floatingobjectlist")?;
    if !floating.children.is_empty() {
        return fail("Bu not eklenmiş nesneler taşıyor.");
    }
    let pages = one(&root, "pagelist")?;
    let (mut flow, mut trailing, mut flow_approximate) = (None, None, false);
    if !empty_body_for_ink(body)? {
        // Body text whose formatting the renderer does not take is shown with
        // that formatting dropped (marked approximate) instead of hiding the note.
        let parsed = match flow_body(body) {
            Ok(parsed) => parsed,
            Err(strict) => {
                flow_approximate = true;
                flow_body_lenient(body).map_err(|_| strict)?
            }
        };
        if let Some(parsed) = parsed {
            let (f, t) = flow_layout(attrs, pages, parsed)?;
            flow = Some(f);
            trailing = t;
        }
    }
    let page_based = pages.children.iter().any(|p| {
        p.iter().any(|n| n.tag == "object" || n.tag == "pdfDataList" || (n.tag == "canvasCacheDataMap" && !n.children.is_empty()))
    });
    let bodies: Vec<Vec<u8>> = parts.into_iter().filter(|(name, _)| name.as_deref() == Some("strokeResource")).map(|(_, b)| b).collect();
    let records = decode_records(&snapshot.resource_xml, &bodies)?;
    let files = super::objects::content_files(&root);
    let prepared = Arc::new(Prepared { pages: pages.clone(), flow, flow_approximate, trailing, records, files, page_based });
    let mut guard = PREPARED.lock().unwrap();
    guard.push_back((key.clone(), prepared.clone()));
    while guard.len() > 2 {
        guard.pop_front();
    }
    Ok(prepared)
}

fn renderer_tail(text: &mut String, inkpen2: bool, marker4: bool) {
    if inkpen2 {
        text.push_str(",\"inkpen2Profile\":");
        text.push_str(INKPEN2_PROFILE);
        text.push_str(",\"admit\":\"calibrated\"");
    } else if marker4 {
        text.push_str(",\"admit\":\"calibrated\"");
    }
}

fn admitted_stroke(records: &Records, o: &Element) -> bool {
    o.get("type") == Some("stroke") && matches!(o.get("id").and_then(|i| records.get(i)), Some(Some(Rec { admitted: true, .. })))
}

fn page_inputs(prepared: &Prepared, only_index: Option<i64>, build: bool) -> P<Vec<PageInput>> {
    let records = &prepared.records;
    let blocked = ["pdfDataList", "pdfData", "strokeGroup", "customObject", "shape", "image"];
    let mut output = Vec::new();
    for (index, page) in prepared.pages.children.iter().enumerate() {
        let index = index as i64;
        if only_index.is_some_and(|i| i != index) {
            continue;
        }
        let width = integer(page.get("width"))?;
        let height = integer(page.get("height"))?;
        let all_objects: Vec<&Element> = iter_tag(page, "object").collect();
        // Recognised shapes/lines are drawn by the page composite.
        let (shapes, objects): (Vec<&Element>, Vec<&Element>) =
            all_objects.iter().copied().partition(|o| rs_ops::page_shapes::renderer_skippable_shape(o));
        let has_shape = !shapes.is_empty();
        let pdf = page.iter().any(|n| n.tag == "pdfDataList" || n.tag == "pdfData");
        if width as i128 * height as i128 > MAX_PIXELS as i128 || page.get_or("orientation", "0") != "0" {
            continue;
        }
        if pdf {
            // PDF pages get their background from Poppler; the page's own
            // objects become a local layer over it (`overPdf`), used when
            // Samsung's canvas overlay is not available.
            if !all_objects.is_empty() {
                if let Some(page) = composite_layer(prepared, page, index, width, height, page_color(page).ok(), build, false, true)? {
                    output.push(page);
                }
            }
            continue;
        }
        if Some(index) == prepared.trailing {
            // The empty list-compatibility page after a text flow: a blank
            // sheet (the flow places no text on it).
            if let Some(page) = composite_page(prepared, page, index, width, height, page_color(page).ok(), build, false)? {
                output.push(page);
            }
            continue;
        }
        let skip = objects.iter().any(|o| !admitted_stroke(records, o));
        let children_ok = page.children.iter().all(|n| matches!(n.tag.as_str(), "layerList" | "drawnRect" | "canvasCacheDataMap"));
        let strict = !skip && !page.iter().any(|n| blocked.contains(&n.tag.as_str())) && children_ok
            && !page.attrib.iter().any(|(k, _)| !PAGE_ATTRS.contains(&k.as_str()))
            && page.get_or("bgImageId", "-1") == "-1" && page.get_or("bgImageMode", "2") == "2";
        let color = if strict {
            // Pages the renderer always admitted keep their note-level checks.
            if page.has("bgWidth") {
                let w = integer(page.get("bgWidth"))?;
                if !(0 < w && w <= 65535) {
                    return fail("Bu sayfanın arka planı veya dönüşümü desteklenmiyor.");
                }
            }
            if page.has("editableFormatVersion") {
                let editable = page.get("editableFormatVersion");
                if !(editable == page.get("formatVersion") && editable == Some("4000")) {
                    return fail("Bu sayfanın sürümü henüz desteklenmiyor.");
                }
            }
            Some(page_color(page)?)
        } else {
            page_color(page).ok()
        };
        let blank = prepared.flow.is_none() && all_objects.is_empty();
        if blank && !prepared.page_based {
            continue;
        }
        let dark_of = |c: u32| ((c >> 16) & 255) + ((c >> 8) & 255) + (c & 255) < 384;
        if strict && !has_shape && !blank && !prepared.flow_approximate {
            let color = color.unwrap();
            let dark = dark_of(color);
            if !build {
                output.push(PageInput { index, width, height, dark_mode: dark, body: Vec::new(), composite: false, notice: None, over_pdf: false });
                continue;
            }
            let mut text = format!("{{\"schemaVersion\":1,\"width\":{width},\"height\":{height},\"backgroundArgb\":{color},\"strokes\":[");
            let mut distance = 0.0f64;
            let mut inkpen2 = false;
            let mut marker4 = false;
            let mut count = 0usize;
            for o in &objects {
                let Some(Some(Rec { record, binding, .. })) = records.get(o.get("id").unwrap_or("")) else { unreachable!("admitted page") };
                if count > 0 {
                    text.push(',');
                }
                count += 1;
                inkpen2 |= binding.pen_name == INKPEN2;
                marker4 |= binding.pen_name == MARKER4;
                distance += stroke_json(&mut text, record, binding, width, height)?;
                if distance > MAX_DISTANCE {
                    break;
                }
            }
            if (count == 0 && prepared.flow.is_none()) || distance > MAX_DISTANCE {
                continue;
            }
            text.push(']');
            if let Some(flow) = &prepared.flow {
                let mut copy = flow.clone();
                copy.push(("pageIndex".into(), J::Int(index as i128)));
                text.push_str(",\"flow\":");
                J::Obj(copy).dump(&mut text, true).map_err(PrepErr)?;
            }
            renderer_tail(&mut text, inkpen2, marker4);
            text.push('}');
            if text.len() > MAX_INPUT {
                return fail("Çizim verisi boyut sınırını aşıyor.");
            }
            output.push(PageInput { index, width, height, dark_mode: dark, body: text.into_bytes(), composite: false, notice: None, over_pdf: false });
            continue;
        }
        // Page composite: images, shapes, text boxes, approximated strokes
        // and blank sheets.
        if let Some(page) = composite_page(prepared, page, index, width, height, color, build, true)? {
            output.push(page);
        }
    }
    Ok(output)
}

/// A `page-reconstructed` page: the composite body (see `ink::render_composite`).
#[allow(clippy::too_many_arguments)]
fn composite_page(prepared: &Prepared, page: &Element, index: i64, width: i64, height: i64, color: Option<u32>, build: bool,
                  with_flow: bool) -> P<Option<PageInput>> {
    composite_layer(prepared, page, index, width, height, color, build, with_flow, false)
}

/// Composite name of the PDF page image a PDF page's local layer is drawn on.
pub const PDF_UNDERLAY: &str = "pdf-underlay";

#[allow(clippy::too_many_arguments)]
fn composite_layer(prepared: &Prepared, page: &Element, index: i64, width: i64, height: i64, color: Option<u32>, build: bool,
                   with_flow: bool, over_pdf: bool) -> P<Option<PageInput>> {
    use super::objects::{self, Item};
    let flow = if with_flow { prepared.flow.as_ref() } else { None };
    let records = &prepared.records;
    let mut notices: Vec<&'static str> = Vec::new();
    let note = |notices: &mut Vec<&'static str>, n: &'static str| {
        if !notices.contains(&n) {
            notices.push(n);
        }
    };
    let color = color.unwrap_or_else(|| {
        note(&mut notices, objects::NOTICE_HIDDEN);
        0xffff_ffff
    });
    if prepared.flow_approximate && flow.is_some() {
        note(&mut notices, objects::NOTICE_TEXT);
    }
    if page.get_or("bgImageId", "-1") != "-1" || page.get_or("bgImageMode", "2") != "2" {
        note(&mut notices, objects::NOTICE_BACKGROUND);
    }
    if page.children.iter().any(|n| !matches!(n.tag.as_str(), "layerList" | "drawnRect" | "canvasCacheDataMap")
        && !(over_pdf && n.tag == "pdfDataList"))
    {
        note(&mut notices, objects::NOTICE_HIDDEN);
    }
    let mut ordered = objects::layer_objects(page);
    if ordered.len() != iter_tag(page, "object").count() {
        // Objects outside the plain layer/objectList structure (groups,
        // containers): drawn where possible, flagged as incomplete.
        note(&mut notices, objects::NOTICE_HIDDEN);
        ordered = iter_tag(page, "object").filter(|o| o.get("type").is_some()).collect();
    }
    let mut items: Vec<Item> = Vec::new();
    if over_pdf {
        items.push(Item::Image { sha256: PDF_UNDERLAY.into(), rect: [0.0, 0.0, width as f64, height as f64], crop: None,
                                 flip_h: false, flip_v: false, degrees: 0.0, opacity: 1.0 });
    }
    let mut strokes = String::new();
    let mut fallback = String::new();
    let mut boxes: Vec<J> = Vec::new();
    let (mut count, mut distance, mut inkpen2, mut marker4) = (0usize, 0.0f64, false, false);
    for o in ordered {
        match o.get("type") {
            Some("stroke") => {
                let Some(Some(Rec { record, binding, admitted })) = o.get("id").and_then(|i| records.get(i)) else {
                    note(&mut notices, objects::NOTICE_HIDDEN);
                    continue;
                };
                if !build {
                    if !admitted {
                        note(&mut notices, objects::NOTICE_INK);
                    }
                    continue;
                }
                let polyline = objects::stroke_polyline(record, binding, width, height);
                // The renderer refuses a whole page for samples far outside it
                // (`EDGE`); such strokes are approximated instead.
                let inside = rs_ink::core::coords::page_coordinate_scale(&record.frames[0], rs_ink::core::num::Val::Int(width as i128),
                    rs_ink::core::num::Val::Int(height as i128), rs_ink::core::num::Val::Int(0)).is_ok_and(|scale| {
                    record.samples.points.iter().all(|(x, y)| {
                        let (x, y) = (x * scale, y * scale);
                        x >= -500.0 && y >= -500.0 && x <= width as f64 + 500.0 && y <= height as f64 + 500.0
                    })
                });
                if *admitted && inside && distance <= MAX_DISTANCE {
                    let mut one = String::new();
                    if let Ok(d) = stroke_json(&mut one, record, binding, width, height) {
                        distance += d;
                        if count > 0 {
                            strokes.push(',');
                        }
                        strokes.push_str(&one);
                        count += 1;
                        inkpen2 |= binding.pen_name == INKPEN2;
                        marker4 |= binding.pen_name == MARKER4;
                        fallback.push_str(polyline.as_deref().unwrap_or(""));
                        continue;
                    }
                }
                note(&mut notices, objects::NOTICE_INK);
                // Eraser and replay-only records paint nothing of their own.
                let p = props(record);
                if prop(&p, "eraser") || prop(&p, "replay_only") {
                    continue;
                }
                match polyline {
                    Some(svg) => items.push(Item::Svg(svg)),
                    None => note(&mut notices, objects::NOTICE_HIDDEN),
                }
            }
            Some("textBox") => {
                if let Some(frame) = objects::shape_family(o, &prepared.files) {
                    items.extend(frame.items);
                    for n in frame.notices {
                        if n != objects::NOTICE_SHAPES {
                            note(&mut notices, n);
                        }
                    }
                }
                match text_box_input(o, count) {
                    Ok(None) => {}
                    Ok(Some((input, approximate))) => {
                        if approximate {
                            note(&mut notices, objects::NOTICE_TEXT);
                        }
                        boxes.push(input);
                    }
                    Err(_) => note(&mut notices, objects::NOTICE_HIDDEN),
                }
            }
            Some("shape") | Some("line") | Some("image") => {
                if let Some(c) = objects::shape_family(o, &prepared.files) {
                    items.extend(c.items);
                    for n in c.notices {
                        note(&mut notices, n);
                    }
                }
                // Only this client's exact shape profile is drawn as written;
                // any other shape/line is our approximation of Samsung's look.
                if o.get("type") != Some("image") && !rs_ops::page_shapes::renderer_skippable_shape(o) {
                    note(&mut notices, objects::NOTICE_SHAPES);
                }
            }
            _ => {
                let c = objects::unknown_object(o);
                items.extend(c.items);
                for n in c.notices {
                    note(&mut notices, n);
                }
            }
        }
    }
    let dark = ((color >> 16) & 255) + ((color >> 8) & 255) + (color & 255) < 384;
    let notice = notices.first().map(|n| n.to_string());
    if !build {
        return Ok(Some(PageInput { index, width, height, dark_mode: dark, body: Vec::new(), composite: true, notice, over_pdf }));
    }
    let renderer = |with_boxes: bool| -> P<Option<String>> {
        if count == 0 && flow.is_none() && (boxes.is_empty() || !with_boxes) {
            return Ok(None);
        }
        let mut text = format!("{{\"schemaVersion\":1,\"width\":{width},\"height\":{height},\"backgroundArgb\":{},\"strokes\":[{strokes}]",
                               color | 0xff00_0000);
        if let Some(flow) = flow {
            let mut copy = flow.clone();
            copy.push(("pageIndex".into(), J::Int(index as i128)));
            text.push_str(",\"flow\":");
            J::Obj(copy).dump(&mut text, true).map_err(PrepErr)?;
        }
        if with_boxes && !boxes.is_empty() {
            text.push_str(",\"textBoxes\":");
            J::List(boxes.clone()).dump(&mut text, true).map_err(PrepErr)?;
        }
        renderer_tail(&mut text, inkpen2, marker4);
        text.push('}');
        Ok(Some(text))
    };
    let full = renderer(true)?;
    let plain = if boxes.is_empty() { None } else { renderer(false)? };
    let make = |fallback: String| J::obj(vec![
        ("composite", J::Int(1)),
        ("width", J::Int(width as i128)),
        ("height", J::Int(height as i128)),
        ("backgroundArgb", J::Int(color as i128)),
        ("notices", J::List(notices.iter().map(|n| J::Str(n.to_string())).collect())),
        ("items", J::List(items.iter().map(Item::json).collect())),
        ("renderer", full.clone().map_or(J::Null, J::Str)),
        ("rendererPlain", plain.clone().map_or(J::Null, J::Str)),
        ("fallback", J::Str(fallback)),
    ]).dumps(true).map_err(PrepErr);
    let mut body = make(fallback)?;
    if body.len() > MAX_INPUT {
        // Drop the approximated-ink fallback first; then give up.
        body = make(String::new())?;
        if body.len() > MAX_INPUT {
            return fail("Çizim verisi boyut sınırını aşıyor.");
        }
    }
    Ok(Some(PageInput { index, width, height, dark_mode: dark, body: body.into_bytes(), composite: true, notice, over_pdf }))
}

/// `_prepare_pages(snapshot, only_index=None, build=True)` for a stored
/// snapshot dict (`DocumentSnapshot.from_dict` first). Memoized like the
/// Python `_CACHE` (two results, 16 MiB of bodies).
pub fn prepare_pages(snapshot: &notes_host::Val, only_index: Option<i64>, build: bool) -> P<Arc<Vec<PageInput>>> {
    let snapshot = notes_host::snapshot::from_dict(snapshot).map_err(|e| PrepErr(e.message))?;
    let source: SourceKey = (sha256(&snapshot.raw_body), sha256(&snapshot.resource_xml), snapshot.content_type.clone(),
                             snapshot.local_id.clone(), snapshot.commit_id.clone());
    let key: PageKey = (source.clone(), only_index, build);
    {
        let mut guard = PAGES.lock().unwrap();
        if let Some(at) = guard.iter().position(|(k, _, _)| *k == key) {
            let entry = guard.remove(at).unwrap();
            let result = entry.1.clone();
            guard.push_back(entry);
            return Ok(result);
        }
    }
    let prepared = prepare_source(&snapshot, &source)?;
    let pages = Arc::new(page_inputs(&prepared, only_index, build)?);
    let size: usize = pages.iter().map(|p| p.body.len()).sum();
    let mut guard = PAGES.lock().unwrap();
    guard.push_back((key, pages.clone(), size));
    while guard.len() > 2 || guard.iter().map(|e| e.2).sum::<usize>() > 16 * 1024 * 1024 {
        guard.pop_front();
    }
    Ok(pages)
}

/// The page as a composite body even when the renderer alone would take it:
/// the stand-in drawn when the renderer fails on a renderer-only page
/// (sample range, output size, text layout). Empty when not drawable.
pub fn forced_composite(snapshot: &notes_host::Val, index: i64) -> Vec<u8> {
    (|| -> P<Vec<u8>> {
        let snapshot = notes_host::snapshot::from_dict(snapshot).map_err(|e| PrepErr(e.message))?;
        let source: SourceKey = (sha256(&snapshot.raw_body), sha256(&snapshot.resource_xml), snapshot.content_type.clone(),
                                 snapshot.local_id.clone(), snapshot.commit_id.clone());
        let prepared = prepare_source(&snapshot, &source)?;
        let Some(page) = usize::try_from(index).ok().and_then(|i| prepared.pages.children.get(i)) else { return fail("page") };
        let (width, height) = (integer(page.get("width"))?, integer(page.get("height"))?);
        let flow = Some(index) != prepared.trailing;
        let built = composite_page(&prepared, page, index, width, height, page_color(page).ok(), true, flow)?;
        Ok(built.map(|p| p.body).unwrap_or_default())
    })().unwrap_or_default()
}

/// The inventory JSON the preparation child writes (Python `json.dumps`
/// default separators): `[{"index": 0, "width": W, "height": H, "darkMode": false}]`.
/// Failures give `[]` like the Python child.
pub fn inventory_json(snapshot: &notes_host::Val) -> String {
    let pages = prepare_pages(snapshot, None, false).map(|p| p.to_vec()).unwrap_or_default();
    let items: Vec<String> = pages.iter().map(|p| {
        let base = format!("{{\"index\": {}, \"width\": {}, \"height\": {}, \"darkMode\": {}",
            p.index, p.width, p.height, if p.dark_mode { "true" } else { "false" });
        if !p.composite {
            return base + "}";
        }
        let mut notice = String::new();
        match &p.notice {
            Some(n) => json_str(&mut notice, n),
            None => notice.push_str("null"),
        }
        format!("{base}, \"kind\": \"{}\", \"notice\": {notice}}}", if p.over_pdf { "pdf-ink" } else { "page" })
    }).collect();
    format!("[{}]", items.join(", "))
}

/// Body of one admitted page (empty when it could not be prepared).
pub fn page_body(snapshot: &notes_host::Val, index: i64) -> Vec<u8> {
    match prepare_pages(snapshot, Some(index), true) {
        Ok(pages) => pages.iter().find(|p| p.index == index).map(|p| p.body.clone()).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Saved pen of every stroke the page renderer admits as FountainPen/InkPen2,
/// exactly as the renderer reads it (page units, `stroke_json`'s `rendering`):
/// `{"<strokeId>":{"width":W,"height":H,"rendering":{…}},…}`. The live pen
/// preview (crates/notes-ink-live) copies it for a stroke drawn from that
/// template. Pen settings only: no sample geometry.
pub fn pen_templates(snapshot: &notes_host::Val) -> P<String> {
    let snapshot = notes_host::snapshot::from_dict(snapshot).map_err(|e| PrepErr(e.message))?;
    let source: SourceKey = (sha256(&snapshot.raw_body), sha256(&snapshot.resource_xml), snapshot.content_type.clone(),
                             snapshot.local_id.clone(), snapshot.commit_id.clone());
    let prepared = prepare_source(&snapshot, &source)?;
    let mut items = Vec::new();
    for page in &prepared.pages.children {
        let width = integer(page.get("width"))?;
        let height = integer(page.get("height"))?;
        for object in iter_tag(page, "object") {
            let Some(id) = object.get("id") else { continue };
            let Some(Some(Rec { record, binding, admitted: true })) = prepared.records.get(id) else { continue };
            if binding.pen_name != FOUNTAIN && binding.pen_name != INKPEN2 {
                continue;
            }
            let mut text = String::new();
            if stroke_json(&mut text, record, binding, width, height).is_err() {
                continue;
            }
            let (Some(rendering), Some(end)) = (text.strip_prefix("{\"rendering\":"), text.find(",\"bbox\":")) else { continue };
            let rendering = &rendering[..end - "{\"rendering\":".len()];
            let mut key = String::new();
            json_str(&mut key, id);
            items.push(format!("{key}:{{\"width\":{width},\"height\":{height},\"rendering\":{rendering}}}"));
        }
    }
    Ok(format!("{{{}}}", items.join(",")))
}
