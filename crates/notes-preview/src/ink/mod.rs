//! Reconstructed ink pages (`ink_render`, `ink_render_job`).
//!
//! Page admission and renderer input are computed by [`prepare`] (pure Rust).
//! Where that work runs:
//!
//! * **In-process (default)**: a background thread per note prepares the
//!   inventory once and builds pages on demand into the bounded per-note body
//!   cache. No interpreter is started.
//! * **Self-exec** ([`crate::PreviewEnv::ink_child_executable`] returns a path):
//!   the engine binary is re-executed with the hidden first argument
//!   [`CHILD_FLAG`]; it must call [`run_child`] at the top of `main()`. The
//!   child gets the Python child's isolation: own session (killed as a group
//!   on the 180 s deadline), PDEATHSIG bound to the parent, RLIMIT_AS 3 GiB,
//!   RLIMIT_CPU 170 s, no core files.
//!
//! The GPL sdocx renderer (`tools/ink-renderer`) always runs as a separate,
//! bwrap-sandboxed, rlimited process; the SVG is rasterized in-process with
//! resvg (`SAMSUNG_NOTES_INK_RASTER=rsvg` selects the sandboxed rsvg-convert),
//! exactly as rs-render does.
pub mod objects;
pub mod prepare;

use crate::util::{ink_render, sha256};
use notes_host::{Fail, Val};
use rs_render::pixels as px;
use rs_render::sandbox;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

thread_local! {
    static STAGE_TIMES: std::cell::Cell<(Duration, Duration)> = const { std::cell::Cell::new((Duration::ZERO, Duration::ZERO)) };
}

pub(crate) fn take_stage_times() -> (Duration, Duration) {
    STAGE_TIMES.with(|times| times.replace((Duration::ZERO, Duration::ZERO)))
}

pub(crate) fn measure_png<T>(f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    STAGE_TIMES.with(|times| { let (render, png) = times.get(); times.set((render, png + start.elapsed())); });
    result
}

fn measure_renderer<T>(f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    STAGE_TIMES.with(|times| { let (render, png) = times.get(); times.set((render + start.elapsed(), png)); });
    result
}

// Per-worker measurements; spans may overlap. Bounded timing frames from the
// isolated preparation child are attributed to the first waiter of each page.
thread_local! {
    static DETAIL_TIMES: std::cell::Cell<[Duration; 19]> = const { std::cell::Cell::new([Duration::ZERO; 19]) };
}
pub(crate) const SNAPSHOT: usize = 0;
pub(crate) const INVENTORY_STAGE: usize = 1;
pub(crate) const BODY: usize = 2;
pub(crate) const MEDIA: usize = 3;
pub(crate) const COMPOSITE: usize = 4;
pub(crate) const PAYLOAD: usize = 5;
pub(crate) const CACHE_WRITE: usize = 6;
pub(crate) const RASTER: usize = 7;
pub(crate) const IDENTITY: usize = 8;
const CHILD_SPAWN: usize = 9;
const CHILD_PAGE: usize = 17;
const CHILD_WRITE: usize = 18;

pub(crate) fn take_detail_times() -> [Duration; 19] {
    DETAIL_TIMES.with(|v| v.replace([Duration::ZERO; 19]))
}
pub(crate) fn record_elapsed(stage: usize, elapsed: Duration) {
    DETAIL_TIMES.with(|v| { let mut times = v.get(); times[stage] += elapsed; v.set(times); });
}
pub(crate) fn measure<T>(stage: usize, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    DETAIL_TIMES.with(|v| { let mut times = v.get(); times[stage] += start.elapsed(); v.set(times); });
    result
}

fn renderer_error(message: &str) -> Fail {
    if message == "Çizim görüntüleme süresi doldu." {
        coded_ink_render(UNSAFE, "ink_renderer_timeout")
    } else {
        ink_render(UNSAFE)
    }
}

fn coded_ink_render(message: &str, code: &str) -> Fail {
    let mut error = ink_render(message);
    error.code = code.into();
    error
}

/// Hidden argv[1] of the self-exec preparation child.
pub const CHILD_FLAG: &str = "__notes-ink-render-job";
pub const MAX_SVG: usize = 16 * 1024 * 1024;
pub const MAX_PNG: usize = 32 * 1024 * 1024;
const JOB_LIMITS: sandbox::Limits = sandbox::Limits { address_space: 3 * 1024 * 1024 * 1024, cpu_seconds: 170, file_size: None };
const BODY_BUDGET: usize = 96 * 1024 * 1024;
const NOT_FOUND: &str = "Bu sayfada desteklenen çizim bulunmuyor.";
const TIMED_OUT: &str = "Çizim hazırlama süresi doldu.";
const NOT_PREPARED: &str = "Bu sayfanın çizimi hazırlanamadı.";
const UNSAFE: &str = "Bu çizginin görünümü güvenle oluşturulamadı.";

/// One admitted page of `inspect_render_pages` (`darkMode` may be absent in
/// pages a custom backend returns; the manager reads `.get('darkMode', False)`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct InkPage {
    pub index: i64,
    pub width: i64,
    pub height: i64,
    pub dark_mode: Option<bool>,
    /// A `page-reconstructed` composite page (inventory `"kind": "page"`).
    pub composite: bool,
    /// Calm reason the composite is approximate (None: complete).
    pub notice: Option<String>,
    /// A PDF page's own objects, drawn over its PDF page image
    /// (inventory `"kind": "pdf-ink"`).
    pub over_pdf: bool,
}

