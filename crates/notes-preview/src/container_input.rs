//! Verified page-local inputs for the renderer. A revision-bound context must
//! prove both resource validation and whole-note stroke decoding and binding;
//! the existing renderer still performs its page-specific admission checks.
//! Missing or unsupported proof falls back to the full snapshot; integrity
//! failures remain errors.
use crate::util::store_fail;
use notes_host::Fail;
use rs_store::db::{self, Sql};
use rs_store::document_store::{DocumentStore, RawDocumentRef};
use rs_store::note_container;
use rs_store::note_container_page::{self, PageReadError, PageReadExpectation, PageStroke};
use rs_store::page_context::PageContext;
use rs_store::storage::StoreCore;
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::collections::VecDeque;

#[derive(Clone)]
struct CachedProjection {
    path: std::path::PathBuf,
    note_id: String,
    reference: rs_store::document_store::DocumentRef,
    fingerprint: rs_store::document_store::DocumentFingerprint,
    projection: Arc<rs_store::preview_render_index::RenderIndex>,
    weight: usize,
}
static PROJECTIONS: LazyLock<Mutex<VecDeque<CachedProjection>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));
const PROJECTION_CACHE_LIMIT: usize = 16 * 1024 * 1024;

/// Read a source-bound, content-addressed projection under the store lock.
pub(crate) fn render_index(store: &StoreCore, identity: &str) -> Result<Option<Arc<rs_store::preview_render_index::RenderIndex>>, Fail> {
    let cached = PROJECTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|item| item.path == store.path && item.note_id == identity).cloned();
    let Some((reference, fingerprint, bytes)) = store.preview_render_blob_cached(identity, crate::PREVIEW_INVENTORY_RECIPE, cached.as_ref().map(|c| (&c.reference, &c.fingerprint))).map_err(store_fail)? else { return Ok(None) };
    let projection = if let Some(bytes) = bytes {
        let projection = Arc::new(rs_store::preview_render_index::RenderIndex::decode(&bytes)
            .map_err(|_| Fail::store("preview_storage_corrupt", "Sayfa verisi doğrulanamadı.", 422))?);
        // Account conservatively for decoded nodes, strings and allocation overhead.
        let weight = bytes.len().saturating_mul(4);
        if weight <= PROJECTION_CACHE_LIMIT {
            let mut cache = PROJECTIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.retain(|item| item.path != store.path || item.note_id != identity);
            while cache.iter().map(|item| item.weight).sum::<usize>().saturating_add(weight) > PROJECTION_CACHE_LIMIT { cache.pop_front(); }
            cache.push_back(CachedProjection { path: store.path.clone(), note_id: identity.into(), reference, fingerprint, projection: projection.clone(), weight });
        }
        projection
    } else { cached.ok_or_else(|| Fail::store("preview_storage_corrupt", "Sayfa verisi doğrulanamadı.", 422))?.projection };
    use sha2::{Digest, Sha256};
    let rows = store.with_db(|db| db.query("SELECT raw_sha256,json_extract(container_metadata,'$.context_sha256') AS context_sha256,json_extract(container_metadata,'$.manifest_sha256') AS manifest_sha256 FROM notes WHERE id=?", &rs_store::p![identity])).map_err(store_fail)?;
    let row = rows.one().ok_or_else(|| Fail::store("not_found", "Not bulunamadı.", 404))?;
    let corrupt = || Fail::store("preview_storage_corrupt", "Sayfa verisi doğrulanamadı.", 422);
    let raw = row.text("raw_sha256").ok_or_else(corrupt)?;
    if row.text("manifest_sha256") != Some(projection.manifest_sha256.as_str()) || row.text("context_sha256") != Some(format!("{:x}", Sha256::digest(&projection.context)).as_str()) || rs_store::page_context::parse(&projection.context, raw).is_err() { return Err(corrupt()); }
    Ok(Some(projection))
}

/// Verified material, not a replacement DocumentSnapshot. The global XML is
/// the verbatim envelope with all page spans and download URLs removed.
pub struct PageMaterial {
    pub(crate) revision: i64,
    pub(crate) page_index: i64,
    pub(crate) page_id: String,
    pub(crate) raw_sha256: String,
    pub(crate) global: note_container::GlobalPageContext,
    pub(crate) projected_resource: Option<rs_store::preview_render_index::Node>,
    pub(crate) page_xml: Vec<u8>,
    pub(crate) strokes: Vec<PageStroke>,
}

