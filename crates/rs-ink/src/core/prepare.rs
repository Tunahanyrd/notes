//! ink_editor.bind_stroke_metadata and _prepare_ink_document (offline append).
use super::binary::{
    decode_point_channels, decode_stroke_resource, f64_at, mask64, put, read_stroke_style, Frame, RawSamples, Record,
    Samples, Style, StyleVal, MAX_STROKE_BYTES,
};
use super::capture::{basic_pen_drawn_bounds, NormalizedCapture};
use super::coords::{override_style_bytes, page_coordinate_scale, rebase_style_bytes, stroke_owner_extent};
use super::num::{f32r, py_float, py_max, py_min, py_repr, Val};
use super::xml::{NodeId, Tree};
use super::{
    b64decode_strict, doc_integer, fromhex, hex, ink, is_canonical_uuid, lower, object_hash, parent_hash, py_int,
    sha256, InkErr, R,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// ink_editor.MAX_DOCUMENT_PAGES: page bound of recipe>=2 / erase documents.
pub const MAX_DOCUMENT_PAGES: usize = 2048;

pub const MAX_READONLY_PARTS: usize = 16384;
pub const FOUNTAIN: &str = "com.samsung.android.sdk.pen.pen.preload.FountainPen";
pub const INKPEN2: &str = "com.samsung.android.sdk.pen.pen.preload.InkPen2";

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub object_id: String,
    pub modified_time: i64,
    pub bounds: [f64; 4],
    pub pen_name: String,
    pub advanced_settings: Option<String>,
    pub style: Style,
}

/// ink_editor.bind_stroke_metadata. `table` is attributes/stringIdList (or None).
pub fn bind_stroke_metadata(frames: &[Frame], obj: (&Tree, NodeId), table: Option<(&Tree, NodeId)>) -> R<Binding> {
    let frame = &frames[0];
    let fixed = super::binary::py_slice(&frame.raw, frame.fixed_start, Some(frame.flexible_start));
    if fixed.len() < 6 {
        return ink("truncated base frame");
    }
    let version = u32::from_le_bytes(fixed[..4].try_into().unwrap());
    let n = u16::from_le_bytes([fixed[4], fixed[5]]) as usize;
    if version != 4000 || n != 36 || fixed.len() != 6 + n + 45 {
        return ink("unsupported base frame profile");
    }
    let id_bytes = &fixed[6..6 + n];
    let identity = match std::str::from_utf8(id_bytes) {
        Ok(s) if s.is_ascii() && is_canonical_uuid(s) => s.to_string(),
        _ => return ink("invalid stroke identity"),
    };
    let time_value = i64::from_le_bytes(fixed[6 + n..14 + n].try_into().unwrap());
    let mut bounds = [0f64; 4];
    for (i, b) in bounds.iter_mut().enumerate() {
        *b = f64_at(fixed, (14 + n + 8 * i) as i64)?;
    }
    if time_value <= 0 || bounds.iter().any(|x| !x.is_finite()) || bounds[0] > bounds[2] || bounds[1] > bounds[3] {
        return ink("invalid stroke time or bounds");
    }
    let digest = super::sha256_hex(format!("{identity}{time_value}").as_bytes());
    let (ot, on) = obj;
    let tv = time_value.to_string();
    if ot.tag(on) != "object" || ot.get(on, "type") != Some("stroke") || ot.get(on, "id") != Some(identity.as_str())
        || ot.get(on, "modifiedTime") != Some(tv.as_str()) || ot.get(on, "hash") != Some(digest.as_str())
    {
        return ink("Cloud XML and native stroke identity/time/hash differ");
    }
    let (tt, tn) = match table {
        Some((tt, tn)) if tt.tag(tn) == "stringIdList" && tt.len(tn) <= 4096 => (tt, tn),
        _ => return ink("missing or oversized brush string table"),
    };
    let mut strings: HashMap<i128, String> = HashMap::new();
    for child in tt.children(tn) {
        let fail = || InkErr::Ink("invalid brush string table".into());
        let node = &tt.nodes[*child];
        let keys: HashSet<&str> = node.attrs.iter().map(|(k, _)| k.as_str()).collect();
        if node.tag != "stringId" || !node.children.is_empty() || keys != HashSet::from(["id", "text"]) {
            return Err(fail());
        }
        let key = py_int(tt.get(*child, "id").unwrap()).ok_or_else(fail)?;
        if !(0..(1i128 << 31)).contains(&key) || strings.contains_key(&key) {
            return Err(fail());
        }
        let encoded = tt.get(*child, "text").unwrap();
        if encoded.chars().count() > 65536 {
            return Err(fail());
        }
        let raw = b64decode_strict(encoded).ok_or_else(fail)?;
        strings.insert(key, String::from_utf8(raw).map_err(|_| fail())?);
    }
    let style = read_stroke_style(&frames[1])?;
    let mut key = style.int("penNameId").unwrap_or(-1);
    if key == -1 {
        key = style.int("legacyPenNameId").unwrap_or(-1);
    }
    let pen = strings.get(&(key as i128));
    let pen = match pen {
        Some(p) if !p.is_empty() && p.starts_with("com.samsung.android.sdk.pen.pen.preload.") => p.clone(),
        _ => return ink("unresolved or unsupported brush name"),
    };
    let advanced_key = style.int("advancedSettingId").unwrap_or(-1);
    if advanced_key != -1 && !strings.contains_key(&(advanced_key as i128)) {
        return ink("unresolved advanced brush settings");
    }
    Ok(Binding { object_id: identity, modified_time: time_value, bounds, pen_name: pen,
                 advanced_settings: strings.get(&(advanced_key as i128)).cloned(), style })
}

