//! Schema 7 compatibility for snapshot readers, using synthetic replay data.
use _store::snapshot_migration::migrate_v7;
use _store::storage::StoreCore;
use rusqlite::Connection;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn v7_readers_restore_snapshots_and_metadata_queries_do_not_open_containers() {
    let temp = Temp(std::env::temp_dir().join(format!("schema7-store-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    let db = Connection::open(&path).unwrap();
    let (id, expected): (String, String) = db
        .query_row(
            "SELECT id,base_document FROM notes WHERE base_document IS NOT NULL LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    drop(db);
    migrate_v7(&temp.0, &path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute("DELETE FROM jobs", []).unwrap();
    db.execute("INSERT INTO jobs (id,note_id,kind,local_revision,title,body,snapshot,state,error,created_ms) SELECT 'test-job',id,'update',revision,title,body,container_metadata,'queued',NULL,1 FROM notes WHERE id=?", [&id]).unwrap();
    drop(db);
    let store = StoreCore {
        directory: temp.0.clone(),
        path,
    };
    let row = store.raw(&id).unwrap();
    let actual = row.get("base_document").unwrap().as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(actual).unwrap(),
        serde_json::from_str::<serde_json::Value>(&expected).unwrap()
    );
    let job = store.next_job().unwrap().unwrap();
    let snapshot = job.get("snapshot").unwrap().as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(snapshot).unwrap(),
        serde_json::from_str::<serde_json::Value>(&expected).unwrap()
    );
    let document = _store::val::loads(snapshot).unwrap();
    let result = _store::val::Val::dict(vec![
        ("server_id", row.get_or_null("server_id")),
        ("local_id", document.get_or_null("local_id")),
        ("snapshot", document.clone()),
    ]);
    assert!(store.complete_job(&Host(true), &job, &result).is_err());
    let db = Connection::open(&store.path).unwrap();
    let (state, retained): (String, bool) = db
        .query_row(
            "SELECT state,snapshot IS NOT NULL FROM jobs WHERE id='test-job'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "queued");
    assert!(retained);
    drop(db);
    store.complete_job(&Host(false), &job, &result).unwrap();
    let db = Connection::open(&store.path).unwrap();
    let (state, no_snapshot): (String, bool) = db
        .query_row(
            "SELECT state,snapshot IS NULL FROM jobs WHERE id='test-job'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "done");
    assert!(no_snapshot);
    let (inline, count): (bool, i64) = db.query_row("SELECT base_document IS NOT NULL,(SELECT COUNT(*) FROM note_container_revisions WHERE note_id=notes.id) FROM notes WHERE id=?", [&id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert!(!inline);
    assert!(count <= 2);
    drop(db);
    let refreshed = store.raw(&id).unwrap();
    let actual = refreshed.get("base_document").unwrap().as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(actual).unwrap(),
        serde_json::from_str::<serde_json::Value>(&expected).unwrap()
    );
    std::fs::rename(temp.0.join("sdocx"), temp.0.join("hidden")).unwrap();
    assert!(store.list("", "all", None).is_ok());
    assert!(store.counts().is_ok());
    assert!(store.get(&id).is_ok());
    assert!(!temp.0.join("sdocx").exists());
}

struct Host(bool);
impl _store::host::Host for Host {
    type Snapshot = ();
    fn apply_favorite_tx(
        &self,
        _: &_store::storage::StoreCore,
        db: &dyn _store::db::Sql,
        id: &str,
        document: &_store::val::Val,
    ) -> _store::db::Result<()> {
        if self.0 {
            return Err(_store::db::Error::Value(
                "synthetic failure after publication".into(),
            ));
        }
        _store::storage::apply_favorite_tx(db, id, document)
    }
    fn normalize_model(&self, _: &_store::val::Val) -> _store::db::Result<_store::val::Val> {
        unimplemented!()
    }
    fn page_background_state(
        &self,
        _: Option<&_store::val::Val>,
    ) -> _store::db::Result<_store::val::Val> {
        unimplemented!()
    }
    fn prepare_page_background(
        &self,
        _: &_store::val::Val,
        _: i64,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn prepare_pdf_pages(
        &self,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn page_ops_state(&self, _: Option<&_store::val::Val>) -> _store::db::Result<_store::val::Val> {
        unimplemented!()
    }
    fn prepare_add_page(
        &self,
        _: &_store::val::Val,
        _: &_store::val::Val,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn prepare_delete_page(
        &self,
        _: &_store::val::Val,
        _: &_store::val::Val,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn prepare_reorder_pages(
        &self,
        _: &_store::val::Val,
        _: &_store::val::Val,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn prepare_add_shape(
        &self,
        _: &_store::val::Val,
        _: &_store::val::Val,
    ) -> _store::host::FaultResult<_store::val::Val> {
        unimplemented!()
    }
    fn prepare_ink_delete(
        &self,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn prepare_ink_move(
        &self,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: &_store::val::Val,
        _: f64,
        _: f64,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn snapshot_from_dict(&self, _: &_store::val::Val) -> _store::db::Result<()> {
        unimplemented!()
    }
    fn prepare_first_stroke(
        &self,
        _: &(),
        _: &str,
        _: &_store::val::Val,
        _: i64,
        _: f64,
        _: i64,
        _: &str,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn prepare_highlighter_stroke(
        &self,
        _: &(),
        _: &str,
        _: &_store::val::Val,
        _: i64,
        _: f64,
        _: i64,
        _: &str,
    ) -> _store::host::FaultResult<()> {
        unimplemented!()
    }
    fn prepare_ink_append(
        &self,
        _: &(),
        _: i64,
        _: &_store::val::Val,
        _: i64,
    ) -> _store::host::FaultResult<_store::val::Val> {
        unimplemented!()
    }
}

#[test]
fn container_gc_marks_jobs_revisions_and_receipts_and_fails_closed() {
    use _store::document_store::DocumentStore;
    use std::time::Duration;
    let temp = Temp(std::env::temp_dir().join(format!("schema7-gc-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    migrate_v7(&temp.0, &path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let docs = DocumentStore::open(&temp.0).unwrap();
    let refs = docs
        .with_write(|writer| {
            (0..11)
                .map(|i| writer.put_raw_bytes(format!("test-container-{i:02}").as_bytes()))
                .collect::<_store::db::Result<Vec<_>>>()
        })
        .unwrap();
    let metadata = |i: usize| {
        serde_json::json!({"container_sha256":refs[i].sha256,"container_size":refs[i].size})
            .to_string()
    };
    let db = Connection::open(&path).unwrap();
    let id: String = db
        .query_row("SELECT id FROM notes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    for (i, state) in ["queued", "running", "uncertain", "failed"]
        .iter()
        .enumerate()
    {
        db.execute(
            "INSERT INTO jobs VALUES(?,?,?,1,'x','x',?,?,NULL,1)",
            rusqlite::params![format!("gc-{i}"), id, "update", metadata(i), state],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO jobs VALUES('gc-done',?,'update',1,'x','x',?,'done',NULL,1)",
        rusqlite::params![id, metadata(4)],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ink_jobs VALUES('gc-0','{}',0,?)",
        [metadata(5)],
    )
    .unwrap();
    db.execute(
        "INSERT INTO organization_jobs VALUES('gc-org',?,'{}',NULL,?,1,'failed',NULL,1)",
        rusqlite::params![id, metadata(6)],
    )
    .unwrap();
    db.execute("INSERT INTO organization_jobs VALUES('gc-legacy',?,'{}',NULL,'{}',1,'uncertain','Eski işlemin kaynak sürümü kayıtlı değil; yeniden gönderilmedi.',1)", [&id]).unwrap();
    db.execute(
        "INSERT INTO note_container_revisions VALUES(?,10000,?)",
        rusqlite::params![id, metadata(7)],
    )
    .unwrap();
    drop(db);
    let receipt_dir = temp.0.join("cloud/operations/test");
    std::fs::create_dir_all(&receipt_dir).unwrap();
    for path in [
        temp.0.join("cloud"),
        temp.0.join("cloud/operations"),
        receipt_dir.clone(),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let receipt = receipt_dir.join("result.json");
    std::fs::write(&receipt, format!("{{\"snapshot\":{}}}", metadata(8))).unwrap();
    std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
    let store = StoreCore {
        directory: temp.0.clone(),
        path,
    };
    let file_path = |i: usize| {
        temp.0
            .join("sdocx")
            .join(&refs[i].sha256[..2])
            .join(&refs[i].sha256[2..4])
            .join(format!("{}.sdocx", refs[i].sha256))
    };
    let report = store.doctor_containers().unwrap();
    assert_eq!((report.missing, report.corrupt), (0, 0));
    let db = Connection::open(&store.path).unwrap();
    db.execute(
        "INSERT INTO organization_jobs VALUES('gc-invalid',?,'{}',NULL,'{}',1,'uncertain',NULL,1)",
        [&id],
    )
    .unwrap();
    drop(db);
    assert!(store.collect_containers(Duration::ZERO).is_err());
    assert!(file_path(9).exists());
    let db = Connection::open(&store.path).unwrap();
    db.execute("DELETE FROM organization_jobs WHERE id='gc-invalid'", [])
        .unwrap();
    drop(db);
    std::fs::write(&receipt, b"{").unwrap();
    assert!(store.collect_containers(Duration::ZERO).is_err());
    assert!(file_path(9).exists(), "failed mark must not sweep");
    std::fs::write(&receipt, format!("{{\"snapshot\":{}}}", metadata(8))).unwrap();
    let report = store.collect_containers(Duration::ZERO).unwrap();
    assert!(report.removed >= 3);
    for i in [0, 1, 2, 3, 5, 6, 7, 8] {
        assert!(file_path(i).exists(), "live reference {i}");
    }
    for i in [4, 9, 10] {
        assert!(!file_path(i).exists(), "orphan {i}");
    }
    std::fs::remove_file(file_path(0)).unwrap();
    std::fs::write(file_path(1), b"damaged-container").unwrap();
    let report = store.doctor_containers().unwrap();
    assert_eq!((report.missing, report.corrupt), (1, 1));
}

#[test]
fn maintenance_uses_grace_retries_failures_and_runs_at_most_daily() {
    use _store::document_store::DocumentStore;
    use std::fs::{File, FileTimes};
    use std::time::{Duration, SystemTime};
    let temp = Temp(std::env::temp_dir().join(format!("schema7-maint-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    StoreCore::prepare_schema7(&temp.0, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let store = StoreCore::open(&temp.0).unwrap();
    let docs = DocumentStore::open(&temp.0).unwrap();
    let reference = docs
        .with_write(|writer| writer.put_raw_bytes(b"old orphan"))
        .unwrap();
    let path = temp
        .0
        .join("sdocx")
        .join(&reference.sha256[..2])
        .join(&reference.sha256[2..4])
        .join(format!("{}.sdocx", reference.sha256));
    File::open(&path)
        .unwrap()
        .set_times(
            FileTimes::new()
                .set_modified(SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60)),
        )
        .unwrap();
    let receipt_dir = temp.0.join("cloud/operations/test");
    std::fs::create_dir_all(&receipt_dir).unwrap();
    for path in [
        temp.0.join("cloud"),
        temp.0.join("cloud/operations"),
        receipt_dir.clone(),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let receipt = receipt_dir.join("result.json");
    std::fs::write(&receipt, b"{").unwrap();
    std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(store.maintain_containers().is_err());
    assert!(path.exists());
    assert_eq!(store.meta("schema7_gc_last_ms").unwrap(), None);
    std::fs::remove_file(receipt).unwrap();
    assert_eq!(store.maintain_containers().unwrap().unwrap().removed, 1);
    assert!(!path.exists());
    assert!(store.maintain_containers().unwrap().is_none());
}

#[test]
fn revision_history_keeps_chronological_a_b_a_revisions() {
    let temp = Temp(std::env::temp_dir().join(format!("schema7-history-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let path = temp.0.join("library.sqlite3");
    std::fs::copy(source, &path).unwrap();
    let db = Connection::open(&path).unwrap();
    let (id, server, original): (String, String, String) = db.query_row("SELECT id,server_id,base_document FROM notes WHERE server_id IS NOT NULL AND base_document IS NOT NULL LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    db.execute("UPDATE notes SET sync_state='synced' WHERE id=?", [&id])
        .unwrap();
    drop(db);
    migrate_v7(&temp.0, &path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let store = StoreCore {
        directory: temp.0.clone(),
        path,
    };
    let a = _store::val::loads(&original).unwrap();
    let mut b = a.clone();
    b.set("commit_id", _store::val::Val::s("synthetic-other-commit"));
    store
        .apply_document(&Host(false), &_store::val::Val::s(&server), &b, None)
        .unwrap();
    store
        .apply_document(&Host(false), &_store::val::Val::s(&server), &a, None)
        .unwrap();
    let db = Connection::open(&store.path).unwrap();
    let mut statement = db.prepare("SELECT container_metadata FROM note_container_revisions WHERE note_id=? ORDER BY revision").unwrap();
    let history: Vec<String> = statement
        .query_map([&id], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(history.len(), 2);
    let first = _store::val::loads(&history[0]).unwrap();
    let second = _store::val::loads(&history[1]).unwrap();
    assert_eq!(
        first.get("commit_id").unwrap().as_str(),
        Some("synthetic-other-commit")
    );
    assert_eq!(second.get("commit_id"), a.get("commit_id"));
}