/// The `ink_render` functions the manager calls (tests substitute it the way
/// the Python tests patch `sys.modules['samsung_notes.ink_render']`).
pub trait InkBackend: Send + Sync {
    /// `ink_render.provider_available()`
    fn provider_available(&self) -> bool;
    /// `ink_render.inspect_render_pages(snapshot)`; `[]` on any failure.
    fn inspect_render_pages(&self, snapshot: &Val) -> Vec<InkPage>;
    /// `ink_render.render_ink_page(snapshot, page_index)`: contract RGBA PNG.
    fn render_ink_page(&self, snapshot: &Val, page_index: i64) -> Result<Vec<u8>, Fail>;
    /// Content-file SHA256s (images) the page composite draws.
    fn page_media(&self, _snapshot: &Val, _page_index: i64) -> Vec<String> {
        Vec::new()
    }
    /// Shared snapshots allow the native backend to reuse their serialized
    /// identity. Other backends keep their existing interface.
    fn page_media_shared(&self, snapshot: &Arc<Val>, page_index: i64) -> Vec<String> {
        self.page_media(snapshot, page_index)
    }
    fn render_page_shared(&self, snapshot: &Arc<Val>, page_index: i64, media: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<(Vec<u8>, bool), Fail> {
        self.render_page(snapshot, page_index, media)
    }
    /// Priority separates foreground preparation from background preparation
    /// for backends which share a long-lived per-note worker.
    fn page_media_priority(&self, snapshot: &Arc<Val>, page_index: i64, _priority: i64) -> Vec<String> {
        self.page_media_shared(snapshot, page_index)
    }
    fn render_page_priority(&self, snapshot: &Arc<Val>, page_index: i64, media: &dyn Fn(&str) -> Option<Vec<u8>>,
                            _priority: i64) -> Result<(Vec<u8>, bool), Fail> {
        self.render_page_shared(snapshot, page_index, media)
    }
    /// The page PNG with images from `media` (SHA256 -> verified bytes) and
    /// whether every image could be drawn (false: placeholders were used).
    fn render_page(&self, snapshot: &Val, page_index: i64, _media: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<(Vec<u8>, bool), Fail> {
        self.render_ink_page(snapshot, page_index).map(|png| (png, true))
    }
}

/// The real renderer pipeline.
pub struct NativeInk {
    root: PathBuf,
    child: Option<PathBuf>,
    identities: Mutex<VecDeque<(Arc<Val>, [u8; 32], Arc<Vec<u8>>)>>,
}

impl NativeInk {
    pub fn new(root: PathBuf, child: Option<PathBuf>) -> NativeInk {
        NativeInk { root, child, identities: Mutex::new(VecDeque::new()) }
    }

    /// `ink_render.RENDERER`: the optimized build when present.
    pub fn renderer(&self) -> PathBuf {
        renderer_path(&self.root)
    }
}

pub fn renderer_path(root: &Path) -> PathBuf {
    let release = root.join("tools/ink-renderer/target/release/ink-render");
    let debug = root.join("tools/ink-renderer/target/debug/ink-render");
    if release.is_file() { release } else { debug }
}

impl InkBackend for NativeInk {
    fn provider_available(&self) -> bool {
        rs_render::ink_render::renderer_available(&self.renderer())
    }

    fn inspect_render_pages(&self, snapshot: &Val) -> Vec<InkPage> {
        inspect_render_pages(snapshot, self.child.as_deref())
    }

    fn render_ink_page(&self, snapshot: &Val, page_index: i64) -> Result<Vec<u8>, Fail> {
        self.render_page(snapshot, page_index, &|_| None).map(|(png, _)| png)
    }

    fn page_media(&self, snapshot: &Val, page_index: i64) -> Vec<String> {
        match measure(BODY, || page_body(snapshot, page_index, self.child.as_deref())) {
            Ok((_, body)) if is_composite(&body) => composite_media(&body),
            _ => Vec::new(),
        }
    }

    fn page_media_priority(&self, snapshot: &Arc<Val>, page_index: i64, priority: i64) -> Vec<String> {
        let identity = self.identity(snapshot);
        match measure(BODY, || page_body_with_identity(snapshot, page_index, self.child.as_deref(), identity, priority <= 0)) {
            Ok((_, body)) if is_composite(&body) => composite_media(&body),
            _ => Vec::new(),
        }
    }

    fn render_page_priority(&self, snapshot: &Arc<Val>, page_index: i64, media: &dyn Fn(&str) -> Option<Vec<u8>>,
                            priority: i64) -> Result<(Vec<u8>, bool), Fail> {
        self.render_page_identity(snapshot, page_index, media, self.identity(snapshot), priority <= 0)
    }
    fn page_media_shared(&self, snapshot: &Arc<Val>, page_index: i64) -> Vec<String> {
        self.page_media_priority(snapshot, page_index, 0)
    }
    fn render_page_shared(&self, snapshot: &Arc<Val>, page_index: i64, media: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<(Vec<u8>, bool), Fail> {
        self.render_page_priority(snapshot, page_index, media, 0)
    }

    fn render_page(&self, snapshot: &Val, page_index: i64, media: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<(Vec<u8>, bool), Fail> {
        self.render_page_identity(snapshot, page_index, media, None, true)
    }

}

impl NativeInk {
    fn identity(&self, snapshot: &Arc<Val>) -> Option<([u8; 32], Arc<Vec<u8>>)> {
        measure(IDENTITY, || self.identity_inner(snapshot))
    }

    fn identity_inner(&self, snapshot: &Arc<Val>) -> Option<([u8; 32], Arc<Vec<u8>>)> {
        {
            let mut cache = self.identities.lock().unwrap();
            if let Some(at) = cache.iter().position(|(s, _, _)| Arc::ptr_eq(s, snapshot)) {
                let entry = cache.remove(at).unwrap();
                let result = (entry.1, entry.2.clone());
                cache.push_back(entry);
                return Some(result);
            }
        }
        // Serialization can be expensive; never hold the shared LRU lock
        // while doing it, so another note's foreground request can proceed.
        let data = Arc::new(snapshot_bytes(snapshot)?);
        let key = sha256(&data);
        if data.len() <= 64 * 1024 * 1024 {
            let mut cache = self.identities.lock().unwrap();
            if let Some(at) = cache.iter().position(|(s, _, _)| Arc::ptr_eq(s, snapshot)) {
                let entry = cache.remove(at).unwrap();
                let result = (entry.1, entry.2.clone());
                cache.push_back(entry);
                return Some(result);
            }
            cache.push_back((snapshot.clone(), key, data.clone()));
            while cache.len() > 4 || cache.iter().map(|e| e.2.len()).sum::<usize>() > 64 * 1024 * 1024 {
                cache.pop_front();
            }
        }
        Some((key, data))
    }

    fn render_page_identity(&self, snapshot: &Val, page_index: i64, media: &dyn Fn(&str) -> Option<Vec<u8>>,
                            identity: Option<([u8; 32], Arc<Vec<u8>>)>, foreground: bool) -> Result<(Vec<u8>, bool), Fail> {
        let (page, body) = measure(BODY, || page_body_with_identity(snapshot, page_index, self.child.as_deref(), identity, foreground))?;
        if is_composite(&body) {
            return measure(COMPOSITE, || render_composite(&self.renderer(), &body, (page.width, page.height), media));
        }
        let size = (page.width, page.height);
        match self.render_strokes(page, body) {
            Ok(png) => Ok((png, true)),
            Err(e) => {
                // The renderer refused a renderer-only page: draw it as a
                // composite stand-in (approximated ink), marked incomplete.
                let forced = single_body(snapshot, page_index, "forced", self.child.as_deref());
                if !is_composite(&forced) {
                    return Err(e);
                }
                render_composite(&self.renderer(), &forced, size, media).map(|(png, _)| (png, false))
            }
        }
    }
}

impl NativeInk {
    /// A renderer-only ink page (unchanged `ink-reconstructed` path).
    fn render_strokes(&self, page: InkPage, body: Vec<u8>) -> Result<Vec<u8>, Fail> {
        // Shape-only page (shapes come from the UI overlay): plain background.
        if let Some((width, height, argb)) = rs_render::ink_render::background_only(&body) {
            if (width, height) != (page.width, page.height) {
                return Err(ink_render(UNSAFE));
            }
            let pixels = rs_render::ink_render::background_pixels(width, height, argb);
            return measure_png(|| px::rgba_png(width, height, &pixels)).map_err(|_| ink_render(px::FilteredError::Format.message()));
        }
        if !self.provider_available() {
            return Err(coded_ink_render("Çizim görüntüleyicisi bu cihazda hazır değil.", "ink_renderer_unavailable"));
        }
        let binary = self.renderer();
        let (width, height) = (page.width, page.height);
        let svg = measure_renderer(|| rs_render::ink_render::run_renderer(&binary, false, body, MAX_SVG)).map_err(renderer_error)?;
        let pixels = if std::env::var("SAMSUNG_NOTES_INK_RASTER").is_ok_and(|v| v == "rsvg") {
            let png = measure_renderer(|| rs_render::ink_render::run_renderer(&sandbox::rsvg_path(), true, svg, MAX_PNG)).map_err(renderer_error)?;
            px::unfilter_png(&png, width, height, MAX_PNG).map_err(|e| match e {
                px::FilteredError::Invalid => ink_render(UNSAFE),
                other => ink_render(other.message()),
            })?
        } else {
            // resvg in-process; the SVG is our own renderer's bounded output.
            measure(RASTER, || rs_render::raster::rasterize(&svg, width as u32, height as u32)).map_err(|_| ink_render(UNSAFE))?
        };
        measure_png(|| px::rgba_png(width, height, &pixels)).map_err(|_| ink_render(px::FilteredError::Format.message()))
    }
}

/// Whether a prepared page body is a page composite (see `prepare::composite_page`).
pub fn is_composite(body: &[u8]) -> bool {
    body.starts_with(b"{\"composite\":1,")
}

fn composite_value(body: &[u8]) -> Option<Val> {
    rs_store::val::loads(std::str::from_utf8(body).ok()?).ok()
}

/// The image SHA256s a composite body draws (deduplicated, in order).
pub fn composite_media(body: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(Val::List(items)) = composite_value(body).and_then(|v| v.get("items").cloned()) {
        for item in items {
            if let Some(sha) = item.get("image").and_then(|i| i.get("sha256")).and_then(Val::as_str) {
                if !out.iter().any(|s| s == sha) {
                    out.push(sha.to_string());
                }
            }
        }
    }
    out
}

fn floats(v: Option<&Val>, n: usize) -> Option<Vec<f64>> {
    let Some(Val::List(items)) = v else { return None };
    let out: Vec<f64> = items.iter().filter_map(|x| match x {
        Val::Int(i) => Some(*i as f64),
        Val::Float(f) => Some(*f),
        _ => None,
    }).collect();
    (out.len() == n && items.len() == n).then_some(out)
}

fn float_of(v: Option<&Val>, default: f64) -> f64 {
    match v {
        Some(Val::Int(i)) => *i as f64,
        Some(Val::Float(f)) => *f,
        _ => default,
    }
}

/// Paint a composite body: page colour, its items in order (images resolved
/// through `media`), then the renderer's ink/text layer. A renderer failure
/// falls back to the text-box-free input, then to approximated polylines.
pub fn render_composite(renderer: &Path, body: &[u8], size: (i64, i64), media: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<(Vec<u8>, bool), Fail> {
    use rs_render::compose::{self, ImagePlacement, Item};
    let fail = || ink_render(UNSAFE);
    let value = composite_value(body).ok_or_else(fail)?;
    let width = value.get("width").and_then(Val::as_i64).ok_or_else(fail)?;
    let height = value.get("height").and_then(Val::as_i64).ok_or_else(fail)?;
    let color = value.get("backgroundArgb").and_then(Val::as_i64).filter(|c| (0..=u32::MAX as i64).contains(c)).ok_or_else(fail)? as u32;
    if (width, height) != size {
        return Err(fail());
    }
    let mut complete = true;
    // Renderer layer (strokes, body flow, text boxes).
    let mut layer: Option<(String, String)> = None;
    let attempts = ["renderer", "rendererPlain"];
    let wanted = value.get("renderer").and_then(Val::as_str).is_some();
    if wanted {
        let available = rs_render::ink_render::renderer_available(renderer);
        for key in attempts {
            let Some(input) = value.get(key).and_then(Val::as_str) else { continue };
            if !available {
                break;
            }
            let Ok(svg) = measure_renderer(|| rs_render::ink_render::run_renderer(renderer, false, input.as_bytes().to_vec(), MAX_SVG)) else { continue };
            let Ok(text) = String::from_utf8(svg) else { continue };
            if let Ok(found) = compose::renderer_layer(&text, width, height) {
                if key != "renderer" {
                    complete = false;
                }
                layer = Some(found);
                break;
            }
        }
    }
    let images: Vec<(usize, Option<Vec<u8>>)> = match value.get("items") {
        Some(Val::List(items)) => items.iter().enumerate().filter_map(|(i, item)| {
            let sha = item.get("image")?.get("sha256")?.as_str()?;
            Some((i, media(sha)))
        }).collect(),
        _ => Vec::new(),
    };
    let mut items: Vec<Item> = Vec::new();
    if let Some(Val::List(list)) = value.get("items") {
        for (i, item) in list.iter().enumerate() {
            if let Some(svg) = item.get("svg").and_then(Val::as_str) {
                items.push(Item::Svg(svg));
            } else if let Some(image) = item.get("image") {
                let rect = floats(image.get("rect"), 4).ok_or_else(fail)?;
                let crop = floats(image.get("crop"), 4).map(|c| [c[0], c[1], c[2], c[3]]);
                let place = ImagePlacement {
                    rect: [rect[0], rect[1], rect[2], rect[3]], crop,
                    flip_h: image.get("flipH").is_some_and(Val::truthy), flip_v: image.get("flipV").is_some_and(Val::truthy),
                    degrees: float_of(image.get("degrees"), 0.0), opacity: float_of(image.get("opacity"), 1.0),
                };
                let bytes = images.iter().find(|(at, _)| *at == i).and_then(|(_, b)| b.as_deref());
                items.push(Item::Image(bytes, place));
            }
        }
    }
    let fallback = value.get("fallback").and_then(Val::as_str).unwrap_or("");
    if wanted && layer.is_none() {
        complete = false;
        items.push(Item::Svg(fallback));
    }
    let inner = layer.as_ref().map(|(a, b)| (a.as_str(), b.as_str()));
    let (pixels, drawn) = compose::compose(width, height, color, &items, inner).map_err(|_| fail())?;
    let png = measure_png(|| px::rgba_png(width, height, &pixels)).map_err(|_| ink_render(px::FilteredError::Format.message()))?;
    Ok((png, complete && drawn))
}

/// `_snapshot_bytes(snapshot)` for a snapshot dict (compact JSON).
fn snapshot_bytes(snapshot: &Val) -> Option<Vec<u8>> {
    rs_store::val::dumps(snapshot, true, true).ok().map(String::into_bytes)
}

fn parse_inventory(raw: &[u8]) -> Option<Vec<InkPage>> {
    let text = std::str::from_utf8(raw).ok()?;
    let Val::List(items) = rs_store::val::loads(text).ok()? else { return None };
    items.iter().map(|p| {
        Some(InkPage {
            index: p.get("index")?.as_i64()?,
            width: p.get("width")?.as_i64()?,
            height: p.get("height")?.as_i64()?,
            dark_mode: p.get("darkMode").map(Val::truthy),
            composite: matches!(p.get("kind").and_then(Val::as_str), Some("page") | Some("pdf-ink")),
            over_pdf: p.get("kind").and_then(Val::as_str) == Some("pdf-ink"),
            notice: p.get("notice").and_then(Val::as_str).map(str::to_string),
        })
    }).collect()
}

// ---- inventory memo (ink_render._INVENTORY)

static INVENTORY: Mutex<VecDeque<([u8; 32], Arc<Vec<InkPage>>)>> = Mutex::new(VecDeque::new());

fn inventory_get(key: &[u8; 32]) -> Option<Arc<Vec<InkPage>>> {
    let mut guard = INVENTORY.lock().unwrap();
    let at = guard.iter().position(|(k, _)| k == key)?;
    let entry = guard.remove(at).unwrap();
    let value = entry.1.clone();
    guard.push_back(entry);
    Some(value)
}

fn inventory_put(key: [u8; 32], pages: Arc<Vec<InkPage>>) {
    let mut guard = INVENTORY.lock().unwrap();
    guard.retain(|(k, _)| *k != key);
    guard.push_back((key, pages));
    while guard.len() > 512 {
        guard.pop_front();
    }
}

fn child_args(exe: &Path, mode: &str, page: Option<i64>) -> Vec<String> {
    let mut args = vec![exe.to_string_lossy().into_owned(), CHILD_FLAG.into(), mode.into(), std::process::id().to_string()];
    if let Some(i) = page {
        args.push(i.to_string());
    }
    args
}

/// One page's body prepared alone (`page`) or its forced composite (`forced`):
/// in the isolated child when self-exec is configured, else in-process.
fn single_body(snapshot: &Val, index: i64, mode: &str, child: Option<&Path>) -> Vec<u8> {
    match child {
        None if mode == "page" => prepare::page_body(snapshot, index),
        None => prepare::forced_composite(snapshot, index),
        Some(exe) => {
            let Some(data) = snapshot_bytes(snapshot) else { return Vec::new() };
            let args = child_args(exe, mode, Some(index));
            match sandbox::run_with_cwd(&args, Some(data), JOB_LIMITS, true, Duration::from_secs(180), None, false) {
                Ok(o) if o.code == Some(0) && o.stdout.len() <= prepare::MAX_INPUT => o.stdout,
                _ => Vec::new(),
            }
        }
    }
}

/// `ink_render.inspect_render_pages(snapshot)`: admitted page metadata,
/// memoized per exact snapshot; `[]` on any failure.
pub fn inspect_render_pages(snapshot: &Val, child: Option<&Path>) -> Vec<InkPage> {
    let Some(data) = snapshot_bytes(snapshot) else { return Vec::new() };
    let key = sha256(&data);
    if let Some(hit) = inventory_get(&key) {
        return hit.to_vec();
    }
    // Inventory and bodies use the same note worker: the first page need not
    // launch another child and decode the entire note a second time.
    let slot_key = (key, true);
    let (slot, fresh) = note_slot(slot_key);
    if fresh && start(snapshot, data, -1, slot.clone(), slot_key, child).is_err() {
        return Vec::new();
    }
    let raw = match wait_for(&slot, None, Duration::from_secs(185)) {
        Some(Lookup::Inventory(Some(raw))) => raw,
        _ => { forget_note(slot_key); return Vec::new(); }
    };
    match parse_inventory(&raw) {
        Some(pages) => {
            let pages = Arc::new(pages);
            inventory_put(key, pages.clone());
            pages.to_vec()
        }
        None => Vec::new(),
    }
}

// ---- per-note preparation cache filled by one producer (child or thread)

#[derive(Default)]
struct NoteBodies {
    inventory: Option<Vec<u8>>,
    pages: HashMap<i64, Option<Vec<u8>>>,
    done: bool,
    bytes: usize,
    requested: HashSet<i64>,
    pending: VecDeque<i64>,
    sender: Option<std::sync::mpsc::Sender<i64>>,
    child_init: Option<[Duration; 8]>,
    page_times: HashMap<i64, (Duration, Duration)>,
}

type Slot = Arc<(Mutex<NoteBodies>, Condvar)>;
type SlotKey = ([u8; 32], bool);
static BODIES: Mutex<VecDeque<(SlotKey, Slot)>> = Mutex::new(VecDeque::new());

fn note_slot(key: SlotKey) -> (Slot, bool) {
    let mut guard = BODIES.lock().unwrap();
    if let Some(at) = guard.iter().position(|(k, _)| *k == key) {
        let entry = guard.remove(at).unwrap();
        let slot = entry.1.clone();
        guard.push_back(entry);
        return (slot, false);
    }
    let slot: Slot = Arc::new((Mutex::new(NoteBodies::default()), Condvar::new()));
    guard.push_back((key, slot.clone()));
    while guard.len() > 4 || guard.iter().map(|(_, s)| s.0.lock().map(|b| b.bytes).unwrap_or(0)).sum::<usize>() > BODY_BUDGET {
        if guard.len() <= 1 {
            break;
        }
        // Background work must not evict a foreground preparation slot.
        let at = guard.iter().position(|(key, _)| !key.1).unwrap_or(0);
        if let Some((_, old)) = guard.remove(at) { cancel_note(&old); }
    }
    (slot, true)
}

fn cancel_note(slot: &Slot) {
    let mut state = slot.0.lock().unwrap();
    state.done = true;
    state.sender.take();
    slot.1.notify_all();
}

fn forget_note(key: SlotKey) {
    let mut cache = BODIES.lock().unwrap();
    if let Some(at) = cache.iter().position(|(k, _)| *k == key) {
        let (_, slot) = cache.remove(at).unwrap();
        cancel_note(&slot);
    }
}

fn request_body(slot: &Slot, index: i64) {
    let mut state = slot.0.lock().unwrap();
    if !state.requested.insert(index) { return; }
    if let Some(sender) = &state.sender {
        let _ = sender.send(index);
    } else {
        state.pending.push_back(index);
        slot.1.notify_all();
    }
}

fn read_exact(stream: &mut impl Read, n: usize) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; n];
    stream.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

/// Store one prepared body; false when the budget is exhausted.
fn store_body(slot: &Slot, index: i64, body: Option<Vec<u8>>) -> bool {
    let (lock, wake) = &**slot;
    let size = body.as_ref().map_or(0, Vec::len);
    let mut state = lock.lock().unwrap();
    if state.bytes + size > BODY_BUDGET {
        state.pages.insert(index, None);
        wake.notify_all();
        return false;
    }
    state.bytes += size;
    state.pages.insert(index, body);
    wake.notify_all();
    true
}

fn finish(slot: &Slot) {
    let (lock, wake) = &**slot;
    let mut state = lock.lock().unwrap();
    state.done = true;
    state.sender.take();
    wake.notify_all();
}

fn set_inventory(slot: &Slot, inventory: Option<Vec<u8>>) {
    let (lock, wake) = &**slot;
    let mut state = lock.lock().unwrap();
    state.inventory = inventory;
    wake.notify_all();
}

/// Inventory first, then the wanted page, then every other admitted page.
fn order(inventory: &[u8], wanted: i64) -> Vec<i64> {
    let mut order: Vec<i64> = parse_inventory(inventory).unwrap_or_default().iter().map(|p| p.index).collect();
    match order.iter().position(|i| *i == wanted) {
        Some(at) => {
            order.remove(at);
            order.insert(0, wanted);
            order
        }
        None => Vec::new(),
    }
}

/// In-process producer: retain the verified note and prepare only requested pages.
fn start_note_thread(snapshot: Val, _wanted: i64, slot: Slot) {
    let fallback = slot.clone();
    let spawned = std::thread::Builder::new().name("notes-ink-prepare".into()).spawn(move || {
        let session = prepare::PreparedSession::new(&snapshot).ok();
        let inventory = session.as_ref().map_or_else(|| "[]".to_string(), |s| s.inventory()).into_bytes();
        set_inventory(&slot, Some(inventory));
        loop {
            let index = {
                let mut state = slot.0.lock().unwrap();
                while state.pending.is_empty() && !state.done {
                    let (next, timed) = slot.1.wait_timeout(state, Duration::from_secs(180)).unwrap();
                    state = next;
                    if timed.timed_out() { state.done = true; }
                }
                if state.done { break; }
                state.pending.pop_front().unwrap()
            };
            let body = session.as_ref().map_or_else(Vec::new, |s| s.page_body(index));
            let body = if body.is_empty() { None } else { Some(body) };
            if !store_body(&slot, index, body) { break; }
        }
        finish(&slot);
    });
    if spawned.is_err() { finish(&fallback); }
}

/// Self-exec producer: inventory first, then requested body and timing frames.
fn start_note_child(exe: &Path, data: Vec<u8>, wanted: i64, slot: Slot, key: SlotKey) -> Result<(), Fail> {
    let args = child_args(exe, "render-demand", Some(wanted));
    let started = Instant::now();
    let (mut child, mut stdin, mut stdout) = match sandbox::spawn_duplex(&args, JOB_LIMITS, true, None) {
        Ok(v) => v,
        Err(_) => {
            forget_note(key);
            return Err(Fail::os("ink preparation could not start"));
        }
    };
    let spawn_time = started.elapsed();
    let (tx, rx) = std::sync::mpsc::channel::<i64>();
    {
        let mut state = slot.0.lock().unwrap();
        if state.done { drop(state); let _ = child.kill(); let _ = child.wait(); return Err(coded_ink_render(NOT_PREPARED, "ink_prepare_unavailable")); }
        state.sender = Some(tx.clone());
        while let Some(index) = state.pending.pop_front() { let _ = tx.send(index); }
    }
    std::thread::spawn(move || {
        if stdin.write_all(&(data.len() as u64).to_le_bytes()).and_then(|_| stdin.write_all(&data)).is_err() { return; }
        for index in rx {
            if stdin.write_all(&(index as i32).to_le_bytes()).and_then(|_| stdin.flush()).is_err() { break; }
        }
    });
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(180);
        let killer_pid = child.id() as i32;
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            if stop_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_err() {
                // SAFETY: kill() takes plain integers. The child was spawned in its own session (pgid == pid) and is not reaped until after this watchdog is stopped, so the group id cannot have been reused.
                unsafe { libc::kill(-killer_pid, libc::SIGKILL) };
            }
        });
        // The child parses hostile note data: its frame lengths are bounded
        // like the body frames below instead of allocating up to 4 GiB.
        let header = read_exact(&mut stdout, 4)
            .and_then(|n| <[u8; 4]>::try_from(n.as_slice()).ok())
            .map(|n| u32::from_le_bytes(n) as usize)
            .filter(|&size| size <= prepare::MAX_INPUT + 1)
            .and_then(|size| read_exact(&mut stdout, size));
        let ready_time = started.elapsed();
        set_inventory(&slot, header);
        let mut pending_body: Option<(i64, Option<Vec<u8>>)> = None;
        loop {
            let Some(head) = read_exact(&mut stdout, 8) else { break };
            let index = i32::from_le_bytes(head[..4].try_into().unwrap()) as i64;
            let size = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
            if size > prepare::MAX_INPUT + 1 {
                break;
            }
            let body = if size == 0 { None } else { read_exact(&mut stdout, size) };
            if size != 0 && body.is_none() { break; }
            if index == -1 {
                let Some(raw) = body.filter(|b| b.len() == 6 * 8) else { break };
                let mut times = [Duration::ZERO; 8];
                times[0] = spawn_time; times[1] = ready_time;
                for i in 0..6 { times[i + 2] = Duration::from_micros(u64::from_le_bytes(raw[i*8..i*8+8].try_into().unwrap())); }
                slot.0.lock().unwrap().child_init = Some(times);
            } else if index == -2 {
                let Some(raw) = body.filter(|b| b.len() == 20) else { break };
                let page = i32::from_le_bytes(raw[..4].try_into().unwrap()) as i64;
                let build = Duration::from_micros(u64::from_le_bytes(raw[4..12].try_into().unwrap()));
                let write = Duration::from_micros(u64::from_le_bytes(raw[12..20].try_into().unwrap()));
                let Some((body_page, body)) = pending_body.take() else { break };
                if page != body_page { break; }
                slot.0.lock().unwrap().page_times.insert(page, (build, write));
                if !store_body(&slot, page, body) { break; }
            } else if index >= 0 && pending_body.is_none() {
                pending_body = Some((index, body));
            } else { break; }
        }
        // Stop a child we no longer read from (body budget reached, bad
        // frame): it would otherwise block on its full pipe until the 180 s
        // watchdog, holding every waiter of this note. The child is not reaped
        // yet, so its process group id cannot have been reused.
        // SAFETY: kill() takes plain integers; see the watchdog above.
        unsafe { libc::kill(-killer_pid, libc::SIGKILL) };
        let _ = child.wait();
        let _ = stop_tx.send(());
        let _ = watchdog.join();
        finish(&slot);
    });
    Ok(())
}

