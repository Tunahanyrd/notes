//! Only synthetic fixtures; exercises the SQLite-reference/context binding.
use _store::document_store::DocumentStore;
use _store::note_container;
use _store::snapshot_store::{load_snapshot, raw_reference, store_snapshot};
use _store::val::{loads, py_eq};
use std::os::unix::fs::PermissionsExt;

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("context-store-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn snapshot_reference_binds_context_and_restores_original_fields() {
    let temp = Temp::new();
    let store = DocumentStore::open(&temp.0).unwrap();
    let original = loads(include_str!(
        "../../notes-host/tests/fixtures/ink_objects_base.json"
    ))
    .unwrap();
    store
        .with_write(|writer| {
            let reference = store_snapshot(writer, &original, None)?;
            let expected = reference
                .get("context_sha256")
                .and_then(|v| v.as_str())
                .unwrap();
            let raw = reference
                .get("raw_sha256")
                .and_then(|v| v.as_str())
                .unwrap();
            let file = writer.open_raw(&raw_reference(&reference)?)?;
            let index = note_container::read_context(file, expected, raw).unwrap();
            assert_eq!(index.pages.len(), 1);
            assert!(index.page_based);
            let file = writer.open_raw(&raw_reference(&reference)?)?;
            assert!(note_container::read_context(file, &"0".repeat(64), raw).is_err());
            let restored = load_snapshot(&store, &reference)?;
            assert!(py_eq(&restored, &original));
            assert!(restored.get("context_sha256").is_none());
            Ok(())
        })
        .unwrap();
}
