use _store::document_store::DocumentStore;
use _store::snapshot_store::{load_snapshot_cached, load_snapshot_from_file, raw_reference, store_snapshot};
use _store::val::{loads, py_eq};
use std::os::unix::fs::PermissionsExt;

#[test]
fn cached_snapshot_does_not_mask_container_corruption() {
    let directory = std::env::temp_dir().join(format!("snapshot-cache-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = DocumentStore::open(&directory).unwrap();
    let snapshot = loads(include_str!("../../notes-host/tests/fixtures/ink_objects_base.json")).unwrap();
    let reference = store.with_write(|writer| store_snapshot(writer, &snapshot, None)).unwrap();
    store.with_write(|_| {
        assert!(py_eq(&load_snapshot_cached(&store, &directory, &reference)?, &snapshot));
        assert!(py_eq(&load_snapshot_cached(&store, &directory, &reference)?, &snapshot));
        Ok(())
    }).unwrap();
    let raw = raw_reference(&reference).unwrap();
    let path = directory.join("sdocx").join(&raw.sha256[..2]).join(&raw.sha256[2..4]).join(format!("{}.sdocx", raw.sha256));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let offset = zip.by_name("cloud/note.xml").unwrap().data_start().unwrap() as usize;
    drop(zip);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[offset] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(store.with_write(|_| load_snapshot_cached(&store, &directory, &reference)).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn pinned_container_remains_readable_after_unreferenced_collection() {
    let directory = std::env::temp_dir().join(format!("snapshot-pinned-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = DocumentStore::open(&directory).unwrap();
    let snapshot = loads(include_str!("../../notes-host/tests/fixtures/ink_objects_base.json")).unwrap();
    let reference = store.with_write(|writer| store_snapshot(writer, &snapshot, None)).unwrap();
    let raw = raw_reference(&reference).unwrap();
    let file = store.with_write(|writer| writer.open_raw(&raw)).unwrap();
    let collected = store.collect_raw(std::time::Duration::ZERO, || Ok(vec![])).unwrap();
    assert_eq!(collected.removed, 1);
    assert!(store.open_raw(&raw).is_err());
    assert!(py_eq(&load_snapshot_from_file(&reference, file).unwrap(), &snapshot));
    std::fs::remove_dir_all(directory).unwrap();
}
