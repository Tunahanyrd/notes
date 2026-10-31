//! Authenticated per-page stroke extraction for renderer adapters.
use crate::note_container::{archive, page_strokes, read_entry, verified_manifest};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageReadError {
    Unsupported(String),
    Corrupt(String),
}
impl std::fmt::Display for PageReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(s) | Self::Corrupt(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for PageReadError {}

pub struct PageReadExpectation<'a> {
    pub page_id: &'a str,
    pub page_sha256: &'a str,
    pub xml_sha256: &'a str,
    pub context_sha256: &'a str,
    pub raw_sha256: &'a str,
    pub manifest_sha256: &'a str,
    pub local_id: &'a str,
    pub commit_id: &'a str,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageStroke {
    pub id: String,
    pub filename: String,
    /// Original complete Cloud part body including its twelve-byte wrapper.
    pub body: Vec<u8>,
}
#[derive(Debug, Clone)]
pub struct PageStrokeMaterial {
    pub page: crate::note_container::PageData,
    pub strokes: Vec<PageStroke>,
}

/// URL-free manifest facts required to extract a single page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageReadIndex {
    boundary: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    noncanonical: bool,
    part_order: Option<Vec<String>>,
    prefix_exceptions: BTreeMap<String, Vec<u8>>,
    suffix_exceptions: BTreeMap<String, String>,
}

/// Capture facts after authenticating the manifest against SQLite.
pub fn capture_page_index<R: Read + Seek>(reader: R, manifest_sha256: &str) -> Result<PageReadIndex, String> {
    let mut zip = archive(reader)?;
    let m = verified_manifest(&mut zip, manifest_sha256)?;
    index_from_manifest(&mut zip, m)
}

// Noncanonical MIME framing still has a verified, lossless framing entry.
// Parse only its headers using one-byte placeholder bodies; the renderer needs
// the original wrapper/order/filename, not the entire multipart snapshot.
fn index_from_manifest<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, m: crate::note_container::Manifest) -> Result<PageReadIndex, String> {
    if m.boundary.is_some() {
        return Ok(PageReadIndex { boundary: true, noncanonical: false, part_order: m.part_order,
            prefix_exceptions: m.prefix_exceptions, suffix_exceptions: m.suffix_exceptions });
    }
    let fallback = || PageReadIndex { boundary: false, noncanonical: true, part_order: None, prefix_exceptions: BTreeMap::new(), suffix_exceptions: BTreeMap::new() };
    if m.parts.iter().any(|part| !part.xml && part.entry.is_some()) { return Ok(fallback()); }
    if zip.by_name("cloud/framing").map_err(|e| e.to_string())?.size() > 16 * 1024 * 1024 { return Ok(fallback()); }
    let framing = read_entry(zip, "cloud/framing", &m.entry_sha256)?;
    let mut pos = 0usize;
    let mut sample = Vec::new();
    for _ in 0..m.parts.len() {
        let len_bytes = framing.get(pos..pos.saturating_add(4)).ok_or("truncated framing")?;
        let len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
        pos += 4;
        let end = pos.checked_add(len).filter(|end| *end <= framing.len()).ok_or("invalid framing")?;
        sample.extend_from_slice(&framing[pos..end]);
        sample.push(b'x');
        pos = end;
    }
    let len_bytes = framing.get(pos..pos.saturating_add(4)).ok_or("truncated framing")?;
    let len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
    pos += 4;
    let end = pos.checked_add(len).filter(|end| *end == framing.len()).ok_or("invalid trailing framing")?;
    sample.extend_from_slice(&framing[pos..end]);
    let limits = rs_doc::mime::Limits::default();
    let Ok(parts) = rs_doc::mime::parts(&sample, &m.content_type, limits.max_readonly_parts, &limits, &mut rs_doc::mime::NoHooks) else { return Ok(fallback()); };
    if parts.len() != m.parts.len() { return Ok(fallback()); }
    let mut order = Vec::new();
    let mut prefixes = BTreeMap::new();
    let mut suffixes = BTreeMap::new();
    for (part, original) in parts.iter().zip(&m.parts) {
        if original.xml || original.entry.is_some() { continue; }
        let Some((id, suffix)) = part.filename.as_deref().and_then(|value| value.split_once('_')) else { return Ok(fallback()); };
        if part.name.as_deref() != Some("strokeResource") || id != original.id || !canonical_uuid(id) || !suffix_ok(suffix) || original.prefix.len() != 12 {
            return Ok(fallback());
        }
        order.push(id.to_string());
        prefixes.insert(id.to_string(), original.prefix.clone());
        suffixes.insert(id.to_string(), suffix.to_string());
    }
    Ok(PageReadIndex { boundary: true, noncanonical: true, part_order: Some(order), prefix_exceptions: prefixes, suffix_exceptions: suffixes })
}
fn corrupt(message: impl Into<String>) -> PageReadError {
    PageReadError::Corrupt(message.into())
}
fn unsupported(message: impl Into<String>) -> PageReadError {
    PageReadError::Unsupported(message.into())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn canonical_uuid(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id)
}
fn tag(node: roxmltree::Node<'_, '_>, value: &str) -> bool {
    node.is_element() && node.tag_name().name() == value
}
fn suffix_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

