//! Compare generated benchmark snapshots against migrated container reads.
use _store::{storage::StoreCore, val::loads};
use rusqlite::{Connection, OpenFlags};
use std::{env, path::PathBuf};

fn main() {
    let mut args = env::args_os().skip(1);
    let seed = PathBuf::from(args.next().expect("synthetic seed directory"))
        .canonicalize()
        .expect("seed directory");
    let migrated = PathBuf::from(args.next().expect("synthetic migrated directory"))
        .canonicalize()
        .expect("migrated directory");
    assert!(
        seed.starts_with("/tmp") && migrated.starts_with("/tmp"),
        "synthetic directories only"
    );
    let conn = Connection::open_with_flags(
        seed.join("library.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut stmt = conn
        .prepare("SELECT id,base_document FROM notes WHERE base_document IS NOT NULL ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    let store = StoreCore::open(&migrated).unwrap();
    let mut count = 0;
    for row in rows {
        let (id, old) = row.unwrap();
        let old = loads(&old).unwrap();
        let now = store.raw(&id).unwrap();
        let now = loads(now.get("base_document").and_then(|v| v.as_str()).unwrap()).unwrap();
        for key in ["raw_body", "raw_sha256", "resource_xml", "content_type"] {
            assert_eq!(
                old.get(key),
                now.get(key),
                "synthetic {key} differs after migration"
            );
        }
        count += 1;
    }
    println!("verified {count} snapshot/container round trips");
}
