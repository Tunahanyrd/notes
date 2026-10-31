//! Replacement for the Python module `ink_erase`: offline whole-stroke delete
//! and in-place stroke move (pure document preparation, no network).
use crate::bridge;
#[cfg(feature = "python")]
use crate::bridge::PyDeps;
use crate::bridge::RPy;
use crate::deps::*;
use crate::err::*;
use crate::util::*;
use crate::xml::{self, Element};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyTuple};
use std::any::Any;
use std::collections::{BTreeSet, HashMap};

pub const MAX_EDIT_STROKES: usize = 64;
pub const MAX_TRANSLATION: f64 = 32768.0;
pub const PROP_MOVABLE: u64 = 4;
pub const PROP_TEMPLATE: u64 = 64;
pub const PROP_LOCKED: u64 = 512;
pub const PROP_NOT_REMOVABLE: u64 = 4096;
pub const DELETE_BASE_FIELDS: u64 = 0x1 | 0x20 | 0x2000 | 0x4000 | 0x40000;
pub const MOVE_BASE_FIELDS: u64 = 0x20 | 0x2000 | 0x4000;
pub const SUPPORTED_SECTIONS: [&str; 6] =
    ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];

/// Pure-Rust result of a stroke delete/move preparation.
pub struct StrokeEdit {
    pub kind: &'static str,
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub requested_ids: Vec<String>,
    pub page_id: String,
    pub layer_id: String,
    pub stroke_ids: Vec<String>,
    pub modified_time: i64,
    pub binary_parts: Vec<BinaryPart>,
    pub removed_part_filenames: Vec<String>,
    pub content_file_ids: Vec<String>,
    pub snapshot: Snapshot,
}

/// Per-stroke delete profile from the base frame property/field masks.
pub fn erasable_masks(properties: u64, fields: u64) -> bool {
    properties & (PROP_LOCKED | PROP_TEMPLATE | PROP_NOT_REMOVABLE) == 0 && fields & !DELETE_BASE_FIELDS == 0
}

pub fn stroke_erasable(record: &Record) -> bool {
    erasable_masks(record.frames[0].properties, record.frames[0].fields)
}

fn canonical(identity: Option<&str>) -> R<String> {
    match identity {
        Some(s) if canonical_uuid(s) => Ok(s.to_string()),
        _ => ink("canonical stroke UUID required"),
    }
}

/// Caller-supplied stroke id collection: `None` = str/bytes (rejected).
pub type StrokeIds = Option<Vec<Option<String>>>;

pub(crate) struct Ctx {
    pub snap: Snapshot,
    pub root: Element,
    pub tree_i: usize,
    pub attrs_i: usize,
    pub pages_i: usize,
    pub page_i: usize,
    pub page_id: String,
    pub layer_id: String,
    pub records: HashMap<String, (Record, Binding)>,
    pub stroke_ids: Vec<String>,
    pub tick: i64,
    pub revision: i64,
}

fn sect(e: &Element, name: &str) -> R<usize> {
    one_idx(e, &name.to_lowercase()).remap(&[Kind::Doc], Kind::Ink, "ambiguous or missing document section")
}

impl Ctx {
    pub fn page(&self) -> &Element {
        &self.root.children[self.pages_i].children[self.page_i]
    }
    fn page_mut(&mut self) -> &mut Element {
        &mut self.root.children[self.pages_i].children[self.page_i]
    }
    fn layer_pos(&self) -> R<(usize, usize)> {
        let page = self.page();
        let li = sect(page, "layerList")?;
        let lj = page.children[li].children.iter().position(|l| l.get("id") == Some(self.layer_id.as_str())).unwrap();
        Ok((li, lj))
    }
    fn layer_mut(&mut self) -> R<&mut Element> {
        let (li, lj) = self.layer_pos()?;
        Ok(&mut self.page_mut().children[li].children[lj])
    }
    fn tree_pos(&self) -> R<(usize, usize, usize, usize)> {
        let tree = &self.root.children[self.tree_i];
        let tpl = sect(tree, "pageList")?;
        let tp = tree.children[tpl].children.iter().position(|p| p.get("id") == Some(self.page_id.as_str())).unwrap();
        let tpage = &tree.children[tpl].children[tp];
        let tll = sect(tpage, "layerList")?;
        let tl = tpage.children[tll].children.iter().position(|l| l.get("id") == Some(self.layer_id.as_str())).unwrap();
        Ok((tpl, tp, tll, tl))
    }
    fn tree_page_mut(&mut self) -> R<&mut Element> {
        let (tpl, tp, _, _) = self.tree_pos()?;
        let ti = self.tree_i;
        Ok(&mut self.root.children[ti].children[tpl].children[tp])
    }
    fn tree_layer_mut(&mut self) -> R<&mut Element> {
        let (tpl, tp, tll, tl) = self.tree_pos()?;
        let ti = self.tree_i;
        Ok(&mut self.root.children[ti].children[tpl].children[tp].children[tll].children[tl])
    }
}

pub(crate) fn load(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    layer_id: Option<&str>,
    stroke_ids: &StrokeIds,
    now_ms: Option<i128>,
) -> R<Ctx> {
    load_mode(deps, snapshot, page_id, layer_id, stroke_ids, now_ms, false)
}

