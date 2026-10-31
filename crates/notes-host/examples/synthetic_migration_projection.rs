//! Profile a single generated migration snapshot without opening user data.
use rs_store::snapshot_migration::ProjectionVerifier;
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let seed = PathBuf::from(args.next().expect("synthetic seed directory"))
        .canonicalize().expect("seed directory");
    assert!(seed.parent().and_then(|p| p.parent()) == Some(std::path::Path::new("/tmp"))
        && seed.parent().and_then(|p| p.file_name()).is_some_and(|s| s.to_string_lossy().starts_with("samsung-mixed-"))
        && seed.file_name().is_some_and(|s| s == "seed"), "synthetic seed only");
    let mode = args.next().expect("legacy or bounded");
    let db = Connection::open_with_flags(seed.join("library.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let json: String = db.query_row("SELECT base_document FROM notes ORDER BY length(base_document) DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    let snapshot = rs_store::val::loads(&json).unwrap();
    drop(json);
    let verifier: Box<dyn ProjectionVerifier> = match mode.to_str() {
        Some("legacy") => Box::new(notes_host::areas::MigrationProjectionVerifier),
        Some("bounded") => Box::new(notes_host::migration_projection::StreamingMigrationProjectionVerifier),
        _ => panic!("legacy or bounded"),
    };
    let start = std::time::Instant::now();
    let (editor, ink) = verifier.project_hashes(&snapshot).unwrap();
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let peak = status.lines().find(|line| line.starts_with("VmHWM:")).unwrap();
    println!("editor={editor} ink={ink} elapsed_ms={} {peak}", start.elapsed().as_millis());
}