// ---- prepare --------------------------------------------------------------

/// The fields of a payload part used by the append preparation.
#[derive(Clone, Debug)]
pub struct PartIn {
    pub name: Option<String>,
    pub filename: Option<String>,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// tools.notes_payload.parse_payload(raw, ct, max_parts=MAX_READONLY_PARTS) parts via rs-doc.
pub fn payload_parts(raw: &[u8], content_type: &str, limits: &rs_doc::mime::Limits) -> R<Vec<PartIn>> {
    let parsed = rs_doc::payload::parse_payload(raw, content_type, Some(limits.max_readonly_parts), limits,
                                                &mut rs_doc::mime::NoHooks)?;
    Ok(parsed.parts.into_iter().map(|p| PartIn {
        name: p.name, filename: p.filename, content_type: p.content_type, body: raw[p.body].to_vec(),
    }).collect())
}

pub struct SnapshotIn<'a> {
    pub resource_xml: &'a [u8],
    pub raw_body: &'a [u8],
    pub content_type: &'a str,
}

#[derive(Clone, Debug)]
pub struct InkAppendEdit {
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub requested_ids: Vec<String>,
    pub page_id: String,
    pub layer_id: String,
    pub stroke_id: String,
    pub stroke_modified_time: i64,
    /// (name, filename, body); content type is application/octet-stream
    pub binary_parts: Vec<(String, String, Vec<u8>)>,
    pub content_file_ids: Vec<String>,
    pub recipe_version: u8,
}

pub enum Geometry<'a> {
    Translate(Val, Val),
    Capture(&'a NormalizedCapture),
}

pub struct StyleOverride {
    pub color_argb: Val,
    pub pen_size: Val,
}

struct Inventory {
    part_names: Vec<(Option<String>, String)>,
    stroke_count: usize,
    records: HashMap<String, (Record, Binding)>,
}

type MemoKey = ([u8; 32], [u8; 32], String);
static MEMO: Mutex<Vec<(MemoKey, Arc<Inventory>)>> = Mutex::new(Vec::new());