/// Read only the authenticated global entries, without opening any page.
/// Older containers without an independently bound context use the full path.
pub fn read_global(
    store: &StoreCore,
    identity: &str,
) -> Result<Option<note_container::GlobalPageContext>, Fail> {
    let is_v7 = store
        .with_db(|db| {
            let rows = db.query("SELECT value FROM metadata WHERE key='schema_version'", &[])?;
            Ok(rows.one().and_then(|r| r.text("value")) == Some("7"))
        })
        .map_err(store_fail)?;
    if !is_v7 {
        return Ok(None);
    }
    let documents = DocumentStore::open(&store.directory).map_err(store_fail)?;
    documents.with_write(|writer| store.with_db(|db| {
        let rows = db.query("SELECT container_sha256,container_size,raw_sha256,json_extract(container_metadata,'$.context_sha256') AS context_sha256 FROM notes WHERE id=?", &rs_store::p![identity])?;
        let row = rows.one().ok_or_else(|| db::store_err("not_found", "Not bulunamadı.", 404))?;
        let Some(hash) = row.text("container_sha256") else { return Ok(None) };
        let Some(context_hash) = row.text("context_sha256") else { return Ok(None) };
        let raw = row.text("raw_sha256").ok_or_else(|| db::Error::Value("missing raw snapshot digest".into()))?;
        let size = row.int("container_size");
        if size < 0 { return Err(db::Error::Value("invalid container size".into())); }
        let reference = RawDocumentRef { sha256: hash.into(), size: size as u64 };
        let global = note_container::read_global_context(writer.open_raw(&reference)?, context_hash, raw).map_err(db::Error::Value)?;
        Ok(Some(global))
    })).map_err(store_fail)
}
impl PageMaterial {
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn raw_sha256(&self) -> &str {
        &self.raw_sha256
    }
    pub fn page_id(&self) -> &str {
        &self.page_id
    }
    pub fn page_index(&self) -> i64 {
        self.page_index
    }
}
fn split_page_xml(raw: &[u8]) -> Result<(&[u8], &[u8]), db::Error> {
    let header: [u8; 4] = raw
        .get(..4)
        .ok_or_else(|| db::Error::Value("truncated page XML".into()))?
        .try_into()
        .unwrap();
    let end = 4usize
        .checked_add(u32::from_le_bytes(header) as usize)
        .filter(|end| *end <= raw.len())
        .ok_or_else(|| db::Error::Value("invalid page XML span".into()))?;
    Ok((&raw[4..end], &raw[end..]))
}
fn validate_page(
    context: &PageContext,
    index: usize,
    page_id: &str,
    tree: &[u8],
    xml: &[u8],
) -> Result<(), db::Error> {
    let facts = context
        .pages
        .get(index)
        .ok_or_else(|| db::Error::Value("page absent from context".into()))?;
    let tree = rs_doc::xml::fromstring(tree)
        .map_err(|_| db::Error::Value("invalid tree page XML".into()))?;
    let page =
        rs_doc::xml::fromstring(xml).map_err(|_| db::Error::Value("invalid page XML".into()))?;
    let attrs: BTreeMap<String, String> = page.attrs.iter().cloned().collect();
    if tree.tag != "page"
        || page.tag != "page"
        || tree.get("id") != Some(page_id)
        || page.get("id") != Some(page_id)
        || facts.id != page_id
        || attrs != facts.attributes
    {
        return Err(db::Error::Value(
            "page differs from verified context".into(),
        ));
    }
    let has_object = page.iter().any(|n| n.tag == "object");
    let has_pdf = page.iter().any(|n| n.tag == "pdfDataList");
    let has_canvas = page
        .iter()
        .any(|n| n.tag == "canvasCacheDataMap" && !n.children.is_empty());
    if (has_object, has_pdf, has_canvas)
        != (facts.has_object, facts.has_pdf, facts.has_canvas_cache)
    {
        return Err(db::Error::Value(
            "page facts differ from verified context".into(),
        ));
    }
    Ok(())
}
/// Access under the publication lock. Legacy/unsplit containers and missing
/// whole-note proof return None for full-snapshot fallback; integrity errors
/// are propagated. Rendering still performs page-specific admission.
pub fn read_material(
    store: &StoreCore,
    identity: &str,
    index: i64,
    expected_sha256: &str,
) -> Result<Option<PageMaterial>, Fail> {
    if index < 0 {
        return Err(Fail::store("page_not_found", "Sayfa bulunamadı.", 404));
    }
    let is_v7 = store
        .with_db(|db| {
            let rows = db.query("SELECT value FROM metadata WHERE key='schema_version'", &[])?;
            Ok(rows.one().and_then(|r| r.text("value")) == Some("7"))
        })
        .map_err(store_fail)?;
    if !is_v7 {
        return Ok(None);
    }
    let projection = render_index(store, identity)?;
    let documents = DocumentStore::open(&store.directory).map_err(store_fail)?;
    documents.with_write(|writer| store.with_db(|db| {
        let rows = db.query("SELECT n.container_sha256,n.container_size,n.raw_sha256,n.revision,p.page_id,p.page_sha256,p.xml_sha256,json_extract(n.container_metadata,'$.context_sha256') AS context_sha256,json_extract(n.container_metadata,'$.manifest_sha256') AS manifest_sha256,json_extract(n.container_metadata,'$.local_id') AS local_id,json_extract(n.container_metadata,'$.commit_id') AS commit_id FROM notes n LEFT JOIN note_pages p ON p.note_id=n.id AND p.idx=? WHERE n.id=?", &rs_store::p![index, identity])?;
        if rows.rows.len() > 1 { return Err(db::Error::Value("duplicate page index".into())); }
        let row = rows.one().ok_or_else(|| db::store_err("not_found", "Not bulunamadı.", 404))?;
        let Some(hash) = row.text("container_sha256") else { return Ok(None); };
        let raw = row.text("raw_sha256").ok_or_else(|| db::Error::Value("missing raw snapshot digest".into()))?;
        if raw != expected_sha256 { return Err(db::store_err("preview_source_changed", "Not değişti.", 409)); }
        let Some(context_hash) = row.text("context_sha256") else { return Ok(None); };
        let (Some(manifest_hash), Some(local_id), Some(commit_id)) = (row.text("manifest_sha256"), row.text("local_id"), row.text("commit_id")) else { return Ok(None); };
        let page_id = row.text("page_id").ok_or_else(|| db::store_err("page_not_found", "Sayfa bulunamadı.", 404))?;
        let page_hash = row.text("page_sha256").ok_or_else(|| db::Error::Value("missing page hash".into()))?;
        let xml_hash = row.text("xml_sha256").ok_or_else(|| db::Error::Value("missing page XML hash".into()))?;
        if xml_hash.is_empty() { return Ok(None); }
        let size = row.at(1).as_i64().filter(|s| *s >= 0).ok_or_else(|| db::Error::Value("invalid container size".into()))?;
        let reference = RawDocumentRef { sha256: hash.into(), size: size as u64 };
        let global = if let Some(projection) = &projection {
            use sha2::{Digest, Sha256};
            if projection.manifest_sha256 != manifest_hash || format!("{:x}", Sha256::digest(&projection.context)) != context_hash { return Err(db::Error::Value("render projection source mismatch".into())); }
            let index = rs_store::page_context::parse(&projection.context, raw).map_err(db::Error::Value)?;
            note_container::GlobalPageContext { index, note_xml: Vec::new(), download_urls: Vec::new() }
        } else {
            note_container::read_global_context(writer.open_raw(&reference)?, context_hash, raw).map_err(db::Error::Value)?
        };
        if !global.index.stroke_bindings_match(Some(local_id), Some(commit_id)) { return Ok(None); }
        let expectation = PageReadExpectation { page_id, page_sha256: page_hash, xml_sha256: xml_hash, context_sha256: context_hash, raw_sha256: raw, manifest_sha256: manifest_hash, local_id, commit_id };
        let material = match if let Some(projection) = &projection {
            note_container_page::read_page_strokes_indexed(writer.open_raw(&reference)?, &expectation, &projection.strokes, &projection.context)
        } else { note_container_page::read_page_strokes(writer.open_raw(&reference)?, &expectation) } {
            Ok(material) => material,
            Err(PageReadError::Unsupported(_)) => return Ok(None),
            Err(PageReadError::Corrupt(message)) => return Err(db::Error::Value(message)),
        };
        let data = material.page;
        let (tree, page) = split_page_xml(&data.xml)?;
        validate_page(&global.index, index as usize, page_id, tree, page)?;
        Ok(Some(PageMaterial { revision: row.int("revision"), page_index: index, page_id: page_id.into(), raw_sha256: raw.into(), global, projected_resource: projection.as_ref().map(|p| p.resource.clone()), page_xml: page.to_vec(), strokes: material.strokes }))
    })).map_err(store_fail)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_common::{note_with_document, store, TempDir};

    pub(crate) fn publish_index(store: &StoreCore, key: &rs_store::preview_inventory::InventoryKey, fixture: &rs_store::val::Val) {
        let inventory = crate::inventory_worker::compute_static(fixture).unwrap();
        let parsed = crate::payload::payload_from_snapshot(fixture).unwrap();
        let ids = parsed.result.download_urls.iter().map(|u| u.id.to_ascii_lowercase()).filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).collect();
        let snapshot = notes_host::snapshot::from_dict(fixture).unwrap();
        let blob = rs_store::preview_render_index::capture(store, key, &snapshot.resource_xml, ids).unwrap().expect("supported projection");
        assert!(store.publish_preview_inventory_with_blob(key, &rs_store::preview_inventory::InventoryOutcome::Ready(inventory), Some(&blob)).unwrap());
    }

    #[test]
    fn cached_projection_rejects_file_damage() {
        let input = rs_store::val::loads(include_str!("../tests/fixtures/objects_note.json")).unwrap();
        let fixture = canonical_fixture(input.get("snapshot").unwrap());
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
        publish_index(&store, &key, &fixture);
        assert!(render_index(&store, &identity).unwrap().is_some());
        let sha = store.with_db(|db| {
            let rows = db.query("SELECT json_extract(render_blob,'$.source_sha256') AS sha FROM preview_inventories WHERE container_sha256=?", &rs_store::p![&key.container_sha256])?;
            Ok(rows.one().unwrap().text("sha").unwrap().to_string())
        }).unwrap();
        let path = dir.path().join("documents").join(&sha[..2]).join(&sha[2..4]).join(&sha);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 1;
        std::fs::write(path, bytes).unwrap();
        assert!(render_index(&store, &identity).is_err());
    }

    #[test]
    fn projection_capture_rejects_aba_source_with_identical_raw_digest() {
        let input = rs_store::val::loads(include_str!("../tests/fixtures/objects_note.json")).unwrap();
        let fixture = canonical_fixture(input.get("snapshot").unwrap());
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
        let parsed = crate::payload::payload_from_snapshot(&fixture).unwrap();
        let ids: Vec<String> = parsed.result.download_urls.iter().map(|u| u.id.to_ascii_lowercase()).filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).collect();
        let snapshot = notes_host::snapshot::from_dict(&fixture).unwrap();
        store.with_db(|db| { db.exec("UPDATE notes SET container_sha256=? WHERE id=?", &rs_store::p!["b".repeat(64), &identity])?; Ok(()) }).unwrap();
        assert!(rs_store::preview_render_index::capture(&store, &key, &snapshot.resource_xml, ids.clone()).unwrap().is_none());
        store.with_db(|db| { db.exec("UPDATE notes SET container_sha256=? WHERE id=?", &rs_store::p![&key.container_sha256, &identity])?; Ok(()) }).unwrap();
        assert!(rs_store::preview_render_index::capture(&store, &key, &snapshot.resource_xml, ids).unwrap().is_some());
    }

    pub(crate) fn canonical_fixture(input: &rs_store::val::Val) -> rs_store::val::Val {
        canonical_fixture_with_flow_pages(input, false)
    }

    fn canonical_fixture_with_flow_pages(
        input: &rs_store::val::Val,
        add_pages: bool,
    ) -> rs_store::val::Val {
        let raw = notes_host::snapshot::authenticated_raw(input).unwrap();
        let content_type = input.get("content_type").unwrap().as_str().unwrap();
        let limits = rs_doc::mime::Limits::default();
        let parts = rs_doc::mime::parts(
            &raw,
            content_type,
            limits.max_readonly_parts,
            &limits,
            &mut rs_doc::mime::NoHooks,
        )
        .unwrap();
        let mut new = Vec::new();
        for part in parts {
            let original = &raw[part.body];
            let name = part.name.as_deref().unwrap();
            let body = if add_pages && name == "noteResource" {
                let mut document = rs_ops::xml::parse_bytes(original).unwrap();
                let note = document
                    .children
                    .iter_mut()
                    .find(|n| n.local() == "noteresource")
                    .unwrap();
                let attrs = note
                    .children
                    .iter_mut()
                    .find(|n| n.local() == "attributes")
                    .unwrap();
                attrs.set("height", "4629");
                let original_tree = note
                    .children
                    .iter()
                    .find(|n| n.local() == "notetree")
                    .unwrap()
                    .children
                    .iter()
                    .find(|n| n.local() == "pagelist")
                    .unwrap()
                    .children[0]
                    .clone();
                let original_page = note
                    .children
                    .iter()
                    .find(|n| n.local() == "pagelist")
                    .unwrap()
                    .children[0]
                    .clone();
                fn clear_objects(element: &mut rs_ops::xml::Element) {
                    element.children.retain(|c| c.tag != "object");
                    for child in &mut element.children {
                        clear_objects(child);
                    }
                }
                for (id, layer) in [
                    (
                        "00000000-0000-4000-8000-000000000101",
                        "00000000-0000-4000-8000-000000000111",
                    ),
                    (
                        "00000000-0000-4000-8000-000000000102",
                        "00000000-0000-4000-8000-000000000112",
                    ),
                ] {
                    let mut tree_page = original_tree.clone();
                    let mut page = original_page.clone();
                    tree_page.set("id", id);
                    page.set("id", id);
                    clear_objects(&mut tree_page);
                    clear_objects(&mut page);
                    for p in [&mut tree_page, &mut page] {
                        if let Some(list) = p.children.iter_mut().find(|n| n.tag == "layerList") {
                            if let Some(l) = list.children.iter_mut().find(|n| n.tag == "layer") {
                                l.set("id", layer);
                            }
                        }
                    }
                    note.children
                        .iter_mut()
                        .find(|n| n.local() == "notetree")
                        .unwrap()
                        .children
                        .iter_mut()
                        .find(|n| n.local() == "pagelist")
                        .unwrap()
                        .children
                        .push(tree_page);
                    note.children
                        .iter_mut()
                        .find(|n| n.local() == "pagelist")
                        .unwrap()
                        .children
                        .push(page);
                }
                rs_ops::xml::xml_bytes(&document)
            } else {
                original.to_vec()
            };
            let file = part
                .filename
                .as_deref()
                .map(|s| format!("; filename=\"{s}\""))
                .unwrap_or_default();
            let ct = if part.filename.is_some() {
                "application/octet-stream"
            } else {
                "application/xml"
            };
            new.extend_from_slice(format!("--RenderTest\r\nContent-Disposition: form-data; name=\"{name}\"{file}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n",body.len()).as_bytes());
            new.extend_from_slice(&body);
            new.extend_from_slice(b"\r\n");
        }
        new.extend_from_slice(b"--RenderTest--\r\n");
        let parsed = notes_host::snapshot::parse_document(
            &new,
            "multipart/form-data;boundary=RenderTest",
            input.get("server_id").and_then(|v| v.as_str()),
        )
        .unwrap();
        notes_host::snapshot::to_dict(&parsed)
    }

    #[test]
    fn material_is_revision_bound_and_preserves_global_and_page_xml() {
        let dir = TempDir::new();
        let store = store(dir.path());
        let source = rs_store::val::loads(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let fixture = canonical_fixture(&source);
        let identity = note_with_document(&store, &fixture);
        let raw = fixture.get("raw_sha256").unwrap().as_str().unwrap();
        assert!(read_material(&store, &identity, 0, raw).unwrap().is_none());
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let material = read_material(&store, &identity, 0, raw).unwrap().unwrap();
        let page = rs_doc::xml::fromstring(&material.page_xml).unwrap();
        assert_eq!(page.get("id"), Some(material.page_id.as_str()));
        assert_eq!(material.global.index.pages[0].id, material.page_id);
        assert!(!material.strokes.is_empty());
        let input_raw = notes_host::snapshot::authenticated_raw(&fixture).unwrap();
        let content_type = fixture.get("content_type").unwrap().as_str().unwrap();
        let limits = rs_doc::mime::Limits::default();
        let parts = rs_doc::mime::parts(
            &input_raw,
            content_type,
            limits.max_readonly_parts,
            &limits,
            &mut rs_doc::mime::NoHooks,
        )
        .unwrap();
        let original: Vec<_> = parts
            .iter()
            .filter(|p| p.name.as_deref() == Some("strokeResource"))
            .collect();
        assert_eq!(material.strokes.len(), original.len());
        for (stroke, part) in material.strokes.iter().zip(original) {
            assert_eq!(Some(stroke.filename.as_str()), part.filename.as_deref());
            assert_eq!(stroke.body, input_raw[part.body.clone()]);
        }
        for build in [false, true] {
            let full =
                crate::ink::prepare::prepare_pages(&fixture, Some(0), build).map(|v| (*v).clone());
            let partial = crate::ink::prepare::prepare_container_page(&material, build);
            assert_eq!(partial, full);
        }
        let global = rs_doc::xml::fromstring(&material.global.note_xml).unwrap();
        let resource = global.iter().find(|n| n.local() == "noteresource").unwrap();
        assert!(resource.iter().any(|n| n.tag == "bodyText"));
        assert!(resource.iter().any(|n| n.tag == "floatingObjectList"));
        assert!(read_material(&store, &identity, 0, &"0".repeat(64)).is_err());
        let original_metadata = store
            .with_db(|db| {
                let rows = db.query(
                    "SELECT container_metadata FROM notes WHERE id=?",
                    &rs_store::p![&identity],
                )?;
                Ok(rows
                    .one()
                    .unwrap()
                    .text("container_metadata")
                    .unwrap()
                    .to_owned())
            })
            .unwrap();
        for key in ["context_sha256", "manifest_sha256"] {
            store.with_db(|db| {
                db.exec("UPDATE notes SET container_metadata=json_set(container_metadata,?,?) WHERE id=?", &rs_store::p![format!("$.{key}"), "0".repeat(64), &identity])?;
                Ok(())
            }).unwrap();
            assert!(read_material(&store, &identity, 0, raw).is_err(), "{key}");
            store
                .with_db(|db| {
                    db.exec(
                        "UPDATE notes SET container_metadata=? WHERE id=?",
                        &rs_store::p![&original_metadata, &identity],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        store
            .with_db(|db| {
                db.exec(
                    "UPDATE note_pages SET xml_sha256=? WHERE note_id=?",
                    &rs_store::p!["0".repeat(64), &identity],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(read_material(&store, &identity, 0, raw).is_err());
    }

    #[test]
    fn legacy_multipart_framing_reads_the_selected_page() {
        let dir = TempDir::new();
        let store = store(dir.path());
        let fixture = rs_store::val::loads(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let identity = note_with_document(&store, &fixture);
        let raw = fixture.get("raw_sha256").unwrap().as_str().unwrap();
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
        let trace = rs_store::read_trace::Session::for_container(dir.path(), &key.container_sha256);
        let page = read_material(&store, &identity, 0, raw).unwrap().expect("legacy framing has a verified page index");
        assert_eq!(page.page_index(), 0);
        let seen = trace.finish();
        assert_eq!(seen.iter().filter(|name| name.starts_with("cloud/pages/")).count(), 1, "{seen:?}");
    }

    #[test]
    fn urls_in_both_global_entries_use_full_snapshot_and_match_legacy() {
        let input = rs_store::val::loads(include_str!("../tests/fixtures/objects_note.json")).unwrap();
        let snapshot = notes_host::snapshot::from_dict(input.get("snapshot").unwrap()).unwrap();
        let marker = b"</CompareNoteView>";
        let at = snapshot.raw_body.windows(marker.len()).position(|window| window == marker).unwrap();
        let extra = format!("<downloadUrl><id>{}</id><url>https://example.invalid/extra?signature=synthetic</url></downloadUrl>", "f".repeat(64));
        let mut raw = snapshot.raw_body[..at].to_vec();
        raw.extend_from_slice(extra.as_bytes());
        raw.extend_from_slice(&snapshot.raw_body[at..]);
        let changed = notes_host::snapshot::to_dict(&notes_host::snapshot::parse_document(&raw, &snapshot.content_type, snapshot.server_id.as_deref()).unwrap());
        let fixture = canonical_fixture(&changed);
        let parsed = crate::payload::payload_from_snapshot(&fixture).unwrap();
        assert!(parsed.result.download_urls.len() > 1);
        let ids = parsed.result.download_urls.iter().map(|url| url.id.to_ascii_lowercase()).filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).collect();

        let old_dir = TempDir::new();
        let old_store = store(old_dir.path());
        let old_id = note_with_document(&old_store, &fixture);
        let old_manager = crate::PreviewManager::new(old_store, crate::test_common::TestEnv::new(None, true)).unwrap();
        let old_manifest = old_manager.manifest(&old_id).unwrap();
        let old_preview = old_manager.request(&old_id, 0, 0, 30.0).unwrap();

        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
        let resource = notes_host::snapshot::from_dict(&fixture).unwrap();
        let global = read_global(&store, &identity).unwrap().unwrap();
        let global_urls = crate::payload::parse_payload(global.note_xml.clone(), "application/xml").unwrap().result.download_urls;
        let mut wrapped = b"<CompareNoteView>".to_vec();
        wrapped.extend_from_slice(&global.download_urls);
        wrapped.extend_from_slice(b"</CompareNoteView>");
        let separate_urls = crate::payload::parse_payload(wrapped, "application/xml").unwrap().result.download_urls;
        assert!(!global_urls.is_empty() && !separate_urls.is_empty());
        assert!(rs_store::preview_render_index::capture(&store, &key, &resource.resource_xml, ids).unwrap().is_none());
        let inventory = crate::inventory_worker::compute_static(&fixture).unwrap();
        assert!(store.publish_preview_inventory_with_blob(&key, &rs_store::preview_inventory::InventoryOutcome::Ready(inventory), None).unwrap());
        let trace = rs_store::read_trace::Session::for_container(dir.path(), &key.container_sha256);
        let manager = crate::PreviewManager::new(store, crate::test_common::TestEnv::new(None, true)).unwrap();
        assert_eq!(manager.manifest(&identity).unwrap(), old_manifest);
        assert_eq!(manager.request(&identity, 0, 0, 30.0).unwrap(), old_preview);
        let seen = trace.finish();
        assert!(seen.iter().filter(|name| name.starts_with("cloud/pages/")).count() > 1, "full-snapshot fallback was not observed: {seen:?}");
        manager.close();
        manager.join(std::time::Duration::from_secs(5));
        old_manager.close();
        old_manager.join(std::time::Duration::from_secs(5));
    }

    #[test]
    fn local_inputs_match_full_renderer_for_floating_and_page_objects() {
        for text in [
            include_str!("../tests/fixtures/floating_note.json"),
            include_str!("../tests/fixtures/objects_note.json"),
        ] {
            let data = rs_store::val::loads(text).unwrap();
            let fixture = canonical_fixture(data.get("snapshot").unwrap());
            let dir = TempDir::new();
            let store = store(dir.path());
            let identity = note_with_document(&store, &fixture);
            let raw = fixture.get("raw_sha256").unwrap().as_str().unwrap();
            rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
            let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
            publish_index(&store, &key, &fixture);
            let full = crate::ink::prepare::prepare_pages(&fixture, None, true).unwrap();
            assert!(!full.is_empty());
            let count = read_material(&store, &identity, 0, raw)
                .unwrap()
                .unwrap()
                .global
                .index
                .pages
                .len();
            for index in 0..count {
                let Some(material) = read_material(&store, &identity, index as i64, raw).unwrap()
                else {
                    panic!("synthetic page unexpectedly fell back");
                };
                for build in [false, true] {
                    let actual = crate::ink::prepare::prepare_container_page(&material, build);
                    let expected =
                        crate::ink::prepare::prepare_pages(&fixture, Some(index as i64), build)
                            .map(|v| (*v).clone());
                    assert_eq!(actual, expected, "page {index}, build {build}");
                }
                assert_eq!(
                    crate::ink::prepare::forced_container_composite(&material),
                    crate::ink::prepare::forced_composite(&fixture, index as i64)
                );
            }
        }
    }

    #[test]
    fn local_inputs_match_pdf_flow_and_trailing_blank_pages() {
        let cases = rs_store::val::loads(include_str!("../tests/fixtures/ink_cases.json")).unwrap();
        for (name, add_pages) in [
            ("pdf_list", false),
            ("rich_flow", false),
            ("rich_flow", true),
            ("bg_bad", false),
        ] {
            let fixture = canonical_fixture_with_flow_pages(
                cases.get(name).unwrap().get("snapshot").unwrap(),
                add_pages,
            );
            let dir = TempDir::new();
            let store = store(dir.path());
            let identity = note_with_document(&store, &fixture);
            let raw = fixture.get("raw_sha256").unwrap().as_str().unwrap();
            rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
            let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
            publish_index(&store, &key, &fixture);
            let first = read_material(&store, &identity, 0, raw).unwrap().unwrap();
            let count = first.global.index.pages.len();
            if add_pages {
                assert_eq!(count, 3);
            }
            let full = crate::ink::prepare::prepare_pages(&fixture, None, true);
            if name == "pdf_list" {
                assert!(full.as_ref().unwrap().iter().any(|p| p.over_pdf));
            }
            if name == "rich_flow" {
                assert!(full
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|p| String::from_utf8_lossy(&p.body).contains("\"flow\"")));
            }
            if add_pages {
                let last = full
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|p| p.index == 2)
                    .expect("trailing blank page");
                assert!(last.composite);
                assert!(!String::from_utf8_lossy(&last.body).contains("\"flow\""));
            }
            if name == "bg_bad" {
                assert!(full.is_err());
            }
            for index in 0..count {
                let material = read_material(&store, &identity, index as i64, raw)
                    .unwrap()
                    .unwrap();
                for build in [false, true] {
                    let actual = crate::ink::prepare::prepare_container_page(&material, build);
                    let expected =
                        crate::ink::prepare::prepare_pages(&fixture, Some(index as i64), build)
                            .map(|v| (*v).clone());
                    assert_eq!(actual, expected, "{name}, page {index}, build {build}");
                }
                assert_eq!(
                    crate::ink::prepare::forced_container_composite(&material),
                    crate::ink::prepare::forced_composite(&fixture, index as i64)
                );
            }
        }
    }

    #[test]
    fn native_prepared_render_matches_full_snapshot_render() {
        use crate::ink::InkBackend;
        let input =
            rs_store::val::loads(include_str!("../tests/fixtures/objects_note.json")).unwrap();
        let fixture = canonical_fixture(input.get("snapshot").unwrap());
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        let raw = fixture.get("raw_sha256").unwrap().as_str().unwrap();
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
        publish_index(&store, &key, &fixture);
        let material = read_material(&store, &identity, 0, raw).unwrap().unwrap();
        let page = crate::ink::prepare::prepare_container_page(&material, true)
            .unwrap()
            .remove(0);
        let ink = crate::ink::NativeInk::new(crate::env::default_project_root(), None);
        let partial = ink
            .render_prepared(
                &page,
                &|| crate::ink::prepare::forced_container_composite(&material),
                &|_| None,
            )
            .unwrap();
        let full = ink.render_page(&fixture, 0, &|_| None);
        match (partial, full) {
            (Ok(a), Ok(b)) => assert_eq!(a, b),
            (Err(a), Err(b)) => {
                assert_eq!((a.kind, a.code, a.message), (b.kind, b.code, b.message))
            }
            _ => panic!("page-local and full rendering disagree"),
        }
    }

    #[test]
    fn schema7_source_changes_when_raw_snapshot_digest_changes() {
        let dir = TempDir::new();
        let store = store(dir.path());
        let source = rs_store::val::loads(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let fixture = canonical_fixture(&source);
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let manager =
            crate::PreviewManager::new(store.clone(), crate::test_common::TestEnv::new(None, true))
                .unwrap();
        let before = manager.source(&identity).unwrap();
        store
            .with_db(|db| {
                db.exec(
                    "UPDATE notes SET raw_sha256=? WHERE id=?",
                    &rs_store::p!["0".repeat(64), &identity],
                )?;
                Ok(())
            })
            .unwrap();
        assert_ne!(before, manager.source(&identity).unwrap());
        manager.close();
    }

    #[test]
    fn manifest_and_page_request_read_only_the_selected_page_entries() {
        let data =
            rs_store::val::loads(include_str!("../tests/fixtures/objects_note.json")).unwrap();
        let original = data.get("snapshot").unwrap();
        let source = notes_host::snapshot::from_dict(original).unwrap();
        let resource = rs_doc::xml::fromstring(&source.resource_xml).unwrap();
        let pages = resource
            .children
            .iter()
            .find(|e| e.local() == "pagelist")
            .unwrap();
        let mut raw = source.raw_body.clone();
        for (i, page) in pages.children.iter().enumerate() {
            let old = page.get("id").unwrap().as_bytes();
            let new = format!(
                "10000000-0000-4000-8000-{:012x}",
                std::process::id() as u64 * 100 + i as u64
            );
            let mut replaced = Vec::new();
            let mut cursor = 0;
            while let Some(at) = raw[cursor..].windows(old.len()).position(|w| w == old) {
                let at = cursor + at;
                replaced.extend_from_slice(&raw[cursor..at]);
                replaced.extend_from_slice(new.as_bytes());
                cursor = at + old.len();
            }
            replaced.extend_from_slice(&raw[cursor..]);
            raw = replaced;
        }
        let changed = notes_host::snapshot::to_dict(
            &notes_host::snapshot::parse_document(
                &raw,
                &source.content_type,
                source.server_id.as_deref(),
            )
            .unwrap(),
        );
        let fixture = canonical_fixture(&changed);
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let keys = store
            .pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1)
            .unwrap();
        publish_index(&store, &keys[0], &fixture);
        let ids = store
            .with_db(|db| {
                let rows = db.query(
                    "SELECT page_id FROM note_pages WHERE note_id=? ORDER BY idx",
                    &rs_store::p![&identity],
                )?;
                Ok(rows
                    .iter()
                    .map(|row| row.text("page_id").unwrap().to_owned())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert!(ids.len() >= 3);
        let global = read_global(&store, &identity).unwrap().unwrap();
        let note_result = crate::payload::parse_payload(global.note_xml.clone(), "application/xml");
        let url_result = if global.download_urls.is_empty() {
            None
        } else {
            let mut xml = b"<CompareNoteView>".to_vec();
            xml.extend_from_slice(&global.download_urls);
            xml.extend_from_slice(b"</CompareNoteView>");
            Some(crate::payload::parse_payload(xml, "application/xml"))
        };
        assert!(
            note_result.is_ok(),
            "global note failed: {:?}",
            note_result.err()
        );
        assert!(
            url_result.as_ref().is_none_or(|r| r.is_ok()),
            "global URLs failed: {:?}",
            url_result.and_then(Result::err)
        );
        let trace = rs_store::read_trace::Session::for_container(dir.path(), &keys[0].container_sha256);
        let manager =
            crate::PreviewManager::new(store.clone(), crate::test_common::TestEnv::new(None, true))
                .unwrap();
        let manifest = manager.manifest(&identity).unwrap();
        assert!(!manifest.get("pending").is_some_and(|v| v.truthy()));
        assert_eq!(
            manifest
                .get("pages")
                .and_then(|v| if let rs_store::val::Val::List(a) = v {
                    Some(a.len())
                } else {
                    None
                }),
            Some(ids.len())
        );
        let png = manager
            .request(&identity, 0, 0, 30.0)
            .unwrap()
            .expect("page ready");
        assert!(crate::png_valid(&png, 1080, 1527));
        manager.close();
        manager.join(std::time::Duration::from_secs(5));
        let seen = trace.finish();
        assert!(
            seen.contains(&format!("{}.page", ids[0]))
                && seen.contains(&format!("cloud/pages/{}.xml", ids[0])),
            "{seen:?}"
        );
        assert!(
            seen.iter().all(|name| *name == format!("{}.page", ids[0])
                || *name == format!("cloud/pages/{}.xml", ids[0])),
            "{seen:?}"
        );
    }

    #[test]
    fn pending_manifest_retries_until_inventory_is_ready() {
        let source = rs_store::val::loads(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let fixture = canonical_fixture(&source);
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let held = crate::inventory_worker::hold_computation();
        let manager =
            crate::PreviewManager::new(store.clone(), crate::test_common::TestEnv::new(None, true))
                .unwrap();
        assert!(manager
            .manifest(&identity)
            .unwrap()
            .get("pending")
            .is_some_and(|v| v.truthy()));
        assert!(manager.request(&identity, 0, 0, 0.0).unwrap().is_none());
        drop(held);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let manifest = manager.manifest(&identity).unwrap();
            if !manifest.get("pending").is_some_and(|v| v.truthy()) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "inventory never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(manager.request(&identity, 0, 0, 30.0).unwrap().is_some());
        manager.close();
        manager.join(std::time::Duration::from_secs(5));
    }

    #[test]
    fn shutdown_respects_deadline_while_inventory_worker_is_busy() {
        let source = rs_store::val::loads(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let fixture = canonical_fixture(&source);
        let dir = TempDir::new();
        let store = store(dir.path());
        note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let held = crate::inventory_worker::hold_computation();
        let manager =
            crate::PreviewManager::new(store, crate::test_common::TestEnv::new(None, true))
                .unwrap();
        manager.close();
        let start = std::time::Instant::now();
        manager.join(std::time::Duration::from_millis(40));
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
        drop(held);
        manager.join(std::time::Duration::from_secs(2));
    }

    #[test]
    fn layered_ink_request_reads_only_the_selected_page_entries() {
        use crate::ink::InkBackend;
        let native = crate::ink::NativeInk::new(crate::env::default_project_root(), None);
        if !native.provider_available() {
            return;
        }
        let cases = rs_store::val::loads(include_str!("../tests/fixtures/ink_cases.json")).unwrap();
        let fixture = canonical_fixture_with_flow_pages(
            cases.get("fountain18").unwrap().get("snapshot").unwrap(),
            true,
        );
        let source = notes_host::snapshot::from_dict(&fixture).unwrap();
        let resource = rs_doc::xml::fromstring(&source.resource_xml).unwrap();
        let page_list = resource
            .children
            .iter()
            .find(|e| e.local() == "pagelist")
            .unwrap();
        let mut raw = source.raw_body.clone();
        for (i, page) in page_list.children.iter().enumerate() {
            let old = page.get("id").unwrap().as_bytes();
            let new = format!(
                "20000000-0000-4000-8000-{:012x}",
                std::process::id() as u64 * 100 + i as u64
            );
            let mut replaced = Vec::new();
            let mut cursor = 0;
            while let Some(at) = raw[cursor..].windows(old.len()).position(|w| w == old) {
                let at = cursor + at;
                replaced.extend_from_slice(&raw[cursor..at]);
                replaced.extend_from_slice(new.as_bytes());
                cursor = at + old.len();
            }
            replaced.extend_from_slice(&raw[cursor..]);
            raw = replaced;
        }
        let fixture = notes_host::snapshot::to_dict(
            &notes_host::snapshot::parse_document(
                &raw,
                &source.content_type,
                source.server_id.as_deref(),
            )
            .unwrap(),
        );
        let (expected_base, _, complete) = native.split_ink_page(&fixture, 0, &|_| None).unwrap();
        assert!(complete);
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store
            .pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1)
            .unwrap()
            .remove(0);
        publish_index(&store, &key, &fixture);
        let rows = store
            .with_db(|db| {
                db.query(
                    "SELECT page_id FROM note_pages WHERE note_id=? ORDER BY idx",
                    &rs_store::p![&identity],
                )
            })
            .unwrap();
        let ids: Vec<String> = rows
            .iter()
            .map(|row| row.text("page_id").unwrap().to_owned())
            .collect();
        let trace = rs_store::read_trace::Session::for_container(dir.path(), &key.container_sha256);
        let manager =
            crate::PreviewManager::new(store.clone(), crate::test_common::TestEnv::new(None, true))
                .unwrap();
        let revision = store
            .with_db(|db| {
                db.query(
                    "SELECT revision FROM notes WHERE id=?",
                    &rs_store::p![&identity],
                )
            })
            .unwrap()
            .one()
            .unwrap()
            .int("revision");
        let (base, token) = manager
            .ink_layer(&identity, 0, revision, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(base, expected_base);
        let (tile, same) = manager
            .ink_layer(&identity, 0, revision, Some((3, 0, 0)), Some(&token))
            .unwrap()
            .unwrap();
        assert_eq!(token, same);
        assert!(crate::png_valid(&tile, 512, 512));
        manager.close();
        manager.join(std::time::Duration::from_secs(5));
        let seen = trace.finish();
        assert!(
            seen.contains(&format!("{}.page", ids[0]))
                && seen.contains(&format!("cloud/pages/{}.xml", ids[0])),
            "{seen:?}"
        );
        assert!(
            seen.iter().all(|name| *name == format!("{}.page", ids[0])
                || *name == format!("cloud/pages/{}.xml", ids[0])),
            "{seen:?}"
        );
    }
}
