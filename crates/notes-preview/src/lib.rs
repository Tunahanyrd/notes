//! Pure-Rust page previews and thumbnails for the Python-free engine.
//!
//! Port of `samsung_notes/previews.py` (PreviewManager and helpers),
//! `samsung_notes/thumbnails.py` and the preparation they drive
//! (`pdf_background`, `ink_render` / `ink_render_job`, `preview_codec`,
//! `page_image`). No PyO3 call is reachable from this crate: only the pure
//! layers of rs-doc / rs-ink / rs-ops / rs-editor / rs-store / rs-render are
//! used.
//!
//! * [`manager::PreviewManager`] — `manifest`, `request`, `prefetch`,
//!   `close`, `join` with the Python cache paths, job table, priority queue
//!   and error codes.
//! * [`thumbnails::ThumbnailManager`] — revision-bound first-page thumbnails.
//! * [`env::PreviewEnv`] — what the manager needs from the engine (session
//!   path, offline flag, account headers, network transport, project root).
//! * [`pdf`] — `inspect_pdf_backgrounds`, `cached_pdf`, `fetch_pdf`,
//!   `render_pdf_background`, `compose_pdf_canvas` for `apply_template`.
//! * [`ink`] — reconstructed ink pages (`_prepare_pages`, the per-note body
//!   cache, renderer/rasterizer) and [`ink::run_child`] for the optional
//!   self-exec preparation child.
pub mod codec;
pub mod env;
pub mod fetch;
pub mod ink;
pub mod manager;
pub mod payload;
pub mod pdf;
pub mod spi;
pub mod thumbnails;
mod util;

pub use env::PreviewEnv;
pub use ink::{run_child, InkBackend, InkPage, NativeInk};
pub use manager::{Candidate, Entry, InkGeometryPreview, PreviewManager, PreviewOptions, Source};
pub use notes_host::{Fail, Kind, Val};
pub use pdf::PdfBackground;
pub use rs_ink::core::payload::CanvasPreviewCandidate;
pub use thumbnails::{downsample_page, thumbnail_size, PagePreviews, ThumbnailManager};

/// `preview_contract.MAX_INPUT` (= `previews.MAX_DOWNLOAD`).
/// Raised from 8 MiB (2026-10-28): canvas caches of photo-heavy pages and the
/// pictures a page composite draws can exceed 8 MiB.
pub const MAX_DOWNLOAD: usize = 32 * 1024 * 1024;
/// `preview_contract.MAX_OUTPUT` (= `previews.MAX_PNG`).
pub const MAX_PNG: usize = 64 * 1024 * 1024;
/// `preview_contract.MAX_PIXELS`.
pub const MAX_PIXELS: i64 = 4 * 1024 * 1024;
/// `previews.MAX_QUEUED`.
pub const MAX_QUEUED: usize = 256;
/// `previews.INK_INVENTORY_RECIPE`.
pub const INK_INVENTORY_RECIPE: &str = "ink-inventory-v5:body-flow:calibrated-inkpen2:per-page:stylusless-inkpen2:page-composite";
/// `previews.BROKER_HOSTS`.
pub const BROKER_HOSTS: &[&str] = &["ew1a-api.samsungcloud.com"];
/// `previews.FILE_HOSTS`.
pub const FILE_HOSTS: &[&str] = &["eu1rgn-down.file.samsungcloud.com"];

/// `previews.WORKERS`: `max(1, min(4, (os.cpu_count() or 2) // 2))`.
pub fn default_workers() -> usize {
    // SAFETY: sysconf takes a constant name and has no preconditions; -1 is handled below.
    let online = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    let cpus = if online > 0 { online as usize } else { 2 };
    (cpus / 2).clamp(1, 4)
}

/// `previews._png_valid`: the narrow RGBA8 PNG contract with every CRC.
pub fn png_valid(raw: &[u8], width: i64, height: i64) -> bool {
    rs_render::pixels::png_valid(raw, width, height)
}
