//! `previews.PreviewManager`: private, read-only page previews decoded on
//! demand by a few parallel workers, cached as verified RGBA PNGs.
use crate::codec;
use crate::env::PreviewEnv;
use crate::ink::{InkBackend, InkPage, NativeInk};
use crate::payload::{inspect_ink_payload, payload_from_snapshot};
use crate::pdf::{self, PdfBackground};
use crate::util::{self, preview, private_directory, read_cached, store_fail, J};
use crate::{MAX_PIXELS, MAX_PNG, MAX_QUEUED};
use notes_host::{Fail, Kind, Val};
use rs_ink::core::payload::CanvasPreviewCandidate;
use rs_store::storage::StoreCore;
use rs_store::val::py_eq;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const GENERIC_FAILURE: &str = "Sayfa görüntüsü hazırlanamadı. Metin önizlemesi korunuyor.";
const PDF_PARTIAL: &str = "PDF zemini gösteriliyor. Bu sayfadaki not ve çizimler henüz bu görüntüye eklenmiyor.";
const PDF_UNSUPPORTED: &str = "Bu sayfanın PDF yerleşimi henüz tam görüntülenemiyor.";

/// `previews.InkGeometryPreview`
#[derive(Clone, Debug, PartialEq)]
pub struct InkGeometryPreview {
    pub page_index: i64,
    pub width: i64,
    pub height: i64,
    pub sha256: String,
    pub is_dark_mode: bool,
    /// `page-reconstructed` composite (images, shapes, text boxes, blank
    /// sheet, approximated ink) instead of a renderer-only ink page.
    pub composite: bool,
    /// Calm reason the composite is approximate (None: complete).
    pub notice: Option<String>,
}

/// One page's preview source: a native canvas cache, a PDF background (with
/// an optional canvas overlay) or a locally reconstructed ink page.
#[derive(Clone, Debug, PartialEq)]
pub enum Candidate {
    Canvas(CanvasPreviewCandidate),
    Pdf(PdfBackground),
    Ink(InkGeometryPreview),
}

impl Candidate {
    pub fn width(&self) -> i64 {
        match self {
            Candidate::Canvas(c) => c.width,
            Candidate::Pdf(c) => c.width,
            Candidate::Ink(c) => c.width,
        }
    }
    pub fn height(&self) -> i64 {
        match self {
            Candidate::Canvas(c) => c.height,
            Candidate::Pdf(c) => c.height,
            Candidate::Ink(c) => c.height,
        }
    }
    pub fn sha256(&self) -> &str {
        match self {
            Candidate::Canvas(c) => &c.sha256,
            Candidate::Pdf(c) => &c.sha256,
            Candidate::Ink(c) => &c.sha256,
        }
    }
    pub fn is_dark_mode(&self) -> bool {
        match self {
            Candidate::Canvas(c) => c.is_dark_mode,
            Candidate::Pdf(_) => false,
            Candidate::Ink(c) => c.is_dark_mode,
        }
    }
}

/// `PreviewManager._source(identity)`: the note row identity plus session
/// path and offline flag; compared with Python tuple equality.
#[derive(Clone, Debug)]
pub struct Source(pub Vec<Val>);

impl PartialEq for Source {
    fn eq(&self, other: &Source) -> bool {
        self.0.len() == other.0.len() && self.0.iter().zip(&other.0).all(|(a, b)| py_eq(a, b))
    }
}

/// `previews._Entry`: small inventory metadata, never raw bodies.
#[derive(Clone, Debug)]
pub struct Entry {
    pub source: Source,
    /// Manifest page dicts (`index`, `width`, `height`, `available`,
    /// `darkMode`, optional `previewKind`/`complete`/`previewNotice`).
    pub pages: Vec<Val>,
    pub candidates: BTreeMap<i64, Candidate>,
}

pub type Decoder = Arc<dyn Fn(&[u8], (i64, i64)) -> Result<Vec<u8>, Fail> + Send + Sync>;
pub type Fetcher = Arc<dyn Fn(&CanvasPreviewCandidate, &dyn PreviewEnv, &dyn Fn() -> Result<(), Fail>) -> Result<Vec<u8>, Fail> + Send + Sync>;
pub type EntryHook = Arc<dyn Fn(&str) -> Result<Entry, Fail> + Send + Sync>;
pub type SourceHook = Arc<dyn Fn(&str) -> Result<Source, Fail> + Send + Sync>;

/// Construction options (the Python keyword arguments and the attributes the
/// tests patch). `Default` gives the production behaviour.
#[derive(Default, Clone)]
pub struct PreviewOptions {
    /// `decoder=` (default `previews._codec`, the optional SPI codec).
    pub decoder: Option<Decoder>,
    /// `fetcher=` (default `fetch_preview` over `PreviewEnv::http`).
    pub fetcher: Option<Fetcher>,
    /// Picture downloads for page composites (default: `fetcher`, else
    /// `fetch_preview`).
    pub media_fetcher: Option<Fetcher>,
    /// `samsung_notes.ink_render` (default [`NativeInk`]).
    pub ink: Option<Arc<dyn InkBackend>>,
    /// `previews.WORKERS` (default [`crate::default_workers`]).
    pub workers: Option<usize>,
    /// Replaces `_entry` (tests).
    pub entry_hook: Option<EntryHook>,
    /// Replaces `_source` (tests).
    pub source_hook: Option<SourceHook>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    Ready,
    Error,
}

/// One `self.jobs[key]` record.
#[derive(Clone, Debug)]
pub struct Job {
    pub state: JobState,
    pub error: Option<String>,
    pub priority: i64,
    pub local_pdf: Option<PathBuf>,
    pub retry_after: Option<Instant>,
    /// Ready through another file: the local composite of a canvas page whose
    /// cache could not be fetched/decoded, or a partial composite (pictures
    /// not downloaded yet).
    pub served: Option<PathBuf>,
    /// Failed only because the page is not downloaded and offline.
    pub offline: bool,
}

struct QueueItem {
    priority: i64,
    sequence: u64,
    key: String,
    identity: String,
    source: Source,
    candidate: Candidate,
    path: PathBuf,
    session: Option<PathBuf>,
    local_pdf: Option<PathBuf>,
    queued: Instant,
}

impl PartialEq for QueueItem {
    fn eq(&self, other: &Self) -> bool {
        (self.priority, self.sequence) == (other.priority, other.sequence)
    }
}
impl Eq for QueueItem {}
impl PartialOrd for QueueItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueueItem {
    /// Reversed: BinaryHeap pops the smallest (priority, sequence) first.
    fn cmp(&self, other: &Self) -> Ordering {
        (other.priority, other.sequence).cmp(&(self.priority, self.sequence))
    }
}

/// `queue.PriorityQueue` with `task_done`/`join` accounting.
struct Queue {
    heap: Mutex<(BinaryHeap<QueueItem>, usize)>,
    ready: Condvar,
    idle: Condvar,
}

impl Queue {
    fn put(&self, item: QueueItem) {
        let mut guard = self.heap.lock().unwrap();
        guard.0.push(item);
        guard.1 += 1;
        self.ready.notify_one();
    }

    fn get(&self, timeout: Duration) -> Option<QueueItem> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.heap.lock().unwrap();
        loop {
            if let Some(item) = guard.0.pop() {
                return Some(item);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            guard = self.ready.wait_timeout(guard, deadline - now).unwrap().0;
        }
    }

    fn task_done(&self) {
        let mut guard = self.heap.lock().unwrap();
        guard.1 = guard.1.saturating_sub(1);
        if guard.1 == 0 {
            self.idle.notify_all();
        }
    }

    fn join(&self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut guard = self.heap.lock().unwrap();
        while guard.1 > 0 {
            match deadline {
                None => guard = self.idle.wait(guard).unwrap(),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return false;
                    }
                    guard = self.idle.wait_timeout(guard, d - now).unwrap().0;
                }
            }
        }
        true
    }
}

