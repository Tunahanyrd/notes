//! Replacement for the Python module `ink_first_stroke`: the first stroke on
//! a page without strokes, encoded from scratch (FountainPen, no stylus axes).
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
use crate::ink_projection::{verify, Verified};
use crate::page_style::advance_as;
use crate::util::*;
use crate::xml::{self, Element};
use base64::engine::{general_purpose::GeneralPurpose, general_purpose::GeneralPurposeConfig, DecodePaddingMode};
use base64::Engine as _;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyTuple};
use std::any::Any;
use std::collections::BTreeMap;

pub const FIRST_STROKE_RECIPE: i64 = 4;
pub const PEN_NAME: &str = "com.samsung.android.sdk.pen.pen.preload.FountainPen";
pub const ADVANCED_SETTING: &str = "18;0;100;";
pub const WRAPPER_V0: [u8; 12] = [0xff, 0xff, 0xff, 0xff, 4, 0, 0, 0, 0, 0, 0, 0];
pub const BASE_PROPERTIES: u16 = 0x01bf;
pub const BASE_FIELDS: u32 = 0x6000;
pub const STROKE_PROPERTIES: u16 = 0x0421;
pub const STROKE_FIELDS: u32 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 7) | (1 << 8) | (1 << 10) | (1 << 13);
pub const TOOL_TYPE: i64 = 3;
pub const PARTICLE_DENSITY: i64 = 1;
pub const TOLERANCE_RATIO: f64 = 0.25596845;
pub const MIN_SIZE: f64 = 0.5;
pub const MAX_SIZE: f64 = 128.0;
const ALLOWED_PAGE_CHILDREN: [&str; 4] = ["layerList", "drawnRect", "canvasCacheDataMap", "pdfDataList"];

const B64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true).with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
);

pub(crate) fn frame(kind: i16, properties: u16, fields: u32, fixed: &[u8], flexible: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(((18 + fixed.len() + flexible.len()) as u32).to_le_bytes());
    out.extend(kind.to_le_bytes());
    out.extend(((18 + fixed.len()) as u32).to_le_bytes());
    out.push(2);
    out.extend(properties.to_le_bytes());
    out.push(4);
    out.extend(fields.to_le_bytes());
    out.extend(fixed);
    out.extend(flexible);
    out
}

pub type Style = Vec<(String, Num)>;

/// Style dict exactly as read_stroke_style returns it (ids filled later).
pub fn first_stroke_style(color_argb: Option<i128>, size: Option<f64>) -> R<Style> {
    let color = match color_argb {
        Some(c) if (0..=0xffff_ffff).contains(&c) && c >> 24 == 0xff => c as i64,
        _ => return ink("opaque ARGB colour required"),
    };
    let size = match size {
        Some(s) if s.is_finite() && (MIN_SIZE..=MAX_SIZE).contains(&s) => f32r(s)?,
        _ => return ink("pen size out of range"),
    };
    Ok(vec![
        ("colorARGB".into(), Num::I(color)),
        ("penSize".into(), Num::F(size)),
        ("fixedWidth".into(), Num::F(size)),
        ("particleDensity".into(), Num::I(PARTICLE_DENSITY)),
        ("initialTolerance".into(), Num::F(f32r(size * TOLERANCE_RATIO)?)),
    ])
}

pub(crate) fn style_get(style: &Style, key: &str) -> R<Num> {
    style.iter().find(|(k, _)| k == key).map(|(_, v)| *v).ok_or_else(|| Error::new(Kind::Key, format!("'{key}'")))
}

pub(crate) fn style_map(style: &Style) -> BTreeMap<String, Num> {
    style.iter().cloned().collect()
}

/// Encode one FountainPen stroke resource from scratch (no template).
#[allow(clippy::too_many_arguments)]
pub fn encode_first_stroke(
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
    let profile = EncodeProfile { stroke_properties: STROKE_PROPERTIES, label: "first stroke", check_properties: false };
    encode_stroke(deps, &profile, stroke_id, tick, append_time, bounds, owner, capture, style, pen_name_id, advanced_id)
}

