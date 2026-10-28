//! Ports of tests/test_ink_previews.py plus golden `_prepare_pages` output
//! of the pure Python oracle (tests/fixtures/ink_cases.json, synthetic
//! documents built by tests/test_ink_render.source()).
mod common;
use common::*;
use notes_preview::ink::{prepare, InkBackend, InkPage};
use notes_preview::manager::PreviewOptions;
use notes_preview::{Candidate, Fail, PreviewManager, ThumbnailManager, Val};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn cases() -> Vec<(String, Val)> {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ink_cases.json")).unwrap();
    let Val::Dict(items) = rs_store::val::loads(&text).unwrap() else { panic!() };
    items
}

/// Oracle cases whose pages the renderer refused (whole note or page) and
/// that are now drawn by the local page composite (intended, 2026-10-28):
/// an unadmitted pen is approximated, unsupported body formatting is dropped.
const COMPOSITE_NOW: [&str; 3] = ["unsupported_pen", "hyper_text", "plain_append"];

#[test]
fn prepare_pages_matches_python_oracle_byte_for_byte() {
    for (name, case) in cases() {
        let snapshot = case.get("snapshot").unwrap();
        let meta = prepare::prepare_pages(snapshot, None, false);
        if COMPOSITE_NOW.contains(&name.as_str()) {
            // Formerly refused: every page is now a composite with a notice.
            let pages = meta.unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(!pages.is_empty() && pages.iter().all(|p| p.composite && p.notice.is_some()), "{name}");
            if let Val::List(old) = case.get("inventory").unwrap() {
                assert!(old.is_empty(), "{name}: no page was admitted before");
            }
            continue;
        }
        if name == "pdf_list" {
            // A PDF page keeps its Poppler background; its own strokes are now
            // also prepared as a local layer over it (2026-10-28).
            let pages = meta.unwrap();
            assert!(!pages.is_empty() && pages.iter().all(|p| p.over_pdf && p.composite), "{name}");
            assert_eq!(case.get("inventory").unwrap(), &Val::List(vec![]), "{name}");
            continue;
        }
        match case.get("inventory").unwrap() {
            Val::Null => assert!(meta.is_err(), "{name}: expected failure"),
            Val::List(expected) => {
                let pages = meta.unwrap_or_else(|e| panic!("{name}: {e:?}"));
                assert!(pages.iter().all(|p| !p.composite), "{name}");
                assert_eq!(pages.len(), expected.len(), "{name}");
                for (p, e) in pages.iter().zip(expected) {
                    assert_eq!(p.index, e.get("index").unwrap().as_i64().unwrap(), "{name}");
                    assert_eq!(p.width, e.get("width").unwrap().as_i64().unwrap(), "{name}");
                    assert_eq!(p.height, e.get("height").unwrap().as_i64().unwrap(), "{name}");
                    assert_eq!(p.dark_mode, e.get("darkMode").unwrap().truthy(), "{name}");
                }
            }
            _ => panic!(),
        }
        let built = prepare::prepare_pages(snapshot, None, true);
        match case.get("bodies").unwrap() {
            Val::Null => assert!(built.is_err(), "{name}: expected failure"),
            Val::Dict(bodies) => {
                let pages = built.unwrap();
                assert_eq!(pages.len(), bodies.len(), "{name}");
                for (index, body) in bodies {
                    let page = pages.iter().find(|p| p.index.to_string() == *index).unwrap();
                    assert_eq!(String::from_utf8(page.body.clone()).unwrap(), body.as_str().unwrap(), "{name} page {index}");
                    assert_eq!(prepare::page_body(snapshot, page.index), page.body, "{name}");
                }
            }
            _ => panic!(),
        }
        // The child/in-process inventory JSON form (Python json.dumps).
        let inventory = prepare::inventory_json(snapshot);
        let expected = match case.get("inventory").unwrap() {
            Val::List(items) => format!("[{}]", items.iter().map(|p| format!("{{\"index\": {}, \"width\": {}, \"height\": {}, \"darkMode\": {}}}",
                p.get("index").unwrap().as_i64().unwrap(), p.get("width").unwrap().as_i64().unwrap(),
                p.get("height").unwrap().as_i64().unwrap(), if p.get("darkMode").unwrap().truthy() { "true" } else { "false" }))
                .collect::<Vec<_>>().join(", ")),
            _ => "[]".to_string(),
        };
        assert_eq!(inventory, expected, "{name}");
    }
}