fn load_mode(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    layer_id: Option<&str>,
    stroke_ids: &StrokeIds,
    now_ms: Option<i128>,
    bounded: bool,
) -> R<Ctx> {
    let snap = match deps.snapshot(snapshot)? {
        Some(s) if s.commit_id.as_deref().is_some_and(|c| !c.is_empty()) => s,
        _ => return ink("committed snapshot required"),
    };
    let page_id = canonical(page_id)?;
    let layer_id = canonical(layer_id)?;
    let requested = match stroke_ids {
        Some(v) if (1..=MAX_EDIT_STROKES).contains(&v.len()) => v,
        _ => return ink("1-64 stroke identities required"),
    };
    let stroke_ids: Vec<String> = requested.iter().map(|s| canonical(s.as_deref())).collect::<R<_>>()?;
    if stroke_ids.iter().collect::<BTreeSet<_>>().len() != stroke_ids.len() {
        return ink("duplicate stroke identity in request");
    }
    let root = xml::parse_bytes(&snap.resource_xml).map_err(|m| e_parse(&m))?;
    if root.iter().any(locked_flags) {
        return ink("locked or encrypted ink is unsupported");
    }
    let sync_time = integer(root.get("syncModifiedTime")).remap(&[Kind::Doc], Kind::Ink, "invalid sync timestamp")?;
    match now_ms {
        Some(n) if (sync_time as i128) < n && n < I63 => {}
        _ => return ink("sync timestamp must advance in wall-clock milliseconds"),
    }
    let tree_i = sect(&root, "noteTree")?;
    let attrs_i = sect(&root, "attributes")?;
    let pages_i = sect(&root, "pageList")?;
    let (tree, attrs, pages) = (&root.children[tree_i], &root.children[attrs_i], &root.children[pages_i]);
    if attrs.get("formatVersion") != Some("4000")
        || tree.get("ver") != Some("4000")
        || tree.get_or("isStrokeGrouped", "false").to_lowercase() != "false"
    {
        return ink("unsupported ink document profile");
    }
    if pages.len() > inkcore_pages() || root.iter().any(|n| matches!(n.tag.as_str(), "strokeGroup" | "voiceDataList" | "attachedFileMap")) {
        return ink("grouped/media ink is unsupported");
    }
    sect(&root, "floatingObjectList")?;
    // Free-floating pictures/shapes/text boxes are carried through untouched.
    let floating_ticks: Vec<i64> =
        floating_objects(&root)?.iter().map(|o| integer(o.get("modifiedTime"))).collect::<R<_>>()?;
    if root.children.iter().any(|n| !SUPPORTED_SECTIONS.contains(&n.tag.as_str())) {
        return ink("unknown resource section");
    }
    let parsed = deps.parse_payload(&snap.raw_body, &snap.content_type)?;
    let parts: Vec<&Part> = parsed.parts.iter().filter(|p| p.name.as_deref() == Some("strokeResource")).collect();
    if parts.is_empty()
        || parsed.parts.iter().any(|p| {
            p.name.as_deref() != Some("strokeResource") && !matches!(p.content_type.as_str(), "text/xml" | "application/xml")
        })
    {
        return ink("unsupported ink part inventory");
    }
    // Images, shapes, lines and text boxes are carried through untouched
    // (util::passthrough_object); only stroke objects are edited.
    let (objects, carried) = ink_objects(pages).remap(&[Kind::Ink], Kind::Ink, "unsupported or ambiguous stroke inventory")?;
    let carried_ticks: Vec<i64> = carried.iter().map(|o| integer(o.get("modifiedTime"))).collect::<R<_>>()?;
    let wanted: BTreeSet<&str> = ["type", "id", "modifiedTime", "hash"].into_iter().collect();
    if objects.len() != parts.len()
        || objects.iter().any(|o| o.get("type") != Some("stroke") || !o.is_empty() || o.attr_key_set() != wanted)
    {
        return ink("unsupported or ambiguous stroke inventory");
    }
    let mut by_id: HashMap<Option<&str>, &Element> = HashMap::new();
    for o in &objects {
        by_id.insert(o.get("id"), o);
    }
    if by_id.len() != objects.len() {
        return ink("duplicate stroke identity");
    }
    let mut records: HashMap<String, (Record, Binding)> = HashMap::new();
    let mut seen = BTreeSet::new();
    let mut max_record_time: Option<i64> = None;
    let strings = attrs.find("stringIdList");
    for part in &parts {
        let record = deps.decode_stroke(&part.body)?;
        let identity = record.identity()?;
        if !seen.insert(identity.clone()) || !by_id.contains_key(&Some(identity.as_str())) {
            return ink("binary/XML inventory differs");
        }
        let binding = deps.bind(&record, by_id[&Some(identity.as_str())], strings)?;
        if part.filename.as_deref() != Some(format!("{}_{}", identity, binding.modified_time).as_str()) {
            return ink("stroke MIME identity/time differs");
        }
        max_record_time = Some(max_record_time.map_or(binding.modified_time, |old| old.max(binding.modified_time)));
        if !bounded || stroke_ids.iter().any(|id| id == &identity) {
            records.insert(identity, (record, binding));
        }
    }
    let record_ids: BTreeSet<Option<&str>> = seen.iter().map(|k| Some(k.as_str())).collect();
    let object_ids: BTreeSet<Option<&str>> = by_id.keys().copied().collect();
    if record_ids != object_ids {
        return ink("binary/XML inventory differs");
    }
    let matches: Vec<usize> =
        pages.children.iter().enumerate().filter(|(_, p)| p.get("id") == Some(page_id.as_str())).map(|(i, _)| i).collect();
    if matches.len() != 1 {
        return ink("page absent or ambiguous");
    }
    let page_i = matches[0];
    let page = &pages.children[page_i];
    let layers = &page.children[sect(page, "layerList")?];
    let found: Vec<&Element> = layers.children.iter().filter(|l| l.get("id") == Some(layer_id.as_str())).collect();
    if found.len() != 1 {
        return ink("layer absent or ambiguous");
    }
    let layer = found[0];
    if layer.get_or("lockState", "false").to_lowercase() != "false" || layer.get_or("thumbnail", "-1") != "-1" {
        return ink("locked or cached layer");
    }
    let old_objects = layer.find("objectList");
    let layer_ids: Vec<Option<&str>> = old_objects.map(|o| o.children.iter().map(|c| c.get("id")).collect()).unwrap_or_default();
    if stroke_ids.iter().any(|s| !layer_ids.contains(&Some(s.as_str()))) {
        return ink("stroke is not on the requested layer");
    }
    // Images, shapes, lines and text boxes are never erased or moved.
    if stroke_ids.iter().any(|s| !records.contains_key(s)) {
        return ink("only stroke objects can be erased or moved");
    }
    let old_objects = old_objects.unwrap();
    let tree_pages = &tree.children[sect(tree, "pageList")?];
    let tp: Vec<&Element> = tree_pages.children.iter().filter(|p| p.get("id") == Some(page_id.as_str())).collect();
    if tp.len() != 1 {
        return ink("page absent from tree");
    }
    let tree_page = tp[0];
    let tl: Vec<&Element> = tree_page.children[sect(tree_page, "layerList")?]
        .children
        .iter()
        .filter(|l| l.get("id") == Some(layer_id.as_str()))
        .collect();
    if tl.len() != 1 {
        return ink("layer absent from tree");
    }
    let tree_layer = tl[0];
    let tree_objects = tree_layer.find("objectList");
    let left: Vec<_> = old_objects.children.iter().map(|o| (o.get("id"), o.get("modifiedTime"), o.get("hash"))).collect();
    let right: Vec<_> =
        tree_objects.map(|t| t.children.iter().map(|o| (o.get("id"), o.get("t"), o.get("h"))).collect()).unwrap_or_default();
    if left != right {
        return ink("stroke tree differs from objects");
    }
    let metadata = (|| -> R<(i64, i64)> {
        let layer_time = integer(layer.get("modifiedTime"))?;
        let page_time = integer(page.get("modifiedTime"))?;
        let layer_hash = hex(&parent_hash(Some(&layer_id), layer_time, &hashes(&old_objects.children)?)?);
        if Some(layer_hash.as_str()) != layer.get("hash") || Some(layer_hash.as_str()) != tree_layer.get("h") {
            return ink("layer hash mismatch");
        }
        let page_hash = hex(&parent_hash(Some(&page_id), page_time, &hashes(&layers.children)?)?);
        if Some(page_hash.as_str()) != page.get("hash") || Some(page_hash.as_str()) != tree_page.get("h") {
            return ink("page hash mismatch");
        }
        let mut ticks = Vec::new();
        for f in &root.children[sect(&root, "contentFileList")?].children {
            ticks.push(integer(f.get("modifiedTime"))?);
        }
        ticks.extend([integer(attrs.get("modifiedTime"))?, page_time, layer_time]);
        ticks.extend(carried_ticks.iter().copied());
        ticks.extend(floating_ticks.iter().copied());
        let revision = integer(attrs.get("fileRevision"))? as i128 + 1;
        let max = ticks.into_iter().chain(max_record_time).max().unwrap();
        let tick = max as i128 + 1;
        if tick >= I63 || revision >= 1 << 31 {
            return ink("ink timestamp/revision overflow");
        }
        Ok((tick as i64, revision as i64))
    })();
    let (tick, revision) = match metadata {
        Err(e) if !e.is(Kind::Ink) && e.is(Kind::Value) => return ink("invalid page/layer metadata"),
        other => other?,
    };
    for identity in &stroke_ids {
        let props = records[identity].0.frames[0].properties;
        if props & (PROP_LOCKED | PROP_TEMPLATE) != 0 {
            return ink("locked or template stroke");
        }
    }
    drop(by_id);
    Ok(Ctx { snap, root, tree_i, attrs_i, pages_i, page_i, page_id, layer_id, records, stroke_ids, tick, revision })
}

