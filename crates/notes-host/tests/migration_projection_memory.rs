//! Optional isolated measurement; input must be a synthetic schema-6 seed.
use rs_store::snapshot_migration::ProjectionVerifier;

#[test]
fn measure_synthetic_migration_projection() {
    let Some(path) = std::env::var_os("NOTES_TEST_SYNTHETIC_PROJECTION_DB") else { return };
    let id = std::env::var("NOTES_TEST_SYNTHETIC_PROJECTION_ID").unwrap();
    let mode = std::env::var("NOTES_TEST_SYNTHETIC_PROJECTION_MODE").unwrap();
    let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let source: String = db.query_row("SELECT base_document FROM notes WHERE id=?", [&id], |row| row.get(0)).unwrap();
    drop(db);
    let snapshot = rs_store::val::loads(&source).unwrap();
    drop(source);
    let hashes = match mode.as_str() {
        "legacy" => notes_host::areas::MigrationProjectionVerifier.project_hashes(&snapshot),
        "streaming" => notes_host::migration_projection::StreamingMigrationProjectionVerifier.project_hashes(&snapshot),
        _ => panic!("invalid synthetic measurement mode"),
    }.unwrap();
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let peak = status.lines().find(|line| line.starts_with("VmHWM:")).unwrap();
    println!("projection-mode={mode} editor={} ink={} {peak}", hashes.0, hashes.1);
}
