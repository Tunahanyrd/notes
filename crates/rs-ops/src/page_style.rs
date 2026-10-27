//! Replacement for the Python module `page_style`: page background colour.
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
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use std::any::Any;
use std::collections::HashMap;

pub const DEFAULT_PAGE_COLOR: u32 = 0xFFFCFCFC;
pub const PAGE_PALETTE: [[(u32, &str); 6]; 3] = [
    [
        (DEFAULT_PAGE_COLOR, "Renk yok"),
        (0xFFE6E6E6, "Gümüş gri"),
        (0xFFF4E4D7, "Krem"),
        (0xFFF5DDDD, "Pudra pembe"),
        (0xFFCBDADD, "Gri mavi"),
        (0xFF010101, "Kara"),
    ],
    [
        (0xFFFFC69E, "Şeftali"),
        (0xFFFFEC94, "Limon sarısı"),
        (0xFFC8F0E7, "Nane"),
        (0xFFC9E0F9, "Bulut mavisi"),
        (0xFFE5DCF9, "Lavanta"),
        (0xFFFFCACD, "Şeker pembe"),
    ],
    [
        (0xFFEFC9BC, "Somon"),
        (0xFFA7C9CC, "Turkuaz gri"),
        (0xFFB0B2C8, "Gri mor"),
        (0xFF77B68E, "Yaprak yeşili"),
        (0xFF6491AF, "Çelik mavisi"),
        (0xFFCE9192, "Gül kurusu"),
    ],
];

pub struct PageStyle {
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub page_ids: Vec<String>,
    pub tick: i64,
    pub color_argb: u32,
}

fn signed(value: u32) -> i64 {
    value as i32 as i64
}

fn color(value: Option<&str>) -> R<u32> {
    let v = value.unwrap_or("");
    let digits = v.strip_prefix('-').unwrap_or(v);
    let ok = !digits.is_empty() && digits.len() <= 10 && digits.bytes().all(|b| b.is_ascii_digit());
    let n: i64 = if ok { v.parse().unwrap_or(i64::MAX) } else { i64::MAX };
    if !ok || !(-(1i64 << 31) <= n && n < (1i64 << 32)) {
        return doc("Sayfa rengi okunamadı.");
    }
    Ok((n & 0xFFFF_FFFF) as u32)
}

pub(crate) fn sections(root: &Element) -> R<(usize, usize, usize)> {
    Ok((one_idx(root, "attributes")?, one_idx(root, "notetree")?, one_idx(root, "pagelist")?))
}

/// Shared "now must advance" rule.
pub(crate) fn advance(root: &Element, now_ms: Option<i128>) -> R<i64> {
    advance_as(root, now_ms, Kind::Doc, "Kayıt zamanı ilerlemeli.")
}

/// `type(now_ms) is not int or not integer(syncModifiedTime) < now_ms < 2**63`.
pub(crate) fn advance_as(root: &Element, now_ms: Option<i128>, kind: Kind, msg: &str) -> R<i64> {
    let n = match now_ms {
        Some(n) => n,
        None => return Err(Error::new(kind, msg)),
    };
    let sync = integer(root.get("syncModifiedTime"))? as i128;
    if sync < n && n < I63 {
        Ok(n as i64)
    } else {
        Err(Error::new(kind, msg))
    }
}

/// `max(attributes.modifiedTime, page/layer/contentFile modifiedTime) + 1`.
pub(crate) fn next_tick(root: &Element, attrs: &Element) -> R<i128> {
    let mut max = integer(attrs.get("modifiedTime"))?;
    for n in root.iter() {
        if matches!(n.tag.as_str(), "page" | "layer" | "contentFile") && n.get("modifiedTime").is_some_and(|v| !v.is_empty()) {
            max = max.max(integer(n.get("modifiedTime"))?);
        }
    }
    Ok(max as i128 + 1)
}

