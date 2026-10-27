//! Replacement for the Python module `page_ops`: page sorter operations
//! (add a blank page, delete pages, reorder pages), offline only.
use crate::bridge;
#[cfg(feature = "python")]
use crate::bridge::PyDeps;
use crate::bridge::RPy;
use crate::deps::*;
use crate::err::*;
use crate::page_style::advance_as;
use crate::util::*;
use crate::xml::{self, Element};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use std::any::Any;
use std::collections::HashSet;

pub const BLANK_PAGE_CHILDREN: [&str; 3] = ["layerList", "drawnRect", "canvasCacheDataMap"];
pub const MAX_PAGES: usize = 1000;
const STRUCTURE: &str = "Belgenin sayfa yapısı güvenli düzenleme için uygun değil.";
const GROUPED: [&str; 3] = ["strokeGroup", "voiceDataList", "attachedFileMap"];

pub struct PageOps {
    pub kind: &'static str,
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub requested_ids: Vec<String>,
    pub page_ids: Vec<String>,
    pub layer_ids: Vec<String>,
    pub content_file_ids: Vec<String>,
    pub page_order: Vec<String>,
    pub tick: i64,
}

fn one_po(parent: &Element, name: &str) -> R<usize> {
    one_idx(parent, &name.to_lowercase()).remap(&[Kind::Doc], Kind::PageOps, STRUCTURE)
}

fn canonical(v: Option<&str>) -> R<String> {
    match v {
        Some(s) if canonical_uuid(s) => Ok(s.into()),
        _ => pageops("Geçersiz sayfa kimliği."),
    }
}

/// Validated document state shared by the page operations (public for rs-store).
pub struct Loaded {
    pub root: Element,
    pub ai: usize,
    pub ti: usize,
    pub pi: usize,
    pub tick: i64,
}

impl Loaded {
    pub fn pages(&self) -> &Vec<Element> {
        &self.root.children[self.pi].children
    }
}