fn rehash(ctx: &mut Ctx) -> R<()> {
    let tick = ctx.tick;
    let layer_id = ctx.layer_id.clone();
    let page_id = ctx.page_id.clone();
    let layer = ctx.layer_mut()?;
    let children = match layer.find("objectList") {
        Some(o) => hashes(&o.children)?,
        None => vec![],
    };
    let layer_hash = hex(&parent_hash(Some(&layer_id), tick, &children)?);
    layer.set("modifiedTime", &tick.to_string());
    layer.set("hash", &layer_hash);
    let tl = ctx.tree_layer_mut()?;
    tl.set("t", &tick.to_string());
    tl.set("h", &layer_hash);
    let page = ctx.page();
    let lh = hashes(&page.children[sect(page, "layerList")?].children)?;
    let page_hash = hex(&parent_hash(Some(&page_id), tick, &lh)?);
    let page = ctx.page_mut();
    page.set("modifiedTime", &tick.to_string());
    page.set("hash", &page_hash);
    let tp = ctx.tree_page_mut()?;
    tp.set("t", &tick.to_string());
    tp.set("h", &page_hash);
    Ok(())
}

fn finish(
    mut ctx: Ctx,
    now_ms: i64,
    kind: &'static str,
    binary_parts: Vec<BinaryPart>,
    removed: Vec<String>,
    changed_files: Vec<String>,
    keep_objects: Vec<String>,
) -> R<StrokeEdit> {
    let (revision, tick) = (ctx.revision, ctx.tick);
    {
        let attrs = &mut ctx.root.children[ctx.attrs_i];
        attrs.set("fileRevision", &revision.to_string());
        attrs.set("modifiedTime", &tick.to_string());
        attrs.set("displayModifiedTime", &now_ms.to_string());
    }
    ctx.root.set("syncModifiedTime", &now_ms.to_string());
    let full = xml::xml_bytes(&ctx.root);
    let mut upload = upload_copy(&ctx.root);
    let fi = sect(&upload, "contentFileList")?;
    upload.children[fi].children.retain(|f| f.get("id").is_some_and(|i| changed_files.iter().any(|c| c == i)));
    let pi = sect(&upload, "pageList")?;
    let mut pages = std::mem::take(&mut upload.children[pi].children);
    pages.retain(|p| p.get("id") == Some(ctx.page_id.as_str()));
    for p in pages.iter_mut() {
        let li = sect(p, "layerList")?;
        let layers = &mut p.children[li].children;
        layers.retain(|l| l.get("id") == Some(ctx.layer_id.as_str()));
        for l in layers.iter_mut() {
            if let Some(oi) = l.find_index("objectList") {
                l.children[oi].children.retain(|o| o.get("id").is_some_and(|i| keep_objects.iter().any(|k| k == i)));
                if l.children[oi].is_empty() {
                    l.children.remove(oi);
                }
            }
        }
    }
    upload.children[pi].children = pages;
    let mut requested = vec![ctx.page_id.clone(), ctx.layer_id.clone()];
    requested.extend(keep_objects.iter().cloned());
    requested.extend(changed_files.iter().cloned());
    Ok(StrokeEdit {
        kind,
        full_xml: full,
        tree_xml: xml::xml_bytes(&ctx.root.children[ctx.tree_i]),
        upload_xml: xml::xml_bytes(&upload),
        requested_ids: requested,
        page_id: ctx.page_id.clone(),
        layer_id: ctx.layer_id.clone(),
        stroke_ids: ctx.stroke_ids.clone(),
        modified_time: tick,
        binary_parts,
        removed_part_filenames: removed,
        content_file_ids: changed_files,
        snapshot: ctx.snap,
    })
}