/// ink_editor._prepare_ink_document. `parse` supplies tools.notes_payload.parse_payload parts.
#[allow(clippy::too_many_arguments)]
pub fn prepare_ink_document(
    snapshot: &SnapshotIn,
    source_stroke_id: Option<&str>,
    new_stroke_id: Option<&str>,
    geometry: Geometry,
    now_ms: Val,
    recipe_version: u8,
    style: Option<&StyleOverride>,
    parse: &dyn Fn(&[u8], &str) -> R<Vec<PartIn>>,
) -> R<InkAppendEdit> {
    let capture = match &geometry {
        Geometry::Capture(c) => Some(*c),
        _ => None,
    };
    if !matches!(recipe_version, 1..=3) || (recipe_version >= 2 && capture.is_none()) || ((recipe_version == 3) != style.is_some()) {
        return ink("unsupported ink recipe");
    }
    for identity in [source_stroke_id, new_stroke_id] {
        if !identity.is_some_and(is_canonical_uuid) {
            return ink("canonical stroke UUID required");
        }
    }
    let (source_stroke_id, new_stroke_id) = (source_stroke_id.unwrap(), new_stroke_id.unwrap());
    if let Geometry::Translate(dx, dy) = &geometry {
        let bad = |v: &Val| -> R<bool> { Ok(!v.is_num() || !v.is_finite()? || v.abs_gt(4096.0)) };
        if bad(dx)? || bad(dy)? || (dx.is_zero() && dy.is_zero()) {
            return ink("bounded nonzero translation required");
        }
    }
    let mut t = Tree::parse(snapshot.resource_xml)?;
    let root = t.root;
    let all = t.iter(root);
    if all.iter().any(|n| {
        lower(t.get_or(*n, "isLocked", "false")) != "false" || lower(t.get_or(*n, "isEncrypted", "false")) != "false"
            || lower(t.get_or(*n, "encrypted", "false")) != "false"
    }) {
        return ink("locked or encrypted ink is unsupported");
    }
    if all.iter().any(|n| t.get(*n, "id") == Some(new_stroke_id)) {
        return ink("new stroke identity already exists");
    }
    let now = match now_ms {
        Val::Int(i) => {
            let sync = doc_integer(t.get(root, "syncModifiedTime"))? as i128;
            if !(sync < i && i < (1i128 << 63)) {
                return ink("sync timestamp must advance in wall-clock milliseconds");
            }
            i as i64
        }
        Val::Big(_) => {
            doc_integer(t.get(root, "syncModifiedTime"))?;
            return ink("sync timestamp must advance in wall-clock milliseconds");
        }
        _ => return ink("sync timestamp must advance in wall-clock milliseconds"),
    };
    let tree = t.one(root, "noteTree")?;
    let attrs = t.one(root, "attributes")?;
    let pages = t.one(root, "pageList")?;
    if t.get(attrs, "formatVersion") != Some("4000") || t.get(tree, "ver") != Some("4000")
        || lower(t.get_or(tree, "isStrokeGrouped", "false")) != "false"
    {
        return ink("unsupported ink document profile");
    }
    let forbidden: &[&str] = if recipe_version == 1 {
        &["canvasCacheDataMap", "strokeGroup", "voiceDataList", "attachedFileMap"]
    } else {
        &["strokeGroup", "voiceDataList", "attachedFileMap"]
    };
    // Recipe 1 keeps its original 32-page bound; later recipes carry every
    // other page byte-identically (PDF books).
    let page_bound = if recipe_version == 1 { 32 } else { MAX_DOCUMENT_PAGES };
    if t.len(pages) > page_bound || all.iter().any(|n| forbidden.contains(&t.tag(*n))) {
        return ink("cached/grouped/media ink requires a separate invalidation contract");
    }
    if (recipe_version == 1 && t.len(t.one(root, "contentFileList")?) > 0)
        || (recipe_version == 1 && t.len(t.one(root, "floatingObjectList")?) > 0)
    {
        return ink("attachments and floating objects are unsupported");
    }
    // Recipe 2+: free-floating pictures/shapes/text boxes are carried through
    // byte-identically like page-layer objects (core::objects); anything
    // else in the list keeps the note refused.
    let floating = if t.len(t.one(root, "floatingObjectList")?) > 0 {
        super::objects::tree_floating(&t, root).map_err(|_| InkErr::Ink("attachments and floating objects are unsupported".into()))?
    } else {
        Vec::new()
    };
    let sections = ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];
    if t.children(root).iter().any(|n| !sections.contains(&t.tag(*n))) {
        return ink("unknown resource section");
    }
    let memo_key: MemoKey = (sha256(snapshot.raw_body), sha256(snapshot.resource_xml), snapshot.content_type.to_string());
    let memo = MEMO.lock().unwrap().iter().find(|(k, _)| *k == memo_key).map(|(_, v)| v.clone());
    let fresh_parts = if memo.is_none() { Some(parse(snapshot.raw_body, snapshot.content_type)?) } else { None };
    let part_names: Vec<(Option<String>, String)> = match (&memo, &fresh_parts) {
        (Some(m), _) => m.part_names.clone(),
        (None, Some(p)) => p.iter().map(|p| (p.name.clone(), p.content_type.clone())).collect(),
        _ => unreachable!(),
    };
    let stroke_count = part_names.iter().filter(|p| p.0.as_deref() == Some("strokeResource")).count();
    if !(1..=MAX_READONLY_PARTS).contains(&stroke_count)
        || part_names.iter().any(|(n, ct)| n.as_deref() != Some("strokeResource") && ct != "text/xml" && ct != "application/xml")
    {
        return ink("unsupported ink part inventory");
    }
    // Top-level layer objects: strokes, plus (recipe 2+) self-contained images,
    // shapes, lines and text boxes the edit carries through byte-identically
    // (core::objects). Their content files are checked by release_page_cache.
    let mut objects: Vec<NodeId> = Vec::new();
    let mut passthrough_ticks: Vec<i64> = Vec::new();
    for layer_objects in t.iter(pages).into_iter().filter(|n| t.tag(*n) == "objectList") {
        for o in t.children(layer_objects).to_vec() {
            if t.get(o, "type") == Some("stroke") {
                objects.push(o);
            } else if recipe_version >= 2 && super::objects::tree_passthrough(&t, o).is_some() {
                passthrough_ticks.push(doc_integer(t.get(o, "modifiedTime"))?);
            } else {
                return ink("unsupported or ambiguous stroke inventory");
            }
        }
    }
    for o in &floating {
        passthrough_ticks.push(doc_integer(t.get(*o, "modifiedTime"))?);
    }
    let floating_count = floating.len();
    let wanted: HashSet<&str> = HashSet::from(["type", "id", "modifiedTime", "hash"]);
    let nested = t.iter(pages).into_iter().filter(|n| t.tag(*n) == "object").count();
    if objects.len() != stroke_count
        || nested + floating_count != objects.len() + passthrough_ticks.len()
        || objects.iter().any(|o| {
            let keys: HashSet<&str> = t.nodes[*o].attrs.iter().map(|(k, _)| k.as_str()).collect();
            t.get(*o, "type") != Some("stroke") || t.len(*o) != 0 || keys != wanted
        })
    {
        return ink("unsupported or ambiguous stroke inventory");
    }
    let mut by_id: HashMap<String, NodeId> = HashMap::new();
    for o in &objects {
        by_id.insert(t.get(*o, "id").unwrap().to_string(), *o);
    }
    if by_id.len() != objects.len() {
        return ink("duplicate stroke identity");
    }
    let inventory: Arc<Inventory> = match memo {
        Some(m) => m,
        None => {
            let parts = fresh_parts.unwrap();
            let string_table = t.find(attrs, "stringIdList");
            let mut records: HashMap<String, (Record, Binding)> = HashMap::new();
            for part in parts.iter().filter(|p| p.name.as_deref() == Some("strokeResource")) {
                let record = decode_stroke_resource(&part.body)?;
                let base = &record.frames[0];
                let fs = base.fixed_start as usize;
                let id_bytes = super::binary::py_slice(&base.raw, (fs + 6) as i64, Some((fs + 42) as i64));
                if let Some(pos) = id_bytes.iter().position(|b| !b.is_ascii()) {
                    return Err(InkErr::Unicode("ascii", id_bytes.to_vec(), pos, pos + 1, "ordinal not in range(128)"));
                }
                let identity = String::from_utf8(id_bytes.to_vec()).unwrap();
                if records.contains_key(&identity) || !by_id.contains_key(&identity) {
                    return ink("binary/XML inventory differs");
                }
                let binding = bind_stroke_metadata(&record.frames, (&t, by_id[&identity]), string_table.map(|s| (&t, s)))?;
                if part.filename.as_deref() != Some(format!("{}_{}", identity, binding.modified_time).as_str()) {
                    return ink("stroke MIME identity/time differs");
                }
                records.insert(identity, (record, binding));
            }
            let inv = Arc::new(Inventory {
                part_names: part_names.clone(),
                stroke_count,
                records,
            });
            let mut memo = MEMO.lock().unwrap();
            memo.retain(|(k, _)| *k != memo_key);
            memo.push((memo_key, inv.clone()));
            while memo.len() > 4 {
                memo.remove(0);
            }
            inv
        }
    };
    let _ = inventory.stroke_count;
    let records = &inventory.records;
    let (record, binding) = match records.get(source_stroke_id) {
        Some(r) => r,
        None => return ink("source stroke absent"),
    };
    let base = &record.frames[0];
    let stroke = &record.frames[1];
    if record.frames.len() != 2 || mask64(&base.fields) != (0x6000, false) {
        return ink("source common metadata needs unsupported geometry handling");
    }
    let (props, props_high) = mask64(&stroke.properties);
    if props_high || props & !(1u64 | 4 | 16 | 32 | 256 | 1024 | 2048) != 0 {
        return ink("eraser/replay/effect stroke clone is unsupported");
    }
    if binding.pen_name != FOUNTAIN && binding.pen_name != INKPEN2 {
        return ink("brush clone profile not verified");
    }
    if !record.compressed || record.samples.points.is_empty() {
        return ink("clone currently requires nonempty compressed stroke");
    }
    let mut candidates: Vec<(NodeId, NodeId)> = Vec::new();
    for page in t.children(pages).to_vec() {
        let layers = t.one(page, "layerList")?;
        for layer in t.children(layers).to_vec() {
            let objs = t.one(layer, "objectList")?;
            if t.children(objs).iter().any(|o| t.get(*o, "id") == Some(source_stroke_id)) {
                candidates.push((page, layer));
            }
        }
    }
    if candidates.len() != 1 {
        return ink("ambiguous source layer");
    }
    let (page, layer) = candidates[0];
    if recipe_version == 1 {
        let extent = stroke_owner_extent(base)?;
        let w = doc_integer(t.get(page, "width"))?;
        let h = doc_integer(t.get(page, "height"))?;
        if extent != (w, h) {
            return ink("stored owner extent differs; a versioned coordinate rewrite is required");
        }
    }
    if capture.is_some() && t.get_or(page, "orientation", "0") != "0" {
        return ink("capture page orientation transform is unsupported");
    }
    if lower(t.get_or(layer, "lockState", "false")) != "false" || t.get_or(layer, "thumbnail", "-1") != "-1" {
        return ink("locked or cached layer");
    }
    let page_id = t.get(page, "id").map(str::to_string);
    let layer_id = t.get(layer, "id").map(str::to_string);
    let tree_pages = t.one(tree, "pageList")?;
    let tree_page = match t.children(tree_pages).iter().copied().find(|p| t.get(*p, "id").map(str::to_string) == page_id) {
        Some(p) => p,
        None => return ink("page absent from tree"),
    };
    let tree_layers = t.one(tree_page, "layerList")?;
    let tree_layer = match t.children(tree_layers).iter().copied().find(|l| t.get(*l, "id").map(str::to_string) == layer_id) {
        Some(l) => l,
        None => return ink("layer absent from tree"),
    };
    let old_objects = t.one(layer, "objectList")?;
    let tree_objects = t.one(tree_layer, "objectList")?;
    let a: Vec<_> = t.children(old_objects).iter().map(|o| (t.get(*o, "id"), t.get(*o, "modifiedTime"), t.get(*o, "hash"))).collect();
    let b: Vec<_> = t.children(tree_objects).iter().map(|o| (t.get(*o, "id"), t.get(*o, "t"), t.get(*o, "h"))).collect();
    if a != b {
        return ink("stroke tree differs from objects");
    }
    let hashes = |t: &Tree, parent: NodeId| -> R<Vec<Vec<u8>>> {
        t.children(parent).iter().map(|o| fromhex(t.get(*o, "hash"))).collect()
    };
    let layer_ms = doc_integer(t.get(layer, "modifiedTime"))?;
    let old_layer_hash = hex(&parent_hash(layer_id.as_deref(), layer_ms as i128, &hashes(&t, old_objects)?)?);
    if Some(old_layer_hash.as_str()) != t.get(layer, "hash") || Some(old_layer_hash.as_str()) != t.get(tree_layer, "h") {
        return ink("layer hash mismatch");
    }
    let page_ms = doc_integer(t.get(page, "modifiedTime"))?;
    let page_layers = t.one(page, "layerList")?;
    let old_page_hash = hex(&parent_hash(page_id.as_deref(), page_ms as i128, &hashes(&t, page_layers)?)?);
    if Some(old_page_hash.as_str()) != t.get(page, "hash") || Some(old_page_hash.as_str()) != t.get(tree_page, "h") {
        return ink("page hash mismatch");
    }
    let mut tick: i128 = i128::MIN;
    if recipe_version >= 2 {
        let files = t.one(root, "contentFileList")?;
        for f in t.children(files) {
            tick = tick.max(doc_integer(t.get(*f, "modifiedTime"))? as i128);
        }
    }
    tick = tick.max(doc_integer(t.get(attrs, "modifiedTime"))? as i128);
    tick = tick.max(doc_integer(t.get(page, "modifiedTime"))? as i128);
    tick = tick.max(doc_integer(t.get(layer, "modifiedTime"))? as i128);
    for (_, b) in records.values() {
        tick = tick.max(b.modified_time as i128);
    }
    for t in &passthrough_ticks {
        tick = tick.max(*t as i128);
    }
    tick += 1;
    let mut changed_files: Vec<String> = Vec::new();
    if recipe_version >= 2 {
        changed_files = super::cache::release_page_cache(&mut t, Some(page), tick)?;
    }
    let revision = doc_integer(t.get(attrs, "fileRevision"))? as i128 + 1;
    if tick >= (1i128 << 63) || revision >= (1i128 << 31) {
        return ink("ink timestamp/revision overflow");
    }
    let tick = tick as i64;
    let mut native = record.raw.clone();
    let base_offset = 12 + base.fixed_start as usize;
    let mut expected_style = binding.style.clone();
    let mut style_bytes = super::binary::py_slice(&stroke.raw, stroke.flexible_start, None).to_vec();
    if recipe_version >= 2 {
        let width = doc_integer(t.get(page, "width"))?;
        let height = doc_integer(t.get(page, "height"))?;
        let (wv, hv) = (Val::Int(width as i128), Val::Int(height as i128));
        let scale = page_coordinate_scale(base, wv, hv, Val::Int(0))?;
        style_bytes = rebase_style_bytes(base, stroke, wv, hv)?;
        for key in ["penSize", "fixedWidth", "initialTolerance"] {
            if let Some(v) = expected_style.float(key) {
                expected_style.set(key, StyleVal::F(f32r(v * scale)?));
            }
        }
        let off = 12 + base.flexible_start as usize + 8;
        put(&mut native, off, &(width as i32).to_le_bytes())?;
        put(&mut native, off + 4, &(height as i32).to_le_bytes())?;
    }
    if recipe_version == 3 {
        let style = style.unwrap();
        let old_size = expected_style.float("penSize").ok_or_else(|| InkErr::KeyError("penSize".into()))?;
        style_bytes = override_style_bytes(stroke, &style_bytes, style.color_argb, style.pen_size)?;
        let size = style.pen_size.f32v()?;
        let factor = size / old_size;
        let color = if let Val::Int(c) = style.color_argb { c as i64 } else { unreachable!() };
        expected_style.set("colorARGB", StyleVal::I(color));
        expected_style.set("penSize", StyleVal::F(size));
        if let Some(fixed) = expected_style.float("fixedWidth") {
            let v = if fixed == old_size { size } else { f32r(fixed * factor)? };
            expected_style.set("fixedWidth", StyleVal::F(v));
        }
        if let Some(tol) = expected_style.float("initialTolerance") {
            expected_style.set("initialTolerance", StyleVal::F(f32r(tol * factor)?));
        }
    }
    native[base_offset + 6..base_offset + 42].copy_from_slice(new_stroke_id.as_bytes());
    put(&mut native, base_offset + 42, &tick.to_le_bytes())?;
    let bounds: [f64; 4] = match &geometry {
        Geometry::Translate(dx, dy) => {
            let (dx, dy) = (dx.to_f64()?, dy.to_f64()?);
            let b = binding.bounds;
            [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy]
        }
        Geometry::Capture(c) => {
            if c.stylus {
                return ink("first capture profile requires no stylus-axis channels");
            }
            if decode_point_channels(&c.channels, c.samples.points.len(), true, false)? != c.samples {
                return ink("capture channels differ from normalized samples");
            }
            let pts = RawSamples::from_samples(&c.samples).points;
            let size = match expected_style.get("penSize") {
                Some(StyleVal::F(f)) => Val::Float(*f),
                Some(StyleVal::I(i)) => Val::Int(*i as i128),
                _ => Val::Bad,
            };
            basic_pen_drawn_bounds(Some(&pts), Some(&binding.pen_name), size, Val::Float(0.0))?
        }
    };
    let width = doc_integer(t.get(page, "width"))? as f64;
    let height = doc_integer(t.get(page, "height"))? as f64;
    if !(0.0 <= bounds[0] && bounds[0] <= bounds[2] && bounds[2] <= width)
        || !(0.0 <= bounds[1] && bounds[1] <= bounds[3] && bounds[3] <= height)
    {
        return ink(if capture.is_none() { "translated drawn bounds leave the page" } else { "captured drawn bounds leave the page" });
    }
    for (i, v) in bounds.iter().enumerate() {
        put(&mut native, base_offset + 50 + 8 * i, &v.to_le_bytes())?;
    }
    let new_record: Record;
    match &geometry {
        Geometry::Translate(dx, dy) => {
            let (dx, dy) = (dx.to_f64()?, dy.to_f64()?);
            let point_offset = 12 + base.raw.len() + stroke.fixed_start as usize + 2;
            let sx = f64_at(&native, point_offset as i64)?;
            let sy = f64_at(&native, point_offset as i64 + 8)?;
            put(&mut native, point_offset, &(sx + dx).to_le_bytes())?;
            put(&mut native, point_offset + 8, &(sy + dy).to_le_bytes())?;
            new_record = decode_stroke_resource(&native)?;
            let expected: Vec<(f64, f64)> = record.samples.points.iter()
                .map(|(x, y)| Ok((f32r(x + dx)?, f32r(y + dy)?)))
                .collect::<R<_>>()?;
            let s = &new_record.samples;
            let o = &record.samples;
            if s.points != expected || s.pressures != o.pressures || s.timestamps != o.timestamps || s.tilts != o.tilts
                || s.orientations != o.orientations
            {
                return ink("translation changes native channel precision");
            }
        }
        Geometry::Capture(c) => {
            let fs = stroke.fixed_start as usize;
            let mut fixed = (c.samples.points.len() as u16).to_le_bytes().to_vec();
            fixed.extend_from_slice(&c.channels);
            fixed.extend(record.tool_type.to_le_bytes());
            let mut rebuilt = stroke.raw[..fs].to_vec();
            rebuilt.extend_from_slice(&fixed);
            rebuilt.extend_from_slice(&style_bytes);
            let n = rebuilt.len() as u32;
            put(&mut rebuilt, 0, &n.to_le_bytes())?;
            put(&mut rebuilt, 6, &((fs + fixed.len()) as u32).to_le_bytes())?;
            rebuilt[11] &= !4;
            native.truncate(12 + base.raw.len());
            native.extend_from_slice(&rebuilt);
            if native.len() > MAX_STROKE_BYTES + 12 {
                return ink("captured stroke resource bound");
            }
            new_record = decode_stroke_resource(&native)?;
            if new_record.samples != c.samples || new_record.stylus {
                return ink("captured native channel reconstruction mismatch");
            }
            if read_stroke_style(&new_record.frames[1])? != expected_style {
                return ink("captured stroke brush style changed");
            }
        }
    }
    let digest = hex(&object_hash(Some(new_stroke_id), tick as i128)?);
    let tick_s = tick.to_string();
    let new_object = t.add("object", vec![
        ("type".into(), "stroke".into()), ("id".into(), new_stroke_id.into()),
        ("hash".into(), digest.clone()), ("modifiedTime".into(), tick_s.clone()),
    ], None);
    let string_table = t.find(attrs, "stringIdList");
    bind_stroke_metadata(&new_record.frames, (&t, new_object), string_table.map(|s| (&t, s)))?;
    t.append(old_objects, new_object);
    t.add("object", vec![("id".into(), new_stroke_id.into()), ("t".into(), tick_s.clone()), ("h".into(), digest.clone())],
          Some(tree_objects));
    let layer_hash = hex(&parent_hash(layer_id.as_deref(), tick as i128, &hashes(&t, old_objects)?)?);
    t.set(layer, "modifiedTime", &tick_s);
    t.set(layer, "hash", &layer_hash);
    t.set(tree_layer, "t", &tick_s);
    t.set(tree_layer, "h", &layer_hash);
    let page_layers = t.one(page, "layerList")?;
    let page_hash = hex(&parent_hash(page_id.as_deref(), tick as i128, &hashes(&t, page_layers)?)?);
    t.set(page, "modifiedTime", &tick_s);
    t.set(page, "hash", &page_hash);
    t.set(tree_page, "t", &tick_s);
    t.set(tree_page, "h", &page_hash);
    let drawn = match t.find(page, "drawnRect") {
        Some(d) => d,
        None => return ink("source drawn rectangle missing"),
    };
    let mut original = [0f64; 4];
    for (i, k) in ["l", "t", "r", "b"].iter().enumerate() {
        original[i] = match t.get(drawn, k) {
            Some(s) => py_float(s)?,
            None => return Err(InkErr::TypeError("float() argument must be a string or a real number, not 'NoneType'".into())),
        };
    }
    if original.iter().any(|v| !v.is_finite()) {
        return ink("invalid page drawn rectangle");
    }
    let union = [py_min(original[0], bounds[0]), py_min(original[1], bounds[1]), py_max(original[2], bounds[2]),
                 py_max(original[3], bounds[3])];
    for (k, v) in ["l", "t", "r", "b"].iter().zip(union) {
        t.set(drawn, k, &py_repr(v));
    }
    t.set(attrs, "fileRevision", &revision.to_string());
    t.set(attrs, "modifiedTime", &tick_s);
    t.set(attrs, "displayModifiedTime", &now.to_string());
    t.set(root, "syncModifiedTime", &now.to_string());
    let full_xml = t.to_bytes(root);
    let mut up = t.clone();
    up.nodes[root].tag = "NoteResource".into();
    for n in up.children(root).to_vec() {
        if matches!(up.tag(n), "bodyText" | "floatingObjectList") {
            up.remove(root, n);
        }
    }
    if recipe_version >= 2 {
        let upload_files = up.one(root, "contentFileList")?;
        for f in up.children(upload_files).to_vec() {
            if !up.get(f, "id").is_some_and(|id| changed_files.iter().any(|c| c == id)) {
                up.remove(upload_files, f);
            }
        }
    }
    let page_id = page_id.unwrap();
    let layer_id = layer_id.unwrap();
    let upload_pages = up.one(root, "pageList")?;
    for p in up.children(upload_pages).to_vec() {
        if up.get(p, "id") != Some(page_id.as_str()) {
            up.remove(upload_pages, p);
            continue;
        }
        let layers = up.one(p, "layerList")?;
        for l in up.children(layers).to_vec() {
            if up.get(l, "id") != Some(layer_id.as_str()) {
                up.remove(layers, l);
                continue;
            }
            let objects = up.one(l, "objectList")?;
            for o in up.children(objects).to_vec() {
                if up.get(o, "id") != Some(new_stroke_id) {
                    up.remove(objects, o);
                }
            }
        }
    }
    let mut requested = vec![page_id.clone(), layer_id.clone(), new_stroke_id.to_string()];
    requested.extend(changed_files.iter().cloned());
    Ok(InkAppendEdit {
        full_xml,
        tree_xml: t.to_bytes(tree),
        upload_xml: up.to_bytes(root),
        requested_ids: requested,
        page_id,
        layer_id,
        stroke_id: new_stroke_id.to_string(),
        stroke_modified_time: tick,
        binary_parts: vec![("strokeResource".into(), format!("{new_stroke_id}_{tick}"), native)],
        content_file_ids: changed_files,
        recipe_version,
    })
}

