//! Deterministic mutation fuzzing of the hostile-input document pipeline:
//! Cloud bytes -> parse_document -> projections (editor, ink, page state,
//! shapes, geometry). Every input must produce Ok or Err, never a panic.
//!
//! Seeds are the synthetic snapshots in tests/fixtures/cases.json. Set
//! `NOTES_FUZZ_ITERS` to run longer locally and `NOTES_FUZZ_SEEDS=<dir>` to add
//! extra synthetic seeds (`*.bin` raw body + `*.ct` content type).
use std::sync::atomic::{AtomicUsize, Ordering};

/// Parsed documents, editable editor projections, ink projections (coverage hint).
static STATS: [AtomicUsize; 3] = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];

#[path = "support/fuzzkit.rs"]
mod fuzzkit;

use fuzzkit::b64_decode;
use notes_host::{areas, snapshot};
use rs_store::val::Val;

/// The engine's hostile-document pipeline.
fn pipeline(raw: &[u8], content_type: &str) {
    let _ = rs_doc::doc::page_inventory(raw);
    let _ = rs_editor::edit::editor_content_fingerprint(raw);
    let _ = rs_editor::edit::title_content_fingerprint(raw);
    let Ok(snap) = snapshot::parse_document(raw, content_type, Some("srv-fuzz")) else { return };
    STATS[0].fetch_add(1, Ordering::Relaxed);
    let _ = rs_doc::doc::page_inventory(&snap.resource_xml);
    let _ = rs_editor::edit::editor_content_fingerprint(&snap.resource_xml);
    let _ = rs_editor::edit::title_content_fingerprint(&snap.resource_xml);
    let dict = snapshot::to_dict(&snap);
    if areas::editor_projection(&snap).is_ok_and(|v| !matches!(v.get("model"), None | Some(Val::Null))) {
        STATS[1].fetch_add(1, Ordering::Relaxed);
    }
    let _ = areas::editor_projection_val(&dict);
    let _ = areas::project_ink_snapshot(&snap, true);
    let ink = areas::project_ink(&dict, true);
    let _ = areas::page_background_state(Some(&dict));
    let _ = areas::page_ops_state(Some(&dict));
    let _ = areas::project_shapes(&dict);
    if ink.is_ok() {
        STATS[2].fetch_add(1, Ordering::Relaxed);
    }
    if let Ok(ink) = ink {
        if let Some(Val::List(pages)) = ink.get("pages") {
            for p in pages {
                if let Some(Val::Str(id)) = p.get("id") {
                    let _ = areas::project_page_geometry(&dict, id);
                }
            }
        }
    }
}

fn seeds() -> Vec<(Vec<u8>, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cases.json");
    let fx = rs_store::val::loads(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut out = Vec::new();
    if let Some(Val::Dict(docs)) = fx.get("docs") {
        for (_, doc) in docs {
            let (Some(Val::Str(ct)), Some(Val::Str(raw))) = (doc.get("content_type"), doc.get("raw_body")) else { continue };
            if let Some(raw) = b64_decode(raw.as_bytes()) {
                out.push((raw, ct.clone()));
            }
        }
    }
    if let Ok(dir) = std::env::var("NOTES_FUZZ_SEEDS") {
        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        names.sort();
        for p in names {
            if p.extension().is_some_and(|e| e == "bin") {
                let ct = std::fs::read_to_string(p.with_extension("ct")).unwrap_or_default();
                out.push((std::fs::read(&p).unwrap(), ct));
            }
        }
    }
    out
}

#[test]
fn hostile_documents_never_panic() {
    let failures = fuzzkit::run("documents", 0x9e37_79b9_7f4a_7c15, 150, seeds(), pipeline);
    eprintln!("parsed/editable/ink = {:?}", STATS.iter().map(|s| s.load(Ordering::Relaxed)).collect::<Vec<_>>());
    assert!(failures.is_empty(), "panics at: {failures:?}");
}