/// What differs between the from-scratch encoders (first stroke, highlighter).
pub(crate) struct EncodeProfile {
    pub stroke_properties: u16,
    /// Message prefix of the reconstruction checks.
    pub label: &'static str,
    /// Re-check the decoded stroke property word (highlighter).
    pub check_properties: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_stroke(
    deps: &dyn Deps,
    profile: &EncodeProfile,
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
    let stroke_id = match stroke_id {
        Some(s) if canonical_uuid(s) => s,
        Some(s) if s.len() == 36 && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') => {
            return ink("canonical stroke UUID required")
        }
        _ => return value("badly formed hexadecimal UUID string"),
    };
    let (tick, append_time) = match (tick, append_time) {
        (Some(t), Some(a)) if 0 < a && a < t && t < I63 => (t as i64, a as i64),
        _ => return ink("stroke times invalid"),
    };
    let capture = match capture {
        Some(c) if !c.stylus => c,
        _ => return ink("no-stylus capture required"),
    };
    let n = capture.samples.points.len();
    if deps.decode_point_channels(&capture.channels, n, true, false)? != capture.samples {
        return ink("capture channels differ from normalized samples");
    }
    let (width, height) = match owner {
        (Some(w), Some(h)) if (1..=32768).contains(&w) && (1..=32768).contains(&h) => (w as i32, h as i32),
        _ => return ink("invalid owner extent"),
    };
    if !(0..1i128 << 31).contains(&pen_name_id) || !(0..1i128 << 31).contains(&advanced_id) {
        return ink("invalid string id");
    }
    let mut fixed = Vec::new();
    fixed.extend(4000u32.to_le_bytes());
    fixed.extend(36u16.to_le_bytes());
    fixed.extend(stroke_id.as_bytes());
    fixed.extend(tick.to_le_bytes());
    for b in bounds {
        fixed.extend(b.to_le_bytes());
    }
    fixed.extend(0i32.to_le_bytes());
    fixed.push(0);
    let mut flexible = Vec::new();
    flexible.extend(append_time.to_le_bytes());
    flexible.extend(width.to_le_bytes());
    flexible.extend(height.to_le_bytes());
    let base = frame(0, BASE_PROPERTIES, BASE_FIELDS, &fixed, &flexible);
    let f = |key: &str| -> R<[u8; 4]> { Ok((style_get(style, key)?.as_f64() as f32).to_le_bytes()) };
    let i = |key: &str| -> R<[u8; 4]> {
        Ok(match style_get(style, key)? {
            Num::I(v) => (v as i32).to_le_bytes(),
            Num::F(v) => return type_err(&format!("required argument is not an integer ({v})")),
        })
    };
    let mut style_bytes = Vec::new();
    style_bytes.extend((advanced_id as i32).to_le_bytes());
    style_bytes.extend(match style_get(style, "colorARGB")? {
        Num::I(v) => (v as u32).to_le_bytes(),
        Num::F(_) => return type_err("required argument is not an integer"),
    });
    style_bytes.extend(f("penSize")?);
    style_bytes.extend((pen_name_id as i32).to_le_bytes());
    style_bytes.extend(f("fixedWidth")?);
    style_bytes.extend(i("particleDensity")?);
    style_bytes.extend(f("initialTolerance")?);
    let mut channels = Vec::new();
    channels.extend((n as u16).to_le_bytes());
    channels.extend(&capture.channels);
    channels.extend((TOOL_TYPE as u16).to_le_bytes());
    let mut raw = WRAPPER_V0.to_vec();
    raw.extend(base);
    raw.extend(frame(1, profile.stroke_properties, STROKE_FIELDS, &channels, &style_bytes));
    if raw.len() > MAX_STROKE_BYTES + 12 {
        return ink("stroke resource bound");
    }
    let record = deps.decode_stroke(&raw)?;
    let mut expected = style_map(style);
    expected.insert("advancedSettingId".into(), Num::I(advanced_id as i64));
    expected.insert("penNameId".into(), Num::I(pen_name_id as i64));
    if record.samples != capture.samples || record.stylus || !record.compressed || record.tool_type != TOOL_TYPE {
        return ink(&format!("{} channel reconstruction mismatch", profile.label));
    }
    if profile.check_properties && record.frames[1].properties != profile.stroke_properties as u64 {
        return ink(&format!("{} property reconstruction mismatch", profile.label));
    }
    if style_map(&deps.read_style(&record)?) != expected {
        return ink(&format!("{} style reconstruction mismatch", profile.label));
    }
    if deps.owner_extent(&record)? != (width as i64, height as i64) {
        return ink("owner extent reconstruction mismatch");
    }
    Ok((raw, record))
}