pub fn samples_eq(a: &Samples, b: &Samples) -> bool {
    a == b
}

// ---- public pure-Rust append API (for other crates; no PyO3) --------------

/// Which append recipe to build (ink_capture.prepare_ink_append / _v2 / _v3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AppendRecipe {
    /// Recipe 1: no owner rebase, no cache release (uncached notes only).
    V1,
    /// Recipe 2: page-coordinate capture, native owner rebase, cache-only release.
    V2,
    /// Recipe 3: recipe 2 plus explicit opaque ARGB colour and page-unit pen size.
    V3 { color_argb: u32, pen_size: f64 },
}

/// The DocumentSnapshot fields the preparation reads.
#[derive(Clone, Copy, Debug)]
pub struct AppendSnapshot<'a> {
    pub resource_xml: &'a [u8],
    pub raw_body: &'a [u8],
    pub content_type: &'a str,
    /// `snapshot.commit_id`; None or empty -> InkError('committed snapshot required').
    pub commit_id: Option<&'a str>,
}

impl<'a> AppendSnapshot<'a> {
    pub fn from_doc(s: &'a rs_doc::doc::DocumentSnapshot) -> Self {
        AppendSnapshot { resource_xml: &s.resource_xml, raw_body: &s.raw_body, content_type: &s.content_type,
                         commit_id: s.commit_id.as_deref() }
    }
}

