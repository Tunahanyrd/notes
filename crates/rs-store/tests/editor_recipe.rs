//! Cached editor projections computed under older capability rules are
//! dropped on open for synced Cloud notes only; local models survive.
extern crate _editor;
extern crate _store as store;

use store::db::*;
use store::p;
use store::storage::StoreCore;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("notes-editor-recipe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

#[test]
fn stale_editor_projections_of_synced_notes_are_dropped_once() {
    let dir = temp_dir("stale");
    let store = StoreCore::open(&dir).unwrap();
    store
        .with_db(|db| {
            for (id, state, base, title_ok) in [("cloud", "synced", Some("{}"), 0), ("granted", "synced", Some("{}"), 1), ("local", "pending", None, 0)] {
                db.exec(
                    "INSERT INTO notes (id,resource_id,title,body,created_ms,modified_ms,revision,sync_state,editable,title_editable,loaded,base_document)
                     VALUES (?,?,'t','b',1,1,1,?,0,?,1,?)",
                    &vec![text(id), text(id), text(state), int(title_ok), match base { Some(b) => text(b), None => rusqlite::types::Value::Null }],
                )?;
                db.exec("INSERT INTO editor_documents VALUES (?,1,'{\"old\":true}')", &p![id])?;
            }
            // Simulate a library cached under the previous rules.
            db.exec("DELETE FROM metadata WHERE key='editor_projection_recipe'", &[])?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let store = StoreCore::open(&dir).unwrap();
    let (cached, flags) = store
        .with_db(|db| {
            let cached = db.query("SELECT note_id FROM editor_documents ORDER BY note_id", &[])?;
            let flags = db.query("SELECT id, title_editable IS NULL FROM notes ORDER BY id", &[])?;
            Ok((cached.iter().map(|r| r.text("note_id").unwrap().to_string()).collect::<Vec<_>>(),
                flags.iter().map(|r| (r.text("id").unwrap().to_string(), r.int("title_editable IS NULL"))).collect::<Vec<(String, i64)>>()))
        })
        .unwrap();
    assert_eq!(cached, vec!["local".to_string()], "only the synced Cloud note's projection is recomputed");
    assert_eq!(flags, vec![("cloud".into(), 1), ("granted".into(), 0), ("local".into(), 0)]);
    assert_eq!(store.meta("editor_projection_recipe").unwrap().unwrap().as_str(), Some(_editor::edit::PROJECTION_RECIPE));
    // A second open with the current recipe keeps fresh projections.
    store.with_db(|db| db.exec("INSERT INTO editor_documents VALUES ('cloud',1,'{}')", &[]).map(|_| ())).unwrap();
    drop(store);
    let store = StoreCore::open(&dir).unwrap();
    let n = store.with_db(|db| Ok(db.query("SELECT count(*) AS n FROM editor_documents", &[])?.iter().next().unwrap().int("n"))).unwrap();
    assert_eq!(n, 2, "local kept + fresh cloud kept");
    let _ = std::fs::remove_dir_all(&dir);
}