pub fn prepare_ink_delete(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    layer_id: Option<&str>,
    stroke_ids: &StrokeIds,
    now_ms: Option<i128>,
) -> R<StrokeEdit> {
    prepare_ink_delete_mode(deps, snapshot, page_id, layer_id, stroke_ids, now_ms, false)
}

/// The migration verifier only needs the selected stroke after validating
/// every part; retaining all decoded records is unnecessary for this probe.
pub fn prepare_ink_delete_for_migration(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    layer_id: Option<&str>,
    stroke_ids: &StrokeIds,
    now_ms: Option<i128>,
) -> R<StrokeEdit> {
    prepare_ink_delete_mode(deps, snapshot, page_id, layer_id, stroke_ids, now_ms, true)
}

fn prepare_ink_delete_mode(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    layer_id: Option<&str>,
    stroke_ids: &StrokeIds,
    now_ms: Option<i128>,
    bounded: bool,
) -> R<StrokeEdit> {
    let mut ctx = load_mode(deps, snapshot, page_id, layer_id, stroke_ids, now_ms, bounded)?;
    for identity in &ctx.stroke_ids {
        if !stroke_erasable(&ctx.records[identity].0) {
            return ink("stroke is not removable in the delete profile");
        }
    }
    if !ctx.page().iter_tag("object").any(|o| !o.get("id").is_some_and(|i| ctx.stroke_ids.iter().any(|s| s == i))) {
        return ink("removing every stroke of a page is unsupported");
    }
    let page_id = ctx.page_id.clone();
    let changed_files = deps.release_page_cache(&mut ctx.root, &page_id, ctx.tick)?;
    let ids = ctx.stroke_ids.clone();
    let mut removed = Vec::new();
    let is_target = |o: &Element| o.get("id").is_some_and(|i| ids.iter().any(|s| s == i));
    {
        let layer = ctx.layer_mut()?;
        let oi = layer.find_index("objectList").unwrap();
        for o in &layer.children[oi].children {
            if is_target(o) {
                removed.push(format!("{}_{}", o.get("id").unwrap(), o.get("modifiedTime").unwrap_or("")));
            }
        }
        layer.children[oi].children.retain(|o| !is_target(o));
        if layer.children[oi].is_empty() {
            layer.children.remove(oi);
        }
    }
    {
        let tl = ctx.tree_layer_mut()?;
        let oi = tl.find_index("objectList").unwrap();
        tl.children[oi].children.retain(|o| !is_target(o));
        if tl.children[oi].is_empty() {
            tl.children.remove(oi);
        }
    }
    rehash(&mut ctx)?;
    let now = now_ms.unwrap() as i64;
    finish(ctx, now, "delete", vec![], removed, changed_files, vec![])
}