/// (stringIdList index in attrs or None, {id: text}) after validation.
pub(crate) fn string_ids(attrs: &Element) -> R<(Option<usize>, Vec<(i64, String)>)> {
    let idx = attrs.find_index("stringIdList");
    if attrs.findall("stringIdList").len() > 1 {
        return ink("duplicate brush string table");
    }
    let mut strings: Vec<(i64, String)> = Vec::new();
    let table = match idx {
        None => return Ok((None, strings)),
        Some(i) => &attrs.children[i],
    };
    if table.len() > 4096 {
        return ink("oversized brush string table");
    }
    let wanted: std::collections::BTreeSet<&str> = ["id", "text"].into_iter().collect();
    for child in &table.children {
        let parsed = (|| -> Option<(i64, String)> {
            if child.tag != "stringId" || !child.is_empty() || child.attr_key_set() != wanted {
                return None;
            }
            let id = child.get("id")?;
            let key: i64 = id.parse().ok()?;
            if !(0..1i64 << 31).contains(&key) || strings.iter().any(|(k, _)| *k == key) || key.to_string() != id {
                return None;
            }
            let raw = B64.decode(child.get("text")?).ok()?;
            Some((key, String::from_utf8(raw).ok()?))
        })();
        match parsed {
            Some(entry) => strings.push(entry),
            None => return ink("invalid brush string table"),
        }
    }
    Ok((idx, strings))
}

