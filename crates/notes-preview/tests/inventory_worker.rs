//! Event-driven inventory preparation over a synthetic library copy.
mod common;
use notes_preview::{
    inventory_worker::{compute_static, InventoryWorker},
    PREVIEW_INVENTORY_RECIPE,
};
use rs_store::{
    db::{sql_to_val, Sql},
    preview_inventory::InventoryOutcome,
    snapshot_migration::migrate_v7,
    storage::StoreCore,
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
fn worker_publishes_static_index_and_stops_cleanly() {
    let temp = common::TempDir::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.path().join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(temp.path(), &path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let store = Arc::new(StoreCore::open(temp.path()).unwrap());
    let keys = store
        .pending_preview_inventories(PREVIEW_INVENTORY_RECIPE, 128)
        .unwrap();
    let (key, snapshot, expected) = keys
        .into_iter()
        .find_map(|key| {
            let snapshot = store.preview_inventory_source(&key).ok().flatten()?;
            let expected = compute_static(&snapshot).ok()?;
            Some((key, snapshot, expected))
        })
        .expect("fixture has a supported synthetic snapshot");
    let expected_ink = notes_preview::ink::prepare::prepare_pages(&snapshot, None, false).unwrap();
    assert_eq!(expected.ink_pages.len(), expected_ink.len());
    for (actual, reference) in expected.ink_pages.iter().zip(expected_ink.iter()) {
        assert_eq!(
            (
                actual.page_index as i64,
                actual.width,
                actual.height,
                actual.composite,
                actual.over_pdf
            ),
            (
                reference.index,
                reference.width,
                reference.height,
                reference.composite,
                reference.over_pdf
            )
        );
    }
    let worker = InventoryWorker::start(store.clone()).unwrap();
    assert!(worker.wake());
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(result) = store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap()
        {
            assert_eq!(result, InventoryOutcome::Ready(expected));
            break;
        }
        assert!(
            Instant::now() < until,
            "worker did not publish; {:?}",
            worker.last_error()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let payload = store
        .with_db(|db| {
            Ok(db
                .one(
                    "SELECT payload FROM preview_inventories WHERE container_sha256=? AND recipe=?",
                    &rs_store::p![&key.container_sha256, PREVIEW_INVENTORY_RECIPE],
                )?
                .map(|row| sql_to_val(&row[0])))
        })
        .unwrap()
        .unwrap();
    let payload = payload.as_str().unwrap().to_ascii_lowercase();
    assert!(!payload.contains("https://") && !payload.contains("http://"));
    assert!(worker.join_for(Duration::from_secs(5)));
    assert!(!worker.wake(), "stopped worker cannot accept work");
}

#[test]
fn static_index_matches_existing_full_inventory_on_synthetic_cases() {
    let cases = rs_store::val::loads(include_str!("fixtures/ink_cases.json")).unwrap();
    let notes_preview::Val::Dict(cases) = cases else {
        panic!("cases")
    };
    let mut compared = 0;
    for (name, case) in cases {
        let snapshot = case.get("snapshot").unwrap();
        let payload = match notes_preview::payload::payload_from_snapshot(snapshot) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let reference = match notes_preview::payload::inspect_ink_payload(&payload) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let actual = compute_static(snapshot).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(actual.pages.len(), reference.page_count, "{name}");
        assert_eq!(
            actual.canvases.len(),
            reference.canvas_candidates.len(),
            "{name}"
        );
        for (a, b) in actual.canvases.iter().zip(&reference.canvas_candidates) {
            assert_eq!(
                (
                    a.page_index,
                    a.width,
                    a.height,
                    a.dark,
                    &a.sha256,
                    &a.bind_number
                ),
                (
                    b.page_index,
                    b.width,
                    b.height,
                    b.is_dark_mode,
                    &b.sha256,
                    &b.bind_number
                ),
                "{name}"
            );
        }
        match notes_preview::pdf::inspect_pdf_backgrounds(&payload) {
            Ok(pdfs) => {
                assert_eq!(actual.pdfs.len(), pdfs.len(), "{name}");
                for (a, b) in actual.pdfs.iter().zip(&pdfs) {
                    assert_eq!(
                        (
                            a.page_index as i64,
                            a.pdf_page_index,
                            a.width,
                            a.height,
                            a.rect,
                            &a.sha256
                        ),
                        (
                            b.page_index,
                            b.pdf_page_index,
                            b.width,
                            b.height,
                            b.rect,
                            &b.sha256
                        ),
                        "{name}"
                    );
                }
            }
            Err(_) => assert!(actual.pdfs.is_empty(), "{name}"),
        }
        match notes_preview::ink::prepare::prepare_pages(snapshot, None, false) {
            Ok(pages) => {
                assert!(actual.ink_supported, "{name}");
                assert_eq!(actual.ink_pages.len(), pages.len(), "{name}");
                for (a, b) in actual.ink_pages.iter().zip(pages.iter()) {
                    assert_eq!(
                        (
                            a.page_index as i64,
                            a.width,
                            a.height,
                            a.dark,
                            a.composite,
                            &a.notice,
                            a.over_pdf
                        ),
                        (
                            b.index,
                            b.width,
                            b.height,
                            b.dark_mode,
                            b.composite,
                            &b.notice,
                            b.over_pdf
                        ),
                        "{name}"
                    );
                }
            }
            Err(_) => {
                assert!(!actual.ink_supported, "{name}");
                assert!(actual.ink_pages.is_empty());
            }
        }
        compared += 1;
    }
    assert!(
        compared >= 10,
        "insufficient supported fixtures: {compared}"
    );
}

#[test]
fn corrupt_source_fails_boundedly_and_only_explicit_retry_restarts_it() {
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
    let file = temp
        .path()
        .join("sdocx")
        .join(&key.container_sha256[..2])
        .join(&key.container_sha256[2..4])
        .join(format!("{}.sdocx", key.container_sha256));
    let original = std::fs::read(&file).unwrap();
    std::fs::write(&file, vec![0; original.len()]).unwrap();
    let worker = InventoryWorker::start(store.clone()).unwrap();
    assert!(worker.wake());
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap()
            == Some(InventoryOutcome::Failed)
        {
            break;
        }
        assert!(Instant::now() < until, "failed source remained pending");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(&file, original).unwrap();
    assert!(worker.wake());
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap(),
        Some(InventoryOutcome::Failed)
    );
    assert!(store
        .retry_preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
        .unwrap());
    assert!(worker.wake());
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        match store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap()
        {
            Some(InventoryOutcome::Ready(_) | InventoryOutcome::Unsupported) => break,
            Some(InventoryOutcome::Failed) => panic!("explicit retry failed"),
            None => {}
        }
        assert!(Instant::now() < until, "explicit retry remained pending");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(worker.join_for(Duration::from_secs(5)));
}