pub fn prepare_page_background(deps: &dyn Deps, snapshot: &dyn Any, color_argb: Option<i128>, now_ms: Option<i128>) -> R<PageStyle> {
    let snapshot = deps.convert(snapshot, "samsung_notes.page_style")?;
    let snapshot: &dyn Any = &*snapshot;
    let color_argb = match color_argb {
        Some(c) if (0..=0xFFFF_FFFF).contains(&c) && c >> 24 == 0xFF => c as u32,
        _ => return doc("Sayfa rengi opak bir renk olmalı."),
    };
    if let Some(r) = deps.verified(snapshot)? {
        return doc(&r);
    }
    let mut root = xml::parse_bytes(&deps.resource_xml(snapshot)?).map_err(|m| e_parse(&m))?;
    let (ai, ti, pi) = sections(&root)?;
    if root.children[ai].get("formatVersion") != Some("4000") || root.children[ti].get("ver") != Some("4000") {
        return doc("Bu notun biçim sürümü desteklenmiyor.");
    }
    let now_ms = advance(&root, now_ms)?;
    let tpi = one_idx(&root.children[ti], "pagelist")?;
    let pages = &root.children[pi];
    let tree_pages = &root.children[ti].children[tpi];
    let tree_map: HashMap<Option<String>, usize> =
        tree_pages.children.iter().enumerate().map(|(i, p)| (p.get("id").map(str::to_string), i)).collect();
    if pages.is_empty() || ids(&pages.children) != ids(&tree_pages.children) {
        return doc("Sayfa ağacı notla uyuşmuyor.");
    }
    let stored = signed(color_argb).to_string();
    let changed: Vec<usize> =
        pages.children.iter().enumerate().filter(|(_, p)| p.get("bgColor") != Some(stored.as_str())).map(|(i, _)| i).collect();
    if changed.is_empty() {
        return doc("Sayfalar zaten bu renkte.");
    }
    for page in &pages.children {
        let layers = hashes(&one(page, "layerlist")?.children)?;
        let expected = hex(&parent_hash(page.get("id"), integer(page.get("modifiedTime"))?, &layers)?);
        let node = &tree_pages.children[tree_map[&page.get("id").map(str::to_string)]];
        if Some(expected.as_str()) != page.get("hash") || Some(expected.as_str()) != node.get("h") || node.get("t") != page.get("modifiedTime") {
            return doc("Sayfa özeti doğrulanamadı; renk değiştirilmedi.");
        }
    }
    let tick = next_tick(&root, &root.children[ai])?;
    let revision = integer(root.children[ai].get("fileRevision"))? as i128 + 1;
    if tick >= I63 || revision >= 1 << 31 {
        return doc("Not sürümü taşması.");
    }
    let tick = tick as i64;
    let mut changed_ids = Vec::new();
    for &i in &changed {
        let page = &root.children[pi].children[i];
        let layers = hashes(&one(page, "layerlist")?.children)?;
        let digest = hex(&parent_hash(page.get("id"), tick, &layers)?);
        let id = page.get("id").map(str::to_string);
        let page = &mut root.children[pi].children[i];
        page.set("bgColor", &stored);
        page.set("modifiedTime", &tick.to_string());
        page.set("hash", &digest);
        let node = &mut root.children[ti].children[tpi].children[tree_map_index(&tree_map, &id)];
        node.set("t", &tick.to_string());
        node.set("h", &digest);
        changed_ids.push(id);
    }
    let attrs = &mut root.children[ai];
    attrs.set("fileRevision", &revision.to_string());
    attrs.set("modifiedTime", &tick.to_string());
    attrs.set("displayModifiedTime", &now_ms.to_string());
    root.set("syncModifiedTime", &now_ms.to_string());
    let mut upload = upload_copy(&root);
    one_mut(&mut upload, "contentfilelist")?.children.clear();
    let upload_pages = one_mut(&mut upload, "pagelist")?;
    upload_pages.children.retain(|p| changed_ids.contains(&p.get("id").map(str::to_string)));
    for p in upload_pages.children.iter_mut() {
        p.children.retain(|c| c.tag != "layerList");
    }
    Ok(PageStyle {
        full_xml: xml::xml_bytes(&root),
        tree_xml: xml::xml_bytes(&root.children[ti]),
        upload_xml: xml::xml_bytes(&upload),
        page_ids: changed_ids.into_iter().map(|i| i.unwrap_or_default()).collect(),
        tick,
        color_argb,
    })
}

fn tree_map_index(map: &HashMap<Option<String>, usize>, id: &Option<String>) -> usize {
    map[id]
}

pub struct BackgroundState {
    pub background_argb: Option<u32>,
    pub editable: bool,
    pub reason: Option<String>,
}

