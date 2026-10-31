//! Exercise migration against the repository's synthetic replay library.
use rusqlite::Connection;
use _store::document_store::DocumentStore;
use _store::snapshot_migration::migrate_v7;
use _store::snapshot_store::load_snapshot;
use _store::val::loads;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn synthetic_replay_library_roundtrips_all_inflight_snapshots() {
    let dir = Temp(std::env::temp_dir().join(format!("schema7-replay-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../notes-engine/conformance/golden/library/main/library.sqlite3");
    let db_path = dir.0.join("library.sqlite3");
    std::fs::copy(source, &db_path).unwrap();
    let original = Connection::open(&db_path).unwrap();
    let queries = [
        ("notes", "id", "base_document", "base_document IS NOT NULL"),
        ("notes", "id", "remote_document", "remote_document IS NOT NULL"),
        ("jobs", "id", "snapshot", "snapshot IS NOT NULL AND state NOT IN ('done','cancelled')"),
        ("organization_jobs", "id", "snapshot", "state NOT IN ('done','cancelled')"),
        ("ink_jobs", "job_id", "current_snapshot", "1"),
        ("ink_erase_jobs", "job_id", "current_snapshot", "1"),
        ("ink_move_jobs", "job_id", "current_snapshot", "1"),
    ];
    let mut snapshots = Vec::new();
    for (table, key, column, condition) in queries {
        let sql = format!("SELECT {key},{column} FROM {table} WHERE {condition}");
        let mut statement = original.prepare(&sql).unwrap();
        for row in statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap() {
            let (id, value) = row.unwrap();
            snapshots.push((table, key, column, id, serde_json::from_str::<serde_json::Value>(&value).unwrap()));
        }
    }
    drop(original);
    migrate_v7(&dir.0, &db_path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
    let db = Connection::open(&db_path).unwrap();
    let store = DocumentStore::open(&dir.0).unwrap();
    for (table, key, column, id, expected) in snapshots {
        let reference_column = match (table, column) {
            ("notes", "base_document") => "container_metadata",
            ("notes", "remote_document") => "remote_container_metadata",
            _ => column,
        };
        let sql = format!("SELECT {reference_column} FROM {table} WHERE {key}=?");
        let text: String = db.query_row(&sql, [&id], |row| row.get(0)).unwrap();
        let value = load_snapshot(&store, &loads(&text).unwrap()).unwrap();
        let encoded = _store::db::dumps_utf8(&value).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&encoded).unwrap(), expected, "{table}.{column} {id}");
    }
}