/// Exactly ink_capture.prepare_ink_append{,_v2,_v3}(snapshot, source_stroke_id,
/// new_stroke_id=..., samples=..., now_ms=..., [color_argb, pen_size]) for typed input.
/// Same checks, order, errors and byte output as the Python oracle. `limits` are the
/// tools.notes_payload limits (`None` = defaults; pass `rs_doc::pyapi::payload_limits(py)`
/// to honour patched module constants). The InkAppendEdit `title`/`body` fields of the
/// Python object are the snapshot's title/body (not part of this struct).
pub fn prepare_append(
    snapshot: &AppendSnapshot,
    source_stroke_id: &str,
    new_stroke_id: &str,
    samples: &RawSamples,
    now_ms: i64,
    recipe: AppendRecipe,
    limits: Option<&rs_doc::mime::Limits>,
) -> R<InkAppendEdit> {
    let capture = super::capture::normalize_capture(samples, false)?;
    if !snapshot.commit_id.is_some_and(|c| !c.is_empty()) {
        return ink("committed snapshot required");
    }
    let default_limits = rs_doc::mime::Limits::default();
    let limits = limits.unwrap_or(&default_limits);
    let parse = |raw: &[u8], ct: &str| payload_parts(raw, ct, limits);
    let snap = SnapshotIn { resource_xml: snapshot.resource_xml, raw_body: snapshot.raw_body,
                            content_type: snapshot.content_type };
    let (version, style) = match recipe {
        AppendRecipe::V1 => (1, None),
        AppendRecipe::V2 => (2, None),
        AppendRecipe::V3 { color_argb, pen_size } => (3, Some(StyleOverride {
            color_argb: Val::Int(color_argb as i128), pen_size: Val::Float(pen_size) })),
    };
    prepare_ink_document(&snap, Some(source_stroke_id), Some(new_stroke_id), Geometry::Capture(&capture),
                         Val::Int(now_ms as i128), version, style.as_ref(), &parse)
}

