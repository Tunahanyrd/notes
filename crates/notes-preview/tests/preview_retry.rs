//! Synthetic end-to-end retry through the actual preview manager and worker.
mod common;
use notes_preview::{manager::PreviewOptions, PreviewManager, PREVIEW_INVENTORY_RECIPE};
use rs_store::{
    preview_inventory::InventoryOutcome, snapshot_migration::migrate_v7, storage::StoreCore,
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
fn terminal_failure_stays_failed_until_explicit_retry() {
    let temp = common::TempDir::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.path().join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(temp.path(), &path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let store = Arc::new(StoreCore::open(temp.path()).unwrap());
    let key = store
        .pending_preview_inventories(PREVIEW_INVENTORY_RECIPE, 32)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(store
        .publish_preview_inventory(&key, &InventoryOutcome::Failed)
        .unwrap());
    let manager = PreviewManager::with_options(
        store.clone(),
        common::TestEnv::new(None, true),
        PreviewOptions::default(),
    )
    .unwrap();
    assert_eq!(
        manager.manifest(&key.note_id).unwrap_err().code,
        "preview_inventory_failed"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap(),
        Some(InventoryOutcome::Failed)
    );
    assert_eq!(
        manager
            .retry_inventory(&key.note_id)
            .unwrap()
            .get("pending")
            .map(|v| v.truthy()),
        Some(true)
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap()
        {
            Some(InventoryOutcome::Ready(_) | InventoryOutcome::Unsupported) => break,
            Some(InventoryOutcome::Failed) => panic!("retry failed"),
            None => {}
        }
        assert!(Instant::now() < deadline, "retry timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        manager.retry_inventory("unknown-note").unwrap_err().status,
        404
    );
    manager.close();
    manager.join(Duration::from_secs(5));
}
