//! `thumbnails.ThumbnailManager`: real first-page thumbnails with bounded
//! work and revision-bound responses.
use crate::manager::{Candidate, Entry, Source};
use crate::pdf;
use crate::util::{self, private_directory, read_cached, store_fail};
use notes_host::{Fail, Kind, Val};
use rs_render::pixels as px;
use rs_store::storage::StoreCore;
use rs_store::val::py_eq;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub const WIDTH: i64 = 320;
pub const HEIGHT: i64 = 384;
pub const MAX_THUMB_BYTES: usize = (WIDTH * HEIGHT * 5 + 4096) as usize;

/// The parts of `engine.previews` the thumbnail manager uses.
pub trait PagePreviews: Send + Sync {
    fn source(&self, identity: &str) -> Result<Source, Fail>;
    fn entry(&self, identity: &str) -> Result<Arc<Entry>, Fail>;
    fn path(&self, candidate: &Candidate) -> Result<PathBuf, Fail>;
    /// `request(identity, page, priority=priority)` (no wait).
    fn request(&self, identity: &str, page_index: i64, priority: i64) -> Result<Option<Vec<u8>>, Fail>;
}

/// `thumbnail_size(width, height)` (ZeroDivisionError for a zero side).
pub fn thumbnail_size(width: i64, height: i64) -> Result<(i64, i64), Fail> {
    rs_render::thumbnails::size(width, height).ok_or_else(|| Fail::new(Kind::Other, "division by zero"))
}

/// `downsample_page(png, width, height, dark=dark)`: alpha-weighted box
/// filter composited on the page background.
pub fn downsample_page(png: &[u8], width: i64, height: i64, dark: bool) -> Result<Vec<u8>, Fail> {
    let source = pdf::rgba(png, width, height)?;
    let (tw, th) = thumbnail_size(width, height)?;
    let out = px::box_downsample(&source, width as usize, height as usize, tw as usize, th as usize, if dark { 0 } else { 255 })
        .map_err(|e| match e {
            px::DownsampleError::Length => Fail::value("cannot reshape array: pixel buffer size mismatch"),
            px::DownsampleError::ZeroDivision => Fail::new(Kind::Other, "integer division or modulo by zero"),
            px::DownsampleError::ByteRange => Fail::value("bytes must be in range(0, 256)"),
        })?;
    px::rgba_png(tw, th, &out).map_err(Fail::value)
}

pub struct ThumbnailManager {
    store: Arc<StoreCore>,
    previews: Arc<dyn PagePreviews>,
    directory: PathBuf,
    /// `threading.BoundedSemaphore(2)`
    slots: AtomicUsize,
}

impl ThumbnailManager {
    /// `ThumbnailManager(engine)`
    pub fn new(store: Arc<StoreCore>, previews: Arc<dyn PagePreviews>) -> Result<ThumbnailManager, Fail> {
        let directory = private_directory(&store.directory.join("thumbnails"))?;
        Ok(ThumbnailManager { store, previews, directory, slots: AtomicUsize::new(2) })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn revision(&self, identity: &str, revision: i64) -> Result<(), Fail> {
        let rows = self.store.with_db(|db| {
            use rs_store::db::Sql;
            db.query("SELECT revision,deleted_remote FROM notes WHERE id=?", &rs_store::p![identity])
        }).map_err(store_fail)?;
        let row = rows.one();
        if row.is_none_or(|r| r.truthy("deleted_remote")) {
            return Err(Fail::store("not_found", "Not önizlemesi bulunamadı.", 404));
        }
        if !py_eq(&row.unwrap().val("revision"), &Val::Int(revision)) {
            return Err(Fail::store("revision_conflict", "Not değişti; güncel önizleme alınmalı.", 409));
        }
        Ok(())
    }

    fn acquire(&self) -> bool {
        self.slots.try_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1)).is_ok()
    }

    /// `request(identity, revision)` -> `(PNG | None, complete)`.
    pub fn request(&self, identity: &str, revision: i64) -> Result<(Option<Vec<u8>>, bool), Fail> {
        self.revision(identity, revision)?;
        let previews = &self.previews;
        let source = previews.source(identity)?;
        let entry = previews.entry(identity)?;
        let Some(candidate) = entry.candidates.get(&0) else {
            return Err(Fail::store("thumbnail_unavailable", "Bu notun ilk sayfa görüntüsü bulunmuyor.", 404));
        };
        let Some(page) = entry.pages.iter().find(|p| p.get("index").is_some_and(|i| py_eq(i, &Val::Int(0)))) else {
            return Err(Fail::new(Kind::Other, "StopIteration"));
        };
        let complete = page.get("complete").is_none_or(Val::truthy);
        let dark = page.get("darkMode").ok_or_else(|| Fail::key("'darkMode'"))?.truthy();
        let (width, height) = thumbnail_size(candidate.width(), candidate.height())?;
        let stem = previews.path(candidate)?.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let recipe = format!("{stem}:thumbnail-v1:{}", if dark { "True" } else { "False" });
        let path = self.directory.join(format!("{}.png", util::sha256_hex(recipe.as_bytes())));
        let guard = || -> Result<(), Fail> {
            self.revision(identity, revision)?;
            if previews.source(identity)? != source {
                return Err(Fail::store("revision_conflict", "Not değişti; güncel önizleme alınmalı.", 409));
            }
            Ok(())
        };
        if let Some(raw) = read_cached(&path, MAX_THUMB_BYTES as u64)? {
            if crate::png_valid(&raw, width, height) {
                guard()?;
                return Ok((Some(raw), complete));
            }
        }
        let Some(raw) = previews.request(identity, 0, 1)? else { return Ok((None, complete)) };
        if !self.acquire() {
            return Ok((None, complete));
        }
        let result = (|| -> Result<Vec<u8>, Fail> {
            guard()?;
            let image = downsample_page(&raw, candidate.width(), candidate.height(), dark)?;
            guard()?;
            if image.len() > MAX_THUMB_BYTES || !crate::png_valid(&image, width, height) {
                return Err(Fail::store("thumbnail_unavailable", "Not önizlemesi hazırlanamadı.", 422));
            }
            // Python guards after the fsync only (before os.replace).
            let first = std::cell::Cell::new(true);
            util::write_private(&path, &image, 8, true, &|| if first.replace(false) { Ok(()) } else { guard() })?;
            Ok(image)
        })();
        self.slots.fetch_add(1, Ordering::SeqCst);
        result.map(|image| (Some(image), complete))
    }
}
