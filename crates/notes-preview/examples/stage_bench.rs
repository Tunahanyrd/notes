//! Cold synthetic preview benchmark. No network or user library access.
//! Run: cargo run --release -p notes-preview --example stage_bench -- 5
#[path = "../tests/common/mod.rs"]
mod common;
use common::*;
use notes_preview::manager::{Entry, PreviewOptions, Source};
use notes_preview::{Candidate, PreviewManager, Val};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let n: usize = std::env::args().nth(1).unwrap_or_else(|| "5".into()).parse().unwrap();
    assert!(n > 0);
    let temp = TempDir::new();
    let log = temp.path().join("timing.jsonl");
    std::env::set_var("NOTES_PREVIEW_TIMING", &log);
    let fixture = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/objects_note.json")).unwrap();
    let fixture = rs_store::val::loads(&fixture).unwrap();
    let snapshot = fixture.get("snapshot").unwrap().clone();
    let Val::Dict(images) = fixture.get("images").unwrap() else { panic!() };
    let images: BTreeMap<String, Vec<u8>> = images.iter().map(|(k, v)| (k.clone(), rs_editor::pyval::b64decode_strict(v.as_str().unwrap()).unwrap())).collect();
    let spi = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/spi/alpha-literal-65x65.spi")).unwrap();
    for i in 0..n {
        for kind in ["canvas", "stand-in", "ink", "pdf"] {
            let dir = temp.path().join(format!("{kind}-{i}"));
            std::fs::create_dir(&dir).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let store = store(&dir);
            let env = TestEnv::new(Some(&dir), false);
            let (id, options) = if kind == "canvas" {
                let mut c = candidate(&spi); c.width = 65; c.height = 65;
                let source = Source(vec![Val::s("synthetic")]);
                let entry = Entry { source: source.clone(), pages: vec![], candidates: BTreeMap::from([(0, Candidate::Canvas(c))]) };
                let raw = spi.clone();
                ("synthetic".to_string(), PreviewOptions {
                    entry_hook: Some(Arc::new(move |_| Ok(entry.clone()))),
                    source_hook: Some(Arc::new(move |_| Ok(source.clone()))),
                    fetcher: Some(Arc::new(move |_, _, _| Ok(raw.clone()))), ..Default::default()
                })
            } else if kind == "pdf" {
                let xml = pdf_xml(None, "", "false");
                let id = note_with_document(&store, &common::snapshot(xml.as_bytes(), "application/xml"));
                *env.download.lock().unwrap() = Some(Box::new(|_, _, path, _, _| { std::fs::write(path, pdf_fixture()).unwrap(); Ok((200, None)) }));
                (id, PreviewOptions::default())
            } else {
                let id = note_with_document(&store, &snapshot);
                let media = images.clone();
                (id, PreviewOptions {
                    fetcher: Some(Arc::new(|_, _, _| Ok(b"invalid synthetic cache".to_vec()))),
                    media_fetcher: Some(Arc::new(move |c, _, _| media.get(&c.sha256).cloned().ok_or_else(|| notes_preview::Fail::os("missing synthetic image")))),
                    ..Default::default()
                })
            };
            let manager = PreviewManager::with_options(store, env, options).unwrap();
            let page = if kind == "stand-in" { 1 } else { 0 };
            assert!(manager.request(&id, page, 0, 60.0).unwrap().is_some(), "{kind}");
            manager.close(); manager.join(Duration::from_secs(5));
        }
    }
    let mut samples: BTreeMap<String, Vec<Vec<i64>>> = BTreeMap::new();
    for line in std::fs::read_to_string(log).unwrap().lines() {
        let v = rs_store::val::loads(line).unwrap();
        let kind = v.get("kind").unwrap().as_str().unwrap();
        let outcome = v.get("outcome").unwrap().as_str().unwrap();
        let name = if kind == "canvas" && outcome == "stand-in" { "stand-in" } else { kind };
        let row = ["workMs", "downloadMs", "decodeMs", "localMs", "rendererMs", "pngEncodeMs"].iter().map(|key| v.get(key).unwrap().as_i64().unwrap()).collect();
        samples.entry(name.to_string()).or_default().push(row);
    }
    println!("kind work_ms download_ms decode_ms local_ms renderer_ms png_encode_ms (medians; stages overlap)");
    for (kind, rows) in samples {
        let medians: Vec<i64> = (0..6).map(|col| { let mut a: Vec<_> = rows.iter().map(|r| r[col]).collect(); a.sort(); a[a.len()/2] }).collect();
        println!("{kind} {}", medians.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "));
    }
}