/// prepare_append with AppendRecipe::V2.
pub fn append_v2(snapshot: &AppendSnapshot, source_stroke_id: &str, new_stroke_id: &str, samples: &RawSamples,
                 now_ms: i64, limits: Option<&rs_doc::mime::Limits>) -> R<InkAppendEdit> {
    prepare_append(snapshot, source_stroke_id, new_stroke_id, samples, now_ms, AppendRecipe::V2, limits)
}

/// prepare_append with AppendRecipe::V3.
#[allow(clippy::too_many_arguments)]
pub fn append_v3(snapshot: &AppendSnapshot, source_stroke_id: &str, new_stroke_id: &str, samples: &RawSamples,
                 now_ms: i64, color_argb: u32, pen_size: f64, limits: Option<&rs_doc::mime::Limits>) -> R<InkAppendEdit> {
    prepare_append(snapshot, source_stroke_id, new_stroke_id, samples, now_ms,
                   AppendRecipe::V3 { color_argb, pen_size }, limits)
}

#[cfg(test)]
mod api_tests {
    //! Oracle fixtures: set INK_API_CASES to a JSON file dumped from the Python implementation.
    use super::*;
    use base64::Engine;
    #[test]
    fn public_append_matches_python_oracle() {
        let Ok(path) = std::env::var("INK_API_CASES") else { return };
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let b64 = |s: &serde_json::Value| base64::engine::general_purpose::STANDARD.decode(s.as_str().unwrap()).unwrap();
        let pts: Vec<(f64, f64)> = v["pts"].as_array().unwrap().iter()
            .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap())).collect();
        let pr: Vec<f64> = v["pr"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        let ts: Vec<i64> = v["ts"].as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect();
        let samples = RawSamples::from_parts(&pts, &pr, &ts);
        for c in v["cases"].as_array().unwrap() {
            let (raw, xml) = (b64(&c["raw"]), b64(&c["xml"]));
            let snap = AppendSnapshot { resource_xml: &xml, raw_body: &raw, content_type: c["ct"].as_str().unwrap(),
                                        commit_id: c["commit"].as_str() };
            let recipe = match c["recipe"].as_str().unwrap() {
                "v1" => AppendRecipe::V1,
                "v2" => AppendRecipe::V2,
                _ => AppendRecipe::V3 { color_argb: 0xffff0000, pen_size: 7.25 },
            };
            let got = prepare_append(&snap, v["source"].as_str().unwrap(), v["new"].as_str().unwrap(), &samples,
                                     v["now"].as_i64().unwrap(), recipe, None);
            let exp = &c["exp"];
            if exp["ok"].as_bool().unwrap() {
                let e = got.unwrap_or_else(|e| panic!("{}: {:?}", c["name"], e));
                assert_eq!(e.full_xml, b64(&exp["full"]), "{}", c["name"]);
                assert_eq!(e.tree_xml, b64(&exp["tree"]));
                assert_eq!(e.upload_xml, b64(&exp["upload"]));
                assert_eq!(e.binary_parts[0].2, b64(&exp["bin"]));
                assert_eq!(e.stroke_modified_time, exp["tick"].as_i64().unwrap());
            } else {
                match got {
                    Err(InkErr::Ink(m)) => assert_eq!(m, exp["err"].as_str().unwrap()),
                    other => panic!("{}: expected error, got {:?}", c["name"], other.map(|_| ())),
                }
            }
        }
    }
}