/// `page_ops._load(snapshot, now_ms)`.
pub fn load(deps: &dyn Deps, snapshot: &dyn Any, now_ms: Option<i128>) -> R<Loaded> {
    let snap = match deps.snapshot(snapshot)? {
        Some(s) if s.commit_id.as_deref().is_some_and(|c| !c.is_empty()) => s,
        _ => return pageops("Kayıtlı not sürümü gerekli."),
    };
    let handle: &dyn Any = match snap.handle.as_deref() {
        Some(h) => h,
        None => snapshot,
    };
    match deps.verified(handle) {
        Ok(Some(r)) => return pageops(&r),
        Ok(None) => {}
        Err(e) if e.is(Kind::Value) => return Err(Error::new(Kind::PageOps, e.msg)),
        Err(e) => return Err(e),
    }
    let root = xml::parse_bytes(&snap.resource_xml).map_err(|m| e_parse(&m))?;
    let known = ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];
    if root.children.iter().any(|n| !known.contains(&n.tag.as_str())) {
        return pageops("Bilinmeyen not bölümü.");
    }
    let ai = one_po(&root, "attributes")?;
    let ti = one_po(&root, "noteTree")?;
    let pi = one_po(&root, "pageList")?;
    let (attrs, tree, pages) = (&root.children[ai], &root.children[ti], &root.children[pi]);
    if attrs.get("formatVersion") != Some("4000") || tree.get("ver") != Some("4000") {
        return pageops("Bu notun biçim sürümü desteklenmiyor.");
    }
    advance_as(&root, now_ms, Kind::PageOps, "Kayıt zamanı ilerlemeli.")?;
    let tpi = one_po(tree, "pageList")?;
    let tree_list = &tree.children[tpi];
    if !(1..=MAX_PAGES).contains(&pages.len()) || pages.children.iter().chain(tree_list.children.iter()).any(|p| p.tag != "page") {
        return pageops("Sayfa listesi desteklenmiyor.");
    }
    let page_ids: Vec<String> = pages.children.iter().map(|p| canonical(p.get("id"))).collect::<R<_>>()?;
    let tree_ids = ids(&tree_list.children);
    if page_ids.iter().map(|s| Some(s.as_str())).collect::<Vec<_>>() != tree_ids
        || page_ids.iter().collect::<HashSet<_>>().len() != page_ids.len()
    {
        return pageops("Sayfa ağacı notla uyuşmuyor.");
    }
    for page in &pages.children {
        let node = tree_list.children.iter().rev().find(|n| n.get("id") == page.get("id")).unwrap();
        let layers = &page.children[one_po(page, "layerList")?].children;
        let tlayers = &node.children[one_po(node, "layerList")?].children;
        if ids(layers) != ids(tlayers) {
            return pageops("Katman ağacı uyuşmuyor.");
        }
        for (layer, tl) in layers.iter().zip(tlayers.iter()) {
            let objs: &[Element] = layer.find("objectList").map(|o| o.children.as_slice()).unwrap_or(&[]);
            let tobjs: &[Element] = tl.find("objectList").map(|o| o.children.as_slice()).unwrap_or(&[]);
            let left: Vec<_> = objs.iter().map(|o| (o.get("id"), o.get("modifiedTime"), o.get("hash"))).collect();
            let right: Vec<_> = tobjs.iter().map(|o| (o.get("id"), o.get("t"), o.get("h"))).collect();
            if left != right {
                return pageops("Nesne ağacı uyuşmuyor.");
            }
            let t = integer(layer.get("modifiedTime"))?;
            let digest = hex(&parent_hash(layer.get("id"), t, &hashes(objs)?)?);
            if Some(digest.as_str()) != layer.get("hash") || Some(digest.as_str()) != tl.get("h") || tl.get("t") != layer.get("modifiedTime") {
                return pageops("Katman özeti doğrulanamadı.");
            }
        }
        let t = integer(page.get("modifiedTime"))?;
        let digest = hex(&parent_hash(page.get("id"), t, &hashes(layers)?)?);
        if Some(digest.as_str()) != page.get("hash") || Some(digest.as_str()) != node.get("h") || node.get("t") != page.get("modifiedTime") {
            return pageops("Sayfa özeti doğrulanamadı.");
        }
    }
    let mut max = i64::MIN;
    for n in root.iter() {
        if n.has("modifiedTime") {
            max = max.max(integer(n.get("modifiedTime"))?);
        }
    }
    for n in tree.iter() {
        if n.has("t") {
            max = max.max(integer(n.get("t"))?);
        }
    }
    if max == i64::MIN {
        return value("max() iterable argument is empty");
    }
    let tick = max as i128 + 1;
    if tick >= I63 || integer(attrs.get("fileRevision"))? as i128 + 1 >= 1 << 31 {
        return pageops("Not sürümü taşması.");
    }
    let _ = tpi;
    Ok(Loaded { root, ai, ti, pi, tick: tick as i64 })
}

/// `sum(floor(h·W/w)) + pageVerticalPadding·(n−1)`; None if inputs are absent.
pub fn note_height(attrs: &Element, pages: &[&Element]) -> Option<i128> {
    let width = integer(attrs.get("width")).ok()? as i128;
    let pad = integer(Some(attrs.get_or("pageVerticalPadding", "0"))).ok()? as i128;
    let mut sizes = Vec::new();
    for p in pages {
        sizes.push((integer(p.get("width")).ok()? as i128, integer(p.get("height")).ok()? as i128));
    }
    if width == 0 || sizes.iter().any(|(w, _)| *w == 0) {
        return None;
    }
    Some(sizes.iter().map(|(w, h)| h * width / w).sum::<i128>() + pad * (sizes.len() as i128 - 1))
}

/// Integer-typed argument: None = not an exact int.
pub type IntArg = Option<i128>;

