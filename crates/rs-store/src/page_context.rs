//! Compact, revision-bound page facts for renderers which need global layout
//! context while loading one page at a time. This is an index, not an
//! admission decision: renderers must reject unknown constructs or fall back
//! to the full snapshot, and must use the verbatim global XML for body text,
//! floating objects, signed URLs, and content-file metadata.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

const FORMAT: &str = "notes-linux/page-context/1";
const PROOF_FORMAT: &str = "notes-linux/page-context/2";
const BINDING_FORMAT: &str = "notes-linux/page-context/3";
pub const ADMISSION_VERSION: &str = _ops::ink_projection::VERIFIED_RESOURCE_VERSION;
const MAX_PAGES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageContext {
    pub format: String,
    pub raw_sha256: String,
    pub xml_sha256: String,
    pub note_xml_sha256: String,
    pub download_urls_sha256: String,
    pub pages: Vec<PageFacts>,
    /// True exactly when prepare_source sees a floating object, a page object,
    /// a PDF node, or a nonempty canvas-cache map anywhere in the note.
    pub page_based: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<AdmissionProof>,
}
/// Bound to the raw snapshot and metadata that passed the shared verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionProof {
    pub version: String,
    pub local_id: String,
    pub commit_id: String,
    pub resource_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_binding_version: Option<String>,
}
impl PageContext {
    /// Call only after `parse` verifies the independently stored context digest
    /// and expected snapshot digest. Metadata must still match the proof.
    pub fn verified_resource_matches(
        &self,
        local_id: Option<&str>,
        commit_id: Option<&str>,
    ) -> bool {
        matches!(self.format.as_str(), PROOF_FORMAT | BINDING_FORMAT)
            && self.admission.as_ref().is_some_and(|p| {
                p.version == ADMISSION_VERSION
                    && Some(p.local_id.as_str()) == local_id
                    && Some(p.commit_id.as_str()) == commit_id
            })
    }
    /// Certifies only the renderer's shared stroke decode/bind phase.
    pub fn stroke_bindings_match(&self, local_id: Option<&str>, commit_id: Option<&str>) -> bool {
        self.format == BINDING_FORMAT
            && self.verified_resource_matches(local_id, commit_id)
            && self
                .admission
                .as_ref()
                .and_then(|p| p.stroke_binding_version.as_deref())
                == Some(rs_ink::core::records::VERSION)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageFacts {
    pub id: String,
    /// Verbatim decoded attributes, including width, height and bgColor.
    pub attributes: BTreeMap<String, String>,
    pub has_object: bool,
    pub has_pdf: bool,
    pub has_canvas_cache: bool,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn tag(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

/// Return None for any XML layout this index cannot faithfully describe.
/// The caller must keep the snapshot and use its existing full-read path.
pub fn build(raw: &[u8], content_type: &str) -> Result<Option<Vec<u8>>, String> {
    let limits = rs_doc::mime::Limits::default();
    let parts = rs_doc::mime::parts(
        raw,
        content_type,
        limits.max_readonly_parts,
        &limits,
        &mut rs_doc::mime::NoHooks,
    )
    .map_err(|e| e.to_string())?;
    let xml_idx = parts
        .iter()
        .position(|p| p.name.as_deref() == Some("noteResource"))
        .or_else(|| (parts.len() == 1).then_some(0));
    let Some(xml_idx) = xml_idx else {
        return Ok(None);
    };
    let xml = &raw[parts[xml_idx].body.clone()];
    let xml_text = std::str::from_utf8(xml).map_err(|e| e.to_string())?;
    let doc = roxmltree::Document::parse(xml_text).map_err(|e| e.to_string())?;
    let notes: Vec<_> = doc
        .descendants()
        .filter(|n| tag(*n, "NoteResource") || tag(*n, "noteResource"))
        .collect();
    if notes.len() != 1 {
        return Ok(None);
    }
    let note = notes[0];
    let direct = |name: &str| {
        note.children()
            .filter(|n| tag(*n, name))
            .collect::<Vec<_>>()
    };
    let pages = direct("pageList");
    let trees = direct("noteTree");
    let floating = direct("floatingObjectList");
    if pages.len() != 1 || trees.len() != 1 || floating.len() > 1 {
        return Ok(None);
    }
    let tree_lists: Vec<_> = trees[0]
        .children()
        .filter(|n| tag(*n, "pageList"))
        .collect();
    if tree_lists.len() != 1 {
        return Ok(None);
    }
    let page_nodes: Vec<_> = pages[0].children().filter(|n| n.is_element()).collect();
    let tree_nodes: Vec<_> = tree_lists[0]
        .children()
        .filter(|n| n.is_element())
        .collect();
    if page_nodes.is_empty()
        || page_nodes.len() > MAX_PAGES
        || page_nodes.len() != tree_nodes.len()
        || page_nodes.iter().any(|n| !tag(*n, "page"))
        || tree_nodes.iter().any(|n| !tag(*n, "page"))
    {
        return Ok(None);
    }
    let mut ids = HashSet::new();
    let mut facts = Vec::with_capacity(page_nodes.len());
    for (page, tree) in page_nodes.iter().zip(&tree_nodes) {
        let Some(id) = page.attribute("id") else {
            return Ok(None);
        };
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !ids.insert(id)
            || tree.attribute("id") != Some(id)
            || page.descendants().skip(1).any(|n| tag(n, "page"))
            || page.attributes().any(|a| a.namespace().is_some())
        {
            return Ok(None);
        }
        let mut attributes = BTreeMap::new();
        for attr in page.attributes() {
            attributes.insert(attr.name().to_string(), attr.value().to_string());
        }
        let has_object = page.descendants().any(|n| tag(n, "object"));
        let has_pdf = page.descendants().any(|n| tag(n, "pdfDataList"));
        let has_canvas_cache = page
            .descendants()
            .any(|n| tag(n, "canvasCacheDataMap") && n.children().any(|c| c.is_element()));
        facts.push(PageFacts {
            id: id.to_string(),
            attributes,
            has_object,
            has_pdf,
            has_canvas_cache,
        });
    }
    let has_floating = floating
        .first()
        .is_some_and(|f| f.children().any(|n| n.is_element()));
    let page_based = has_floating
        || facts
            .iter()
            .any(|p| p.has_object || p.has_pdf || p.has_canvas_cache);
    let context = PageContext {
        format: FORMAT.into(),
        raw_sha256: hash(raw),
        xml_sha256: hash(xml),
        note_xml_sha256: String::new(),
        download_urls_sha256: String::new(),
        pages: facts,
        page_based,
        admission: None,
    };
    let encoded = serde_json::to_vec(&context).map_err(|e| e.to_string())?;
    // Binding two 64-character hashes expands the JSON by 128 bytes. Reserve
    // that space here so optional indexing cannot reject an otherwise valid
    // snapshot after the split succeeds.
    Ok((encoded.len() <= 16 * 1024 * 1024 - 128).then_some(encoded))
}

/// Add proof only after the caller has run the shared full-snapshot verifier.
pub(crate) fn bind_admission(
    context: &[u8],
    local_id: &str,
    commit_id: &str,
    resource_xml: &[u8],
    strokes_verified: bool,
) -> Result<Vec<u8>, String> {
    let mut value: PageContext = serde_json::from_slice(context).map_err(|e| e.to_string())?;
    if value.format != FORMAT
        || value.admission.is_some()
        || !value.note_xml_sha256.is_empty()
        || !value.download_urls_sha256.is_empty()
    {
        return Err("invalid unbound page context".into());
    }
    value.format = if strokes_verified {
        BINDING_FORMAT
    } else {
        PROOF_FORMAT
    }
    .to_string();
    value.admission = Some(AdmissionProof {
        version: ADMISSION_VERSION.to_string(),
        local_id: local_id.to_string(),
        commit_id: commit_id.to_string(),
        resource_sha256: hash(resource_xml),
        stroke_binding_version: strokes_verified
            .then(|| rs_ink::core::records::VERSION.to_string()),
    });
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 * 1024 - 128 {
        return Err("page context is too large".into());
    }
    Ok(bytes)
}

/// Bind the exact split global entries into this independently indexed value.
pub fn bind_globals(
    context: &[u8],
    note_xml: &[u8],
    download_urls: &[u8],
) -> Result<Vec<u8>, String> {
    let mut value: PageContext = serde_json::from_slice(context).map_err(|e| e.to_string())?;
    if !matches!(
        value.format.as_str(),
        FORMAT | PROOF_FORMAT | BINDING_FORMAT
    ) || !value.note_xml_sha256.is_empty()
        || !value.download_urls_sha256.is_empty()
    {
        return Err("invalid unbound page context".into());
    }
    value.note_xml_sha256 = hash(note_xml);
    value.download_urls_sha256 = hash(download_urls);
    let encoded = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    if encoded.len() > 16 * 1024 * 1024 {
        return Err("page context is too large".into());
    }
    Ok(encoded)
}

/// Reject stale, malformed or future-version indexes before trusting any facts.
pub fn parse(bytes: &[u8], expected_raw_sha256: &str) -> Result<PageContext, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("page context is too large".into());
    }
    let context: PageContext = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let valid_hash = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let proof_valid = match (context.format.as_str(), context.admission.as_ref()) {
        (FORMAT, None) => true,
        (format @ (PROOF_FORMAT | BINDING_FORMAT), Some(proof)) => {
            let binding = match format {
                PROOF_FORMAT => proof.stroke_binding_version.is_none(),
                BINDING_FORMAT => {
                    proof.stroke_binding_version.as_deref() == Some(rs_ink::core::records::VERSION)
                }
                _ => false,
            };
            proof.version == ADMISSION_VERSION
                && !proof.local_id.is_empty()
                && !proof.commit_id.is_empty()
                && valid_hash(&proof.resource_sha256)
                && binding
        }
        _ => false,
    };
    if !proof_valid
        || context.raw_sha256 != expected_raw_sha256
        || !valid_hash(&context.xml_sha256)
        || !valid_hash(&context.note_xml_sha256)
        || !valid_hash(&context.download_urls_sha256)
        || context.pages.is_empty()
        || context.pages.len() > MAX_PAGES
    {
        return Err("invalid or stale page context".into());
    }
    let mut ids = HashSet::new();
    for page in &context.pages {
        if page.id.is_empty()
            || !ids.insert(&page.id)
            || page.attributes.get("id") != Some(&page.id)
        {
            return Err("invalid page context index".into());
        }
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    const XML: &str = r#"<CompareNoteView><NoteResource id="note"><noteTree ver="4000"><pageList><page id="p1"/><page id="p2"/></pageList></noteTree><attributes formatVersion="4000"/><contentFileList><contentFile id="image"/></contentFileList><pageList><page id="p1" width="100" height="200" bgColor="-1"><layerList><layer><objectList><object type="image"/></objectList></layer></layerList></page><page id="p2" width="200" height="300" bgColor="-1"><pdfDataList/><canvasCacheDataMap><map/></canvasCacheDataMap></page></pageList><floatingObjectList><object pageIndex="1"/></floatingObjectList><bodyText><object><text>synthetic text</text></object></bodyText></NoteResource></CompareNoteView>"#;
    #[test]
    fn context_contains_ordered_geometry_and_global_page_facts() {
        let bytes = XML.as_bytes();
        let unbound = build(bytes, "application/xml").unwrap().unwrap();
        assert!(parse(&unbound, &hash(bytes)).is_err());
        let encoded = bind_globals(&unbound, b"global xml", b"urls").unwrap();
        let context = parse(&encoded, &hash(bytes)).unwrap();
        assert_eq!(context.note_xml_sha256, hash(b"global xml"));
        assert_eq!(context.download_urls_sha256, hash(b"urls"));
        assert_eq!(context.xml_sha256, hash(bytes));
        assert_eq!(
            context
                .pages
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["p1", "p2"]
        );
        assert_eq!(context.pages[0].attributes.get("width").unwrap(), "100");
        assert!(context.pages[0].has_object);
        assert!(!context.pages[0].has_pdf);
        assert!(context.pages[1].has_pdf && context.pages[1].has_canvas_cache);
        assert!(context.page_based);
        assert!(parse(&encoded, &"0".repeat(64)).is_err());
    }
    #[test]
    fn ambiguous_or_unpaired_pages_fall_back_to_full_snapshot() {
        let bad = XML.replace(
            "<page id=\"p2\"/></pageList></noteTree>",
            "<page id=\"other\"/></pageList></noteTree>",
        );
        assert!(build(bad.as_bytes(), "application/xml").unwrap().is_none());
        let duplicate = XML.replace("<page id=\"p2\" width", "<page id=\"p1\" width");
        assert!(build(duplicate.as_bytes(), "application/xml")
            .unwrap()
            .is_none());
    }
}