#[test]
fn native_inventory_and_render_when_the_renderer_is_built() {
    let root = notes_preview::env::default_project_root();
    let native = notes_preview::NativeInk::new(root, None);
    let (_, case) = cases().into_iter().find(|(n, _)| n == "fountain18").unwrap();
    let snapshot = case.get("snapshot").unwrap();
    let pages = native.inspect_render_pages(snapshot);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].dark_mode, Some(false));
    if !native.provider_available() {
        eprintln!("ink renderer not built; skipping render");
        return;
    }
    let png = native.render_ink_page(snapshot, 0).unwrap();
    assert!(notes_preview::png_valid(&png, pages[0].width, pages[0].height));
    // A second page request reuses the per-note body cache.
    assert_eq!(native.render_ink_page(snapshot, 0).unwrap(), png);
    assert!(native.render_ink_page(snapshot, 3).is_err());
}

struct FakeInk {
    png: Vec<u8>,
    rendered: Mutex<Vec<(String, i64)>>,
}

impl InkBackend for FakeInk {
    fn provider_available(&self) -> bool {
        true
    }
    fn inspect_render_pages(&self, _snapshot: &Val) -> Vec<InkPage> {
        vec![InkPage { index: 0, width: 8, height: 8, dark_mode: None, ..Default::default() }]
    }
    fn render_ink_page(&self, snapshot: &Val, index: i64) -> Result<Vec<u8>, Fail> {
        self.rendered.lock().unwrap().push((snapshot.get("raw_sha256").unwrap().as_str().unwrap().to_string(), index));
        Ok(self.png.clone())
    }
}