/// Authenticate independent SQLite digests before extracting one page's Cloud
/// strokes. Caller holds the publication lock and must decode and bind bodies.
/// This returns material only: a context/2 proof is not a renderer admission
/// decision. Renderer callers must separately require the context/3 full-note
/// stroke binding proof and perform their normal per-page validation.
pub fn read_page_strokes<R: Read + Seek>(
    reader: R,
    expected: &PageReadExpectation<'_>,
) -> Result<PageStrokeMaterial, PageReadError> {
    if expected.page_id.is_empty()
        || expected.page_id.len() > 128
        || !expected
            .page_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(corrupt("invalid page ID"));
    }
    let mut zip = archive(reader).map_err(corrupt)?;
    let manifest = verified_manifest(&mut zip, expected.manifest_sha256).map_err(corrupt)?;
    let context =
        read_entry(&mut zip, "cloud/context.json", &manifest.entry_sha256).map_err(corrupt)?;
    let index = index_from_manifest(&mut zip, manifest).map_err(corrupt)?;
    read_indexed(&mut zip, expected, &index, &context)
}

/// Open only the selected page's entries using source-bound SQLite facts.
pub fn read_page_strokes_indexed<R: Read + Seek>(reader: R, expected: &PageReadExpectation<'_>, index: &PageReadIndex, context: &[u8]) -> Result<PageStrokeMaterial, PageReadError> {
    if expected.page_id.is_empty() || expected.page_id.len() > 128 || !expected.page_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(corrupt("invalid page ID"));
    }
    let mut zip = archive(reader).map_err(corrupt)?;
    read_indexed(&mut zip, expected, index, context)
}