/// Reuse or append the pen-name and advanced-setting entries (`texts`).
pub(crate) fn ensure_strings(attrs: &mut Element, texts: [&str; 2]) -> R<(usize, i64, i64)> {
    let (idx, mut strings) = string_ids(attrs)?;
    let idx = match idx {
        Some(i) => i,
        None => {
            let at = attrs.children.iter().position(|c| c.tag == "title").map(|i| i + 1).unwrap_or(0);
            attrs.children.insert(at, Element::new("stringIdList", &[]));
            at
        }
    };
    let mut ids = Vec::new();
    for text in texts {
        let matches: Vec<i64> = strings.iter().filter(|(_, v)| v == text).map(|(k, _)| *k).collect();
        if matches.len() > 1 {
            return ink("ambiguous brush string entry");
        }
        if let Some(k) = matches.first() {
            ids.push(*k);
            continue;
        }
        let table = &mut attrs.children[idx];
        let last = table.children.last().map(|c| c.get("id").unwrap_or("").parse::<i64>()).transpose().map_err(|_| {
            Error::new(Kind::Value, "invalid literal for int() with base 10")
        })?;
        let mut key = last.map(|l| l + 1).unwrap_or(0);
        if strings.iter().any(|(k, _)| *k == key) || key >= 1 << 31 {
            key = strings.iter().map(|(k, _)| *k).max().unwrap() + 1;
        }
        if key >= 1 << 31 {
            return ink("string table id overflow");
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        table.sub_element("stringId", &[("id", &key.to_string()), ("text", &encoded)]);
        strings.push((key, text.to_string()));
        ids.push(key);
    }
    Ok((idx, ids[0], ids[1]))
}

pub struct FirstStroke {
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub requested_ids: Vec<String>,
    pub page_id: String,
    pub layer_id: String,
    pub stroke_id: String,
    pub tick: i64,
    pub part: BinaryPart,
    pub content_file_ids: Vec<String>,
    pub snapshot: Snapshot,
}

pub(crate) fn optional(parent: &Element, name: &str) -> R<Option<usize>> {
    let found: Vec<usize> = parent.children.iter().enumerate().filter(|(_, n)| n.tag == name).map(|(i, _)| i).collect();
    if found.len() > 1 {
        return ink("duplicate document section");
    }
    Ok(found.first().copied())
}

/// `samples`: the caller's StrokeSamples object, or None when it is not one.
#[allow(clippy::too_many_arguments)]
pub fn prepare_first_stroke(
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
    let style = first_stroke_style(color_argb, size)?;
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
        let objects: Vec<&Element> = pages.iter_tag("object").collect();
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
    let (width, height, layer_id, tpl, tp) = {
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
        if !layer.is_empty() && !layer.children[0].is_empty() {
            return ink("page already has strokes; use the append recipe");
        }
        if page.find("drawnRect").is_some() {
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
        if tree_layer.children.iter().any(|n| n.tag != "objectList" || !n.is_empty()) {
            return ink("stroke tree differs from objects");
        }
        let layer_raw = parent_hash(layer.get("id"), integer(layer.get("modifiedTime"))?, &[])?;
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
        (width, height, layer.get("id").unwrap_or("").to_string(), tpl, tp)
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
        // Validates the whole cache/PDF file inventory; a PDF entry stays
        // byte-identical and only the target page cache is released.
        changed_files = deps.release_page_cache(&mut root, &page_id, tick)?;
    }
    let capture = deps.normalize_capture(samples)?;
    let bounds = deps.drawn_bounds(&capture.samples, PEN_NAME, style_get(&style, "penSize")?.as_f64())?;
    let (w, h) = (width as f64, height as f64);
    if !(0.0 <= bounds[0] && bounds[0] <= bounds[2] && bounds[2] <= w) || !(0.0 <= bounds[1] && bounds[1] <= bounds[3] && bounds[3] <= h) {
        return ink("captured drawn bounds leave the page");
    }
    let (table_i, pen_id, advanced_id) = ensure_strings(&mut root.children[ai], [PEN_NAME, ADVANCED_SETTING])?;
    let (raw, record) = encode_first_stroke(
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
    if binding.pen_name != PEN_NAME || binding.advanced_settings.as_deref() != Some(ADVANCED_SETTING) {
        return ink("brush binding mismatch");
    }
    let layer_raw = parent_hash(Some(&layer_id), tick, &[digest_raw])?;
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
        let rect: Vec<String> = bounds.iter().map(|v| py_float_repr(*v)).collect();
        page.children.insert(
            0,
            Element::new("drawnRect", &[("l", &rect[0]), ("t", &rect[1]), ("r", &rect[2]), ("b", &rect[3])]),
        );
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
    one_mut(&mut upload, "pagelist")?.children.retain(|p| p.get("id") == Some(page_id.as_str()));
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

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
pub(crate) fn style_to_py<'py>(py: Python<'py>, style: &Style) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in style {
        match v {
            Num::I(i) => d.set_item(k, i)?,
            Num::F(f) => d.set_item(k, f)?,
        }
    }
    Ok(d)
}

#[cfg(feature = "python")]
pub(crate) fn style_from_py(obj: &Bound<'_, PyAny>) -> PyResult<Style> {
    let mut out = Vec::new();
    for (k, v) in obj.cast::<PyDict>()?.iter() {
        let num = if v.is_instance_of::<pyo3::types::PyFloat>() { Num::F(v.extract()?) } else { Num::I(v.extract()?) };
        out.push((k.extract()?, num));
    }
    Ok(out)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "first_stroke_style")]
fn py_first_stroke_style<'py>(color_argb: &Bound<'py, PyAny>, size: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = color_argb.py();
    let style = first_stroke_style(exact_int(color_argb), int_or_float(size)).py(py)?;
    style_to_py(py, &style)
}

#[pyfunction]
#[pyo3(name = "encode_first_stroke",
       signature = (*, stroke_id, tick, append_time, bounds, owner, capture, style, pen_name_id, advanced_id))]
#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn py_encode_first_stroke<'py>(
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
    let (raw, record) = encode_first_stroke(
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
#[pyo3(name = "prepare_first_stroke",
       signature = (snapshot, page_id, samples, *, color_argb, size, now_ms, new_stroke_id=None))]
#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn py_prepare_first_stroke<'py>(
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
    let e = prepare_first_stroke(
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
            FIRST_STROKE_RECIPE.into_pyobject(py)?.into_any(),
        ],
    )?;
    bridge::ink_append_edit(py, args)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_first_stroke_style, m)?)?;
    m.add_function(wrap_pyfunction!(py_encode_first_stroke, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_first_stroke, m)?)?;
    rs_common::export(m, &["first_stroke_style", "encode_first_stroke", "prepare_first_stroke"])
}
