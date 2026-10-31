//! Versioned, URL-free render projection of a verified immutable container.
use crate::db::{self, Sql};
use crate::document_store::{DocumentStore, RawDocumentRef};
use crate::note_container;
use crate::note_container_page::{self, PageReadIndex};
use crate::preview_inventory::InventoryKey;
use crate::storage::StoreCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use base64::{engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD}, Engine};

const FORMAT: &str = "notes-linux/render-projection/1";
const MAX_BLOB: usize = 16 * 1024 * 1024;

/// Renderer element tree, restricted to the admitted resource's components.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub tag: String,
    pub attributes: Vec<(String, String)>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<Node>,
}
impl Node {
    fn from_element(e: &_ops::xml::Element) -> Self {
        Self { tag: e.tag.clone(), attributes: e.attrib.clone(), text: e.text.clone(), tail: e.tail.clone(), children: e.children.iter().map(Self::from_element).collect() }
    }
    pub fn element(&self) -> _ops::xml::Element {
        _ops::xml::Element { tag: self.tag.clone(), attrib: self.attributes.clone(), text: self.text.clone(), tail: self.tail.clone(), children: self.children.iter().map(Self::element).collect() }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderIndex {
    pub format: String,
    pub context: Vec<u8>,
    pub resource: Node,
    pub strokes: PageReadIndex,
    pub manifest_sha256: String,
    pub url_ids: Vec<String>,
}
impl RenderIndex {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BLOB { return Err("render projection exceeds limit".into()); }
        let index: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if index.format != FORMAT { return Err("unknown render projection".into()); }
        Ok(index)
    }
}

fn verified_urls(global: &note_container::GlobalPageContext) -> Result<Option<(Vec<String>, Vec<String>)>, String> {
    let limits = rs_doc::mime::Limits::default();
    let mut sources = vec![global.note_xml.clone()];
    if !global.download_urls.is_empty() {
        let mut wrapped = b"<CompareNoteView>".to_vec();
        wrapped.extend_from_slice(&global.download_urls);
        wrapped.extend_from_slice(b"</CompareNoteView>");
        sources.push(wrapped);
    }
    let mut ids = Vec::new();
    let mut secrets = Vec::new();
    let mut nonempty_sources = 0;
    for source in sources {
        let parsed = rs_doc::payload::parse_payload(&source, "application/xml", Some(limits.max_readonly_parts), &limits, &mut rs_doc::mime::NoHooks).map_err(|e| e.to_string())?;
        if !parsed.download_urls.is_empty() {
            nonempty_sources += 1;
        }
        for item in parsed.download_urls {
            let id = item.id.to_ascii_lowercase();
            if id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { ids.push(id); }
            secrets.push(item.url);
        }
    }
    // Splitting discards the relative order of URL records in these two
    // entries. Preserve the full-snapshot parser's decision in that case.
    if nonempty_sources > 1 { return Ok(None); }
    ids.sort();
    Ok(Some((ids, secrets)))
}

fn has_signed_url(resource: &Node, urls: &[String]) -> Result<bool, String> {
    let encoded = serde_json::to_string(resource).map_err(|e| e.to_string())?;
    let lower = encoded.to_ascii_lowercase();
    let budget = encoded.len().checked_mul(urls.len()).and_then(|n| n.checked_mul(6)).ok_or("URL scan exceeds limit")?;
    if budget > 512 * 1024 * 1024 { return Err("URL scan exceeds limit".into()); }
    for url in urls {
        let mut percent = String::with_capacity(url.len() * 3);
        let mut standard_percent = String::with_capacity(url.len() * 3);
        for b in url.bytes() { use std::fmt::Write; let _ = write!(percent, "%{b:02x}"); if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { standard_percent.push((b as char).to_ascii_lowercase()); } else { let _ = write!(standard_percent, "%{b:02x}"); } }
        let base64 = STANDARD.encode(url);
        if encoded.contains(url) || lower.contains(&percent) || lower.contains(&standard_percent) || encoded.contains(base64.trim_end_matches('=')) || encoded.contains(&URL_SAFE_NO_PAD.encode(url)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Pin the immutable container under the publication lock; expensive parsing
/// runs outside the lock. The caller must publish only if the source still
/// matches the work key.
pub fn capture(store: &StoreCore, key: &InventoryKey, original_resource: &[u8], mut url_ids: Vec<String>) -> db::Result<Option<Vec<u8>>> {
    let documents = DocumentStore::open(&store.directory)?;
    let pinned = documents.with_write(|writer| store.with_db(|db| {
        let rows = db.query("SELECT container_sha256,container_size,raw_sha256,json_extract(container_metadata,'$.context_sha256') AS context_sha256,json_extract(container_metadata,'$.manifest_sha256') AS manifest_sha256,json_extract(container_metadata,'$.local_id') AS local_id,json_extract(container_metadata,'$.commit_id') AS commit_id FROM notes WHERE id=?", &crate::p![&key.note_id])?;
        let Some(row) = rows.one() else { return Ok(None) };
        let (Some(hash), Some(raw), Some(context_hash), Some(manifest_hash), Some(local), Some(commit)) = (row.text("container_sha256"), row.text("raw_sha256"), row.text("context_sha256"), row.text("manifest_sha256"), row.text("local_id"), row.text("commit_id")) else { return Ok(None) };
        if hash != key.container_sha256 || raw != key.raw_sha256 { return Ok(None); }
        let size = row.at(1).as_i64().filter(|s| *s >= 0).ok_or_else(|| db::Error::Value("invalid container size".into()))?;
        let reference = RawDocumentRef { sha256: hash.into(), size: size as u64 };
        let file = writer.open_raw(&reference)?;
        Ok(Some((file, context_hash.to_owned(), manifest_hash.to_owned(), local.to_owned(), commit.to_owned())))
    }))?;
    let Some((file, context_hash, manifest_hash, local, commit)) = pinned else { return Ok(None); };
    let global = note_container::read_global_context(file.try_clone()?, &context_hash, &key.raw_sha256).map_err(db::Error::Value)?;
    if !global.index.stroke_bindings_match(Some(&local), Some(&commit)) { return Ok(None); }
    let proof = global.index.admission.as_ref().ok_or_else(|| db::Error::Value("missing resource proof".into()))?;
    if format!("{:x}", Sha256::digest(original_resource)) != proof.resource_sha256 { return Err(db::Error::Value("resource proof mismatch".into())); }
    let original = _ops::xml::parse_bytes(original_resource).map_err(db::Error::Value)?;
    if original.iter().any(|n| matches!(n.local().as_str(), "downloadurl" | "downloadurls")) { return Ok(None); }
    let doc = _ops::xml::parse_bytes(&global.note_xml).map_err(db::Error::Value)?;
    let mut roots = doc.iter().filter(|n| n.local() == "noteresource");
    let Some(root) = roots.next() else { return Ok(None) };
    if roots.next().is_some() { return Ok(None); }
    let allowed = ["noteTree", "contentFileList", "attributes", "pageList", "floatingObjectList", "bodyText"];
    if root.children.iter().any(|c| !allowed.contains(&c.tag.as_str())) || original.children.iter().map(|c| &c.tag).ne(root.children.iter().map(|c| &c.tag)) { return Ok(None); }
    let context = serde_json::to_vec(&global.index).map_err(|e| db::Error::Value(e.to_string()))?;
    if format!("{:x}", Sha256::digest(&context)) != context_hash { return Err(db::Error::Value("context serialization differs from source".into())); }
    url_ids.sort();
    let Some((verified_ids, urls)) = verified_urls(&global).map_err(db::Error::Value)? else { return Ok(None) };
    if url_ids != verified_ids { return Err(db::Error::Value("preview URL identities differ from source".into())); }
    let strokes = note_container_page::capture_page_index(file, &manifest_hash).map_err(db::Error::Value)?;
    let resource = Node::from_element(root);
    if has_signed_url(&resource, &urls).map_err(db::Error::Value)? { return Ok(None); }
    let index = RenderIndex { format: FORMAT.into(), context, resource, strokes, manifest_sha256: manifest_hash, url_ids };
    let bytes = serde_json::to_vec(&index).map_err(|e| db::Error::Value(e.to_string()))?;
    if bytes.len() > MAX_BLOB { return Err(db::Error::Value("render projection exceeds limit".into())); }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_signed_urls_in_text_and_attributes_without_rejecting_user_links() {
        let signed = "https://example.invalid/media?signature=synthetic";
        let mut root = Node { tag: "bodyText".into(), attributes: vec![], text: Some("https://example.invalid/ordinary".into()), tail: None, children: vec![] };
        assert!(!has_signed_url(&root, &[signed.into()]).unwrap());
        for value in [signed.to_owned(), STANDARD.encode(signed), "https%3A%2F%2Fexample.invalid%2Fmedia%3Fsignature%3Dsynthetic".into()] {
            root.attributes = vec![("value".into(), value)];
            assert!(has_signed_url(&root, &[signed.into()]).unwrap());
        }
    }
}