/// A numeric argument for (dx, dy): `None` when not an int/float.
pub fn prepare_ink_move(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    page_id: Option<&str>,
    layer_id: Option<&str>,
    stroke_ids: &StrokeIds,
    dx: Option<f64>,
    dy: Option<f64>,
    now_ms: Option<i128>,
) -> R<StrokeEdit> {
    let (dx, dy) = match (dx, dy) {
        (Some(x), Some(y))
            if [x, y].iter().all(|v| v.is_finite() && v.abs() <= MAX_TRANSLATION) && !(x == 0.0 && y == 0.0) =>
        {
            (x, y)
        }
        _ => return ink("bounded nonzero translation required"),
    };
    let mut ctx = load(deps, snapshot, page_id, layer_id, stroke_ids, now_ms)?;
    let tick = ctx.tick;
    if ctx.page().get_or("orientation", "0") != "0" {
        return ink("page orientation transform is unsupported");
    }
    let width = integer(ctx.page().get("width"))?;
    let height = integer(ctx.page().get("height"))?;
    let page_id = ctx.page_id.clone();
    let changed_files = deps.release_page_cache(&mut ctx.root, &page_id, tick)?;
    let strings = ctx.root.children[ctx.attrs_i].find("stringIdList").cloned();
    let mut parts = Vec::new();
    let mut removed = Vec::new();
    let mut union: Option<[f64; 4]> = None;
    for identity in ctx.stroke_ids.clone() {
        let (record, binding) = &ctx.records[&identity];
        let (base, stroke) = (&record.frames[0], &record.frames[1]);
        if base.properties & PROP_MOVABLE == 0 {
            return ink("stroke is not movable");
        }
        if record.frames.len() != 2 || base.fields & !MOVE_BASE_FIELDS != 0 {
            return ink("stroke metadata needs unsupported geometry handling");
        }
        if !record.compressed || record.samples.points.is_empty() {
            return ink("move requires a nonempty compressed stroke");
        }
        if deps.owner_extent(record)? != (width, height) {
            return ink("stored owner extent differs from page; scaled move unsupported");
        }
        let b = binding.bounds;
        let bounds = [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy];
        let (w, h) = (width as f64, height as f64);
        if !(0.0 <= bounds[0] && bounds[0] <= bounds[2] && bounds[2] <= w)
            || !(0.0 <= bounds[1] && bounds[1] <= bounds[3] && bounds[3] <= h)
        {
            return ink("moved drawn bounds leave the page");
        }
        let mut native = record.raw.clone();
        let base_offset = 12 + base.fixed_start;
        native[base_offset + 42..base_offset + 50].copy_from_slice(&tick.to_le_bytes());
        for (i, v) in bounds.iter().enumerate() {
            let at = base_offset + 50 + 8 * i;
            native[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        let point_offset = 12 + base.raw.len() + stroke.fixed_start + 2;
        let sx = f64::from_le_bytes(native[point_offset..point_offset + 8].try_into().unwrap());
        let sy = f64::from_le_bytes(native[point_offset + 8..point_offset + 16].try_into().unwrap());
        native[point_offset..point_offset + 8].copy_from_slice(&(sx + dx).to_le_bytes());
        native[point_offset + 8..point_offset + 16].copy_from_slice(&(sy + dy).to_le_bytes());
        let moved = deps.decode_stroke(&native)?;
        let expected: Vec<(f64, f64)> =
            record.samples.points.iter().map(|(x, y)| Ok((f32r(x + dx)?, f32r(y + dy)?))).collect::<R<_>>()?;
        let old = &record.samples;
        let new = &moved.samples;
        if new.points != expected
            || new.pressures != old.pressures
            || new.timestamps != old.timestamps
            || new.tilts != old.tilts
            || new.orientations != old.orientations
        {
            return ink("translation changes native channel precision");
        }
        let (seed_start, seed_stop) = (stroke.fixed_start + 2, stroke.fixed_start + 18);
        let (new_stroke, new_base) = (&moved.frames[1].raw, &moved.frames[0].raw);
        let (tr_start, tr_stop) = (base.fixed_start + 42, base.fixed_start + 82);
        let head = |v: &[u8], n: usize| v[..n.min(v.len())].to_vec();
        let tail = |v: &[u8], n: usize| v[n.min(v.len())..].to_vec();
        if new_stroke.len() != stroke.raw.len()
            || head(new_stroke, seed_start) != head(&stroke.raw, seed_start)
            || tail(new_stroke, seed_stop) != tail(&stroke.raw, seed_stop)
            || new_base.len() != base.raw.len()
            || head(new_base, tr_start) != head(&base.raw, tr_start)
            || tail(new_base, tr_stop) != tail(&base.raw, tr_stop)
        {
            return ink("move changed bytes outside time, rect and point seed");
        }
        let digest = hex(&object_hash(&identity, tick)?);
        let mut obj_copy = None;
        {
            let layer = ctx.layer_mut()?;
            let objects = layer.find_mut("objectList").unwrap();
            if let Some(obj) = objects.children.iter_mut().rev().find(|o| o.get("id") == Some(identity.as_str())) {
                removed.push(format!("{}_{}", identity, obj.get("modifiedTime").unwrap_or("")));
                obj.set("modifiedTime", &tick.to_string());
                obj.set("hash", &digest);
                obj_copy = Some(obj.clone());
            }
        }
        {
            let tl = ctx.tree_layer_mut()?;
            let objects = tl.find_mut("objectList").unwrap();
            if let Some(t) = objects.children.iter_mut().rev().find(|o| o.get("id") == Some(identity.as_str())) {
                t.set("t", &tick.to_string());
                t.set("h", &digest);
            }
        }
        deps.bind(&moved, obj_copy.as_ref().unwrap(), strings.as_ref())?;
        parts.push(BinaryPart { name: "strokeResource".into(), filename: format!("{identity}_{tick}"), body: native });
        union = Some(match union {
            None => bounds,
            Some(u) => [u[0].min(bounds[0]), u[1].min(bounds[1]), u[2].max(bounds[2]), u[3].max(bounds[3])],
        });
    }
    let union = union.unwrap();
    let page = ctx.page_mut();
    let drawn = match page.find_mut("drawnRect") {
        Some(d) => d,
        None => return ink("page drawn rectangle missing"),
    };
    let mut original = [0.0; 4];
    for (i, k) in ["l", "t", "r", "b"].iter().enumerate() {
        original[i] = match drawn.get(k).and_then(py_float) {
            Some(v) => v,
            None => return ink("invalid page drawn rectangle"),
        };
    }
    if original.iter().any(|v| !v.is_finite()) {
        return ink("invalid page drawn rectangle");
    }
    let pick_min = |a: f64, b: f64| if b < a { b } else { a };
    let pick_max = |a: f64, b: f64| if b > a { b } else { a };
    let merged = [
        pick_min(original[0], union[0]),
        pick_min(original[1], union[1]),
        pick_max(original[2], union[2]),
        pick_max(original[3], union[3]),
    ];
    for (k, v) in ["l", "t", "r", "b"].iter().zip(merged) {
        drawn.set(k, &py_float_repr(v));
    }
    rehash(&mut ctx)?;
    let keep = ctx.stroke_ids.clone();
    let now = now_ms.unwrap() as i64;
    finish(ctx, now, "move", parts, removed, changed_files, keep)
}

/// Python `float(str)`.
pub fn py_float(s: &str) -> Option<f64> {
    let t = s.trim();
    let cleaned: String = if t.contains('_') {
        let b = t.as_bytes();
        for (i, c) in b.iter().enumerate() {
            if *c == b'_' && (i == 0 || i + 1 >= b.len() || !b[i - 1].is_ascii_digit() || !b[i + 1].is_ascii_digit()) {
                return None;
            }
        }
        t.replace('_', "")
    } else {
        t.to_string()
    };
    let lower = cleaned.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if matches!(body, "inf" | "infinity" | "nan") {
        let neg = lower.starts_with('-');
        return Some(match body {
            "nan" => f64::NAN,
            _ if neg => f64::NEG_INFINITY,
            _ => f64::INFINITY,
        });
    }
    if !lower.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'+' | b'-')) {
        return None;
    }
    lower.parse().ok()
}

/// Sorted (filename, sha256) strokeResource inventory after an edit.
pub fn expected_stroke_parts(
    parts: &[Part],
    removed: &[Option<String>],
    added: &[(Option<String>, Vec<u8>)],
) -> R<Vec<(Option<String>, String)>> {
    let mut current: Vec<(Option<String>, String)> = Vec::new();
    for p in parts.iter().filter(|p| p.name.as_deref() == Some("strokeResource")) {
        let digest = hex(&sha256(&p.body));
        match current.iter_mut().find(|(k, _)| *k == p.filename) {
            Some(slot) => slot.1 = digest,
            None => current.push((p.filename.clone(), digest)),
        }
    }
    if removed.iter().any(|n| !current.iter().any(|(k, _)| k == n)) {
        return ink("removed part absent from baseline");
    }
    for n in removed {
        current.retain(|(k, _)| k != n);
    }
    for (name, body) in added {
        if current.iter().any(|(k, _)| k == name) {
            return ink("replacement part already present");
        }
        current.push((name.clone(), hex(&sha256(body))));
    }
    if current.iter().any(|(k, _)| k.is_none()) && current.len() > 1 {
        return type_err("'<' not supported between instances of 'str' and 'NoneType'");
    }
    current.sort();
    Ok(current)
}