/// Build one blank page and its noteTree entry (inserts nothing).
#[allow(clippy::too_many_arguments)]
pub fn build_blank_page(
    root: &Element,
    width: IntArg,
    height: IntArg,
    tick: IntArg,
    page_id: Option<Option<&str>>,
    layer_id: Option<Option<&str>>,
    bg_color: Option<Option<&str>>,
) -> R<(Element, Element)> {
    let page_id = match page_id {
        None => uuid4(),
        Some(p) => canonical(p)?,
    };
    let layer_id = match layer_id {
        None => uuid4(),
        Some(l) => canonical(l)?,
    };
    let existing: HashSet<Option<&str>> = root.iter().map(|n| n.get("id")).collect();
    if page_id == layer_id || existing.contains(&Some(page_id.as_str())) || existing.contains(&Some(layer_id.as_str())) {
        return pageops("Sayfa kimliği zaten var.");
    }
    let (width, height) = match (width, height) {
        (Some(w), Some(h)) if (1..=32768).contains(&w) && (1..=32768).contains(&h) => (w as i64, h as i64),
        _ => return pageops("Sayfa boyutu geçersiz."),
    };
    let tick = match tick {
        Some(t) if 0 < t && t < I63 => t as i64,
        _ => return pageops("Sayfa zamanı geçersiz."),
    };
    let bg: Option<String> = match bg_color {
        Some(Some(c)) => Some(c.to_string()),
        Some(None) => Some("\0".into()),
        None => root.find("pageList").and_then(|p| p.children.iter().find_map(|c| c.get("bgColor"))).map(str::to_string),
    };
    if let Some(b) = &bg {
        let canonical_int = b.parse::<i64>().ok().filter(|v| v.to_string() == *b && -(1i64 << 31) <= *v && *v < (1i64 << 31));
        if canonical_int.is_none() {
            return pageops("Sayfa rengi geçersiz.");
        }
    }
    let layer_raw = parent_hash(Some(&layer_id), tick, &[])?;
    let layer_hash = hex(&layer_raw);
    let page_hash = hex(&parent_hash(Some(&page_id), tick, &[layer_raw])?);
    let (w, h, t) = (width.to_string(), height.to_string(), tick.to_string());
    let mut attrs: Vec<(&str, &str)> = vec![("id", &page_id), ("hash", &page_hash)];
    if let Some(b) = &bg {
        attrs.push(("bgColor", b));
    }
    attrs.extend([
        ("bgWidth", w.as_str()),
        ("createdTime", "0"),
        ("currentLayerIndex", "0"),
        ("formatVersion", "4000"),
        ("height", h.as_str()),
        ("isTextOnly", "false"),
        ("minFormatVersion", "4000"),
        ("modifiedTime", t.as_str()),
        ("offsetX", "0"),
        ("offsetY", "0"),
        ("orientation", "0"),
        ("width", w.as_str()),
    ]);
    let mut page = Element::new("page", &attrs);
    page.sub_element("layerList", &[])
        .sub_element(
            "layer",
            &[
                ("id", &layer_id),
                ("hash", &layer_hash),
                ("createdTime", "0"),
                ("flagEventForwardable", "true"),
                ("layerNumber", "0"),
                ("lockState", "false"),
                ("modifiedTime", &t),
                ("thumbnail", "-1"),
                ("visible", "true"),
            ],
        )
        .sub_element("objectList", &[]);
    let mut node = Element::new("page", &[("id", &page_id), ("t", &t), ("h", &page_hash)]);
    node.sub_element("layerList", &[]).sub_element("layer", &[("id", &layer_id), ("t", &t), ("h", &layer_hash)]);
    Ok((page, node))
}

fn last_page(attrs: &Element, pages: &[Element]) -> Option<String> {
    let index = integer(attrs.get("lastEditedPageIndex")).ok()? as usize;
    pages.get(index).and_then(|p| p.get("id")).map(str::to_string).or(None)
}

struct FinishArgs {
    requested: Vec<String>,
    page_ids: Vec<String>,
    layer_ids: Vec<String>,
    files: Vec<String>,
    upload_pages: Vec<String>,
    last_page_id: Option<String>,
}