fn start(snapshot: &Val, data: Vec<u8>, wanted: i64, slot: Slot, key: SlotKey, child: Option<&Path>) -> Result<(), Fail> {
    match child {
        Some(exe) => start_note_child(exe, data, wanted, slot, key),
        None => {
            start_note_thread(snapshot.clone(), wanted, slot);
            Ok(())
        }
    }
}

enum Lookup {
    Inventory(Option<Vec<u8>>),
    Body(Option<Vec<u8>>),
}

fn wait_for(slot: &Slot, page: Option<i64>, timeout: Duration) -> Option<Lookup> {
    let (lock, wake) = &**slot;
    let deadline = Instant::now() + timeout;
    let mut state = lock.lock().unwrap();
    loop {
        match page {
            None => {
                if state.inventory.is_some() || state.done {
                    return Some(Lookup::Inventory(state.inventory.clone()));
                }
            }
            Some(i) => {
                if let Some(body) = state.pages.get(&i) {
                    return Some(Lookup::Body(body.clone()));
                }
                if state.done {
                    return Some(Lookup::Body(None));
                }
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        state = wake.wait_timeout(state, deadline - now).unwrap().0;
    }
}

/// Inventory page + renderer body for `index`, sharing one producer per note.
/// A page the shared producer left out (its 96 MiB body budget ran out on a
/// note with many heavy pages) is prepared on its own.
fn page_body(snapshot: &Val, index: i64, child: Option<&Path>) -> Result<(InkPage, Vec<u8>), Fail> {
    page_body_with_identity(snapshot, index, child, None, true)
}

fn page_body_with_identity(snapshot: &Val, index: i64, child: Option<&Path>,
                           identity: Option<([u8; 32], Arc<Vec<u8>>)>, foreground: bool) -> Result<(InkPage, Vec<u8>), Fail> {
    let (key, data) = match identity {
        Some(identity) => identity,
        None => { let data = Arc::new(snapshot_bytes(snapshot).ok_or_else(|| ink_render(NOT_FOUND))?); (sha256(&data), data) }
    };
    match shared_page_body(snapshot, index, child, (key, foreground), &data) {
        Err(e) if e.message == NOT_PREPARED => {
            let Some(page) = inventory_get(&key).and_then(|p| p.iter().find(|p| p.index == index).cloned()) else { return Err(e) };
            let body = single_body(snapshot, index, "page", child);
            if body.is_empty() { Err(e) } else { Ok((page, body)) }
        }
        other => other,
    }
}

fn shared_page_body(snapshot: &Val, index: i64, child: Option<&Path>, key: SlotKey, data: &Arc<Vec<u8>>) -> Result<(InkPage, Vec<u8>), Fail> {
    let (slot, fresh) = note_slot(key);
    request_body(&slot, index);
    if fresh {
        start(snapshot, (**data).clone(), index, slot.clone(), key, child)?;
    } else {
        let missing = {
            let known = slot.0.lock().unwrap();
            !known.pages.contains_key(&index) && known.done
        };
        if missing {
            forget_note(key);
            let (again, _) = note_slot(key);
            request_body(&again, index);
            start(snapshot, (**data).clone(), index, again.clone(), key, child)?;
            return finish_page(key, again, index);
        }
    }
    finish_page(key, slot, index)
}

fn finish_page(key: SlotKey, slot: Slot, index: i64) -> Result<(InkPage, Vec<u8>), Fail> {
    let timeout = Duration::from_secs(185);
    let inventory = match inventory_get(&key.0) {
        Some(hit) => hit,
        None => {
            let raw = match wait_for(&slot, None, timeout) {
                Some(Lookup::Inventory(Some(raw))) => raw,
                None => {
                    forget_note(key);
                    return Err(coded_ink_render(TIMED_OUT, "ink_prepare_timeout"));
                }
                _ => {
                    forget_note(key);
                    return Err(ink_render(NOT_FOUND));
                }
            };
            let pages = Arc::new(parse_inventory(&raw).unwrap_or_default());
            inventory_put(key.0, pages.clone());
            pages
        }
    };
    let Some(page) = inventory.iter().find(|p| p.index == index).cloned() else { return Err(ink_render(NOT_FOUND)) };
    match wait_for(&slot, Some(index), timeout) {
        Some(Lookup::Body(Some(body))) => {
            let mut state = slot.0.lock().unwrap();
            if let Some(times) = state.child_init.take() {
                for (offset, time) in times.into_iter().enumerate() { record_elapsed(CHILD_SPAWN + offset, time); }
            }
            if let Some((build, write)) = state.page_times.remove(&index) {
                record_elapsed(CHILD_PAGE, build);
                record_elapsed(CHILD_WRITE, write);
            }
            Ok((page, body))
        },
        None => {
            forget_note(key);
            Err(coded_ink_render(TIMED_OUT, "ink_prepare_timeout"))
        }
        _ => Err(coded_ink_render(NOT_PREPARED, "ink_prepare_unavailable")),
    }
}

// ---- self-exec child

fn frame(out: &mut impl Write, index: i64, body: &[u8]) -> std::io::Result<()> {
    out.write_all(&(index as i32).to_le_bytes())?;
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(body)?;
    out.flush()
}

/// Entry point of the self-exec preparation child. Call it first in the
/// engine's `main()` with the full argv:
///
/// ```ignore
/// let args: Vec<String> = std::env::args().collect();
/// if let Some(code) = notes_preview::run_child(&args) { std::process::exit(code) }
/// ```
///
/// Returns None when argv does not denote a child job. Exit codes follow
/// `ink_render_job.main`: 0 ok, 65 usage, 70 internal failure, 71 parent
/// binding failed. stdin: the snapshot JSON; argv: `CHILD_FLAG mode ppid [page]`.
pub fn run_child(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some(CHILD_FLAG) {
        return None;
    }
    let rest = args[2..].to_vec();
    Some(std::panic::catch_unwind(move || child_main(&rest)).unwrap_or(70))
}

fn child_main(args: &[String]) -> i32 {
    if !(2..=3).contains(&args.len()) {
        return 71;
    }
    let bound = args[1].parse::<i64>().is_ok_and(sandbox::bind_parent);
    if !bound {
        return 71;
    }
    let mode = args[0].as_str();
    let paged = matches!(mode, "render" | "render-demand" | "page" | "forced");
    if !(mode == "inventory" || paged) || paged != (args.len() == 3) {
        return 65;
    }
    if sandbox::apply_limits(&JOB_LIMITS, true).is_err() {
        return 70;
    }
    let read_start = Instant::now();
    let mut raw = Vec::new();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    if mode == "render-demand" {
        let Some(head) = read_exact(&mut input, 8) else { return 70 };
        let size = u64::from_le_bytes(head.try_into().unwrap());
        if size > 256 * 1024 * 1024 { return 70; }
        let Some(bytes) = read_exact(&mut input, size as usize) else { return 70 };
        raw = bytes;
    } else if (&mut input).take(256 * 1024 * 1024).read_to_end(&mut raw).is_err() {
        return 70;
    }
    let read_time = read_start.elapsed();
    let parse_start = Instant::now();
    let Ok(text) = String::from_utf8(raw) else { return 70 };
    let Ok(snapshot) = rs_store::val::loads(&text) else { return 70 };
    let json_time = parse_start.elapsed();
    let mut out = std::io::stdout().lock();
    if mode == "page" || mode == "forced" {
        // One page body only (the streaming producer ran out of budget, or the
        // renderer refused the page and the composite stand-in is wanted).
        let Ok(index) = args[2].parse::<i64>() else { return 65 };
        let body = if mode == "page" { prepare::page_body(&snapshot, index) } else { prepare::forced_composite(&snapshot, index) };
        return if out.write_all(&body).and_then(|_| out.flush()).is_ok() { 0 } else { 70 };
    }
    prepare::take_prep_times();
    let session = prepare::PreparedSession::new(&snapshot).ok();
    let prep_times = prepare::take_prep_times();
    let inventory_start = Instant::now();
    let inventory = session.as_ref().map_or_else(|| "[]".to_string(), |s| s.inventory());
    let inventory_time = inventory_start.elapsed();
    if mode == "inventory" {
        return if out.write_all(inventory.as_bytes()).and_then(|_| out.flush()).is_ok() { 0 } else { 70 };
    }
    let Ok(wanted) = args[2].parse::<i64>() else { return 65 };
    if out.write_all(&(inventory.len() as u32).to_le_bytes()).and_then(|_| out.write_all(inventory.as_bytes()))
        .and_then(|_| out.flush()).is_err()
    {
        return 70;
    }
    if mode == "render-demand" {
        let mut metadata = Vec::with_capacity(48);
        for time in [read_time, json_time, prep_times[0], prep_times[1], prep_times[2], inventory_time] {
            metadata.extend_from_slice(&(time.as_micros().min(u64::MAX as u128) as u64).to_le_bytes());
        }
        if frame(&mut out, -1, &metadata).is_err() { return 70; }
        while let Some(bytes) = read_exact(&mut input, 4) {
            let index = i32::from_le_bytes(bytes.try_into().unwrap()) as i64;
            let start = Instant::now();
            let body = session.as_ref().map_or_else(Vec::new, |s| s.page_body(index));
            let build = start.elapsed();
            let start = Instant::now();
            if frame(&mut out, index, &body).is_err() { break; }
            let write = start.elapsed();
            let mut meta = Vec::with_capacity(20);
            meta.extend_from_slice(&(index as i32).to_le_bytes());
            meta.extend_from_slice(&(build.as_micros().min(u64::MAX as u128) as u64).to_le_bytes());
            meta.extend_from_slice(&(write.as_micros().min(u64::MAX as u128) as u64).to_le_bytes());
            if frame(&mut out, -2, &meta).is_err() { break; }
        }
        return 0;
    }
    for index in order(inventory.as_bytes(), wanted) {
        let body = session.as_ref().map_or_else(Vec::new, |s| s.page_body(index));
        if frame(&mut out, index, &body).is_err() {
            break;
        }
    }
    0
}

#[cfg(test)]
mod demand_tests {
    use super::*;
    #[test]
    fn foreground_has_a_distinct_slot_from_background() {
        let key = [173; 32];
        let (background, _) = note_slot((key, false));
        let (foreground, _) = note_slot((key, true));
        assert!(!Arc::ptr_eq(&background, &foreground));
        forget_note((key, false));
        forget_note((key, true));
    }
    #[test]
    fn producer_only_prepares_requested_pages() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/objects_note.json")).unwrap();
        let fixture = rs_store::val::loads(&text).unwrap();
        let snapshot = fixture.get("snapshot").unwrap().clone();
        let slot: Slot = Arc::new((Mutex::new(NoteBodies::default()), Condvar::new()));
        request_body(&slot, 0);
        start_note_thread(snapshot, 0, slot.clone());
        assert!(matches!(wait_for(&slot, Some(0), Duration::from_secs(5)), Some(Lookup::Body(Some(_)))));
        assert!(wait_for(&slot, Some(2), Duration::from_millis(250)).is_none(), "unrequested page was prepared eagerly");
        request_body(&slot, 2);
        assert!(matches!(wait_for(&slot, Some(2), Duration::from_secs(5)), Some(Lookup::Body(Some(_)))));
        cancel_note(&slot);
    }
}