/// Exact compareForUpsync rule for a stroke delete/move.
pub struct EditView {
    pub kind: String,
    pub page_id: Option<String>,
    pub layer_id: Option<String>,
    pub stroke_ids: Vec<Option<String>>,
    pub content_file_ids: Vec<Option<String>>,
}

fn unsafe_markup(text: &str) -> bool {
    if text.contains('\0') {
        return true;
    }
    let mut rest = text;
    while let Some(i) = rest.find("<!") {
        let after = rest[i + 2..].trim_start();
        for word in ["doctype", "entity"] {
            if after.len() >= word.len() && after.is_char_boundary(word.len()) && after[..word.len()].eq_ignore_ascii_case(word) {
                let next = after[word.len()..].chars().next();
                if !next.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    return true;
                }
            }
        }
        rest = &rest[i + 2..];
    }
    false
}

pub fn check_compare_response(raw: &[u8], baseline_commit: Option<&str>, edit: &EditView) -> R<()> {
    if raw.len() > 2 * 1024 * 1024 {
        return ink("compare response bound");
    }
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    let text = std::str::from_utf8(raw).map_err(|_| Error::new(Kind::Unicode, "'utf-8' codec can't decode bytes"))?;
    if unsafe_markup(text) {
        return ink("unsafe compare response");
    }
    let root = xml::parse_str(text).map_err(|m| e_parse(&m))?;
    if root.local() != "compareforupsyncview" {
        return ink("unexpected compare response");
    }
    let commits: Vec<Option<&str>> =
        root.iter().filter(|n| n.local() == "commitid" && n.is_empty()).map(|n| n.text.as_deref()).collect();
    let same = (|| -> R<bool> {
        Ok(commits.len() == 1 && normalize_commit(commits[0])? == normalize_commit(baseline_commit)?)
    })();
    match same {
        Ok(true) => {}
        Ok(false) => return ink("compare commit differs"),
        Err(e) if e.is(Kind::Doc) => return ink("compare commit differs"),
        Err(e) => return Err(e),
    }
    let objects: BTreeSet<Option<String>> =
        if edit.kind == "move" { edit.stroke_ids.iter().cloned().collect() } else { BTreeSet::new() };
    let files: BTreeSet<Option<String>> = edit.content_file_ids.iter().cloned().collect();
    let mut found: [BTreeSet<Option<String>>; 4] = Default::default();
    // (node, parent, grandparent) pre-order walk.
    let mut stack: Vec<(&Element, Option<&Element>, Option<&Element>)> = vec![(&root, None, None)];
    while let Some((node, parent, grand)) = stack.pop() {
        for c in node.children.iter().rev() {
            stack.push((c, Some(node), parent));
        }
        let kind = node.local();
        if !node.has("id") && !matches!(kind.as_str(), "page" | "layer" | "object" | "bodytext" | "contentfile") {
            continue;
        }
        let identity = node.get("id").map(str::to_string);
        let is_root = |e: Option<&Element>| e.is_some_and(|g| std::ptr::eq(g, &root));
        let slot = match kind.as_str() {
            "page" if identity == edit.page_id && parent.is_some_and(|p| p.local() == "pagelist") && is_root(grand) => Some(0),
            "layer"
                if identity == edit.layer_id
                    && parent.is_some_and(|p| p.local() == "layerlist")
                    && grand.is_some_and(|g| g.local() == "page" && g.get("id").map(str::to_string) == edit.page_id) =>
            {
                Some(1)
            }
            "object"
                if objects.contains(&identity)
                    && parent.is_some_and(|p| p.local() == "objectlist")
                    && grand.is_some_and(|g| g.local() == "layer" && g.get("id").map(str::to_string) == edit.layer_id) =>
            {
                Some(2)
            }
            "contentfile"
                if files.contains(&identity)
                    && node.is_empty()
                    && parent.is_some_and(|p| p.local() == "contentfilelist")
                    && is_root(grand) =>
            {
                Some(3)
            }
            _ => None,
        };
        match slot {
            Some(i) if !found[i].contains(&identity) => {
                found[i].insert(identity);
            }
            _ => return ink("compare response names unexpected content"),
        }
    }
    let want = [
        [edit.page_id.clone()].into_iter().collect::<BTreeSet<_>>(),
        [edit.layer_id.clone()].into_iter().collect(),
        objects,
        files,
    ];
    if found != want {
        return ink("compare response omits required content");
    }
    Ok(())
}

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
#[pyclass(module = "samsung_notes.ink_erase", name = "InkStrokeEdit", frozen, subclass)]
pub struct PyInkStrokeEdit {
    #[pyo3(get)]
    kind: Py<PyAny>,
    #[pyo3(get)]
    full_xml: Py<PyAny>,
    #[pyo3(get)]
    tree_xml: Py<PyAny>,
    #[pyo3(get)]
    upload_xml: Py<PyAny>,
    #[pyo3(get)]
    requested_ids: Py<PyAny>,
    #[pyo3(get)]
    page_id: Py<PyAny>,
    #[pyo3(get)]
    layer_id: Py<PyAny>,
    #[pyo3(get)]
    stroke_ids: Py<PyAny>,
    #[pyo3(get)]
    modified_time: Py<PyAny>,
    #[pyo3(get)]
    binary_parts: Py<PyAny>,
    #[pyo3(get)]
    removed_part_filenames: Py<PyAny>,
    #[pyo3(get)]
    title: Py<PyAny>,
    #[pyo3(get)]
    body: Py<PyAny>,
    #[pyo3(get)]
    content_file_ids: Py<PyAny>,
    #[pyo3(get)]
    recipe_version: Py<PyAny>,
}