fn finish(kind: &'static str, mut l: Loaded, now_ms: i64, a: FinishArgs) -> R<PageOps> {
    let tick = l.tick;
    let revision = integer(l.root.children[l.ai].get("fileRevision"))? + 1;
    let height = {
        let pages: Vec<&Element> = l.pages().iter().collect();
        note_height(&l.root.children[l.ai], &pages)
    };
    let order: Vec<String> = l.pages().iter().map(|p| p.get("id").unwrap_or("").to_string()).collect();
    let attrs = &mut l.root.children[l.ai];
    if let Some(h) = height {
        attrs.set("height", &h.to_string());
    }
    if let (Some(last), true) = (&a.last_page_id, attrs.has("lastEditedPageIndex")) {
        let index = match order.iter().position(|o| o == last) {
            Some(i) => i as i128,
            None => (integer(attrs.get("lastEditedPageIndex"))? as i128).min(order.len() as i128 - 1),
        };
        attrs.set("lastEditedPageIndex", &index.to_string());
    }
    attrs.set("fileRevision", &revision.to_string());
    attrs.set("modifiedTime", &tick.to_string());
    attrs.set("displayModifiedTime", &now_ms.to_string());
    l.root.set("syncModifiedTime", &now_ms.to_string());
    let mut upload = upload_copy(&l.root);
    if let Some(f) = upload.find_mut("contentFileList") {
        f.children.retain(|n| n.get("id").is_some_and(|i| a.files.iter().any(|x| x == i)));
    }
    match upload.find_mut("pageList") {
        Some(p) => p.children.retain(|n| n.get("id").is_some_and(|i| a.upload_pages.iter().any(|x| x == i))),
        None => return type_err("'NoneType' object is not iterable"),
    }
    Ok(PageOps {
        kind,
        full_xml: xml::xml_bytes(&l.root),
        tree_xml: xml::xml_bytes(&l.root.children[l.ti]),
        upload_xml: xml::xml_bytes(&upload),
        requested_ids: a.requested,
        page_ids: a.page_ids,
        layer_ids: a.layer_ids,
        content_file_ids: a.files,
        page_order: order,
        tick,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_add_page(
    deps: &dyn Deps,
    snapshot: &dyn Any,
    after_index: IntArg,
    now_ms: IntArg,
    page_id: Option<Option<&str>>,
    layer_id: Option<Option<&str>>,
    width: Option<IntArg>,
    height: Option<IntArg>,
) -> R<PageOps> {
    let mut l = load(deps, snapshot, now_ms)?;
    let count = l.pages().len() as i128;
    let after = match after_index {
        Some(a) if -1 <= a && a < count => a,
        _ => return pageops("Sayfa konumu geçersiz."),
    };
    if l.pages().len() >= MAX_PAGES {
        return pageops("Sayfa sınırı.");
    }
    let reference = &l.pages()[after.max(0) as usize];
    if l.root.children[l.ai].get_or("orientation", "0") != "0" || reference.get_or("orientation", "0") != "0" {
        return pageops("Yatay sayfa yönü desteklenmiyor.");
    }
    let width = match width {
        None => Some(integer(reference.get("width"))? as i128),
        Some(w) => w,
    };
    let height = match height {
        None => Some(integer(reference.get("height"))? as i128),
        Some(h) => h,
    };
    let bg = reference.get("bgColor").map(str::to_string);
    let last = last_page(&l.root.children[l.ai], l.pages());
    let (page, node) =
        build_blank_page(&l.root, width, height, Some(l.tick as i128), page_id, layer_id, bg.as_deref().map(Some))?;
    let new_page = page.get("id").unwrap().to_string();
    let new_layer = page.find_path("layerList/layer").and_then(|x| x.get("id")).unwrap().to_string();
    let at = (after + 1) as usize;
    l.root.children[l.pi].children.insert(at, page);
    let (ti, tpi) = (l.ti, one_po(&l.root.children[l.ti], "pageList")?);
    l.root.children[ti].children[tpi].children.insert(at, node);
    let now = now_ms.unwrap() as i64;
    finish(
        "add_page",
        l,
        now,
        FinishArgs {
            requested: vec![new_page.clone(), new_layer.clone()],
            page_ids: vec![new_page.clone()],
            layer_ids: vec![new_layer],
            files: vec![],
            upload_pages: vec![new_page],
            last_page_id: last,
        },
    )
}

fn page_has_media(page: &Element) -> bool {
    page.children.iter().any(|n| !BLANK_PAGE_CHILDREN.contains(&n.tag.as_str()))
        || page.get_or("bgImageId", "-1") != "-1"
        || !page.get_or("templateUri", "").is_empty()
}

/// `page_ids`: None = a single str id given; otherwise the iterable's items.
pub fn prepare_delete_page(deps: &dyn Deps, snapshot: &dyn Any, targets: Vec<Option<String>>, now_ms: IntArg) -> R<PageOps> {
    let mut l = load(deps, snapshot, now_ms)?;
    let targets: Vec<String> = targets.iter().map(|t| canonical(t.as_deref())).collect::<R<_>>()?;
    let find = |l: &Loaded, id: &str| l.pages().iter().rposition(|p| p.get("id") == Some(id));
    if targets.is_empty() || targets.iter().collect::<HashSet<_>>().len() != targets.len() || targets.iter().any(|t| find(&l, t).is_none()) {
        return pageops("Silinecek sayfa bulunamadı.");
    }
    if targets.len() >= l.pages().len() {
        return pageops("Notta en az bir sayfa kalmalı.");
    }
    for identity in &targets {
        let page = &l.pages()[find(&l, identity).unwrap()];
        if page_has_media(page) {
            return pageops("PDF, resim veya ekli dosya içeren sayfa silinemiyor.");
        }
        for layer in &page.children[one_po(page, "layerList")?].children {
            let objs: &[Element] = layer.find("objectList").map(|o| o.children.as_slice()).unwrap_or(&[]);
            if layer.children.iter().any(|n| n.tag != "objectList") || objs.iter().any(|o| o.get("type") != Some("stroke") || !o.is_empty()) {
                return pageops("Sayfada desteklenmeyen nesne var.");
            }
        }
    }
    if l.root.iter().any(|n| GROUPED.contains(&n.tag.as_str())) {
        return pageops("Gruplanmış veya ses içeren notlar desteklenmiyor.");
    }
    let last = last_page(&l.root.children[l.ai], l.pages());
    let mut released = Vec::new();
    for identity in &targets {
        let page = &l.pages()[find(&l, identity).unwrap()];
        if page.find("canvasCacheDataMap").is_some() {
            match deps.release_page_cache(&mut l.root, identity, l.tick) {
                Ok(v) => released.extend(v),
                Err(e) if e.is(Kind::Ink) => return Err(Error::new(Kind::PageOps, e.msg)),
                Err(e) => return Err(e),
            }
        }
    }
    let tpi = one_po(&l.root.children[l.ti], "pageList")?;
    for identity in &targets {
        let i = find(&l, identity).unwrap();
        l.root.children[l.pi].children.remove(i);
        let tree_list = &mut l.root.children[l.ti].children[tpi].children;
        if let Some(j) = tree_list.iter().rposition(|n| n.get("id") == Some(identity.as_str())) {
            tree_list.remove(j);
        }
    }
    let mut files = released.clone();
    files.sort();
    let now = now_ms.unwrap() as i64;
    finish(
        "delete_pages",
        l,
        now,
        FinishArgs { requested: released, page_ids: targets, layer_ids: vec![], files, upload_pages: vec![], last_page_id: last },
    )
}

pub fn prepare_reorder_pages(deps: &dyn Deps, snapshot: &dyn Any, order: Vec<Option<String>>, now_ms: IntArg) -> R<PageOps> {
    let mut l = load(deps, snapshot, now_ms)?;
    let order: Vec<String> = order.iter().map(|p| canonical(p.as_deref())).collect::<R<_>>()?;
    let current: Vec<String> = l.pages().iter().map(|p| p.get("id").unwrap().to_string()).collect();
    let (mut a, mut b) = (order.clone(), current.clone());
    a.sort();
    b.sort();
    if a != b || order.iter().collect::<HashSet<_>>().len() != order.len() {
        return pageops("Sayfa sırası notla uyuşmuyor.");
    }
    if order == current {
        return pageops("Sayfa sırası zaten bu.");
    }
    let last = last_page(&l.root.children[l.ai], l.pages());
    let tpi = one_po(&l.root.children[l.ti], "pageList")?;
    let pages = std::mem::take(&mut l.root.children[l.pi].children);
    let tree_list = std::mem::take(&mut l.root.children[l.ti].children[tpi].children);
    let pick = |list: &[Element], id: &str| list.iter().rev().find(|p| p.get("id") == Some(id)).cloned().unwrap();
    l.root.children[l.pi].children = order.iter().map(|id| pick(&pages, id)).collect();
    l.root.children[l.ti].children[tpi].children = order.iter().map(|id| pick(&tree_list, id)).collect();
    let now = now_ms.unwrap() as i64;
    finish(
        "reorder_pages",
        l,
        now,
        FinishArgs { requested: vec![], page_ids: order, layer_ids: vec![], files: vec![], upload_pages: vec![], last_page_id: last },
    )
}

pub struct PageRow {
    pub id: Option<String>,
    pub index: usize,
    pub width: i64,
    pub height: i64,
    pub has_strokes: bool,
    pub deletable: bool,
    pub reason: Option<String>,
}

pub struct OpsState {
    pub editable: bool,
    pub reason: Option<String>,
    pub pages: Vec<PageRow>,
    pub can_reorder: bool,
    pub can_add: bool,
}

pub fn page_ops_state(deps: &dyn Deps, snapshot: Option<&dyn Any>) -> R<OpsState> {
    let loaded = match snapshot {
        None => Err(e_pageops("Notun sayfa bilgisi henüz hazır değil.")),
        Some(s) => load(deps, s, Some(1 << 62)),
    };
    let l = match loaded {
        Ok(l) => l,
        Err(e) if e.any(&[Kind::Doc, Kind::Value, Kind::Type, Kind::Parse]) => {
            return Ok(OpsState {
                editable: false,
                reason: Some(if e.is(Kind::Doc) { e.msg } else { "Sayfa bilgisi okunamadı.".into() }),
                pages: vec![],
                can_reorder: false,
                can_add: false,
            })
        }
        Err(e) => return Err(e),
    };
    let media = l.root.iter().any(|n| GROUPED.contains(&n.tag.as_str()));
    let pages = l.pages();
    let pdf_note = pages.iter().any(|p| p.find("pdfDataList").is_some());
    let mut rows = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let objects: Vec<&Element> = page.iter_tag("object").collect();
        let layer_list = page.find("layerList").ok_or_else(|| Error::new(Kind::Type, "'NoneType' object is not iterable"))?;
        let layer_ok = layer_list.children.iter().all(|layer| layer.children.iter().all(|n| n.tag == "objectList"));
        let reason = if media {
            Some("Gruplanmış veya ses içeren notlarda sayfa silinemiyor.")
        } else if page_has_media(page) {
            Some("PDF veya resim içeren sayfa henüz silinemiyor.")
        } else if !layer_ok || objects.iter().any(|o| o.get("type") != Some("stroke") || !o.is_empty()) {
            Some("Sayfada desteklenmeyen nesne var.")
        } else if page.find("canvasCacheDataMap").is_some() && pdf_note {
            Some("PDF içeren notta önbellekli sayfa henüz silinemiyor.")
        } else {
            None
        };
        let many = pages.len() > 1;
        rows.push(PageRow {
            id: page.get("id").map(str::to_string),
            index,
            width: integer(page.get("width"))?,
            height: integer(page.get("height"))?,
            has_strokes: !objects.is_empty(),
            deletable: reason.is_none() && many,
            reason: match reason {
                Some(r) => Some(r.into()),
                None if many => None,
                None => Some("Notta en az bir sayfa kalmalı.".into()),
            },
        });
    }
    Ok(OpsState {
        editable: true,
        reason: None,
        can_reorder: pages.len() > 1,
        can_add: pages.len() < MAX_PAGES && l.root.children[l.ai].get_or("orientation", "0") == "0",
        pages: rows,
    })
}

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
crate::record_class!(PyPageOpsEdit, "PageOpsEdit", "samsung_notes.page_ops",
    [kind, full_xml, tree_xml, upload_xml, requested_ids, page_ids, layer_ids, content_file_ids, page_order, tick]);

#[cfg(feature = "python")]
fn edit_to_py(py: Python<'_>, e: PageOps) -> PyResult<Bound<'_, PyAny>> {
    let obj = PyPageOpsEdit {
        kind: e.kind.into_pyobject(py)?.into_any().unbind(),
        full_xml: PyBytes::new(py, &e.full_xml).into_any().unbind(),
        tree_xml: PyBytes::new(py, &e.tree_xml).into_any().unbind(),
        upload_xml: PyBytes::new(py, &e.upload_xml).into_any().unbind(),
        requested_ids: bridge::frozenset_str(py, &e.requested_ids)?.unbind(),
        page_ids: PyTuple::new(py, &e.page_ids)?.into_any().unbind(),
        layer_ids: PyTuple::new(py, &e.layer_ids)?.into_any().unbind(),
        content_file_ids: PyTuple::new(py, &e.content_file_ids)?.into_any().unbind(),
        page_order: PyTuple::new(py, &e.page_order)?.into_any().unbind(),
        tick: e.tick.into_pyobject(py)?.into_any().unbind(),
    };
    Ok(Bound::new(py, obj)?.into_any())
}

#[cfg(feature = "python")]
use crate::ink_erase::exact_int;
#[cfg(feature = "python")]
use crate::ink_erase::opt_str;

#[cfg(feature = "python")]
fn opt_id(v: &Option<Bound<'_, PyAny>>) -> Option<Option<String>> {
    match v {
        Some(o) if !o.is_none() => Some(opt_str(o)),
        _ => None,
    }
}

#[pyfunction]
#[pyo3(name = "prepare_add_page",
       signature = (snapshot, after_index, *, now_ms, page_id=None, layer_id=None, width=None, height=None))]
#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn py_prepare_add_page<'py>(
    snapshot: &Bound<'py, PyAny>,
    after_index: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    page_id: Option<Bound<'py, PyAny>>,
    layer_id: Option<Bound<'py, PyAny>>,
    width: Option<Bound<'py, PyAny>>,
    height: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let (p, l) = (opt_id(&page_id), opt_id(&layer_id));
    let int_arg = |v: &Option<Bound<'py, PyAny>>| v.as_ref().filter(|o| !o.is_none()).map(exact_int);
    let e = prepare_add_page(
        &PyDeps { py },
        &bridge::opaque(snapshot),
        exact_int(after_index),
        exact_int(now_ms),
        p.as_ref().map(|x| x.as_deref()),
        l.as_ref().map(|x| x.as_deref()),
        int_arg(&width),
        int_arg(&height),
    )
    .py(py)?;
    edit_to_py(py, e)
}

