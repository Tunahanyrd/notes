//! A panic while rendering one hostile note must not take the preview
//! workers down with it: before the fix every panicking job killed its worker
//! thread (the job stayed "running"), and after as many panics as workers no
//! preview was ever produced again until the engine restarted.
mod common;
use common::*;
use notes_preview::ink::{InkBackend, InkPage};
use notes_preview::manager::PreviewOptions;
use notes_preview::{Fail, PreviewManager, Val};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Panics on the first `panics` renders, then renders a valid page.
struct PanickyInk {
    png: Vec<u8>,
    calls: AtomicUsize,
    panics: usize,
}

impl InkBackend for PanickyInk {
    fn provider_available(&self) -> bool {
        true
    }
    fn inspect_render_pages(&self, _snapshot: &Val) -> Vec<InkPage> {
        vec![InkPage { index: 0, width: 8, height: 8, dark_mode: None, ..Default::default() }]
    }
    fn render_ink_page(&self, _snapshot: &Val, _index: i64) -> Result<Vec<u8>, Fail> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.panics {
            panic!("synthetic renderer panic");
        }
        Ok(self.png.clone())
    }
}

fn snapshots() -> Vec<Val> {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ink_cases.json")).unwrap();
    let Val::Dict(items) = rs_store::val::loads(&text).unwrap() else { panic!() };
    let mut out: Vec<Val> = Vec::new();
    for (_, case) in items {
        let snapshot = case.get("snapshot").unwrap().clone();
        if !out.iter().any(|s| s.get("raw_sha256") == snapshot.get("raw_sha256")) {
            out.push(snapshot);
        }
    }
    out
}

#[test]
fn renderer_panics_do_not_exhaust_the_preview_workers() {
    let dir = TempDir::new();
    let s = store(dir.path());
    let env = TestEnv::new(None, true);
    *env.headers.lock().unwrap() = Err(Fail::new(notes_preview::Kind::Other, "unexpected network"));
    let panics = 4; // the maximum worker count
    let fake = Arc::new(PanickyInk { png: png(8, 8, &[255; 256]), calls: AtomicUsize::new(0), panics });
    let previews = PreviewManager::with_options(s.clone(), env, PreviewOptions { ink: Some(fake.clone()), ..Default::default() }).unwrap();
    let snapshots = snapshots();
    assert!(snapshots.len() > panics);
    let ids: Vec<String> = snapshots.iter().take(panics + 1).map(|snap| note_with_document(&s, snap)).collect();
    for id in &ids[..panics] {
        let _ = previews.request(id, 0, 0, 0.0);
    }
    for _ in 0..200 {
        if fake.calls.load(Ordering::SeqCst) >= panics.min(1) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    let last = &ids[panics];
    let mut rendered = None;
    for _ in 0..300 {
        if let Ok(Some(v)) = previews.request(last, 0, 0, 0.0) {
            rendered = Some(v);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(rendered.as_deref(), Some(fake.png.as_slice()), "no worker left to render after renderer panics");
    // The panicking jobs end as ordinary preview errors, not stuck "running".
    assert!(previews.request(&ids[0], 0, 0, 0.0).is_err());
    previews.close();
}
