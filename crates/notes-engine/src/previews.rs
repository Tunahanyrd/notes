//! Page preview / thumbnail boundary (`engine.previews`, `engine.thumbnails`).
//! The pure implementation is `crates/notes-preview`; see `preview_impl`.
use std::path::PathBuf;
use std::time::Duration;

use notes_host::Val;

use crate::util::R;

pub trait Previews: Send + Sync {
    /// Schedule derived preview metadata after a snapshot is committed.
    fn snapshot_changed(&self) -> R<()> { Ok(()) }
    /// `previews.manifest(identity)`
    fn manifest(&self, identity: &str) -> R<Val>;
    /// Retry a terminal, local-only preview inventory failure.
    fn retry_inventory(&self, identity: &str) -> R<Val> { self.manifest(identity) }
    /// `previews.request(identity, page, priority=, wait=)`
    fn request(&self, identity: &str, page: i64, priority: i64, wait: f64) -> R<Option<Vec<u8>>>;
    fn notext(&self, _identity: &str, _page: i64, _revision: i64) -> R<Option<Vec<u8>>> {
        Err(notes_host::Fail::store("preview_unavailable", "Bu sayfanın metinsiz görüntüsü kullanılamıyor.", 404))
    }
    fn fulltext(&self, _identity: &str, _page: i64, _revision: i64) -> R<Option<Vec<u8>>> {
        Err(notes_host::Fail::store("preview_unavailable", "Bu sayfanın görüntüsü kullanılamıyor.", 404))
    }
    fn text_ink_layer(&self, _identity:&str, _page:i64, _revision:i64, _tile:Option<(u8,u32,u32)>, _expected:Option<&str>) -> R<Option<(Vec<u8>,String)>> {
        Err(notes_host::Fail::store("preview_unavailable", "Bu sayfanın çizim katmanı kullanılamıyor.", 404))
    }
    fn ink_layer(&self, _identity: &str, _page: i64, _revision: i64, _tile: Option<(u8,u32,u32)>, _expected: Option<&str>) -> R<Option<(Vec<u8>,String)>> {
        Err(notes_host::Fail::store("preview_unavailable", "Bu sayfanın çizim katmanı kullanılamıyor.", 404))
    }
    /// `previews.prefetch()`
    fn prefetch(&self) -> R<usize>;
    fn close(&self);
    fn join(&self, timeout: Duration);
    /// `previews.directory`
    fn directory(&self) -> PathBuf;
    /// `apply_template`'s PDF lookup for the template `sha256` in the source
    /// note snapshot dict: Ok(Some(path)) when a cached/fetched copy exists,
    /// Ok(None) when it is not cached and `fetch` is false.
    /// Errors: `Fail::store('template_unavailable', ...)` codes exactly as engine.py.
    fn template_pdf(&self, document: &Val, sha256: &str, account: &str, fetch: bool) -> R<Option<PathBuf>>;
}

pub trait Thumbnails: Send + Sync {
    /// `thumbnails.request(identity, revision)` -> (png | None, complete)
    fn request(&self, identity: &str, revision: i64) -> R<(Option<Vec<u8>>, bool)>;
}
