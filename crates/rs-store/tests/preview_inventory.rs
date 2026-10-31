//! Static inventory cache lifecycle, using only synthetic replay snapshots.
use _store::{
    preview_inventory::{Inventory, InventoryOutcome, PageSize},
    snapshot_migration::migrate_v7,
    storage::StoreCore,
};
use rusqlite::Connection;
use std::{os::unix::fs::PermissionsExt, path::PathBuf};
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn requested_inventory_preempts_an_existing_background_batch() {
    let temp = Temp(std::env::temp_dir().join(format!("schema7-demand-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(&temp.0, &path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let store = StoreCore::open(&temp.0).unwrap();
    let recipe = "test-demand-v1";
    let batch = store.pending_preview_inventories(recipe, 128).unwrap();
    assert!(batch.len() >= 2);
    let first = &batch[0];
    let requested = batch.last().unwrap();
    assert!(store.preview_inventory(&requested.note_id, recipe).unwrap().is_none());
    assert!(store.preview_inventory_source(first).unwrap().is_none(), "the old batch yields to the requested note");
    let priority = store.pending_preview_inventories(recipe, 128).unwrap();
    assert_eq!(priority.len(), 1);
    assert_eq!(priority[0], *requested);
    assert!(store.preview_inventory_source(&priority[0]).unwrap().is_some());
}

#[test]
fn publication_creates_pending_work_and_cas_keeps_it_current() {
    let temp =
        Temp(std::env::temp_dir().join(format!("schema7-inventory-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(
        &temp.0,
        &path,
        &notes_host::areas::MigrationProjectionVerifier,
    )
    .unwrap();
    let store = StoreCore::open(&temp.0).unwrap();
    let recipe = "test-inventory-v1";
    let key = store
        .pending_preview_inventories(recipe, 32)
        .unwrap()
        .pop()
        .expect("fixture has a loaded cloud note");
    assert!(store
        .preview_inventory(&key.note_id, recipe)
        .unwrap()
        .is_none());
    let db = Connection::open(&path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM note_pages WHERE note_id=?",
            [&key.note_id],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE notes SET revision=revision+1 WHERE id=?",
        [&key.note_id],
    )
    .unwrap();
    drop(db);
    let inventory = Inventory {
        pages: vec![
            PageSize {
                width: Some(1080),
                height: Some(1527)
            };
            count as usize
        ],
        canvases: vec![],
        pdfs: vec![],
        unsupported_pdf_pages: vec![],
        ink_pages: vec![],
        ink_supported: true,
    };
    let ready = InventoryOutcome::Ready(inventory.clone());
    let mut invalid = inventory.clone();
    invalid.pages.push(PageSize {
        width: None,
        height: None,
    });
    assert!(store
        .publish_preview_inventory(&key, &InventoryOutcome::Ready(invalid))
        .is_err());
    assert!(
        store
            .preview_inventory(&key.note_id, recipe)
            .unwrap()
            .is_none(),
        "failed work stays pending"
    );
    assert!(store.publish_preview_inventory(&key, &ready).unwrap());
    assert!(
        store.publish_preview_inventory(&key, &ready).unwrap(),
        "retry is idempotent"
    );
    assert_eq!(
        store.preview_inventory(&key.note_id, recipe).unwrap(),
        Some(ready)
    );
    assert!(
        store
            .publish_preview_inventory(&key, &InventoryOutcome::Unsupported)
            .is_err(),
        "one recipe cannot change meaning"
    );
    assert!(
        store
            .preview_inventory(&key.note_id, "test-inventory-v2")
            .unwrap()
            .is_none(),
        "recipe changes invalidate"
    );
    let key_v2 = store
        .pending_preview_inventories("test-inventory-v2", 128)
        .unwrap()
        .into_iter()
        .find(|item| item.note_id == key.note_id)
        .unwrap();
    assert!(store
        .publish_preview_inventory(&key_v2, &InventoryOutcome::Unsupported)
        .unwrap());
    assert_eq!(
        store
            .preview_inventory(&key.note_id, "test-inventory-v2")
            .unwrap(),
        Some(InventoryOutcome::Unsupported)
    );
    let db = Connection::open(&path).unwrap();
    db.execute(
        "DELETE FROM note_container_revisions WHERE note_id=?",
        [&key.note_id],
    )
    .unwrap();
    db.execute("INSERT INTO preview_inventories(container_sha256,raw_sha256,recipe,state,payload) VALUES(?,?,'orphan-recipe','unsupported',NULL)", rusqlite::params!["3".repeat(64), "4".repeat(64)]).unwrap();
    db.execute(
        "UPDATE notes SET container_sha256=?,raw_sha256=? WHERE id=?",
        rusqlite::params!["1".repeat(64), "2".repeat(64), key.note_id],
    )
    .unwrap();
    drop(db);
    assert!(
        !store
            .publish_preview_inventory(&key, &InventoryOutcome::Unsupported)
            .unwrap(),
        "stale worker must not commit"
    );
    assert!(store
        .preview_inventory(&key.note_id, recipe)
        .unwrap()
        .is_none());
    store.collect_containers(std::time::Duration::ZERO).unwrap();
    let db = Connection::open(&path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM preview_inventories WHERE container_sha256=?",
            ["3".repeat(64)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "unreferenced derived indexes are pruned under GC lock"
    );
}

#[test]
fn failed_inventory_needs_explicit_retry() {
    let temp = Temp(
        std::env::temp_dir().join(format!("schema7-inventory-failed-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(
        &temp.0,
        &path,
        &notes_host::areas::MigrationProjectionVerifier,
    )
    .unwrap();
    let store = StoreCore::open(&temp.0).unwrap();
    let recipe = "test-failure-v1";
    let key = store
        .pending_preview_inventories(recipe, 128)
        .unwrap()
        .pop()
        .unwrap();
    assert!(store
        .publish_preview_inventory(&key, &InventoryOutcome::Failed)
        .unwrap());
    assert_eq!(
        store.preview_inventory(&key.note_id, recipe).unwrap(),
        Some(InventoryOutcome::Failed)
    );
    assert!(!store
        .pending_preview_inventories(recipe, 128)
        .unwrap()
        .iter()
        .any(|pending| pending.container_sha256 == key.container_sha256));
    assert!(store.retry_preview_inventory(&key.note_id, recipe).unwrap());
    assert!(store
        .preview_inventory(&key.note_id, recipe)
        .unwrap()
        .is_none());
    assert!(store
        .pending_preview_inventories(recipe, 128)
        .unwrap()
        .iter()
        .any(|pending| pending.container_sha256 == key.container_sha256));
    assert!(!store.retry_preview_inventory(&key.note_id, recipe).unwrap());
}

#[test]
fn worker_source_rejects_mismatched_duplicated_reference_fields() {
    use _store::document_store::{DocumentStore, RawDocumentRef};
    use std::io::{Cursor, Read, Write};
    let temp =
        Temp(std::env::temp_dir().join(format!("schema7-inventory-ref-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(
        &temp.0,
        &path,
        &notes_host::areas::MigrationProjectionVerifier,
    )
    .unwrap();
    let store = StoreCore::open(&temp.0).unwrap();
    let key = store
        .pending_preview_inventories("reference-test-v1", 32)
        .unwrap()
        .pop()
        .unwrap();
    let original_snapshot = store.preview_inventory_source(&key).unwrap().unwrap();
    let db = Connection::open(&path).unwrap();
    let (size, original_json, pages): (i64, String, i64) = db.query_row("SELECT container_size,container_metadata,(SELECT COUNT(*) FROM note_pages WHERE note_id=notes.id) FROM notes WHERE id=?", [&key.note_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    drop(db);
    let docs = DocumentStore::open(&temp.0).unwrap();
    let source = docs
        .open_raw(&RawDocumentRef {
            sha256: key.container_sha256.clone(),
            size: size as u64,
        })
        .unwrap();
    let mut archive = zip::ZipArchive::new(source).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let name = entry.name().to_owned();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        writer
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(&content).unwrap();
    }
    let alternate = docs
        .with_write(|writer_ref| writer_ref.put_raw_bytes(&writer.finish().unwrap().into_inner()))
        .unwrap();
    assert_ne!(alternate.sha256, key.container_sha256);
    let mut changed: serde_json::Value = serde_json::from_str(&original_json).unwrap();
    changed["container_sha256"] = serde_json::json!(alternate.sha256);
    changed["container_size"] = serde_json::json!(alternate.size);
    let alternate_value = _store::val::loads(&changed.to_string()).unwrap();
    let alternate_snapshot =
        _store::snapshot_store::load_snapshot(&docs, &alternate_value).unwrap();
    assert!(
        _store::val::py_eq(&alternate_snapshot, &original_snapshot),
        "alternate ZIP is independently readable and identical"
    );
    let inventory = Inventory {
        pages: vec![
            PageSize {
                width: None,
                height: None
            };
            pages as usize
        ],
        canvases: vec![],
        pdfs: vec![],
        unsupported_pdf_pages: vec![],
        ink_pages: vec![],
        ink_supported: true,
    };
    assert!(store
        .publish_preview_inventory(&key, &InventoryOutcome::Ready(inventory.clone()))
        .unwrap());
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE notes SET container_metadata=? WHERE id=?",
        rusqlite::params![changed.to_string(), key.note_id],
    )
    .unwrap();
    drop(db);
    assert!(
        store.preview_inventory_source(&key).is_err(),
        "metadata must not redirect worker to another ZIP"
    );
    assert!(
        store.preview_inventory(&key.note_id, &key.recipe).is_err(),
        "cached data must not hide metadata mismatch"
    );
    assert!(
        store
            .publish_preview_inventory(&key, &InventoryOutcome::Ready(inventory))
            .is_err(),
        "CAS must not commit on inconsistent metadata"
    );
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE notes SET container_metadata=? WHERE id=?",
        rusqlite::params![original_json, key.note_id],
    )
    .unwrap();
    drop(db);
    assert!(store.preview_inventory_source(&key).unwrap().is_some());
}

#[test]
fn render_blob_is_cas_bound_and_gc_and_doctor_track_it() {
    let temp =
        Temp(std::env::temp_dir().join(format!("schema7-inventory-blob-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(
        &temp.0,
        &path,
        &notes_host::areas::MigrationProjectionVerifier,
    )
    .unwrap();
    let store = StoreCore::open(&temp.0).unwrap();
    let key = store
        .pending_preview_inventories("blob-test-v1", 32)
        .unwrap()
        .pop()
        .unwrap();
    let db = Connection::open(&path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM note_pages WHERE note_id=?",
            [&key.note_id],
            |r| r.get(0),
        )
        .unwrap();
    let inventory = InventoryOutcome::Ready(Inventory {
        pages: vec![
            PageSize {
                width: None,
                height: None
            };
            count as usize
        ],
        canvases: vec![],
        pdfs: vec![],
        unsupported_pdf_pages: vec![],
        ink_pages: vec![],
        ink_supported: true,
    });
    drop(db);
    let data = b"synthetic render projection";
    assert!(store
        .publish_preview_inventory_with_blob(&key, &inventory, Some(data))
        .unwrap());
    assert_eq!(
        store
            .preview_render_blob(&key.note_id, &key.recipe)
            .unwrap()
            .unwrap(),
        data
    );
    let (cached_ref, fingerprint, bytes) = store
        .preview_render_blob_cached(&key.note_id, &key.recipe, None)
        .unwrap()
        .unwrap();
    assert_eq!(bytes.unwrap(), data);
    assert!(store
        .preview_render_blob_cached(&key.note_id, &key.recipe, Some((&cached_ref, &fingerprint)))
        .unwrap()
        .unwrap()
        .2
        .is_none());
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE preview_inventories SET render_blob_sha256=? WHERE container_sha256=? AND recipe=?",
        rusqlite::params!["1".repeat(64), key.container_sha256, key.recipe],
    )
    .unwrap();
    assert!(store
        .preview_render_blob_cached(&key.note_id, &key.recipe, Some((&cached_ref, &fingerprint)))
        .is_err());
    db.execute(
        "UPDATE preview_inventories SET render_blob_sha256=? WHERE container_sha256=? AND recipe=?",
        rusqlite::params![cached_ref.source_sha256, key.container_sha256, key.recipe],
    )
    .unwrap();
    let reference: String = db
        .query_row(
            "SELECT render_blob FROM preview_inventories WHERE container_sha256=? AND recipe=?",
            rusqlite::params![key.container_sha256, key.recipe],
            |r| r.get(0),
        )
        .unwrap();
    let reference: serde_json::Value = serde_json::from_str(&reference).unwrap();
    let hash = reference["source_sha256"].as_str().unwrap();
    let blob_path = temp
        .0
        .join("documents")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash);
    drop(db);
    let mut stale = key.clone();
    stale.note_id = "missing-note".into();
    assert!(!store
        .publish_preview_inventory_with_blob(&stale, &inventory, Some(b"stale projection"))
        .unwrap());
    assert!(
        store
            .collect_containers(std::time::Duration::ZERO)
            .unwrap()
            .removed
            >= 1,
        "a projection published before a failed CAS must be collected"
    );
    assert!(
        blob_path.exists(),
        "referenced projection must survive collection"
    );
    let size = std::fs::metadata(&blob_path).unwrap().len() as usize;
    std::fs::write(&blob_path, vec![0u8; size]).unwrap();
    assert!(store
        .preview_render_blob_cached(&key.note_id, &key.recipe, Some((&cached_ref, &fingerprint)))
        .is_err());
    assert!(store
        .preview_render_blob(&key.note_id, &key.recipe)
        .is_err());
    assert!(store.doctor_containers().unwrap().corrupt >= 1);
    std::fs::remove_file(&blob_path).unwrap();
    assert!(store.doctor_containers().unwrap().missing >= 1);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE preview_inventories SET render_blob='{}' WHERE container_sha256=? AND recipe=?",
        rusqlite::params![key.container_sha256, key.recipe],
    )
    .unwrap();
    drop(db);
    assert!(
        store.collect_containers(std::time::Duration::ZERO).is_err(),
        "invalid marks must abort GC"
    );
}