fn read_indexed<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, expected: &PageReadExpectation<'_>, manifest: &PageReadIndex, context: &[u8]) -> Result<PageStrokeMaterial, PageReadError> {
    if !manifest.boundary { return Err(unsupported("noncanonical MIME framing")); }
    if hash(context) != expected.context_sha256 {
        return Err(corrupt("page context hash differs from index"));
    }
    let context = crate::page_context::parse(context, expected.raw_sha256).map_err(corrupt)?;
    if context.admission.is_none() {
        return Err(unsupported("page context has no admission proof"));
    }
    if !context.verified_resource_matches(Some(expected.local_id), Some(expected.commit_id)) {
        return Err(corrupt("page admission proof differs from note"));
    }
    if context
        .pages
        .iter()
        .filter(|page| page.id == expected.page_id)
        .count()
        != 1
    {
        return Err(corrupt("page is absent from context"));
    }
    let page_name = format!("{}.page", expected.page_id);
    let xml_name = format!("cloud/pages/{}.xml", expected.page_id);
    let hashes = BTreeMap::from([(page_name.clone(), expected.page_sha256.to_owned()), (xml_name.clone(), expected.xml_sha256.to_owned())]);
    let page = read_entry(
        zip,
        &page_name,
        &hashes,
    )
    .map_err(corrupt)?;
    let xml = read_entry(
        zip,
        &xml_name,
        &hashes,
    )
    .map_err(corrupt)?;
    if hash(&page) != expected.page_sha256 || hash(&xml) != expected.xml_sha256 {
        return Err(corrupt("page hash differs from index"));
    }
    let length = u32::from_le_bytes(
        xml.get(..4)
            .ok_or_else(|| corrupt("truncated page XML"))?
            .try_into()
            .unwrap(),
    ) as usize;
    let cut = 4usize
        .checked_add(length)
        .filter(|v| *v <= xml.len())
        .ok_or_else(|| corrupt("invalid page XML span"))?;
    let tree = std::str::from_utf8(&xml[4..cut]).map_err(|_| corrupt("invalid page tree XML"))?;
    let body = std::str::from_utf8(&xml[cut..]).map_err(|_| corrupt("invalid page XML"))?;
    let tree_doc = roxmltree::Document::parse(tree)
        .map_err(|_| unsupported("page tree XML needs full context"))?;
    let page_doc =
        roxmltree::Document::parse(body).map_err(|_| unsupported("page XML needs full context"))?;
    let tree_page = tree_doc.root_element();
    let page_node = page_doc.root_element();
    if !tag(tree_page, "page")
        || !tag(page_node, "page")
        || tree_page.attribute("id") != Some(expected.page_id)
        || page_node.attribute("id") != Some(expected.page_id)
        || tree_page.descendants().skip(1).any(|n| tag(n, "page"))
        || page_node.descendants().skip(1).any(|n| tag(n, "page"))
    {
        return Err(corrupt("page XML identity mismatch"));
    }
    let objects: Vec<_> = page_node
        .descendants()
        .filter(|n| {
            tag(*n, "object")
                && n.attribute("type")
                    .is_some_and(|v| v.eq_ignore_ascii_case("stroke"))
        })
        .collect();
    let payloads = page_strokes(&page).map_err(corrupt)?;
    if objects.len() != payloads.len() {
        return Err(corrupt("stroke inventory differs from page"));
    }
    let mut strokes = HashMap::new();
    for (object, payload) in objects.into_iter().zip(payloads) {
        let id = object
            .attribute("id")
            .ok_or_else(|| corrupt("missing stroke ID"))?;
        if !canonical_uuid(id) || strokes.insert(id.to_owned(), payload).is_some() {
            return Err(corrupt("invalid or duplicate stroke ID"));
        }
    }
    let mut times = HashMap::new();
    let mut tree_order = Vec::new();
    for object in tree_page.descendants().filter(|node| tag(*node, "object")) {
        let Some(id) = object.attribute("id") else {
            continue;
        };
        if strokes.contains_key(id) {
            let time = object
                .attribute("t")
                .ok_or_else(|| corrupt("missing stroke time"))?;
            if !suffix_ok(time) {
                return Err(unsupported("unsupported stroke time"));
            }
            if times.insert(id.to_owned(), time.to_owned()).is_some() {
                return Err(corrupt("duplicate tree stroke"));
            }
            tree_order.push(id.to_owned());
        }
    }
    if times.len() != strokes.len() {
        return Err(corrupt("page tree stroke inventory mismatch"));
    }
    let order = match &manifest.part_order {
        Some(order) => {
            let mut seen = HashSet::new();
            for id in order {
                if !canonical_uuid(id) || !seen.insert(id) {
                    return Err(corrupt("invalid multipart stroke order"));
                }
            }
            order
                .iter()
                .filter(|id| strokes.contains_key(id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        }
        None => tree_order,
    };
    if order.len() != strokes.len() {
        if manifest.noncanonical { return Err(unsupported("noncanonical stroke inventory requires full snapshot")); }
        return Err(corrupt("multipart stroke order incomplete"));
    }
    const DEFAULT_WRAPPER: [u8; 12] = [255, 255, 255, 255, 4, 0, 0, 0, 160, 15, 0, 0];
    let mut result = Vec::with_capacity(order.len());
    for id in order {
        let suffix = manifest
            .suffix_exceptions
            .get(&id)
            .map(String::as_str)
            .unwrap_or(&times[&id]);
        if !suffix_ok(suffix) {
            return Err(unsupported("unsupported stroke filename suffix"));
        }
        let wrapper = manifest
            .prefix_exceptions
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&DEFAULT_WRAPPER);
        if wrapper.len() != 12 {
            return Err(corrupt("invalid stroke wrapper"));
        }
        let mut body = wrapper.to_vec();
        body.extend_from_slice(&strokes[&id]);
        result.push(PageStroke {
            filename: format!("{id}_{suffix}"),
            id,
            body,
        });
    }
    Ok(PageStrokeMaterial {
        page: crate::note_container::PageData { page, xml },
        strokes: result,
    })
}
