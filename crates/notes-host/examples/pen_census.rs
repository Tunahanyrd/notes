//! Read-only pen-availability census on a PRIVATE COPY of the library
//! database (never the live file).
//!
//!     cargo run -p notes-host --example pen_census -- <copy.sqlite3>
//!
//! Runs the real `ink_projection` (no network, discarded writer dry runs) for
//! every loaded note and prints counts only: notes per note-level reason or
//! capability, pages per page-level reason or capability, and the object
//! types the pages carry. Reasons are the fixed messages of our own code. It
//! never prints titles, ids, text, geometry or URLs and writes no files.
use std::collections::BTreeMap;

use notes_host::{areas, Val};

type Counts = BTreeMap<String, usize>;

fn bump(c: &mut Counts, k: impl Into<String>) {
    *c.entry(k.into()).or_default() += 1;
}

fn flag(v: &Val, k: &str) -> bool {
    v.get(k).is_some_and(Val::truthy)
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: pen_census <copy.sqlite3>");
    let db = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open copy");
    let mut stmt = db
        .prepare("SELECT base_document, trashed FROM notes WHERE loaded=1 AND deleted_remote=0 AND base_document IS NOT NULL")
        .expect("query");
    let rows: Vec<(String, bool)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)))
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    let (mut notes, mut pages, mut types) = (Counts::new(), Counts::new(), Counts::new());
    for (raw, trashed) in rows {
        let scope = if trashed { "trash" } else { "library" };
        let document = rs_store::val::loads(&raw).expect("json");
        // Object types per page, from the stored resource XML (structure only).
        if let Some(xml) = document.get("resource_xml").and_then(Val::as_str).and_then(|s| rs_doc::util::b64decode_strict(s.as_bytes())) {
            if let Ok(root) = rs_ops::xml::parse_bytes(&xml) {
                if let Some(list) = root.children.iter().find(|c| c.tag == "pageList") {
                    for page in &list.children {
                        let mut kinds: Vec<&str> = page.iter_tag("object").filter_map(|o| o.get("type")).collect();
                        kinds.sort_unstable();
                        kinds.dedup();
                        bump(&mut types, format!("{scope}: [{}]", kinds.join(",")));
                    }
                }
            }
        }
        let projection = match areas::project_ink(&document, false) {
            Ok(p) => p,
            Err(e) => {
                bump(&mut notes, format!("{scope}: projection error {:?}", e.kind));
                continue;
            }
        };
        let caps = projection.get("capabilities").cloned().unwrap_or(Val::Null);
        let note_key = match projection.get("reason").and_then(Val::as_str) {
            Some(r) if !flag(&caps, "eraseInk") => format!("{scope}: disabled: {r}"),
            _ => {
                let on: Vec<&str> = ["appendInk", "firstStroke", "highlighter", "eraseInk"].into_iter().filter(|k| flag(&caps, k)).collect();
                format!("{scope}: pen on [{}]", on.join(","))
            }
        };
        bump(&mut notes, note_key);
        if let Some(Val::List(items)) = projection.get("pages") {
            for page in items {
                let ink = page.get("ink").cloned().unwrap_or(Val::Null);
                let key = match ink.get("reason").and_then(Val::as_str) {
                    Some(r) => format!("{scope}: {r}"),
                    None => {
                        let on: Vec<&str> =
                            ["appendInk", "firstStroke", "highlighter", "eraseInk"].into_iter().filter(|k| flag(&ink, k)).collect();
                        format!("{scope}: ok [{}]", on.join(","))
                    }
                };
                bump(&mut pages, key);
            }
        }
    }
    for (title, counts) in [("notes", &notes), ("projected pages", &pages), ("pages by object types", &types)] {
        println!("{title}:");
        for (k, v) in counts {
            println!("  {v:5}  {k}");
        }
    }
}