#[cfg(feature = "python")]
fn id_list(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Option<String>>> {
    if obj.is_instance_of::<pyo3::types::PyString>() {
        return Ok(vec![opt_str(obj)]);
    }
    obj.try_iter()?.map(|v| Ok(opt_str(&v?))).collect()
}

#[cfg(feature = "python")]
/// Defer a TypeError from iterating `page_ids` until after `_load`, like Python.
fn deferred<'py, T>(
    py: Python<'py>,
    snapshot: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    ids: &Bound<'py, PyAny>,
    run: impl FnOnce(Vec<Option<String>>) -> R<T>,
) -> PyResult<T> {
    match id_list(ids) {
        Ok(v) => run(v).py(py),
        Err(err) => {
            load(&PyDeps { py }, &bridge::opaque(snapshot), exact_int(now_ms)).py(py)?;
            Err(err)
        }
    }
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "prepare_delete_page", signature = (snapshot, page_ids, *, now_ms))]
fn py_prepare_delete_page<'py>(
    snapshot: &Bound<'py, PyAny>,
    page_ids: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let snap = bridge::opaque(snapshot);
    let e = deferred(py, snapshot, now_ms, page_ids, |ids| prepare_delete_page(&PyDeps { py }, &snap, ids, exact_int(now_ms)))?;
    edit_to_py(py, e)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "prepare_reorder_pages", signature = (snapshot, page_ids_in_order, *, now_ms))]