#[cfg(feature = "python")]
impl PyInkStrokeEdit {
    #[cfg(feature = "python")]
    fn fields<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            [
                &self.kind,
                &self.full_xml,
                &self.tree_xml,
                &self.upload_xml,
                &self.requested_ids,
                &self.page_id,
                &self.layer_id,
                &self.stroke_ids,
                &self.modified_time,
                &self.binary_parts,
                &self.removed_part_filenames,
                &self.title,
                &self.body,
                &self.content_file_ids,
                &self.recipe_version,
            ]
            .map(|f| f.bind(py).clone()),
        )
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl PyInkStrokeEdit {
    #[new]
    #[pyo3(signature = (kind, full_xml, tree_xml, upload_xml, requested_ids, page_id, layer_id, stroke_ids,
                        modified_time, binary_parts, removed_part_filenames, title, body,
                        content_file_ids=None, recipe_version=None))]
    #[cfg(feature = "python")]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        kind: Py<PyAny>,
        full_xml: Py<PyAny>,
        tree_xml: Py<PyAny>,
        upload_xml: Py<PyAny>,
        requested_ids: Py<PyAny>,
        page_id: Py<PyAny>,
        layer_id: Py<PyAny>,
        stroke_ids: Py<PyAny>,
        modified_time: Py<PyAny>,
        binary_parts: Py<PyAny>,
        removed_part_filenames: Py<PyAny>,
        title: Py<PyAny>,
        body: Py<PyAny>,
        content_file_ids: Option<Py<PyAny>>,
        recipe_version: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        Ok(PyInkStrokeEdit {
            kind,
            full_xml,
            tree_xml,
            upload_xml,
            requested_ids,
            page_id,
            layer_id,
            stroke_ids,
            modified_time,
            binary_parts,
            removed_part_filenames,
            title,
            body,
            content_file_ids: content_file_ids.unwrap_or_else(|| PyTuple::empty(py).into_any().unbind()),
            recipe_version: recipe_version.unwrap_or_else(|| 1i64.into_pyobject(py).unwrap().into_any().unbind()),
        })
    }

    #[cfg(feature = "python")]
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "InkStrokeEdit(kind={}, strokes={}, content=PRIVATE)",
            self.kind.bind(py).str()?,
            self.stroke_ids.bind(py).len()?
        ))
    }

    #[cfg(feature = "python")]
    fn __eq__(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        match other.cast::<PyInkStrokeEdit>() {
            Ok(o) if o.get_type().is(slf.get_type()) => {
                let eq = slf.get().fields(py)?.eq(o.get().fields(py)?)?;
                Ok(eq.into_pyobject(py)?.to_owned().into_any().unbind())
            }
            _ => Ok(py.NotImplemented()),
        }
    }

    #[cfg(feature = "python")]
    fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
        self.fields(py)?.hash()
    }
}

#[cfg(feature = "python")]
fn edit_to_py<'py>(py: Python<'py>, e: StrokeEdit) -> PyResult<Bound<'py, PyAny>> {
    let snap = bridge::handle(py, &e.snapshot.handle)?;
    let parts: Vec<Bound<PyAny>> = e.binary_parts.iter().map(|p| bridge::ink_binary_part(py, p)).collect::<PyResult<_>>()?;
    let obj = PyInkStrokeEdit {
        kind: e.kind.into_pyobject(py)?.into_any().unbind(),
        full_xml: PyBytes::new(py, &e.full_xml).into_any().unbind(),
        tree_xml: PyBytes::new(py, &e.tree_xml).into_any().unbind(),
        upload_xml: PyBytes::new(py, &e.upload_xml).into_any().unbind(),
        requested_ids: bridge::frozenset_str(py, &e.requested_ids)?.unbind(),
        page_id: e.page_id.into_pyobject(py)?.into_any().unbind(),
        layer_id: e.layer_id.into_pyobject(py)?.into_any().unbind(),
        stroke_ids: bridge::tuple_str(py, &e.stroke_ids)?.into_any().unbind(),
        modified_time: e.modified_time.into_pyobject(py)?.into_any().unbind(),
        binary_parts: PyTuple::new(py, parts)?.into_any().unbind(),
        removed_part_filenames: bridge::tuple_str(py, &e.removed_part_filenames)?.into_any().unbind(),
        title: snap.getattr("title")?.unbind(),
        body: snap.getattr("body")?.unbind(),
        content_file_ids: bridge::tuple_str(py, &e.content_file_ids)?.into_any().unbind(),
        recipe_version: 1i64.into_pyobject(py)?.into_any().unbind(),
    };
    Ok(Bound::new(py, obj)?.into_any())
}

#[cfg(feature = "python")]
pub fn opt_str(obj: &Bound<'_, PyAny>) -> Option<String> {
    obj.extract::<String>().ok()
}

#[cfg(feature = "python")]
/// `type(v) is int` → value (saturated to i128 range).
pub fn exact_int(obj: &Bound<'_, PyAny>) -> Option<i128> {
    if !obj.get_type().is(obj.py().get_type::<pyo3::types::PyInt>()) {
        return None;
    }
    match obj.extract::<i128>() {
        Ok(v) => Some(v),
        Err(_) => Some(if obj.lt(0).unwrap_or(false) { i128::MIN } else { i128::MAX }),
    }
}

#[cfg(feature = "python")]
/// `type(v) in (int, float)` → f64 (huge ints map to infinity, which fails bounds).
pub fn int_or_float(obj: &Bound<'_, PyAny>) -> Option<f64> {
    let t = obj.get_type();
    if t.is(obj.py().get_type::<pyo3::types::PyFloat>()) {
        return obj.extract().ok();
    }
    exact_int(obj).map(|v| v as f64)
}

#[cfg(feature = "python")]
pub fn stroke_ids_arg(obj: &Bound<'_, PyAny>) -> PyResult<StrokeIds> {
    if obj.is_instance_of::<pyo3::types::PyString>() || obj.is_instance_of::<PyBytes>() {
        return Ok(None);
    }
    let items = PyTuple::new(obj.py(), obj.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
    Ok(Some(items.iter().map(|i| opt_str(&i)).collect()))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "stroke_erasable")]