fn ready(m: &PreviewManager, id: &str) -> Vec<u8> {
    for _ in 0..200 {
        if let Some(v) = m.request(id, 0, 0, 0.0).unwrap() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("local render did not finish");
}

#[test]
fn offline_render_thumbnail_and_restart_do_not_require_cloud_or_native_canvas() {
    let dir = TempDir::new();
    let s = store(dir.path());
    let (_, case) = cases().into_iter().find(|(n, _)| n == "plain_append").unwrap();
    let snapshot = case.get("snapshot").unwrap().clone();
    let id = note_with_document(&s, &snapshot);
    let env = TestEnv::new(None, true);
    *env.headers.lock().unwrap() = Err(Fail::new(notes_preview::Kind::Other, "unexpected network"));
    let fake = Arc::new(FakeInk { png: png(8, 8, &[255; 256]), rendered: Mutex::new(vec![]) });
    let previews = Arc::new(PreviewManager::with_options(s.clone(), env.clone(), PreviewOptions { ink: Some(fake.clone()), ..Default::default() }).unwrap());
    let manifest = previews.manifest(&id).unwrap();
    assert_eq!(manifest.get("providerAvailable"), Some(&Val::Bool(true)));
    let Some(Val::List(pages)) = manifest.get("pages") else { panic!() };
    assert_eq!(pages[0].get("previewKind"), Some(&Val::s("ink-reconstructed")));
    assert_eq!(pages[0].get("complete"), Some(&Val::Bool(true)));
    assert_eq!(ready(&previews, &id), fake.png);
    let thumbnails = ThumbnailManager::new(s.clone(), previews.clone()).unwrap();
    let revision = s.get(&id).unwrap().get("revision").unwrap().as_i64().unwrap();
    let (thumbnail, complete) = thumbnails.request(&id, revision).unwrap();
    assert!(notes_preview::png_valid(&thumbnail.unwrap(), 8, 8));
    assert!(complete);
    let digest = snapshot.get("raw_sha256").unwrap().as_str().unwrap().to_string();
    assert_eq!(*fake.rendered.lock().unwrap(), vec![(digest, 0)]);
    previews.close();
    previews.join(Duration::from_secs(2));

    // Disk cache survives a restart; accounts are separated.
    let reopened = PreviewManager::with_options(s.clone(), env, PreviewOptions { ink: Some(fake.clone()), ..Default::default() }).unwrap();
    assert_eq!(reopened.request(&id, 0, 0, 0.0).unwrap(), Some(fake.png.clone()));
    assert_eq!(fake.rendered.lock().unwrap().len(), 1);
    let candidate: Candidate = reopened.entry(&id).unwrap().candidates[&0].clone();
    let before = reopened.path(&candidate).unwrap();
    s.set_meta("account_key", &Val::s("different-synthetic-account")).unwrap();
    assert_ne!(reopened.path(&candidate).unwrap(), before);
    reopened.close();
}

#[test]
fn self_exec_child_rejects_foreign_argv() {
    assert_eq!(notes_preview::run_child(&["engine".into(), "--port".into()]), None);
    // Bad parent pid -> 71 (no binding), wrong arity -> 71.
    assert_eq!(notes_preview::run_child(&["engine".into(), notes_preview::ink::CHILD_FLAG.into(), "inventory".into(), "1".into()]), Some(71));
    assert_eq!(notes_preview::run_child(&["engine".into(), notes_preview::ink::CHILD_FLAG.into()]), Some(71));
}

#[test]
fn prefetch_warms_first_pages_of_loaded_notes_at_background_priority() {
    let dir = TempDir::new();
    let s = store(dir.path());
    let (_, case) = cases().into_iter().find(|(n, _)| n == "plain_append").unwrap();
    let id = note_with_document(&s, case.get("snapshot").unwrap());
    s.create(&Val::s("Synthetic"), &Val::s("Text only")).unwrap();
    let fake = Arc::new(FakeInk { png: png(8, 8, &[255; 256]), rendered: Mutex::new(vec![]) });
    let previews = PreviewManager::with_options(s, TestEnv::new(None, true), PreviewOptions { ink: Some(fake.clone()), ..Default::default() }).unwrap();
    assert_eq!(previews.prefetch(128).unwrap(), 1);
    assert!(previews.wait_idle(Some(Duration::from_secs(5))));
    assert_eq!(previews.prefetch(128).unwrap(), 0, "cached first pages are not queued again");
    assert_eq!(previews.request(&id, 0, 0, 0.0).unwrap(), Some(fake.png.clone()));
    assert_eq!(fake.rendered.lock().unwrap().len(), 1);
    previews.close();
    previews.join(Duration::from_secs(2));
}

/// Synthetic notes with shape/line objects (generator frozen at
/// reference/python/generators/notes-preview/gen_ink_shape_cases.py).
/// Since 2026-10-28 every shape page is a page composite: shapes and lines
/// (recognised or foreign) are drawn from their saved geometry under the
/// renderer's ink layer, whose input stays byte-identical to the former
/// shape-skipping body. Foreign/odd objects are drawn approximately with a
/// notice instead of refusing the page.
fn shape_cases() -> Vec<(String, Val)> {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ink_shape_cases.json")).unwrap();
    let Val::Dict(items) = rs_store::val::loads(&text).unwrap() else { panic!() };
    items
}

fn composite(body: &[u8]) -> Val {
    assert!(notes_preview::ink::is_composite(body));
    rs_store::val::loads(std::str::from_utf8(body).unwrap()).unwrap()
}

#[test]
fn shape_pages_are_composites_keeping_the_former_ink_input() {
    let cases = shape_cases();
    assert_eq!(cases.len(), 11);
    for (name, case) in cases {
        let snapshot = case.get("snapshot").unwrap();
        let built = prepare::prepare_pages(snapshot, None, true).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let Val::Dict(bodies) = case.get("bodies").unwrap() else { panic!() };
        if name == "empty_page" {
            // No object anywhere: a text note, not a page note.
            assert!(built.is_empty(), "{name}");
            continue;
        }
        assert_eq!(built.len(), 1, "{name}");
        let page = &built[0];
        assert!(page.composite, "{name}");
        let body = composite(&page.body);
        let svg: Vec<&str> = match body.get("items").unwrap() {
            Val::List(items) => items.iter().filter_map(|i| i.get("svg").and_then(Val::as_str)).collect(),
            _ => panic!(),
        };
        match bodies.iter().find(|(i, _)| i == "0") {
            // Formerly admitted with shapes skipped: same renderer input.
            Some((_, old)) => {
                let old = old.as_str().unwrap();
                if old.contains("\"strokes\":[]") {
                    assert_eq!(body.get("renderer"), Some(&Val::Null), "{name}");
                } else {
                    assert_eq!(body.get("renderer").and_then(Val::as_str), Some(old), "{name}");
                }
                assert!(page.notice.is_none(), "{name}: recognised shapes are exact");
            }
            None => assert!(page.notice.is_some(), "{name}: foreign/odd objects are approximate"),
        }
        if name != "textbox_object" {
            assert!(!svg.is_empty() && svg.iter().all(|s| s.contains("<path")), "{name}");
        }
    }
}

#[test]
fn shape_only_page_draws_its_shapes_on_the_page_colour() {
    let (_, case) = shape_cases().into_iter().find(|(n, _)| n == "shapes_only").unwrap();
    let snapshot = case.get("snapshot").unwrap();
    let native = notes_preview::NativeInk::new(notes_preview::env::default_project_root(), None);
    let pages = native.inspect_render_pages(snapshot);
    assert_eq!(pages.len(), 1);
    assert!(pages[0].composite && pages[0].notice.is_none());
    let png = native.render_ink_page(snapshot, 0).unwrap();
    let (w, h) = (pages[0].width, pages[0].height);
    assert!(notes_preview::png_valid(&png, w, h));
    let background = rs_render::pixels::rgba_png(w, h, &rs_render::ink_render::background_pixels(w, h, 0xfffcfcfc)).unwrap();
    assert_ne!(png, background, "the shapes are drawn into the page image");
    let Ok(pixels) = rs_render::pixels::unfilter_png(&png, w, h, 64 << 20) else { panic!("png") };
    assert_eq!(&pixels[..4], [0xfc, 0xfc, 0xfc, 255], "page colour outside the shapes");
    // Only the exact plain-background input qualifies for the old fast path.
    assert_eq!(rs_render::ink_render::background_only(b"{\"schemaVersion\":1,\"width\":8,\"height\":8,\"backgroundArgb\":4294769916,\"strokes\":[],\"flow\":{}}"), None);
    assert_eq!(rs_render::ink_render::background_only(b"{\"schemaVersion\":1,\"width\":8,\"height\":8,\"backgroundArgb\":16579836,\"strokes\":[]}"), None);
}

#[test]
fn pen_templates_carry_the_renderer_pen_of_each_admitted_stroke() {
    // The live pen preview copies these; each must be byte-identical to the
    // `rendering` object the page renderer receives for that stroke.
    let mut seen = 0;
    for (name, case) in cases() {
        let snapshot = case.get("snapshot").unwrap();
        let Ok(text) = prepare::pen_templates(snapshot) else {
            assert!(matches!(case.get("bodies").unwrap(), Val::Null), "{name}: renderable note without pens");
            continue;
        };
        let Val::Dict(pens) = rs_store::val::loads(&text).unwrap() else { panic!("{name}") };
        let bodies: Vec<String> = match case.get("bodies").unwrap() {
            Val::Dict(bodies) => bodies.iter().map(|(_, b)| b.as_str().unwrap().to_string()).collect(),
            _ => Vec::new(),
        };
        for (id, pen) in &pens {
            let rendering = text.split(&format!("\"{id}\":")).nth(1).unwrap();
            let rendering = &rendering[rendering.find("\"rendering\":").unwrap() + 12..];
            let body = bodies.iter().find(|b| b.contains(&format!("{{\"rendering\":{}", &rendering[..rendering.find(",\"style\"").unwrap()])));
            // Pens do not depend on page admission (a refused page has no body).
            assert!(bodies.is_empty() || body.is_some(), "{name}: pen {id} not in a renderer body");
            if body.is_some() {
                seen += 1;
            }
            assert!(pen.get("width").unwrap().as_i64().unwrap() > 0, "{name}");
            let pen_name = pen.get("rendering").unwrap().get("pen_name").unwrap().as_str().unwrap().to_string();
            assert!(pen_name.ends_with(".FountainPen") || pen_name.ends_with(".InkPen2"), "{name}: {pen_name}");
            assert!(!text.contains("\"points\""), "{name}: pens carry no sample geometry");
        }
    }
    assert!(seen >= 2, "fixtures must exercise at least two pens, saw {seen}");
}

#[test]
fn a_page_the_renderer_refuses_is_served_as_a_stand_in_not_left_pending() {
    // The synthetic rich flow page is admitted, but the pinned renderer does
    // not complete its body text: the page is drawn as a composite stand-in
    // (`.partial.png`) and returned, instead of re-queueing forever (before:
    // `request` stayed None until its deadline, the UI showed "açılamadı").
    if !notes_preview::NativeInk::new(notes_preview::env::default_project_root(), None).provider_available() {
        eprintln!("ink renderer not built; skipping render");
        return;
    }
    let (_, case) = cases().into_iter().find(|(n, _)| n == "rich_flow").unwrap();
    let dir = TempDir::new();
    let s = store(dir.path());
    let id = note_with_document(&s, case.get("snapshot").unwrap());
    let m = PreviewManager::new(s, TestEnv::new(None, true)).unwrap();
    let started = std::time::Instant::now();
    let png = m.request(&id, 0, 0, 30.0).unwrap().expect("a page image");
    assert!(started.elapsed() < Duration::from_secs(25));
    assert!(notes_preview::png_valid(&png, 1080, 1527));
    // Served again at once (stand-in on disk).
    assert_eq!(m.request(&id, 0, 0, 0.0).unwrap(), Some(png));
    m.close();
    m.join(Duration::from_secs(2));
}
