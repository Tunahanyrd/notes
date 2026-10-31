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

#[test]
fn notext_removes_body_flow_but_keeps_saved_strokes() {
    let (_, case) = cases().into_iter().find(|(name, _)| name == "rich_flow").unwrap();
    let snapshot = case.get("snapshot").unwrap();
    let normal = prepare::forced_composite(snapshot, 0);
    let normal = rs_store::val::loads(std::str::from_utf8(&normal).unwrap()).unwrap();
    let page = prepare::notext_snapshot_page(snapshot, 0).unwrap().unwrap();
    let without = rs_store::val::loads(std::str::from_utf8(&page.body).unwrap()).unwrap();
    let before = normal.get("renderer").and_then(Val::as_str).unwrap();
    let before = rs_store::val::loads(before).unwrap();
    assert!(before.get("flow").is_some());
    let after = without.get("renderer").and_then(Val::as_str).map(|s| rs_store::val::loads(s).unwrap());
    if let Some(after) = after {
        assert!(after.get("flow").is_none());
        assert_eq!(after.get("strokes"), before.get("strokes"));
    } else {
        assert_eq!(before.get("strokes"), Some(&Val::List(vec![])));
    }
}

#[test]
fn text_edit_split_renders_other_text_boxes_in_the_base_and_saved_ink_above() {
    let native=notes_preview::NativeInk::new(notes_preview::env::default_project_root(),None);
    if !native.provider_available() {return;}
    let (_,case)=cases().into_iter().find(|(name,_)|name=="fountain18").unwrap();
    let mut page=prepare::notext_snapshot_page(case.get("snapshot").unwrap(),0).unwrap().unwrap();
    let mut outer=rs_store::val::loads(std::str::from_utf8(&page.body).unwrap()).unwrap();
    let mut renderer=rs_store::val::loads(outer.get("renderer").and_then(Val::as_str).unwrap()).unwrap();
    renderer.set("textBoxes",Val::List(vec![Val::dict(vec![
        ("at",Val::Int(0)),("bbox",Val::List(vec![Val::Int(100),Val::Int(100),Val::Int(700),Val::Int(300)])),
        ("text",Val::s("Synthetic box")),("spans",Val::List(vec![])),("paragraphs",Val::List(vec![])),
        ("margins",Val::List(vec![Val::Int(0);4])),("gravity",Val::Int(0)),
    ])]));
    outer.set("renderer",Val::s(rs_store::val::dumps_utf8(&renderer).unwrap()));
    page.body=rs_store::val::dumps_utf8(&outer).unwrap().into_bytes();
    let (base,svg,complete)=native.split_text_prepared(&page,&|_|None).unwrap().unwrap();
    assert!(complete);
    let pixels=rs_render::pixels::rgba(&base,page.width,page.height).ok().expect("base decodes");
    assert!(pixels.chunks_exact(4).any(|pixel|pixel[..3]!=[252,252,252]),"other text box is painted in the base");
    let ink=rs_render::raster::rasterize(&svg,page.width as u32,page.height as u32).unwrap();
    assert!(ink.chunks_exact(4).any(|pixel|pixel[3]!=0),"saved ink is retained in the upper layer");
}

