//! Explicit preparation under engine.lock; no production data or network.
mod common;
use notes_engine::{
    engine::{Engine, Options},
    launcher::{lock_library, LockError},
    sessions::NoLogin,
    stub_previews::NoPreviewFactory,
};
use rs_store::{
    db::{text, Sql},
    storage::StoreCore,
    val::Val,
};
use std::sync::Arc;

fn open_engine(path: &std::path::Path) -> Arc<Engine> {
    Engine::new(
        path,
        Options {
            offline: true,
            session: None,
            auto_sync: false,
            interval: 3600.0,
            cloud: notes_engine::cloud_factory(),
            previews: Arc::new(NoPreviewFactory),
            login: Arc::new(NoLogin),
        },
    )
    .unwrap()
}

#[test]
fn fresh_library_prepares_under_lock_and_opens_as_schema7() {
    let dir = common::TempDir::new("schema7-fresh");
    let lock = lock_library(dir.path()).unwrap_or_else(|_| panic!("lock failed"));
    assert!(matches!(lock_library(dir.path()), Err(LockError::Busy)));
    notes_engine::prepare_schema7(dir.path()).unwrap();
    let store = StoreCore::open(dir.path()).unwrap();
    assert_eq!(store.meta("schema_version").unwrap(), Some(Val::Int(7)));
    assert_eq!(store.meta("schema7_vacuum_pending").unwrap(), None);
    store
        .with_db(|db| {
            db.exec(
                "INSERT INTO metadata(key,value) VALUES('schema7_vacuum_pending','true')",
                &[],
            )
            .map(|_| ())
        })
        .unwrap();
    assert!(StoreCore::open(dir.path()).is_err());
    // A pending marker without a baseline cannot be trusted or vacuumed.
    assert!(notes_engine::prepare_schema7(dir.path()).is_err());
    store.with_db(|db| db.exec("DELETE FROM metadata WHERE key='schema7_vacuum_pending'", &[]).map(|_| ())).unwrap();
    assert_eq!(
        StoreCore::open(dir.path())
            .unwrap()
            .meta("schema7_vacuum_pending")
            .unwrap(),
        None
    );
    let engine = open_engine(dir.path());
    engine.close();
    drop(lock);
    assert!(lock_library(dir.path()).is_ok());
}

#[test]
fn running_jobs_stop_migration_uncertain_jobs_survive_and_future_schema_fails() {
    let dir = common::TempDir::new("schema7-jobs");
    let store = StoreCore::open(dir.path()).unwrap();
    store.with_db(|db| {
        db.exec("INSERT INTO notes(id,title,body,created_ms,modified_ms,sync_state) VALUES ('n','t','b',1,1,'local')", &[])?;
        db.exec("INSERT INTO jobs VALUES('j','n','create',1,'t','b',NULL,'running',NULL,1)", &[])?;
        Ok(())
    }).unwrap();
    let _lock = lock_library(dir.path()).unwrap_or_else(|_| panic!("lock failed"));
    assert!(notes_engine::prepare_schema7(dir.path()).is_err());
    assert_eq!(store.meta("schema_version").unwrap(), Some(Val::Int(6)));
    store
        .with_db(|db| {
            db.exec("UPDATE jobs SET state='uncertain' WHERE id='j'", &[])
                .map(|_| ())
        })
        .unwrap();
    notes_engine::prepare_schema7(dir.path()).unwrap();
    let migrated = StoreCore::open(dir.path()).unwrap();
    let state = migrated
        .with_db(|db| Ok(db.one("SELECT state FROM jobs WHERE id='j'", &[])?.unwrap()[0].clone()))
        .unwrap();
    assert_eq!(state, text("uncertain"));
    migrated
        .with_db(|db| {
            db.exec(
                "UPDATE metadata SET value='8' WHERE key='schema_version'",
                &[],
            )
            .map(|_| ())
        })
        .unwrap();
    assert!(notes_engine::prepare_schema7(dir.path()).is_err());
    assert_eq!(migrated.meta("schema_version").unwrap(), Some(Val::Int(8)));
}