fn py_prepare_reorder_pages<'py>(
    snapshot: &Bound<'py, PyAny>,
    page_ids_in_order: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let snap = bridge::opaque(snapshot);
    let ids = |o: &Bound<'py, PyAny>| -> PyResult<Vec<Option<String>>> { o.try_iter()?.map(|v| Ok(opt_str(&v?))).collect() };
    let e = match ids(page_ids_in_order) {
        Ok(v) => prepare_reorder_pages(&PyDeps { py }, &snap, v, exact_int(now_ms)).py(py)?,
        Err(err) => {
            load(&PyDeps { py }, &snap, exact_int(now_ms)).py(py)?;
            return Err(err);
        }
    };
    edit_to_py(py, e)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "page_ops_state")]
fn py_page_ops_state<'py>(snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = snapshot.py();
    let handle = bridge::opaque(snapshot);
    let arg: Option<&dyn Any> = if snapshot.is_none() { None } else { Some(&handle) };
    let s = page_ops_state(&PyDeps { py }, arg).py(py)?;
    let d = PyDict::new(py);
    d.set_item("editable", s.editable)?;
    d.set_item("reason", &s.reason)?;
    let pages = PyList::empty(py);
    for r in &s.pages {
        let p = PyDict::new(py);
        p.set_item("id", &r.id)?;
        p.set_item("index", r.index)?;
        p.set_item("width", r.width)?;
        p.set_item("height", r.height)?;
        p.set_item("hasStrokes", r.has_strokes)?;
        p.set_item("deletable", r.deletable)?;
        p.set_item("reason", &r.reason)?;
        pages.append(p)?;
    }
    d.set_item("pages", pages)?;
    if s.editable {
        d.set_item("canReorder", s.can_reorder)?;
        d.set_item("canAdd", s.can_add)?;
    }
    Ok(d)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "note_height")]
