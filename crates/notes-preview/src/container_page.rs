//! Revision-bound, verified partial access to a schema 7 note container.
use crate::util::store_fail;
use notes_host::Fail;
use rs_store::db::{self, Sql};
use rs_store::document_store::{DocumentStore, RawDocumentRef};
use rs_store::note_container::{self, PageData};
use rs_store::storage::StoreCore;

/// A page read from the current immutable revision. Both entry hashes are
/// checked against `note_pages`, not merely the container's own manifest.
#[derive(Clone, Debug)]
pub struct IndexedPage {
    pub page_id: String,
    pub index: i64,
    pub revision: i64,
    pub raw_sha256: String,
    pub data: PageData,
    /// Optional derived index independently verified against SQLite.
    pub context: Option<rs_store::page_context::PageContext>,
}

/// Returns `None` for schema 6 or a note without a current container. A
/// missing page, a stale expected digest, an unsplit XML fallback, or failed
/// verification is an error. No other page entry is read.
///
/// Callers must obtain global admission/flow/floating-object context from the
/// same revision before rendering; page bytes alone are not that context.
pub fn read(
    store: &StoreCore,
    identity: &str,
    index: i64,
    expected_sha256: &str,
) -> Result<Option<IndexedPage>, Fail> {
    if index < 0 {
        return Err(Fail::store("page_not_found", "Sayfa bulunamadı.", 404));
    }
    let v7 = store
        .with_db(|db| {
            let rows = db.query("SELECT value FROM metadata WHERE key='schema_version'", &[])?;
            Ok(rows.one().and_then(|r| r.text("value")) == Some("7"))
        })
        .map_err(store_fail)?;
    if !v7 {
        return Ok(None);
    }
    let documents = DocumentStore::open(&store.directory).map_err(store_fail)?;
    documents.with_write(|writer| {
        store.with_db(|db| {
            let rows = db.query("SELECT n.container_sha256,n.container_size,n.raw_sha256,n.revision,p.page_id,p.page_sha256,p.xml_sha256,json_extract(n.container_metadata,'$.context_sha256') AS context_sha256 FROM notes n LEFT JOIN note_pages p ON p.note_id=n.id AND p.idx=? WHERE n.id=?", &rs_store::p![index, identity])?;
            if rows.rows.len() > 1 { return Err(db::Error::Value("duplicate page index".into())); }
            let row = rows.one().ok_or_else(|| db::store_err("not_found", "Not bulunamadı.", 404))?;
            let Some(sha256) = row.text("container_sha256") else { return Ok(None); };
            let raw_sha256 = row.text("raw_sha256").ok_or_else(|| db::Error::Value("missing snapshot digest".into()))?;
            if raw_sha256 != expected_sha256 { return Err(db::store_err("preview_source_changed", "Not değişti.", 409)); }
            let page_id = row.text("page_id").ok_or_else(|| db::store_err("page_not_found", "Sayfa bulunamadı.", 404))?;
            let page_sha256 = row.text("page_sha256").ok_or_else(|| db::Error::Value("missing page hash".into()))?;
            let xml_sha256 = row.text("xml_sha256").ok_or_else(|| db::Error::Value("missing page XML hash".into()))?;
            if xml_sha256.is_empty() { return Err(db::Error::Value("page XML is stored unsplit".into())); }
            let size = row.at(1).as_i64().ok_or_else(|| db::Error::Value("missing container size".into()))?;
            if size < 0 { return Err(db::Error::Value("invalid container size".into())); }
            let reference = RawDocumentRef { sha256: sha256.to_owned(), size: size as u64 };
            let file = writer.open_raw(&reference)?;
            let data = note_container::read_page_verified(file, page_id, page_sha256, xml_sha256).map_err(db::Error::Value)?;
            let context = match row.text("context_sha256") {
                Some(hash) => Some(note_container::read_context(writer.open_raw(&reference)?, hash, raw_sha256).map_err(db::Error::Value)?),
                None => None,
            };
            Ok(Some(IndexedPage { page_id: page_id.to_owned(), index, revision: row.int("revision"), raw_sha256: raw_sha256.to_owned(), data, context }))
        })
    }).map_err(store_fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_common::{note_with_document, store, TempDir};
    use rs_store::db::Sql;

    #[test]
    fn schema7_reads_indexed_page_and_rejects_stale_or_corrupt_index() {
        let dir = TempDir::new();
        let store = store(dir.path());
        let fixture = rs_store::val::loads(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let id = note_with_document(&store, &fixture);
        let sha = fixture.get("raw_sha256").unwrap().as_str().unwrap();
        assert!(read(&store, &id, 0, sha).unwrap().is_none());
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let page = read(&store, &id, 0, sha).unwrap().unwrap();
        assert_eq!(page.index, 0);
        assert!(!page.data.page.is_empty());
        assert!(!page.data.xml.is_empty());
        assert!(page.context.is_some());
        // Local edits can advance the note revision without changing its
        // immutable container or replacing the page index.
        store
            .with_db(|db| {
                db.exec(
                    "UPDATE notes SET revision=revision+1 WHERE id=?",
                    &rs_store::p![&id],
                )?;
                Ok(())
            })
            .unwrap();
        let bumped = read(&store, &id, 0, sha).unwrap().unwrap();
        assert_eq!(bumped.revision, page.revision + 1);
        assert_eq!(bumped.data.page, page.data.page);
        assert!(read(&store, &id, 0, &"0".repeat(64)).is_err());
        assert!(read(&store, &id, 1, sha).is_err());
        store
            .with_db(|db| {
                db.exec(
                    "UPDATE note_pages SET page_sha256=? WHERE note_id=?",
                    &rs_store::p!["0".repeat(64), &id],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(read(&store, &id, 0, sha).is_err());
    }
}