fn py_stroke_erasable(record: &Bound<'_, PyAny>) -> PyResult<bool> {
    let base = record.getattr("frames")?.get_item(0)?;
    let le = |name: &str| -> PyResult<u64> {
        let raw: Vec<u8> = base.getattr(name)?.extract()?;
        Ok(raw.iter().enumerate().take(8).fold(0u64, |a, (i, b)| a | (*b as u64) << (8 * i)))
    };
    Ok(erasable_masks(le("properties")?, le("fields")?))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "prepare_ink_delete", signature = (snapshot, page_id, layer_id, stroke_ids, *, now_ms))]
fn py_prepare_ink_delete<'py>(
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    layer_id: &Bound<'py, PyAny>,
    stroke_ids: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let snap = bridge::opaque(snapshot);
    let (p, l) = (opt_str(page_id), opt_str(layer_id));
    // Python validates the snapshot and ids before touching stroke_ids.
    let ids = lazy_ids(stroke_ids);
    let edit = prepare_ink_delete(&deps, &snap, p.as_deref(), l.as_deref(), &ids.0, exact_int(now_ms));
    edit_to_py(py, ids.finish(py, edit)?)
}

#[cfg(feature = "python")]
/// Stroke-id conversion whose TypeError is raised at the Python point of use.
pub struct LazyIds(pub StrokeIds, pub Option<PyErr>);

#[cfg(feature = "python")]
pub fn lazy_ids(obj: &Bound<'_, PyAny>) -> LazyIds {
    match stroke_ids_arg(obj) {
        Ok(v) => LazyIds(v, None),
        Err(e) => LazyIds(Some(vec![]), Some(e)),
    }
}

#[cfg(feature = "python")]
impl LazyIds {
    #[cfg(feature = "python")]
    pub fn finish<T>(self, py: Python<'_>, result: R<T>) -> PyResult<T> {
        match (result, self.1) {
            (Err(e), Some(err)) if e.kind == Kind::Ink && e.msg == "1-64 stroke identities required" => Err(err),
            (r, _) => r.py(py),
        }
    }
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "prepare_ink_move", signature = (snapshot, page_id, layer_id, stroke_ids, dx, dy, *, now_ms))]
#[allow(clippy::too_many_arguments)]
fn py_prepare_ink_move<'py>(
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    layer_id: &Bound<'py, PyAny>,
    stroke_ids: &Bound<'py, PyAny>,
    dx: &Bound<'py, PyAny>,
    dy: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let snap = bridge::opaque(snapshot);
    let (p, l) = (opt_str(page_id), opt_str(layer_id));
    let ids = lazy_ids(stroke_ids);
    let edit = prepare_ink_move(
        &deps,
        &snap,
        p.as_deref(),
        l.as_deref(),
        &ids.0,
        int_or_float(dx),
        int_or_float(dy),
        exact_int(now_ms),
    );
    edit_to_py(py, ids.finish(py, edit)?)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "expected_stroke_parts")]
fn py_expected_stroke_parts<'py>(snapshot: &Bound<'py, PyAny>, edit: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = snapshot.py();
    let cls = bridge::document_snapshot_class(py)?;
    let snapshot =
        if snapshot.is_instance_of::<PyDict>() { cls.call_method1("from_dict", (snapshot,))? } else { snapshot.clone() };
    if !edit.is_instance_of::<PyInkStrokeEdit>() {
        return Err(bridge::to_py(py, e_ink("stroke edit required")));
    }
    let raw: Vec<u8> = snapshot.getattr("raw_body")?.extract()?;
    let ct: String = snapshot.getattr("content_type")?.extract()?;
    let deps = PyDeps { py };
    let parsed = deps.parse_payload(&raw, &ct).py(py)?;
    let removed: Vec<Option<String>> =
        edit.getattr("removed_part_filenames")?.try_iter()?.map(|n| Ok(opt_str(&n?))).collect::<PyResult<_>>()?;
    let mut added = Vec::new();
    for p in edit.getattr("binary_parts")?.try_iter()? {
        let p = p?;
        added.push((opt_str(&p.getattr("filename")?), p.getattr("body")?.extract::<Vec<u8>>()?));
    }
    let items = expected_stroke_parts(&parsed.parts, &removed, &added).py(py)?;
    PyTuple::new(py, items)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "check_compare_response")]
fn py_check_compare_response(raw: &Bound<'_, PyAny>, baseline_commit: &Bound<'_, PyAny>, edit: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = raw.py();
    if !edit.is_instance_of::<PyInkStrokeEdit>() {
        return Err(bridge::to_py(py, e_ink("stroke edit required")));
    }
    let bytes = match raw.cast::<PyBytes>() {
        Ok(b) => b.as_bytes().to_vec(),
        Err(_) => return Err(bridge::to_py(py, e_ink("compare response bound"))),
    };
    let list = |name: &str| -> PyResult<Vec<Option<String>>> {
        edit.getattr(name)?.try_iter()?.map(|v| Ok(opt_str(&v?))).collect()
    };
    let view = EditView {
        kind: opt_str(&edit.getattr("kind")?).unwrap_or_default(),
        page_id: opt_str(&edit.getattr("page_id")?),
        layer_id: opt_str(&edit.getattr("layer_id")?),
        stroke_ids: list("stroke_ids")?,
        content_file_ids: list("content_file_ids")?,
    };
    check_compare_response(&bytes, opt_str(baseline_commit).as_deref(), &view).py(py)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyInkStrokeEdit>()?;
    m.add_function(wrap_pyfunction!(py_stroke_erasable, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_ink_delete, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_ink_move, m)?)?;
    m.add_function(wrap_pyfunction!(py_expected_stroke_parts, m)?)?;
    m.add_function(wrap_pyfunction!(py_check_compare_response, m)?)?;
    rs_common::export(
        m,
        &["InkStrokeEdit", "stroke_erasable", "prepare_ink_delete", "prepare_ink_move", "expected_stroke_parts", "check_compare_response"],
    )
}

/// ink_editor.MAX_DOCUMENT_PAGES (shared with the recipe>=2 writer).
fn inkcore_pages() -> usize {
    _ink::core::prepare::MAX_DOCUMENT_PAGES
}
