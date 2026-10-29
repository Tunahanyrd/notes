//! Synthetic large-note benchmark. No network or user-library access.
//! cargo run --release -p notes-preview --example large_note_bench -- ink
//! cargo run --release -p notes-preview --example large_note_bench -- canvas
#[path = "../tests/common/mod.rs"] mod common;
use common::*;
use notes_preview::manager::{Entry, PreviewOptions, Source};
use notes_preview::{Candidate, Fail, PreviewEnv, PreviewManager, Val};
use rs_ops::xml::{self, Element};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const PAGES: usize = 35;
const STROKES: usize = 90;
const TICK: i64 = 1_700_000_000_000_000;
const BOUNDARY: &str = "LargeSynthetic";
struct Offline;
impl PreviewEnv for Offline {
    fn session_path(&self) -> Option<PathBuf> { None }
    fn offline(&self) -> bool { true }
    fn headers(&self) -> Result<Vec<(String, String)>, Fail> { panic!("network forbidden") }
    fn ink_child_executable(&self) -> Option<PathBuf> { std::env::current_exe().ok() }
}
fn uuid(n: usize) -> String { format!("a0000000-0000-4000-8000-{n:012x}") }
fn mutable_path<'a>(mut node: &'a mut Element, path: &[&str]) -> &'a mut Element {
    for name in path { node = node.find_mut(name).unwrap(); }
    node
}
fn synthetic() -> Val {
    let fixture = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes-host/tests/fixtures/ink_objects_base.json")).unwrap();
    let original = rs_store::val::loads(&fixture).unwrap();
    let snap = notes_host::snapshot::from_dict(&original).unwrap();
    let parts = rs_ink::core::prepare::payload_parts(&snap.raw_body, &snap.content_type, &Default::default()).unwrap();
    let stroke = parts.iter().find(|p| p.name.as_deref() == Some("strokeResource")).unwrap();
    // Keep the pen and binary framing from the synthetic fixture, but give
    // every stroke 192 samples instead of the fixture's tiny three-point line.
    let record = rs_ink::core::binary::decode_stroke_resource(&stroke.body).unwrap();
    let mut samples = record.samples.clone();
    let source = samples.clone();
    for i in samples.points.len()..192 {
        let j = i % source.points.len();
        samples.points.push(source.points[j]); samples.pressures.push(source.pressures[j]);
        samples.timestamps.push(i as i64 * 8);
        if record.stylus { samples.tilts.push(source.tilts[j]); samples.orientations.push(source.orientations[j]); }
    }
    let stroke_body = rs_ink::core::binary::replace_stroke_channels_offline(&record, &samples).unwrap();
    let mut root = xml::parse_bytes(&snap.resource_xml).unwrap();
    let page = root.find("pageList").unwrap().children[0].clone();
    let tree_page = root.find("noteTree").unwrap().find("pageList").unwrap().children[0].clone();
    let original_id = page.iter_tag("object").find(|o| o.get("type") == Some("stroke")).unwrap().get("id").unwrap().to_string();
    let offset = stroke_body.windows(original_id.len()).position(|w| w == original_id.as_bytes()).unwrap();
    let mut pages = Vec::new(); let mut tree_pages = Vec::new(); let mut strokes = Vec::new();
    for p in 0..PAGES {
        let mut pg = page.clone(); let mut tp = tree_page.clone();
        let pid = uuid(100_000 + p); let lid = uuid(200_000 + p);
        pg.set("id", &pid); tp.set("id", &pid);
        let layer = &mut mutable_path(&mut pg, &["layerList"]).children[0]; layer.set("id", &lid);
        let tree_layer = &mut mutable_path(&mut tp, &["layerList"]).children[0]; tree_layer.set("id", &lid);
        let objs = mutable_path(layer, &["objectList"]); let template = objs.children[0].clone(); objs.children.clear();
        let tree_objs = mutable_path(tree_layer, &["objectList"]); let tree_template = tree_objs.children[0].clone(); tree_objs.children.clear();
        let mut hashes = Vec::new();
        for j in 0..STROKES {
            let id = uuid(p * STROKES + j + 1); let hash = rs_ops::util::object_hash(&id, TICK).unwrap();
            let hex = rs_ops::util::hex(&hash); hashes.push(hash);
            let mut o = template.clone(); o.set("id", &id); o.set("hash", &hex); objs.children.push(o);
            let mut to = tree_template.clone(); to.set("id", &id); to.set("h", &hex); tree_objs.children.push(to);
            let mut bytes = stroke_body.clone(); bytes[offset..offset + id.len()].copy_from_slice(id.as_bytes());
            strokes.push((id, bytes));
        }
        let layer_hash = rs_ops::util::parent_hash(Some(&lid), TICK, &hashes).unwrap();
        layer.set("hash", &rs_ops::util::hex(&layer_hash)); tree_layer.set("h", &rs_ops::util::hex(&layer_hash));
        let page_hash = rs_ops::util::parent_hash(Some(&pid), TICK, &[layer_hash]).unwrap();
        pg.set("hash", &rs_ops::util::hex(&page_hash)); tp.set("h", &rs_ops::util::hex(&page_hash));
        pages.push(pg); tree_pages.push(tp);
    }
    root.find_mut("pageList").unwrap().children = pages;
    mutable_path(&mut root, &["noteTree", "pageList"]).children = tree_pages;
    let xml = xml::xml_bytes(&root); let start = xml.windows(2).position(|w| w == b"?>").map_or(0, |i| i+2);
    let mut view = b"<CompareNoteView>".to_vec(); view.extend_from_slice(&xml[start..]); view.extend_from_slice(b"<commitId>synthetic-commit</commitId></CompareNoteView>");
    let mut raw = Vec::new();
    fn part(raw: &mut Vec<u8>, name: &str, filename: Option<&str>, mime: &str, body: &[u8]) {
        raw.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes());
        if let Some(f) = filename { raw.extend_from_slice(format!("; filename=\"{f}\"").as_bytes()); }
        raw.extend_from_slice(format!("\r\nContent-Type: {mime}\r\n\r\n").as_bytes()); raw.extend_from_slice(body); raw.extend_from_slice(b"\r\n");
    }
    part(&mut raw, "noteResource", None, "application/xml", &view);
    for (id, body) in strokes { part(&mut raw, "strokeResource", Some(&format!("{id}_{TICK}")), "application/octet-stream", &body); }
    raw.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    let content_type = format!("multipart/form-data;boundary={BOUNDARY}");
    notes_host::snapshot::to_dict(&notes_host::snapshot::parse_document(&raw, &content_type, Some("synthetic")).unwrap())
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = notes_preview::run_child(&args) { std::process::exit(code); }
    let mode = args.get(1).map(String::as_str).unwrap_or("ink"); assert!(mode == "ink" || mode == "canvas");
    let temp = TempDir::new(); let log = temp.path().join("timing.jsonl"); std::env::set_var("NOTES_PREVIEW_TIMING", &log);
    let snap = synthetic(); let store = store(temp.path()); let id = note_with_document(&store, &snap);
    let options = if mode == "canvas" {
        let source = Source(vec![Val::s("synthetic-source")]);
        let mut candidates = BTreeMap::new();
        for p in 0..PAGES { let mut c = candidate(format!("cache-{p}").as_bytes()); c.page_index = p as _; c.width = 1080; c.height = 1527; candidates.insert(p as i64, Candidate::Canvas(c)); }
        let entry = Entry { source: source.clone(), pages: vec![], candidates };
        PreviewOptions { entry_hook: Some(Arc::new(move |_| Ok(entry.clone()))), source_hook: Some(Arc::new(move |_| Ok(source.clone()))), ..Default::default() }
    } else { PreviewOptions::default() };
    let m = PreviewManager::with_options(store, Arc::new(Offline), options).unwrap();
    if mode == "ink" { let entry = m.entry(&id).unwrap(); assert_eq!(entry.candidates.len(), PAGES, "fixture must admit all pages"); }
    for p in std::iter::once(0).chain((1..PAGES).rev()) { assert!(m.request(&id, p as i64, 0, 120.0).unwrap().is_some(), "page {p}"); }
    m.close(); m.join(Duration::from_secs(10));
    let keys = ["workMs", "snapshotMs", "inventoryMs", "bodyMs", "mediaMs", "compositeMs", "rendererMs", "pngEncodeMs", "cacheWriteMs", "rasterMs", "identityMs", "childSpawnMs", "childReadyMs", "childReadMs", "childJsonMs", "childSnapshotMs", "childVerifyMs", "childStrokeDecodeMs", "childInventoryMs", "childPageBuildMs", "childWriteMs"];
    let rows: Vec<Vec<i64>> = std::fs::read_to_string(log).unwrap().lines().map(|line| {
        let v = rs_store::val::loads(line).unwrap(); keys.iter().map(|k| v.get(k).unwrap().as_i64().unwrap()).collect()
    }).collect();
    assert_eq!(rows.len(), PAGES);
    println!("{mode}: {PAGES} pages, {} strokes (192 samples each); first {:?}, second {:?}", PAGES * STROKES, rows[0], rows[1]);
    let med: Vec<_> = (0..keys.len()).map(|col| { let mut v: Vec<_> = rows[1..].iter().map(|r| r[col]).collect(); v.sort(); v[v.len()/2] }).collect();
    println!("columns: {}", keys.join(" "));
    println!("remaining medians: {med:?}; max work {} ms", rows[1..].iter().map(|r| r[0]).max().unwrap());
}