#[test]
fn text_edit_split_keeps_approximated_saved_pen_in_upper_layer() {
    let native=notes_preview::NativeInk::new(notes_preview::env::default_project_root(),None);
    let (_,case)=cases().into_iter().find(|(name,_)|name=="unsupported_pen").unwrap();
    let page=prepare::text_ink_snapshot_page(case.get("snapshot").unwrap(),0).unwrap().unwrap();
    let (base,svg,complete)=native.split_text_prepared(&page,&|_|None).unwrap().unwrap();
    assert!(complete);assert!(notes_preview::png_valid(&base,page.width,page.height));
    let ink=rs_render::raster::rasterize(&svg,page.width as u32,page.height as u32).unwrap();
    assert!(ink.chunks_exact(4).any(|pixel|pixel[3]!=0));
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

#[test]
fn split_renderer_only_page_retains_strokes_in_transparent_layer_and_not_base() {
    let native = notes_preview::NativeInk::new(notes_preview::env::default_project_root(), None);
    if !native.provider_available() { eprintln!("ink renderer not built; skipping split render"); return; }
    let (_, case) = cases().into_iter().find(|(name, _)| name == "fountain18").unwrap();
    let snapshot = case.get("snapshot").unwrap();
    let page = native.inspect_render_pages(snapshot).into_iter().next().unwrap();
    let (base, svg, complete) = native.split_ink_page(snapshot, page.index, &|_| None).unwrap();
    assert!(complete);
    let pixels = rs_render::pixels::rgba(&base, page.width, page.height).ok().expect("base PNG decodes");
    assert!(pixels.chunks_exact(4).all(|p| p == &pixels[..4]), "base must contain only the page background");
    let layer = rs_render::raster::rasterize(&svg, page.width as u32, page.height as u32).unwrap();
    assert!(layer.chunks_exact(4).any(|p| p[3] != 0), "stroke must be present in the transparent layer");
    assert_eq!(layer[3], 0, "page background must not be in the ink layer");
    let scale = rs_render::raster::InkTileScale::from_half_units(3).unwrap();
    let tile = rs_render::raster::InkTile::new(page.width as u32, page.height as u32, scale, 0, 0).unwrap();
    let rgba = rs_render::raster::rasterize_ink_tile(&svg, tile).unwrap();
    assert_eq!(rgba.len(), 512 * 512 * 4);
    assert_eq!(rgba[3], 0);
    assert!(rgba.chunks_exact(4).any(|p| p[3] != 0));
    assert!(native.split_ink_page(snapshot, 99, &|_| None).is_err());
}

#[test]
fn split_rejects_unprovided_pdf_underlay() {
    let native = notes_preview::NativeInk::new(notes_preview::env::default_project_root(), None);
    let (_, case) = cases().into_iter().find(|(name, _)| name == "pdf_list").unwrap();
    let snapshot = case.get("snapshot").unwrap();
    let pages = prepare::prepare_pages(snapshot, None, false).unwrap();
    assert!(pages.iter().any(|page| page.over_pdf));
    assert!(native.split_ink_page(snapshot, pages[0].index, &|_| None).is_err());
}

#[test]
fn manager_ink_layer_is_revision_and_snapshot_bound() {
    let dir = TempDir::new();
    let s = store(dir.path());
    let (_, case) = cases().into_iter().find(|(name, _)| name == "fountain18").unwrap();
    let id = note_with_document(&s, case.get("snapshot").unwrap());
    let revision = s.raw(&id).unwrap().get("revision").unwrap().as_i64().unwrap();
    let manager = PreviewManager::new(s, TestEnv::new(None, true)).unwrap();
    let Some((base, token)) = manager.ink_layer(&id, 0, revision, None, None).unwrap() else { panic!("layer unexpectedly busy") };
    assert!(notes_preview::png_valid(&base, 1080, 1527));
    let Some((tile, same)) = manager.ink_layer(&id, 0, revision, Some((3,0,0)), Some(&token)).unwrap() else { panic!("tile unexpectedly busy") };
    assert_eq!(token, same);
    assert!(notes_preview::png_valid(&tile, 512, 512));
    assert_eq!(manager.ink_layer(&id, 0, revision+1, None, None).unwrap_err().status, 409);
    assert_eq!(manager.ink_layer(&id, 0, revision, Some((3,0,0)), Some(&"a".repeat(64))).unwrap_err().status, 409);
    assert_eq!(manager.ink_layer(&id, 0, revision, Some((3,999,0)), Some(&token)).unwrap_err().status, 404);
    assert_eq!(manager.ink_layer(&id, 0, revision, Some((3,0,0)), Some(&token)).unwrap().unwrap().0, tile);
    manager.close();manager.join(Duration::from_secs(2));
}

#[test]
fn text_composites_are_bound_to_revision_and_render_the_saved_pending_model() {
    let native = notes_preview::NativeInk::new(notes_preview::env::default_project_root(), None);
    if !native.provider_available() { return; }
    let (_, case) = cases().into_iter().find(|(name, _)| name == "fountain18").unwrap();
    let snapshot = case.get("snapshot").unwrap();
    let dir = TempDir::new();
    let store = store(dir.path());
    let id = note_with_document(&store, snapshot);
    store.with_db(|db| {
        use rs_store::db::Sql;
        db.exec("UPDATE notes SET sync_state='synced' WHERE id=?", &rs_store::p![&id])?;
        Ok(())
    }).unwrap();
    let revision = store.raw(&id).unwrap().get("revision").unwrap().as_i64().unwrap();
    let source = notes_host::snapshot::from_dict(snapshot).unwrap();
    let projection = notes_host::areas::editor_projection(&source).unwrap();
    store.cache_editor(&id, &Val::Int(revision), &projection).unwrap();
    let manager = PreviewManager::new(store.clone(), TestEnv::new(None,true)).unwrap();
    let no_text = manager.notext(&id, 0, revision).unwrap().unwrap();
    assert!(notes_preview::png_valid(&no_text,1080,1527));
    assert_eq!(manager.notext(&id, 0, revision).unwrap().unwrap(), no_text);
    assert_eq!(manager.notext(&id, 0, revision+1).unwrap_err().status,409);
    let (base,token)=manager.text_ink_layer(&id,0,revision,None,None).unwrap().unwrap();
    assert!(notes_preview::png_valid(&base,1080,1527));
    let (tile,same)=manager.text_ink_layer(&id,0,revision,Some((2,0,0)),Some(&token)).unwrap().unwrap();
    assert_eq!(same,token);assert!(notes_preview::png_valid(&tile,512,512));
    assert_eq!(manager.text_ink_layer(&id,0,revision+1,None,None).unwrap_err().status,409);
    assert!(manager.fulltext(&id, 0, revision).unwrap().is_some());
    let mut edited = projection.clone();
    edited.set("model",Val::dict(vec![("schemaVersion",Val::Int(1)),("text",Val::s("Yeni metin")),("spans",Val::List(vec![])),("paragraphs",Val::List(vec![]))]));
    let edited_json=rs_store::val::dumps_utf8(&edited).unwrap();
    store.with_db(|db| {
        use rs_store::db::Sql;
        db.exec("UPDATE notes SET sync_state='pending' WHERE id=?", &rs_store::p![&id])?;
        db.exec("UPDATE editor_documents SET projection=? WHERE note_id=?", &rs_store::p![edited_json, &id])?;
        Ok(())
    }).unwrap();
    // Different model at the same revision is forbidden by the store; use a
    // new revision to represent an accepted editor save and its cache key.
    store.with_db(|db| {
        use rs_store::db::Sql;
        db.exec("UPDATE notes SET revision=revision+1 WHERE id=?", &rs_store::p![&id])?;
        db.exec("UPDATE editor_documents SET revision=revision+1 WHERE note_id=?", &rs_store::p![&id])?;
        Ok(())
    }).unwrap();
    let full=manager.fulltext(&id,0,revision+1).unwrap().unwrap();
    assert_ne!(full,no_text,"the accepted pending model must be painted");
    manager.close();manager.join(Duration::from_secs(2));
}

#[test]
fn concurrent_tile_requests_return_pending_while_one_layer_is_prepared() {
    use std::sync::{Barrier,mpsc};
    struct BlockingInk { native: notes_preview::NativeInk, entered: Mutex<Option<mpsc::Sender<()>>>, release: Arc<Barrier> }
    impl InkBackend for BlockingInk {
        fn provider_available(&self) -> bool { self.native.provider_available() }
        fn inspect_render_pages(&self, snapshot:&Val) -> Vec<InkPage> { self.native.inspect_render_pages(snapshot) }
        fn render_ink_page(&self, snapshot:&Val, page:i64) -> Result<Vec<u8>,Fail> { self.native.render_ink_page(snapshot,page) }
        fn split_page(&self, snapshot:&Val, page:i64, media:&dyn Fn(&str)->Option<Vec<u8>>) -> Option<Result<(Vec<u8>,Vec<u8>,bool),Fail>> {
            if let Some(sender)=self.entered.lock().unwrap().take() { sender.send(()).unwrap(); self.release.wait(); }
            Some(self.native.split_ink_page(snapshot,page,media))
        }
    }
    let dir=TempDir::new();let store=store(dir.path());
    let (_,case)=cases().into_iter().find(|(name,_)|name=="fountain18").unwrap();
    let id=note_with_document(&store,case.get("snapshot").unwrap());
    let revision=store.raw(&id).unwrap().get("revision").unwrap().as_i64().unwrap();
    let (sender,receiver)=mpsc::channel();let barrier=Arc::new(Barrier::new(2));
    let ink=BlockingInk{native:notes_preview::NativeInk::new(notes_preview::env::default_project_root(),None),entered:Mutex::new(Some(sender)),release:barrier.clone()};
    if !ink.provider_available(){return;}
    let manager=Arc::new(PreviewManager::with_options(store,TestEnv::new(None,true),PreviewOptions{ink:Some(Arc::new(ink)),..Default::default()}).unwrap());
    let worker=manager.clone();let note=id.clone();
    let running=std::thread::spawn(move||worker.ink_layer(&note,0,revision,None,None));
    receiver.recv_timeout(Duration::from_secs(20)).unwrap();
    assert!(manager.ink_layer(&id,0,revision,None,None).unwrap().is_none());
    barrier.wait();assert!(running.join().unwrap().unwrap().is_some());
    manager.close();manager.join(Duration::from_secs(2));
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