fn py_note_height(attrs: &Bound<'_, PyAny>, pages: &Bound<'_, PyAny>) -> PyResult<Option<i128>> {
    let a = bridge::from_et(attrs)?;
    let ps: Vec<Element> = pages.try_iter()?.map(|p| bridge::from_et(&p?)).collect::<PyResult<_>>()?;
    Ok(note_height(&a, &ps.iter().collect::<Vec<_>>()))
}

#[pyfunction]
#[pyo3(name = "build_blank_page",
       signature = (snapshot_root, *, width, height, tick, page_id=None, layer_id=None, bg_color=None))]
#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn py_build_blank_page<'py>(
    snapshot_root: &Bound<'py, PyAny>,
    width: &Bound<'py, PyAny>,
    height: &Bound<'py, PyAny>,
    tick: &Bound<'py, PyAny>,
    page_id: Option<Bound<'py, PyAny>>,
    layer_id: Option<Bound<'py, PyAny>>,
    bg_color: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyTuple>> {
    let py = snapshot_root.py();
    let root = bridge::from_et(snapshot_root)?;
    let (p, l, bg) = (opt_id(&page_id), opt_id(&layer_id), opt_id(&bg_color));
    let (page, node) = build_blank_page(
        &root,
        exact_int(width),
        exact_int(height),
        exact_int(tick),
        p.as_ref().map(|x| x.as_deref()),
        l.as_ref().map(|x| x.as_deref()),
        bg.as_ref().map(|x| x.as_deref()),
    )
    .py(py)?;
    PyTuple::new(py, [bridge::to_et(py, &page)?, bridge::to_et(py, &node)?])
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("PageOpsError", bridge::page_ops_error(m.py())?)?;
    m.add_class::<PyPageOpsEdit>()?;
    m.add_function(wrap_pyfunction!(py_prepare_add_page, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_delete_page, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_reorder_pages, m)?)?;
    m.add_function(wrap_pyfunction!(py_page_ops_state, m)?)?;
    m.add_function(wrap_pyfunction!(py_note_height, m)?)?;
    m.add_function(wrap_pyfunction!(py_build_blank_page, m)?)?;
    rs_common::export(
        m,
        &[
            "PageOpsError",
            "PageOpsEdit",
            "prepare_add_page",
            "prepare_delete_page",
            "prepare_reorder_pages",
            "page_ops_state",
            "note_height",
            "build_blank_page",
        ],
    )
}