#[derive(Default)]
struct State {
    /// Insertion-ordered like the Python dict (max 128, oldest evicted).
    entries: Vec<(String, Arc<Entry>)>,
    /// Insertion-ordered `self.jobs`; replacing a key keeps its position.
    jobs: Vec<(String, Job)>,
    workers: usize,
}

impl State {
    fn job(&self, key: &str) -> Option<&Job> {
        self.jobs.iter().find(|(k, _)| k == key).map(|(_, j)| j)
    }
    fn job_mut(&mut self, key: &str) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|(k, _)| k == key).map(|(_, j)| j)
    }
    fn set_job(&mut self, key: &str, job: Job) {
        match self.job_mut(key) {
            Some(slot) => *slot = job,
            None => self.jobs.push((key.to_string(), job)),
        }
    }
    fn pop_job(&mut self, key: &str) {
        self.jobs.retain(|(k, _)| k != key);
    }
    /// Room for one more job at `priority`. A full queue must not turn away a
    /// more urgent request (the page on screen is 0, thumbnails 1, warm-up 2):
    /// it takes the slot of the least urgent pending job, whose queue entry
    /// the worker then skips. False only when nothing less urgent is waiting.
    fn make_room(&mut self, priority: i64) -> bool {
        if self.jobs.iter().filter(|(_, j)| matches!(j.state, JobState::Pending | JobState::Running)).count() < MAX_QUEUED {
            return true;
        }
        let victim = self.jobs.iter()
            .filter(|(_, j)| j.state == JobState::Pending && j.priority > priority)
            .max_by_key(|(_, j)| j.priority)
            .map(|(k, _)| k.clone());
        match victim {
            Some(victim) => { self.pop_job(&victim); true }
            None => false,
        }
    }
}

struct Inner {
    store: Arc<StoreCore>,
    env: Arc<dyn PreviewEnv>,
    directory: PathBuf,
    root: PathBuf,
    decoder: Decoder,
    fetcher: Fetcher,
    media_fetcher: Fetcher,
    ink: Arc<dyn InkBackend>,
    workers: usize,
    entry_hook: Option<EntryHook>,
    source_hook: Option<SourceHook>,
    state: Mutex<State>,
    finished: Condvar,
    queue: Queue,
    sequence: AtomicU64,
    stop: AtomicBool,
    running: AtomicUsize,
    exited: (Mutex<()>, Condvar),
}

/// Shared handle; worker threads keep the inner state alive until `close()`.
pub struct PreviewManager {
    inner: Arc<Inner>,
}

impl Drop for PreviewManager {
    fn drop(&mut self) {
        // Python leaves daemon threads running; here the last handle stops
        // the workers so they release the shared state.
        self.inner.stop.store(true, AtomicOrdering::SeqCst);
    }
}

fn account_of(store: &StoreCore) -> Result<String, Fail> {
    match store.meta("account_key").map_err(store_fail)? {
        None => Ok("offline".into()),
        Some(Val::Str(s)) => Ok(s),
        Some(other) => Err(Fail::type_(format!("can only concatenate str (not \"{}\") to str", other.type_name()))),
    }
}

impl PreviewManager {
    /// `PreviewManager(engine)`
    pub fn new(store: Arc<StoreCore>, env: Arc<dyn PreviewEnv>) -> Result<PreviewManager, Fail> {
        PreviewManager::with_options(store, env, PreviewOptions::default())
    }

    /// `PreviewManager(engine, decoder=..., fetcher=...)` plus test hooks.
    pub fn with_options(store: Arc<StoreCore>, env: Arc<dyn PreviewEnv>, options: PreviewOptions) -> Result<PreviewManager, Fail> {
        let directory = private_directory(&store.directory.join("page-previews"))?;
        let root = env.project_root();
        let decoder = options.decoder.unwrap_or_else(|| {
            let root = root.clone();
            Arc::new(move |raw: &[u8], size: (i64, i64)| codec::decode(&root, raw, size))
        });
        let fetcher = options.fetcher.unwrap_or_else(|| Arc::new(crate::fetch::fetch_preview));
        let media_fetcher = options.media_fetcher.unwrap_or_else(|| fetcher.clone());
        let ink = options.ink.unwrap_or_else(|| Arc::new(NativeInk::new(root.clone(), env.ink_child_executable())));
        Ok(PreviewManager {
            inner: Arc::new(Inner {
                store, env, directory, root, decoder, fetcher, media_fetcher, ink,
                workers: options.workers.unwrap_or_else(crate::default_workers).max(1),
                entry_hook: options.entry_hook, source_hook: options.source_hook,
                state: Mutex::new(State::default()), finished: Condvar::new(),
                queue: Queue { heap: Mutex::new((BinaryHeap::new(), 0)), ready: Condvar::new(), idle: Condvar::new() },
                sequence: AtomicU64::new(0), stop: AtomicBool::new(false), running: AtomicUsize::new(0),
                exited: (Mutex::new(()), Condvar::new()),
            }),
        })
    }

    /// `self.directory` (`<store>/page-previews`, private).
    pub fn directory(&self) -> &Path {
        &self.inner.directory
    }

    /// `manifest(identity)` -> `{'pages': [...], 'providerAvailable': bool}`.
    pub fn manifest(&self, identity: &str) -> Result<Val, Fail> {
        self.inner.manifest(identity)
    }

    /// `request(identity, page_index, priority=, wait=)`: PNG bytes when
    /// ready, else None; `wait` seconds block for a result.
    pub fn request(&self, identity: &str, page_index: i64, priority: i64, wait: f64) -> Result<Option<Vec<u8>>, Fail> {
        Inner::request(&self.inner, identity, page_index, priority, wait)
    }

    /// `prefetch(limit=MAX_QUEUED // 2)`: warm first pages at priority 2.
    pub fn prefetch(&self, limit: usize) -> Result<usize, Fail> {
        Inner::prefetch(&self.inner, limit)
    }

    /// `close()`
    pub fn close(&self) {
        self.inner.stop.store(true, AtomicOrdering::SeqCst);
        self.inner.queue.ready.notify_all();
    }

    /// `join(timeout)`: wait for the worker threads to exit.
    pub fn join(&self, timeout: Duration) {
        let (lock, wake) = &self.inner.exited;
        let deadline = Instant::now() + timeout;
        let mut guard = lock.lock().unwrap();
        while self.inner.running.load(AtomicOrdering::SeqCst) > 0 {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            guard = wake.wait_timeout(guard, deadline - now).unwrap().0;
        }
    }

    /// `_source(identity)`
    pub fn source(&self, identity: &str) -> Result<Source, Fail> {
        self.inner.source(identity)
    }

    /// `_entry(identity)`
    pub fn entry(&self, identity: &str) -> Result<Arc<Entry>, Fail> {
        self.inner.entry(identity)
    }

    /// `_path(candidate)`: `<directory>/<sha256(account:sha256 recipe)>.png`.
    pub fn path(&self, candidate: &Candidate) -> Result<PathBuf, Fail> {
        self.inner.path(candidate)
    }

