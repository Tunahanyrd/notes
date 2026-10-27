//! Port of tests/test_thumbnails.py.
mod common;
use common::*;
use notes_preview::manager::{Entry, Source};
use notes_preview::{downsample_page, thumbnail_size, Candidate, Fail, PagePreviews, ThumbnailManager, Val};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct FakePreviews {
    source: Mutex<Source>,
    entry: Mutex<Entry>,
    png: Vec<u8>,
    calls: AtomicUsize,
    change_source_on_request: bool,
}

impl PagePreviews for FakePreviews {
    fn source(&self, _: &str) -> Result<Source, Fail> {
        Ok(self.source.lock().unwrap().clone())
    }
    fn entry(&self, _: &str) -> Result<Arc<Entry>, Fail> {
        Ok(Arc::new(self.entry.lock().unwrap().clone()))
    }
    fn path(&self, _: &Candidate) -> Result<PathBuf, Fail> {
        Ok(PathBuf::from("account-and-content-key.png"))
    }
    fn request(&self, _: &str, page: i64, _: i64) -> Result<Option<Vec<u8>>, Fail> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(page, 0);
        if self.change_source_on_request {
            *self.source.lock().unwrap() = Source(vec![Val::s("different-account-or-document")]);
        }
        Ok(Some(self.png.clone()))
    }
}

fn setup(change: bool) -> (TempDir, Arc<rs_store::storage::StoreCore>, Arc<FakePreviews>, String, i64) {
    let dir = TempDir::new();
    let s = store(dir.path());
    let note = s.create(&Val::s("Synthetic"), &Val::s("Actual note text")).unwrap();
    let mut c = candidate(b"x");
    c.width = 2;
    c.height = 2;
    let entry = Entry {
        source: Source(vec![Val::s("stable-source")]),
        pages: vec![Val::dict(vec![("index", Val::Int(0)), ("darkMode", Val::Bool(false)), ("complete", Val::Bool(false))])],
        candidates: BTreeMap::from([(0, Candidate::Canvas(c))]),
    };
    let previews = Arc::new(FakePreviews {
        source: Mutex::new(entry.source.clone()), entry: Mutex::new(entry), png: png(2, 2, &[255, 0, 0, 255].repeat(4)),
        calls: AtomicUsize::new(0), change_source_on_request: change,
    });
    let id = note.get("id").unwrap().as_str().unwrap().to_string();
    let revision = note.get("revision").unwrap().as_i64().unwrap();
    (dir, s, previews, id, revision)
}

#[test]
fn cache_reuses_real_first_page_and_retains_partial_flag() {
    let (_dir, s, previews, id, revision) = setup(false);
    let thumbnails = ThumbnailManager::new(s, previews.clone()).unwrap();
    let (first, complete) = thumbnails.request(&id, revision).unwrap();
    assert!(!complete);
    let first = first.unwrap();
    assert_eq!(notes_preview::pdf::rgba(&first, 2, 2).unwrap(), [255, 0, 0, 255].repeat(4));
    let (again, _) = thumbnails.request(&id, revision).unwrap();
    assert_eq!(again.unwrap(), first);
    assert_eq!(previews.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn stale_revision_does_not_fetch_and_source_change_does_not_publish() {
    let (_dir, s, previews, id, revision) = setup(true);
    let thumbnails = ThumbnailManager::new(s, previews.clone()).unwrap();
    let error = thumbnails.request(&id, revision + 1).unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("revision_conflict", 409));
    assert_eq!(previews.calls.load(Ordering::SeqCst), 0);
    let error = thumbnails.request(&id, revision).unwrap_err();
    assert_eq!(error.status, 409);
    assert_eq!(std::fs::read_dir(thumbnails.directory()).unwrap().count(), 0);
}

#[test]
fn missing_first_page_never_uses_another_page() {
    let (_dir, s, previews, id, revision) = setup(false);
    {
        let mut entry = previews.entry.lock().unwrap();
        let c = entry.candidates.remove(&0).unwrap();
        entry.candidates.insert(1, c);
    }
    let thumbnails = ThumbnailManager::new(s, previews.clone()).unwrap();
    let error = thumbnails.request(&id, revision).unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("thumbnail_unavailable", 404));
    assert_eq!(previews.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn unknown_note_is_404() {
    let (_dir, s, previews, _, _) = setup(false);
    let thumbnails = ThumbnailManager::new(s, previews).unwrap();
    let error = thumbnails.request("missing", 1).unwrap_err();
    assert_eq!((error.code.as_str(), error.status, error.message.as_str()), ("not_found", 404, "Not önizlemesi bulunamadı."));
}

#[test]
fn downsample_preserves_thin_ink_and_composites_page_background() {
    let raw = png(640, 2, &[0, 0, 0, 255, 0, 0, 0, 0].repeat(640));
    let white = downsample_page(&raw, 640, 2, false).unwrap();
    let black = downsample_page(&raw, 640, 2, true).unwrap();
    assert_eq!(thumbnail_size(640, 2).unwrap(), (320, 1));
    assert_eq!(notes_preview::pdf::rgba(&white, 320, 1).unwrap(), [128, 128, 128, 255].repeat(320));
    assert_eq!(notes_preview::pdf::rgba(&black, 320, 1).unwrap(), [0, 0, 0, 255].repeat(320));
}