pub fn page_background_state(deps: &dyn Deps, snapshot: Option<&dyn Any>) -> R<BackgroundState> {
    let snapshot = match snapshot {
        None => {
            return Ok(BackgroundState {
                background_argb: None,
                editable: false,
                reason: Some("Notun sayfa bilgisi henüz hazır değil.".into()),
            })
        }
        Some(s) => s,
    };
    let attempt = (|| -> R<std::collections::BTreeSet<u32>> {
        let converted = deps.convert(snapshot, "samsung_notes.page_style")?;
        let handle: &dyn Any = &*converted;
        let root = xml::parse_bytes(&deps.resource_xml(handle)?).map_err(|m| e_parse(&m))?;
        let default = signed(DEFAULT_PAGE_COLOR).to_string();
        let mut colors = std::collections::BTreeSet::new();
        for p in &one(&root, "pagelist")?.children {
            colors.insert(color(Some(p.get("bgColor").unwrap_or(&default)))?);
        }
        let probe = if colors.len() == 1 && colors.contains(&DEFAULT_PAGE_COLOR) { 0xFFF5DDDD } else { DEFAULT_PAGE_COLOR };
        prepare_page_background(deps, handle, Some(probe as i128), Some(1 << 62))?;
        Ok(colors)
    })();
    match attempt {
        Ok(colors) => Ok(BackgroundState {
            background_argb: if colors.len() == 1 { colors.into_iter().next() } else { None },
            editable: true,
            reason: None,
        }),
        Err(e) if e.any(&[Kind::Doc, Kind::Value, Kind::Type, Kind::Parse]) => Ok(BackgroundState {
            background_argb: None,
            editable: false,
            reason: Some(if e.is(Kind::Doc) { e.msg } else { "Sayfa bilgisi okunamadı.".into() }),
        }),
        Err(e) => Err(e),
    }
}

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
crate::record_class!(PyPageStyleEdit, "PageStyleEdit", "samsung_notes.page_style",
    [full_xml, tree_xml, upload_xml, page_ids, tick, color_argb]);

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "prepare_page_background", signature = (snapshot, color_argb, *, now_ms))]
fn py_prepare_page_background<'py>(
    snapshot: &Bound<'py, PyAny>,
    color_argb: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let e = prepare_page_background(
        &deps,
        &bridge::opaque(snapshot),
        crate::ink_erase::exact_int(color_argb),
        crate::ink_erase::exact_int(now_ms),
    )
    .py(py)?;
    let obj = PyPageStyleEdit {
        full_xml: PyBytes::new(py, &e.full_xml).into_any().unbind(),
        tree_xml: PyBytes::new(py, &e.tree_xml).into_any().unbind(),
        upload_xml: PyBytes::new(py, &e.upload_xml).into_any().unbind(),
        page_ids: PyTuple::new(py, &e.page_ids)?.into_any().unbind(),
        tick: e.tick.into_pyobject(py)?.into_any().unbind(),
        color_argb: e.color_argb.into_pyobject(py)?.into_any().unbind(),
    };
    Ok(Bound::new(py, obj)?.into_any())
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "page_background_state")]
fn py_page_background_state<'py>(snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = snapshot.py();
    let deps = PyDeps { py };
    let handle = bridge::opaque(snapshot);
    let arg: Option<&dyn Any> = if snapshot.is_none() { None } else { Some(&handle) };
    let s = page_background_state(&deps, arg).py(py)?;
    let d = PyDict::new(py);
    d.set_item("backgroundArgb", s.background_argb)?;
    d.set_item("editable", s.editable)?;
    d.set_item("reason", &s.reason)?;
    if s.editable {
        let rows = PyList::empty(py);
        for row in PAGE_PALETTE.iter() {
            let r = PyList::empty(py);
            for (c, n) in row {
                let item = PyDict::new(py);
                item.set_item("argb", *c)?;
                item.set_item("name", *n)?;
                r.append(item)?;
            }
            rows.append(r)?;
        }
        d.set_item("palette", rows)?;
    }
    Ok(d)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPageStyleEdit>()?;
    m.add_function(wrap_pyfunction!(py_prepare_page_background, m)?)?;
    m.add_function(wrap_pyfunction!(py_page_background_state, m)?)?;
    rs_common::export(m, &["PageStyleEdit", "prepare_page_background", "page_background_state"])
}