    /// `engine.apply_template`'s PDF lookup: the PDF placement whose sha256 is
    /// `sha256` in the source note snapshot dict, from `<directory>/pdf-files`
    /// or (when `fetch`) downloaded through the engine transport.
    /// `Ok(None)`: not cached and `fetch` is false (the engine raises
    /// `template_offline`). Errors as engine.py: no such placement or an
    /// unsupported layout -> `template_unavailable` 422; a failed download ->
    /// `template_unavailable` 503 with the PdfBackgroundError message.
    pub fn template_pdf(&self, document: &Val, sha256: &str, account: &str, fetch: bool) -> Result<Option<PathBuf>, Fail> {
        let unavailable = || Fail::store("template_unavailable", "Şablon dosyası bu sürümde okunamadı.", 422);
        let payload = payload_from_snapshot(document)?;
        let candidates = match pdf::inspect_pdf_backgrounds(&payload) {
            Ok(c) => c,
            Err(e) if e.kind == Kind::PdfBackground => return Err(unavailable()),
            Err(e) => return Err(e),
        };
        let Some(candidate) = candidates.into_iter().find(|c| c.sha256 == sha256) else { return Err(unavailable()) };
        let directory = self.inner.directory.join("pdf-files");
        if let Some(path) = pdf::cached_pdf(&candidate, &directory, account)? {
            return Ok(Some(path));
        }
        if !fetch {
            return Ok(None);
        }
        match pdf::fetch_pdf(&candidate, self.inner.env.as_ref(), &directory, account, None) {
            Ok(path) => Ok(Some(path)),
            Err(e) if e.kind == Kind::PdfBackground => Err(Fail::store("template_unavailable", &e.message, 503)),
            Err(e) => Err(e),
        }
    }

    /// `queue.join()`: wait until every queued job was processed.
    pub fn wait_idle(&self, timeout: Option<Duration>) -> bool {
        self.inner.queue.join(timeout)
    }

    /// A copy of `self.jobs[key]`.
    pub fn job(&self, key: &str) -> Option<Job> {
        self.inner.state.lock().unwrap().job(key).cloned()
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        // The job table stays usable after a worker panic (see `worker`).
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn source(&self, identity: &str) -> Result<Source, Fail> {
        if let Some(hook) = &self.source_hook {
            return hook(identity);
        }
        let row = self.store.with_db(|db| {
            use rs_store::db::Sql;
            db.query("SELECT id,remote_commit,loaded,deleted_remote,revision,modified_ms FROM notes WHERE id=?",
                     &rs_store::p![identity])
        }).map_err(store_fail)?;
        let Some(row) = row.one() else { return Err(Fail::store("not_found", "Not bulunamadı.", 404)) };
        let mut values: Vec<Val> = (0..6).map(|i| row.at(i)).collect();
        values.push(self.env.session_path().map_or(Val::Null, |p| Val::s(p.to_string_lossy())));
        values.push(Val::Bool(self.env.offline()));
        Ok(Source(values))
    }

    fn connected(&self) -> bool {
        self.env.session_path().is_some() && !self.env.offline()
    }

    fn entry(&self, identity: &str) -> Result<Arc<Entry>, Fail> {
        if let Some(hook) = &self.entry_hook {
            return hook(identity).map(Arc::new);
        }
        let source = self.source(identity)?;
        {
            let state = self.lock();
            if let Some((_, e)) = state.entries.iter().find(|(k, _)| k == identity) {
                if e.source == source {
                    return Ok(e.clone());
                }
            }
        }
        let row = self.store.raw(identity).map_err(store_fail)?;
        let mut candidates: BTreeMap<i64, Candidate> = BTreeMap::new();
        let mut pages: Vec<Val> = Vec::new();
        let base = row.get("base_document").cloned().unwrap_or(Val::Null);
        if base.truthy() && row.get("loaded").is_some_and(Val::truthy) && !row.get("deleted_remote").is_some_and(Val::truthy) {
            if let Err(e) = self.inventory(&base, &mut candidates, &mut pages) {
                // Unknown/locked documents stay text previews. No URL escapes.
                let tolerated = e.is_document() || e.kind == Kind::Ink || e.is_value() || e.is_key() || e.is_type();
                if !tolerated {
                    return Err(e);
                }
                pages.clear();
            }
        }
        let entry = Arc::new(Entry { source, pages, candidates });
        let mut state = self.lock();
        // Retain only small inventory metadata, never raw multipart bodies.
        if state.entries.len() >= 128 {
            state.entries.remove(0);
        }
        match state.entries.iter_mut().find(|(k, _)| k == identity) {
            Some(slot) => slot.1 = entry.clone(),
            None => state.entries.push((identity.to_string(), entry.clone())),
        }
        Ok(entry)
    }

    /// The `try:` body of `_entry`.
    fn inventory(&self, base: &Val, candidates: &mut BTreeMap<i64, Candidate>, pages: &mut Vec<Val>) -> Result<(), Fail> {
        let text = base.as_str().ok_or_else(|| Fail::type_("the JSON object must be str, bytes or bytearray"))?;
        let snapshot = notes_host::snapshot::loads(text)?;
        let payload = payload_from_snapshot(&snapshot)?;
        let inventory = inspect_ink_payload(&payload)?;
        let mut canvases = inventory.canvas_candidates.clone();
        canvases.sort_by_key(|c| (c.is_dark_mode, -(c.width * c.height)));
        for candidate in canvases {
            let index = candidate.page_index as i64;
            if candidate.download_url.as_deref().is_some_and(|u| !u.is_empty()) && candidate.width * candidate.height <= MAX_PIXELS
                && !candidates.contains_key(&index)
            {
                candidates.insert(index, Candidate::Canvas(candidate));
            }
        }
        // Insertion-ordered like the Python dict.
        let mut pdf_pages: Vec<(i64, Vec<PdfBackground>)> = Vec::new();
        let mut unsupported_pdf_pages: BTreeSet<usize> = BTreeSet::new();
        match pdf::inspect_pdf_backgrounds(&payload) {
            Ok(found) => {
                for candidate in found {
                    match pdf_pages.iter_mut().find(|(i, _)| *i == candidate.page_index) {
                        Some((_, list)) => list.push(candidate),
                        None => pdf_pages.push((candidate.page_index, vec![candidate])),
                    }
                }
                let can_decode_overlay = if pdf_pages.is_empty() { false } else { codec::provider_available(&self.root) };
                for (index, placements) in &pdf_pages {
                    if placements.len() == 1 && placements[0].download_url.as_deref().is_some_and(|u| !u.is_empty()) {
                        let mut background = placements[0].clone();
                        if let Some(Candidate::Canvas(overlay)) = candidates.get(index) {
                            if background.annotation_objects != 0 && !overlay.is_dark_mode
                                && (overlay.width, overlay.height) == (background.width, background.height)
                            {
                                // In offline mode keep the usable PDF background
                                // if the optional annotation cache is unavailable.
                                let overlay = overlay.clone();
                                let usable = (self.connected() && can_decode_overlay)
                                    || self.path(&Candidate::Canvas(overlay.clone()))?.is_file();
                                if usable {
                                    background.overlay = Some(overlay);
                                }
                            }
                        }
                        candidates.insert(*index, Candidate::Pdf(background));
                    }
                }
            }
            Err(e) if e.kind == Kind::PdfBackground => {
                // Keep an already supported canvas cache when a PDF layout
                // is unknown. Never guess clipping or multiple-PDF geometry.
                pdf_pages.clear();
                unsupported_pdf_pages = pdf::pdf_page_indices(&payload)?;
            }
            Err(e) => return Err(e),
        }
        let in_pdf = |index: i64| pdf_pages.iter().any(|(i, _)| *i == index);
        let unsupported = |index: i64| index >= 0 && unsupported_pdf_pages.contains(&(index as usize));
        // Native reconstructed geometry is a local fallback for admitted
        // pure-ink pages without a saved canvas, and the local ink layer of PDF
        // pages whose native layer is not available.
        let pdf_without_layer = candidates.values().any(|c| matches!(c, Candidate::Pdf(p) if p.annotation_objects != 0));
        // Page composites (pictures, shapes, blank sheets, approximated ink)
        // are drawn in-process (resvg) and need neither the ink renderer nor
        // its sandbox; without them (renderer not built, no bwrap/rsvg) only
        // the renderer-only pages stay unavailable, as before.
        let mut over_pdf: Vec<InkPage> = Vec::new();
        let renderer = self.ink.provider_available();
        if candidates.len() < inventory.page_count || pdf_without_layer {
            let attempt = (|| -> Result<(), Fail> {
                let digest = snapshot.get("raw_sha256").and_then(Val::as_str).ok_or_else(|| Fail::key("'raw_sha256'"))?.to_string();
                for rendered in self.ink_inventory(&snapshot, &digest)? {
                    if !renderer && !rendered.composite {
                        continue;
                    }
                    let index = rendered.index;
                    if rendered.over_pdf {
                        over_pdf.push(rendered);
                        continue;
                    }
                    if !candidates.contains_key(&index) && !in_pdf(index) && !unsupported(index) {
                        candidates.insert(index, Candidate::Ink(InkGeometryPreview {
                            page_index: index, width: rendered.width, height: rendered.height,
                            sha256: digest.clone(), is_dark_mode: rendered.dark_mode.unwrap_or(false),
                            composite: rendered.composite, notice: rendered.notice.clone(),
                        }));
                    }
                }
                Ok(())
            })();
            if let Err(e) = attempt {
                let tolerated = e.is_document() || e.kind == Kind::Ink || e.is_value() || e.is_key() || e.is_type();
                if !tolerated {
                    return Err(e);
                }
            }
        }
        if let Some(digest) = snapshot.get("raw_sha256").and_then(Val::as_str) {
            for page in over_pdf {
                if let Some(Candidate::Pdf(c)) = candidates.get_mut(&page.index) {
                    // Also kept next to a native layer: drawn if that fails.
                    if c.annotation_objects != 0 && (c.width, c.height) == (page.width, page.height) {
                        c.local_ink = Some(InkGeometryPreview {
                            page_index: page.index, width: page.width, height: page.height, sha256: digest.to_string(),
                            is_dark_mode: false, composite: true, notice: page.notice.clone(),
                        });
                    }
                }
            }
        }
        // Page geometry from the note itself, so a page without an image still
        // has its size (the pen works on it, the placeholder keeps the shape).
        let sizes = page_sizes(&payload);
        let size = |index: i64, pick: fn((i64, i64)) -> i64| {
            sizes.get(index as usize).copied().flatten().map_or(Val::Null, |s| Val::Int(pick(s)))
        };
        for index in 0..inventory.page_count as i64 {
            let item = candidates.get(&index);
            let mut page = vec![
                ("index", Val::Int(index)),
                ("width", item.map_or_else(|| size(index, |s| s.0), |c| Val::Int(c.width()))),
                ("height", item.map_or_else(|| size(index, |s| s.1), |c| Val::Int(c.height()))),
                ("available", Val::Bool(item.is_some())),
                ("darkMode", Val::Bool(match item {
                    Some(Candidate::Pdf(_)) => false,
                    Some(Candidate::Ink(c)) => c.is_dark_mode,
                    _ => false,
                })),
            ];
            match item {
                Some(Candidate::Pdf(c)) => {
                    let notes = c.overlay.is_some() || c.local_ink.is_some();
                    let local_notice = c.overlay.is_none();
                    page.push(("previewKind", Val::s(if notes { "pdf-with-notes" } else { "pdf-background" })));
                    page.push(("complete", Val::Bool(c.complete())));
                    let notice = match &c.local_ink {
                        Some(local) if !c.complete() && local_notice => local.notice.as_deref().map_or(Val::Null, Val::s),
                        _ if c.complete() => Val::Null,
                        _ => Val::s(PDF_PARTIAL),
                    };
                    page.push(("previewNotice", notice));
                }
                Some(Candidate::Ink(c)) if c.composite => {
                    page.push(("previewKind", Val::s("page-reconstructed")));
                    page.push(("complete", Val::Bool(c.notice.is_none())));
                    page.push(("previewNotice", c.notice.as_deref().map_or(Val::Null, Val::s)));
                }
                Some(Candidate::Ink(_)) => {
                    page.push(("previewKind", Val::s("ink-reconstructed")));
                    page.push(("complete", Val::Bool(true)));
                    page.push(("previewNotice", Val::Null));
                }
                _ if in_pdf(index) || unsupported(index) => {
                    page.push(("complete", Val::Bool(false)));
                    page.push(("previewNotice", Val::s(PDF_UNSUPPORTED)));
                }
                _ => {}
            }
            pages.push(Val::dict(page));
        }
        Ok(())
    }

    /// `_ink_inventory(snapshot)`: renderer page admission, persisted per
    /// exact source and recipe under `page-previews/ink-inventory/`.
    fn ink_inventory(&self, snapshot: &Val, digest: &str) -> Result<Vec<InkPage>, Fail> {
        let key = util::sha256_hex(format!("{digest}:{}", crate::INK_INVENTORY_RECIPE).as_bytes());
        let path = private_directory(&self.directory.join("ink-inventory"))?.join(format!("{key}.json"));
        if let Ok(Some(raw)) = read_cached(&path, 1024 * 1024) {
            if let Some(pages) = stored_inventory(&raw) {
                return Ok(pages);
            }
        }
        let pages = self.ink.inspect_render_pages(snapshot);
        let encoded = J::List(pages.iter().map(|p| {
            let mut items = vec![("index", J::Int(p.index as i128)), ("width", J::Int(p.width as i128)), ("height", J::Int(p.height as i128))];
            if let Some(dark) = p.dark_mode {
                items.push(("darkMode", J::Bool(dark)));
            }
            if p.composite {
                items.push(("kind", J::Str(if p.over_pdf { "pdf-ink" } else { "page" }.into())));
                items.push(("notice", p.notice.clone().map_or(J::Null, J::Str)));
            }
            J::obj(items)
        }).collect()).dumps(false).unwrap_or_else(|_| "[]".into());
        let _ = util::write_private(&path, encoded.as_bytes(), 8, false, &|| Ok(()));
        Ok(pages)
    }

    fn manifest(&self, identity: &str) -> Result<Val, Fail> {
        let entry = self.entry(identity)?;
        let values: Vec<&Candidate> = entry.candidates.values().collect();
        let mut available = if values.iter().any(|c| matches!(c, Candidate::Pdf(_))) { pdf::pdf_provider_available() } else { false };
        if values.iter().any(|c| matches!(c, Candidate::Ink(_))) {
            available = available || self.ink.provider_available();
        }
        if values.iter().any(|c| matches!(c, Candidate::Canvas(_))) || values.is_empty() {
            available = available || codec::provider_available(&self.root);
        }
        Ok(Val::dict(vec![("pages", Val::List(entry.pages.clone())), ("providerAvailable", Val::Bool(available))]))
    }

    fn path(&self, candidate: &Candidate) -> Result<PathBuf, Fail> {
        let account = account_of(&self.store)?;
        let recipe = match candidate {
            Candidate::Canvas(c) if c.is_dark_mode => ":native-preview-v1:light-v1".to_string(), // dark cache shown light
            Candidate::Canvas(_) => ":native-preview-v1".to_string(),
            Candidate::Pdf(c) => {
                let mut tuple = String::new();
                J::List(vec![J::Int(c.pdf_page_index as i128), J::Int(c.width as i128), J::Int(c.height as i128),
                    J::List(vec![J::Int(c.rect.0 as i128), J::Int(c.rect.1 as i128), J::Int(c.rect.2 as i128), J::Int(c.rect.3 as i128)]),
                    c.overlay.as_ref().map_or(J::Null, |o| J::Str(o.sha256.clone()))]).dump(&mut tuple, false).map_err(Fail::value)?;
                match (&c.overlay, &c.local_ink) {
                    // The local layer follows the note snapshot, not the PDF.
                    (None, Some(local)) => format!(":pdf-local-ink-v1:{tuple}:{}", local.sha256),
                    _ => format!(":pdf-background-v1:{tuple}"),
                }
            }
            Candidate::Ink(c) if c.composite => format!(":page-reconstructed-v1:[{}, {}, {}]", c.page_index, c.width, c.height),
            Candidate::Ink(c) => format!(":ink-reconstructed-v2:[{}, {}, {}]", c.page_index, c.width, c.height),
        };
        let key = util::sha256_hex(format!("{account}:{}{recipe}", candidate.sha256()).as_bytes());
        Ok(self.directory.join(format!("{key}.png")))
    }

    fn request(this: &Arc<Inner>, identity: &str, page_index: i64, priority: i64, wait: f64) -> Result<Option<Vec<u8>>, Fail> {
        let deadline = Instant::now() + Duration::from_secs_f64(wait.max(0.0));
        loop {
            let raw = Inner::request_once(this, identity, page_index, priority)?;
            if raw.is_some() || wait <= 0.0 {
                return Ok(raw);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let guard = this.lock();
            let _ = this.finished.wait_timeout(guard, (deadline - now).min(Duration::from_secs(1)));
        }
    }

    fn queue_item(&self, key: &str, identity: &str, source: &Source, candidate: &Candidate, path: &Path, priority: i64,
                  local_pdf: Option<PathBuf>) -> QueueItem {
        QueueItem {
            priority, sequence: self.sequence.fetch_add(1, AtomicOrdering::SeqCst), key: key.to_string(),
            identity: identity.to_string(), source: source.clone(), candidate: candidate.clone(), path: path.to_path_buf(),
            session: self.env.session_path(), local_pdf, queued: Instant::now(),
        }
    }

    /// `_request(identity, page_index, priority)`
    fn request_once(this: &Arc<Inner>, identity: &str, page_index: i64, priority: i64) -> Result<Option<Vec<u8>>, Fail> {
        let entry = this.entry(identity)?;
        let Some(candidate) = entry.candidates.get(&page_index) else {
            return Err(Fail::store("preview_unavailable", "Bu sayfa için görüntü önizlemesi bulunmuyor.", 404));
        };
        let path = this.path(candidate)?;
        if let Some(raw) = read_cached(&path, MAX_PNG as u64)? {
            if crate::png_valid(&raw, candidate.width(), candidate.height()) {
                return Ok(Some(raw));
            }
        }
        let key = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // A stand-in image already on disk: the local composite of a canvas
        // page whose cache could not be fetched/decoded, or a composite whose
        // pictures were not downloaded yet. It is returned at once; the full
        // image is retried in the background at most every RETRY_FULL.
        let stand_in = match candidate {
            Candidate::Canvas(_) => Some(local_path(&path)),
            Candidate::Pdf(c) if c.overlay.is_some() && c.local_ink.is_some() => Some(local_path(&path)),
            Candidate::Pdf(c) if c.local_ink.is_some() => Some(partial_path(&path)),
            // Composites, and renderer-only pages whose renderer failed
            // (forced composite stand-in).
            Candidate::Ink(_) => Some(partial_path(&path)),
            _ => None,
        };
        if let Some(stand_in) = &stand_in {
            if let Some(raw) = read_cached(stand_in, MAX_PNG as u64)? {
                if crate::png_valid(&raw, candidate.width(), candidate.height()) {
                    let mut state = this.lock();
                    let retry = match state.job(&key) {
                        None => true,
                        Some(job) => match job.state {
                            JobState::Pending | JobState::Running => false,
                            _ => job.retry_after.is_none_or(|t| Instant::now() >= t),
                        },
                    };
                    if retry && this.connected() && state.jobs.iter().filter(|(_, j)| matches!(j.state, JobState::Pending | JobState::Running)).count() < MAX_QUEUED {
                        state.set_job(&key, Job { state: JobState::Pending, error: None, priority: priority.max(5), local_pdf: None,
                                                  retry_after: None, served: None, offline: false });
                        this.queue.put(this.queue_item(&key, identity, &entry.source, candidate, &path, priority.max(5), None));
                        Inner::spawn_workers(this, &mut state);
                    }
                    return Ok(Some(raw));
                }
            }
        }
        let mut state = this.lock();
        if let Some(job) = state.job(&key).cloned() {
            match job.state {
                JobState::Pending | JobState::Running => {
                    if job.state == JobState::Pending && priority < job.priority {
                        // Re-queue at the higher priority; the stale entry is skipped.
                        if let Some(slot) = state.job_mut(&key) {
                            slot.priority = priority;
                        }
                        this.queue.put(this.queue_item(&key, identity, &entry.source, candidate, &path, priority, job.local_pdf.clone()));
                    }
                    return Ok(None);
                }
                // A missing or corrupt cached file must not remain permanently
                // pending just because an earlier decode once succeeded.
                JobState::Ready => state.pop_job(&key),
                JobState::Error => {
                    if job.retry_after.is_some_and(|t| Instant::now() < t) {
                        // An offline canvas page that cannot be drawn locally
                        // keeps its "not downloaded yet" answer.
                        let (code, status) = if job.offline { ("preview_offline", 503) } else { ("preview_unavailable", 422) };
                        return Err(Fail::store(code, job.error.as_deref().unwrap_or(""), status));
                    }
                    // The frontend stops polling errors; a later open may retry.
                    state.pop_job(&key);
                }
            }
        }
        let local_pdf = match candidate {
            Candidate::Pdf(c) => pdf::cached_pdf(c, &this.directory.join("pdf-files"), &account_of(&this.store)?)?,
            _ => None,
        };
        // Canvas pages are queued offline too: the worker draws them locally.
        if (this.env.session_path().is_none() || this.env.offline()) && local_pdf.is_none()
            && !matches!(candidate, Candidate::Ink(_) | Candidate::Canvas(_))
        {
            return Err(Fail::store("preview_offline", "Bu sayfa henüz indirilmemiş. Bağlantı kurduğunda tekrar açabilirsin.", 503));
        }
        if !state.make_room(priority) {
            return Err(Fail::store("preview_busy", "Diğer sayfalar hazırlanıyor. Biraz sonra tekrar açabilirsin.", 429));
        }
        if state.jobs.len() >= 4 * MAX_QUEUED {
            if let Some(at) = state.jobs.iter().position(|(_, j)| !matches!(j.state, JobState::Pending | JobState::Running)) {
                state.jobs.remove(at);
            }
        }
        state.set_job(&key, Job { state: JobState::Pending, error: None, priority, local_pdf: local_pdf.clone(), retry_after: None, served: None, offline: false });
        this.queue.put(this.queue_item(&key, identity, &entry.source, candidate, &path, priority, local_pdf));
        Inner::spawn_workers(this, &mut state);
        Ok(None)
    }

    fn spawn_workers(this: &Arc<Inner>, state: &mut State) {
        while state.workers < this.workers {
            let number = state.workers;
            state.workers += 1;
            this.running.fetch_add(1, AtomicOrdering::SeqCst);
            let inner = this.clone();
            let spawned = std::thread::Builder::new().name(format!("notes-page-preview-{number}")).spawn(move || {
                inner.worker();
                inner.running.fetch_sub(1, AtomicOrdering::SeqCst);
                let (lock, wake) = &inner.exited;
                let _held = lock.lock().unwrap();
                wake.notify_all();
            });
            if spawned.is_err() {
                this.running.fetch_sub(1, AtomicOrdering::SeqCst);
            }
        }
    }

    fn prefetch(this: &Arc<Inner>, limit: usize) -> Result<usize, Fail> {
        let rows = this.store.with_db(|db| {
            use rs_store::db::Sql;
            db.query("SELECT id FROM notes WHERE loaded=1 AND deleted_remote=0 AND trashed=0 ORDER BY modified_ms DESC", &[])
        }).map_err(store_fail)?;
        let ids: Vec<String> = rows.iter().map(|r| r.text("id").unwrap_or_default().to_string()).collect();
        let mut queued = 0;
        for id in ids {
            if queued >= limit || this.stop.load(AtomicOrdering::SeqCst) {
                break;
            }
            let attempt = (|| -> Result<bool, Fail> {
                if !this.entry(&id)?.candidates.contains_key(&0) {
                    return Ok(false);
                }
                Ok(Inner::request_once(this, &id, 0, 2)?.is_none())
            })();
            match attempt {
                Ok(true) => queued += 1,
                Ok(false) => {}
                Err(e) if e.is_store() => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(queued)
    }

    /// `_cache_png(path, png, width, height, guard)`
    fn cache_png(path: &Path, png: &[u8], width: i64, height: i64, guard: &dyn Fn() -> Result<(), Fail>) -> Result<(), Fail> {
        if !crate::png_valid(png, width, height) {
            return Err(preview("Sayfa görüntüsünün boyutu doğrulanamadı."));
        }
        util::write_private(path, png, 8, true, guard)
    }

    /// `_loop()`
    fn worker(&self) {
        while !self.stop.load(AtomicOrdering::SeqCst) {
            let Some(item) = self.queue.get(Duration::from_millis(500)) else { continue };
            {
                let mut state = self.lock();
                match state.job_mut(&item.key) {
                    Some(job) if job.state == JobState::Pending && job.priority == item.priority => job.state = JobState::Running,
                    _ => {
                        drop(state);
                        self.queue.task_done();
                        continue;
                    }
                }
            }
            // A panic while rendering a hostile note must not kill this worker:
            // the job would stay Running and, once every worker died, previews
            // would never be produced again until restart.
            let started = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.process(&item)))
                .unwrap_or_else(|_| Err(Fail::os("preview worker panicked")));
            timing(&item, started, &result);
            {
                let mut state = self.lock();
                let job = match result {
                    Ok(None) => Job { state: JobState::Ready, error: None, priority: 0, local_pdf: None, retry_after: None, served: None, offline: false },
                    // Served by a stand-in: retry the full image later only.
                    Ok(Some(served)) => Job { state: JobState::Ready, error: None, priority: 0, local_pdf: None,
                                              retry_after: Some(Instant::now() + RETRY_FULL), served: Some(served), offline: false },
                    Err(e) => {
                        let known = e.kind == Kind::Preview || e.kind == Kind::PdfBackground || e.is_api() || e.is_store();
                        let offline = e.is_store() && e.code == "preview_offline";
                        Job { state: JobState::Error, error: Some(if known { e.message } else { GENERIC_FAILURE.to_string() }),
                              priority: 0, local_pdf: None, retry_after: Some(Instant::now() + Duration::from_secs(5)), served: None, offline }
                    }
                };
                state.set_job(&item.key, job);
                self.finished.notify_all();
            }
            self.queue.task_done();
        }
    }

    /// `PreviewManager._fetch_pdf_refreshing(identity, candidate, guard)`:
    /// signed URLs in a stored snapshot expire; on a refused link read the
    /// note once from Cloud (read-only), store it and retry once.
    fn fetch_pdf_refreshing(&self, identity: &str, candidate: &PdfBackground, guard: &dyn Fn() -> Result<(), Fail>) -> Result<PathBuf, Fail> {
        let account = account_of(&self.store)?;
        let directory = self.directory.join("pdf-files");
        match pdf::fetch_pdf(candidate, self.env.as_ref(), &directory, &account, Some(guard)) {
            Err(e) if pdf::is_link_expired(&e) => {
                let fresh = self.env.refresh_note(identity)?;
                let parsed = crate::payload::payload_from_snapshot(&fresh)?;
                let renewed = pdf::inspect_pdf_backgrounds(&parsed)?
                    .into_iter()
                    .find(|c| c.page_index == candidate.page_index && c.sha256 == candidate.sha256);
                let Some(renewed) = renewed.filter(|c| c.download_url.as_deref().is_some_and(|u| !u.is_empty())) else {
                    return Err(pdf_error("PDF sayfası notta değişti; sayfayı yeniden açabilirsin."));
                };
                guard()?;
                pdf::fetch_pdf(&renewed, self.env.as_ref(), &directory, &account, Some(guard))
            }
            other => other,
        }
    }

    fn process(&self, item: &QueueItem) -> Result<Option<PathBuf>, Fail> {
        let candidate = &item.candidate;
        // Local pages and canvas pages (drawn locally when offline) run offline.
        let local_ok = matches!(candidate, Candidate::Ink(_) | Candidate::Canvas(_));
        let guard = || -> Result<(), Fail> {
            if self.stop.load(AtomicOrdering::SeqCst) || self.env.session_path() != item.session
                || (self.env.offline() && item.local_pdf.is_none() && !local_ok)
                || self.source(&item.identity)? != item.source
                || self.path(candidate)? != item.path
            {
                return Err(preview("Not veya hesap değişti. Sayfayı yeniden açabilirsin."));
            }
            Ok(())
        };
        guard()?;
        let (width, height) = (candidate.width(), candidate.height());
        let png = match candidate {
            Candidate::Ink(c) => {
                let snapshot = self.snapshot(&item.identity)?;
                if !snapshot.get("raw_sha256").is_some_and(|v| py_eq(v, &Val::s(c.sha256.clone()))) {
                    return Err(preview("Not değişti. Sayfayı yeniden açabilirsiniz."));
                }
                let (png, complete) = self.render_local(&item.identity, c, &snapshot, &guard)?;
                if !complete {
                    // Pictures not downloaded (or ink approximated): keep it as
                    // a stand-in; the full image is retried later.
                    let partial = partial_path(&item.path);
                    guard()?;
                    Inner::cache_png(&partial, &png, width, height, &guard)?;
                    return Ok(Some(partial));
                }
                png
            }
            Candidate::Pdf(c) => {
                let file = match &item.local_pdf {
                    Some(p) => p.clone(),
                    None => self.fetch_pdf_refreshing(&item.identity, c, &guard)?,
                };
                guard()?;
                let mut png = pdf::render_pdf_background(&file, c)?;
                let native = match &c.overlay {
                    Some(overlay) => self.pdf_native_layer(c, overlay, &png, &guard).map(Some),
                    None => Ok(None),
                };
                match native {
                    Ok(Some(layered)) => png = layered,
                    Err(e) => {
                        // Samsung's layer is unavailable: the local layer stands in.
                        let Some(local) = &c.local_ink else { return Err(e) };
                        let snapshot = self.snapshot(&item.identity)?;
                        if !snapshot.get("raw_sha256").is_some_and(|v| py_eq(v, &Val::s(local.sha256.clone()))) {
                            return Err(e);
                        }
                        let (layered, _) = self.render_local_over(&item.identity, local, &snapshot, &guard, Some(png))?;
                        let stand_in = local_path(&item.path);
                        guard()?;
                        Inner::cache_png(&stand_in, &layered, width, height, &guard)?;
                        return Ok(Some(stand_in));
                    }
                    Ok(None) => {}
                }
                if let (None, Some(local)) = (&c.overlay, &c.local_ink) {
                    // The page's own objects drawn locally over the PDF page.
                    let snapshot = self.snapshot(&item.identity)?;
                    if !snapshot.get("raw_sha256").is_some_and(|v| py_eq(v, &Val::s(local.sha256.clone()))) {
                        return Err(preview("Not değişti. Sayfayı yeniden açabilirsiniz."));
                    }
                    let (layered, complete) = self.render_local_over(&item.identity, local, &snapshot, &guard, Some(png))?;
                    if !complete {
                        let partial = partial_path(&item.path);
                        guard()?;
                        Inner::cache_png(&partial, &layered, width, height, &guard)?;
                        return Ok(Some(partial));
                    }
                    png = layered;
                }
                png
            }
            Candidate::Canvas(c) => {
                let native = (|| -> Result<Vec<u8>, Fail> {
                    if !self.connected() {
                        return Err(Fail::store("preview_offline", "Bu sayfa henüz indirilmemiş. Bağlantı kurduğunda tekrar açabilirsin.", 503));
                    }
                    let raw = (self.fetcher)(c, self.env.as_ref(), &guard)?;
                    guard()?;
                    let png = (self.decoder)(&raw, (width, height))?;
                    if c.is_dark_mode { pdf::light_from_dark(&png, width, height) } else { Ok(png) }
                })();
                match native {
                    Ok(png) => png,
                    Err(e) => {
                        // Samsung's page picture is unavailable (offline, link,
                        // size, or a cache variant the decoder refuses): draw
                        // the page locally when the note allows it.
                        let stopped = self.stop.load(AtomicOrdering::SeqCst);
                        if stopped || guard().is_err() {
                            return Err(e);
                        }
                        match self.canvas_fallback(item, c) {
                            Ok(Some(path)) => return Ok(Some(path)),
                            _ => return Err(e),
                        }
                    }
                }
            }
        };
        guard()?;
        Inner::cache_png(&item.path, &png, width, height, &guard)?;
        Ok(None)
    }

    /// Samsung's canvas layer of a PDF page (cached, else fetched + decoded),
    /// composited over the PDF page image.
    fn pdf_native_layer(&self, c: &PdfBackground, overlay: &CanvasPreviewCandidate, png: &[u8], guard: &dyn Fn() -> Result<(), Fail>) -> Result<Vec<u8>, Fail> {
        guard()?;
        let overlay_candidate = Candidate::Canvas(overlay.clone());
        let layer_path = self.path(&overlay_candidate)?;
        let cached = read_cached(&layer_path, MAX_PNG as u64)?;
        let layer = match cached {
            Some(layer) if crate::png_valid(&layer, c.width, c.height) => layer,
            _ => {
                if self.env.offline() || self.env.session_path().is_none() {
                    return Err(preview("Bu sayfanın çizim katmanı henüz çevrimdışı kullanılabilir değil."));
                }
                let raw = (self.fetcher)(overlay, self.env.as_ref(), guard)?;
                guard()?;
                let layer = (self.decoder)(&raw, (c.width, c.height))?;
                // Keep the verified native layer as well as the composite: the
                // offline inventory then selects the same complete recipe after
                // a restart.
                Inner::cache_png(&layer_path, &layer, c.width, c.height, guard)?;
                layer
            }
        };
        guard()?;
        pdf::compose_pdf_canvas(png, &layer, c.width, c.height)
    }

    fn snapshot(&self, identity: &str) -> Result<Val, Fail> {
        let row = self.store.raw(identity).map_err(store_fail)?;
        let text = row.get("base_document").and_then(Val::as_str)
            .ok_or_else(|| Fail::type_("the JSON object must be str, bytes or bytearray, not NoneType"))?;
        notes_host::snapshot::loads(text)
    }

    /// A local page (renderer-only ink or composite) with its pictures:
    /// verified media cache first, then (connected) a download through the
    /// account transport. Returns the PNG and whether it is complete.
    fn render_local(&self, identity: &str, c: &InkGeometryPreview, snapshot: &Val, guard: &dyn Fn() -> Result<(), Fail>) -> Result<(Vec<u8>, bool), Fail> {
        self.render_local_over(identity, c, snapshot, guard, None)
    }

    /// `render_local` with the PDF page image a PDF page's layer is drawn on.
    fn render_local_over(&self, identity: &str, c: &InkGeometryPreview, snapshot: &Val, guard: &dyn Fn() -> Result<(), Fail>,
                         underlay: Option<Vec<u8>>) -> Result<(Vec<u8>, bool), Fail> {
        let wanted = if c.composite { self.ink.page_media(snapshot, c.page_index) } else { Vec::new() };
        let mut found: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        if let Some(png) = underlay {
            found.insert(crate::ink::prepare::PDF_UNDERLAY.to_string(), png);
        }
        let mut urls: Option<BTreeMap<String, String>> = None;
        for sha in wanted {
            if found.contains_key(&sha) || sha.len() != 64 {
                continue;
            }
            if let Some(bytes) = self.media_cached(&sha)? {
                found.insert(sha, bytes);
                continue;
            }
            if !self.connected() {
                continue;
            }
            if urls.is_none() {
                urls = Some(crate::payload::payload_from_snapshot(snapshot).map(|p| {
                    p.result.download_urls.iter().map(|d| (d.id.to_lowercase(), d.url.clone())).collect()
                }).unwrap_or_default());
            }
            let Some(url) = urls.as_ref().and_then(|u| u.get(&sha)).cloned() else { continue };
            guard()?;
            if let Ok(bytes) = self.fetch_media(identity, &sha, &url, guard) {
                let _ = self.store_media(&sha, &bytes, guard);
                found.insert(sha, bytes);
            }
        }
        guard()?;
        self.ink.render_page(snapshot, c.page_index, &|sha| found.get(sha).cloned())
    }

    fn media_path(&self, sha256: &str) -> Result<PathBuf, Fail> {
        let account = account_of(&self.store)?;
        let key = util::sha256_hex(format!("{account}:{sha256}:media-v1").as_bytes());
        Ok(private_directory(&self.directory.join("media"))?.join(format!("{key}.bin")))
    }

    /// A downloaded picture, only if its bytes still hash to the content id.
    fn media_cached(&self, sha256: &str) -> Result<Option<Vec<u8>>, Fail> {
        let path = self.media_path(sha256)?;
        match read_cached(&path, rs_render::compose::MAX_IMAGE_BYTES as u64)? {
            Some(raw) if util::sha256_hex(&raw) == sha256 => Ok(Some(raw)),
            _ => Ok(None),
        }
    }

    fn store_media(&self, sha256: &str, bytes: &[u8], guard: &dyn Fn() -> Result<(), Fail>) -> Result<(), Fail> {
        util::write_private(&self.media_path(sha256)?, bytes, 8, false, guard)
    }

    /// One picture through the account transport (broker -> signed file URL,
    /// SHA256 verified). A refused link is renewed once by reading the note
    /// from Cloud (read-only), as for PDF pages.
    fn fetch_media(&self, identity: &str, sha256: &str, url: &str, guard: &dyn Fn() -> Result<(), Fail>) -> Result<Vec<u8>, Fail> {
        let item = CanvasPreviewCandidate {
            page_index: 0, key: 0, width: 1, height: 1, is_dark_mode: false, bind_number: String::new(),
            sha256: sha256.to_string(), download_url: Some(url.to_string()),
        };
        match (self.media_fetcher)(&item, self.env.as_ref(), guard) {
            Ok(raw) => Ok(raw),
            Err(first) => {
                if !self.renew_links(identity) {
                    return Err(first);
                }
                let fresh = self.snapshot(identity)?;
                let renewed = crate::payload::payload_from_snapshot(&fresh)?.result.download_urls.iter()
                    .find(|d| d.id.to_lowercase() == sha256).map(|d| d.url.clone());
                match renewed {
                    Some(url) if url != item.download_url.clone().unwrap_or_default() => {
                        guard()?;
                        (self.media_fetcher)(&CanvasPreviewCandidate { download_url: Some(url), ..item }, self.env.as_ref(), guard)
                    }
                    _ => Err(first),
                }
            }
        }
    }

    /// Re-read a note from Cloud at most once per RETRY_FULL to renew its
    /// signed download links. False when not connected or recently renewed.
    fn renew_links(&self, identity: &str) -> bool {
        static RENEWED: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
        if !self.connected() {
            return false;
        }
        {
            let mut done = RENEWED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            done.retain(|(_, at)| at.elapsed() < RETRY_FULL);
            if done.iter().any(|(id, _)| id == identity) {
                return false;
            }
            done.push((identity.to_string(), Instant::now()));
        }
        self.env.refresh_note(identity).is_ok()
    }

    /// The local page of a canvas-cache page, scaled to the canvas size and
    /// cached next to it (`<key>.local.png`). None when the note's local
    /// renderer does not admit the page.
    fn canvas_fallback(&self, item: &QueueItem, c: &CanvasPreviewCandidate) -> Result<Option<PathBuf>, Fail> {
        let guard = || -> Result<(), Fail> {
            if self.stop.load(AtomicOrdering::SeqCst) || self.source(&item.identity)? != item.source {
                return Err(preview("Not veya hesap değişti. Sayfayı yeniden açabilirsin."));
            }
            Ok(())
        };
        let snapshot = self.snapshot(&item.identity)?;
        let Some(digest) = snapshot.get("raw_sha256").and_then(Val::as_str).map(str::to_string) else { return Ok(None) };
        let index = c.page_index as i64;
        let Some(page) = self.ink_inventory(&snapshot, &digest)?.into_iter().find(|p| p.index == index && !p.over_pdf) else { return Ok(None) };
        let local = InkGeometryPreview {
            page_index: index, width: page.width, height: page.height, sha256: digest,
            is_dark_mode: page.dark_mode.unwrap_or(false), composite: page.composite, notice: page.notice.clone(),
        };
        let (png, _) = self.render_local(&item.identity, &local, &snapshot, &guard)?;
        let png = if (page.width, page.height) == (c.width, c.height) { png } else { rescale_png(&png, (page.width, page.height), (c.width, c.height))? };
        let path = local_path(&item.path);
        guard()?;
        Inner::cache_png(&path, &png, c.width, c.height, &guard)?;
        Ok(Some(path))
    }
}

/// How long a stand-in (local drawing of a canvas page, partial composite)
/// is served before the full image is tried again.
const RETRY_FULL: Duration = Duration::from_secs(600);

/// `<key>.local.png`: the local drawing of a canvas-cache page.
fn local_path(path: &Path) -> PathBuf {
    path.with_extension("local.png")
}

/// `<key>.partial.png`: a composite whose pictures are not all drawn yet.
fn partial_path(path: &Path) -> PathBuf {
    path.with_extension("partial.png")
}

/// Resample a page PNG to another size (resvg, bilinear) for a stand-in that
/// must match the canvas candidate's dimensions.
fn rescale_png(png: &[u8], from: (i64, i64), to: (i64, i64)) -> Result<Vec<u8>, Fail> {
    use rs_render::compose::{compose, ImagePlacement, Item};
    let _ = from;
    let place = ImagePlacement { rect: [0.0, 0.0, to.0 as f64, to.1 as f64], crop: None, flip_h: false, flip_v: false, degrees: 0.0, opacity: 1.0 };
    let (pixels, drawn) = compose(to.0, to.1, 0xffff_ffff, &[Item::Image(Some(png), place)], None)
        .map_err(|_| preview("Sayfa görüntüsünün boyutu doğrulanamadı."))?;
    if !drawn {
        return Err(preview("Sayfa görüntüsünün boyutu doğrulanamadı."));
    }
    rs_render::pixels::rgba_png(to.0, to.1, &pixels).map_err(|_| preview("Sayfa görüntüsünün boyutu doğrulanamadı."))
}

/// `(width, height)` of every page in the note's single pageList, from the
/// payload XML (None where an attribute is not a plain positive integer).
fn page_sizes(payload: &crate::payload::Parsed) -> Vec<Option<(i64, i64)>> {
    for xml in payload.xml_documents() {
        let Ok(root) = rs_ops::xml::parse_bytes(&xml) else { continue };
        let Some(resource) = root.iter().find(|n| n.local() == "noteresource") else { continue };
        let Some(list) = resource.children.iter().find(|n| n.tag == "pageList") else { return Vec::new() };
        return list.children.iter().filter(|p| p.tag == "page").map(|p| {
            let dim = |k: &str| p.get(k).filter(|v| !v.is_empty() && v.len() <= 6 && v.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|v| v.parse::<i64>().ok()).filter(|v| (1..=32768).contains(v));
            Some((dim("width")?, dim("height")?))
        }).collect();
    }
    Vec::new()
}

/// A persisted inventory file: a list of exactly-keyed page dicts.
fn stored_inventory(raw: &[u8]) -> Option<Vec<InkPage>> {
    let Val::List(items) = rs_store::val::loads(std::str::from_utf8(raw).ok()?).ok()? else { return None };
    items.iter().map(|p| {
        let Val::Dict(fields) = p else { return None };
        let mut keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        let composite = keys == ["darkMode", "height", "index", "kind", "notice", "width"];
        if keys != ["darkMode", "height", "index", "width"] && !composite {
            return None;
        }
        let over_pdf = composite && p.get("kind")?.as_str()? == "pdf-ink";
        if composite && !over_pdf && p.get("kind")?.as_str()? != "page" {
            return None;
        }
        let notice = match p.get("notice") {
            Some(Val::Str(n)) => Some(n.clone()),
            Some(Val::Null) | None => None,
            Some(_) => return None,
        };
        Some(InkPage {
            index: p.get("index")?.as_i64()?,
            width: p.get("width")?.as_i64()?,
            height: p.get("height")?.as_i64()?,
            dark_mode: Some(p.get("darkMode")?.truthy()),
            composite,
            notice,
            over_pdf,
        })
    }).collect()
}

impl crate::thumbnails::PagePreviews for PreviewManager {
    fn source(&self, identity: &str) -> Result<Source, Fail> {
        PreviewManager::source(self, identity)
    }
    fn entry(&self, identity: &str) -> Result<Arc<Entry>, Fail> {
        PreviewManager::entry(self, identity)
    }
    fn path(&self, candidate: &Candidate) -> Result<PathBuf, Fail> {
        PreviewManager::path(self, candidate)
    }
    fn request(&self, identity: &str, page_index: i64, priority: i64) -> Result<Option<Vec<u8>>, Fail> {
        PreviewManager::request(self, identity, page_index, priority, 0.0)
    }
}

fn pdf_error(message: &str) -> Fail {
    Fail::new(notes_host::Kind::PdfBackground, message)
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn job(state: JobState, priority: i64) -> Job {
        Job { state, error: None, priority, local_pdf: None, retry_after: None, served: None, offline: false }
    }

    #[test]
    fn a_full_queue_gives_the_page_on_screen_a_warm_up_slot() {
        // Before: a full queue answered 429 to the page on screen, the UI gave
        // up and showed "not ready" although the page could be drawn at once.
        let mut state = State::default();
        for i in 0..MAX_QUEUED {
            state.set_job(&format!("warm{i}"), job(JobState::Pending, 2));
        }
        assert!(state.make_room(0));
        assert_eq!(state.jobs.len(), MAX_QUEUED - 1);
        // Equal or more urgent work is never evicted.
        let mut busy = State::default();
        for i in 0..MAX_QUEUED {
            busy.set_job(&format!("open{i}"), job(if i % 2 == 0 { JobState::Running } else { JobState::Pending }, 0));
        }
        assert!(!busy.make_room(0));
        assert!(!busy.make_room(2));
        assert_eq!(busy.jobs.len(), MAX_QUEUED);
    }
}

/// Diagnostics only, off unless NOTES_PREVIEW_TIMING names a file: one JSON
/// line per job with the page kind, priority, queue wait, work time and
/// outcome. No note ids, titles or content.
fn timing(item: &QueueItem, started: Instant, result: &Result<Option<PathBuf>, Fail>) {
    let Some(file) = std::env::var_os("NOTES_PREVIEW_TIMING") else { return };
    let kind = match &item.candidate {
        Candidate::Canvas(_) => "canvas",
        Candidate::Pdf(_) => "pdf",
        Candidate::Ink(_) => "ink",
    };
    let outcome = match result {
        Ok(None) => "ready".to_string(),
        Ok(Some(_)) => "stand-in".to_string(),
        Err(e) => format!("error:{:?}:{}:{}", e.kind, e.code, e.message.replace(['"', '\\', '\n'], " ")),
    };
    let line = format!("{{\"kind\":\"{kind}\",\"priority\":{},\"waitMs\":{},\"workMs\":{},\"outcome\":\"{outcome}\"}}\n",
        item.priority, started.duration_since(item.queued).as_millis(), started.elapsed().as_millis());
    use std::io::Write;
    if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = out.write_all(line.as_bytes());
    }
}
