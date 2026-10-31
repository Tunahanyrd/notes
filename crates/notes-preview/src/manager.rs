//! `previews.PreviewManager`: private, read-only page previews decoded on
//! demand by a few parallel workers, cached as verified RGBA PNGs.
use crate::codec;
use crate::env::PreviewEnv;
use crate::ink::{InkBackend, InkPage, NativeInk};
use crate::ink_tiles::{InkLayerCache, InkLayerCacheKey, InkLayerPageKey};
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
// Internal marker: actual short-lived links are read only when needed.
const INDEXED_URL: &str = "indexed-url";
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
        self.ready.notify_all();
    }

    fn get(&self, timeout: Duration, foreground_only: bool) -> Option<QueueItem> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.heap.lock().unwrap();
        loop {
            if !foreground_only || guard.0.peek().is_some_and(|item| item.priority <= 0) {
                if let Some(item) = guard.0.pop() {
                    return Some(item);
                }
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
    job_sources: BTreeMap<String, (Source, Candidate)>,
    workers: usize,
    decode_failures: BTreeSet<String>,
    download_failures: BTreeSet<String>,
    /// Deterministic PDF full-image failures for each note/source. Old
    /// sources are discarded when the note's source changes.
    pdf_failures: BTreeMap<String, Vec<(String, Source, [u8; 32])>>,
    partial_canvas: BTreeSet<String>,
    retry_counts: BTreeMap<String, u32>,
    snapshots: Vec<(String, Source, Arc<Val>, usize, Option<Arc<BTreeMap<String, String>>>)>,
    snapshot_epoch: Option<i64>,
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
        self.job_sources.remove(key);
    }
    fn discard_stale_job(&mut self, identity: &str, key: &str, source: &Source, candidate: &Candidate) {
        if let Some(failures) = self.pdf_failures.get_mut(identity) {
            failures.retain(|(_, old_source, _)| old_source == source);
            if failures.is_empty() { self.pdf_failures.remove(identity); }
        }
        if self.job_sources.get(key).is_some_and(|(old_source, old_candidate)| old_source != source || old_candidate != candidate) {
            self.pop_job(key);
            self.retry_counts.remove(key);
        }
    }
    fn retry_standin(&self, identity: &str, key: &str, source: &Source, candidate: &Candidate, now: Instant) -> bool {
        let deterministic_failure = match candidate {
            Candidate::Canvas(c) => self.decode_failures.contains(&decode_failure_key(key, c)) || self.download_failures.contains(&download_failure_key(key, c)),
            Candidate::Pdf(_) => {
                let failure_key = pdf_failure_key(key, candidate);
                let fingerprint = pdf_candidate_fingerprint(candidate);
                self.pdf_failures.get(identity).is_some_and(|failures| failures.iter().any(|(k, old_source, old_fingerprint)|
                    *k == failure_key && old_source == source && *old_fingerprint == fingerprint))
            },
            _ => false,
        };
        !deterministic_failure && match self.job(key) {
            None => true,
            Some(job) => match job.state {
                JobState::Pending | JobState::Running => false,
                _ => job.retry_after.is_none_or(|t| now >= t),
            },
        }
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
    schema7: bool,
    inventory_worker: Option<crate::inventory_worker::InventoryWorker>,
    db_monitor: rs_store::db::Db,
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
    ink_layers: Mutex<InkLayerCache>,
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
        let schema7 = store.meta("schema_version").map_err(store_fail)?.as_ref().and_then(Val::as_i64) == Some(7);
        let use_static = schema7 && options.ink.is_none() && options.entry_hook.is_none() && options.source_hook.is_none();
        let inventory_worker = if use_static { Some(crate::inventory_worker::InventoryWorker::start(store.clone())?) } else { None };
        if let Some(worker) = &inventory_worker { worker.wake(); }
        let db_monitor = rs_store::db::Db::open(&store.path).map_err(store_fail)?;
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
                store, schema7, inventory_worker, db_monitor, env, directory, root, decoder, fetcher, media_fetcher, ink,
                workers: options.workers.unwrap_or_else(crate::default_workers).max(1),
                entry_hook: options.entry_hook, source_hook: options.source_hook,
                state: Mutex::new(State::default()), ink_layers: Mutex::new(InkLayerCache::new(64 * 1024 * 1024, 512)), finished: Condvar::new(),
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

    /// Notify the background producer after a committed container pointer changes.
    pub fn wake_inventory(&self) -> Result<(), Fail> {
        if let Some(worker) = &self.inner.inventory_worker {
            if !worker.wake() { return Err(Fail::os("Preview inventory worker unavailable")); }
        }
        Ok(())
    }

    /// Explicitly retry a terminal inventory failure without changing note data.
    pub fn retry_inventory(&self, identity: &str) -> Result<Val, Fail> {
        self.inner.retry_inventory(identity)
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

    pub fn notext(&self, identity: &str, page_index: i64, revision: i64) -> Result<Option<Vec<u8>>, Fail> {
        self.inner.text_composite(identity, page_index, revision, false)
    }

    pub fn fulltext(&self, identity: &str, page_index: i64, revision: i64) -> Result<Option<Vec<u8>>, Fail> {
        self.inner.text_composite(identity, page_index, revision, true)
    }

    pub fn text_ink_layer(&self, identity: &str, page_index: i64, revision: i64, tile: Option<(u8,u32,u32)>, expected: Option<&str>) -> Result<Option<(Vec<u8>,String)>, Fail> {
        self.inner.text_ink_layer(identity,page_index,revision,tile,expected)
    }

    /// A no-ink base (tile=None), or one transparent ink tile, together with
    /// an opaque snapshot token. Busy work returns None for the caller to poll.
    pub fn ink_layer(&self, identity: &str, page_index: i64, revision: i64, tile: Option<(u8,u32,u32)>, expected: Option<&str>) -> Result<Option<(Vec<u8>,String)>, Fail> {
        let started = Instant::now();
        let result = self.inner.ink_layer(identity, page_index, revision, tile, expected);
        if let Some(path) = std::env::var_os("NOTES_PREVIEW_TIMING") {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let outcome = match &result { Ok(Some(_)) => "ready", Ok(None) => "busy", Err(_) => "error" };
                let _ = writeln!(file, "{{\"kind\":\"ink-tile\",\"workMs\":{},\"outcome\":\"{}\"}}", started.elapsed().as_millis(), outcome);
            }
        }
        result
    }

    /// `prefetch(limit=MAX_QUEUED // 2)`: warm first pages at priority 2.
    pub fn prefetch(&self, limit: usize) -> Result<usize, Fail> {
        Inner::prefetch(&self.inner, limit)
    }

    /// `close()`
    pub fn close(&self) {
        self.inner.stop.store(true, AtomicOrdering::SeqCst);
        if let Some(worker) = &self.inner.inventory_worker { worker.stop(); }
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
        drop(guard);
        if self.inner.stop.load(AtomicOrdering::SeqCst) {
            if let Some(worker) = &self.inner.inventory_worker {
                let _ = worker.join_for(deadline.saturating_duration_since(Instant::now()));
            }
        }
    }

    /// `_source(identity)`
    pub fn source(&self, identity: &str) -> Result<Source, Fail> {
        self.inner.source(identity)
    }

    /// Read a current page by index without reconstructing the Cloud snapshot.
    pub fn container_page(&self, identity: &str, page_index: i64, raw_sha256: &str) -> Result<Option<crate::container_page::IndexedPage>, Fail> {
        crate::container_page::read(&self.inner.store, identity, page_index, raw_sha256)
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
        let media = rs_store::media_store::MediaStore::open(&self.inner.store.directory).map_err(store_fail)?;
        if let Some(path) = pdf::cached_pdf_shared(&candidate, &directory, account, &media)? {
            return Ok(Some(path));
        }
        if !fetch {
            return Ok(None);
        }
        match pdf::fetch_pdf_shared(&candidate, self.inner.env.as_ref(), &directory, account, None, &media) {
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
    fn text_ink_layer(&self, identity:&str,index:i64,revision:i64,requested:Option<(u8,u32,u32)>,expected:Option<&str>) -> Result<Option<(Vec<u8>,String)>,Fail> {
        use rs_render::raster::{InkTile,InkTileScale};
        let unavailable=||Fail::store("preview_unavailable","Bu sayfanın çizim katmanı kullanılamıyor.",404);
        let changed=||Fail::store("preview_source_changed","Not değişti.",409);
        let source=self.source(identity)?;
        let actual=source.0.get(4).and_then(Val::as_i64).ok_or_else(unavailable)?;
        let revision=if revision == -1 { actual } else { revision };
        if revision!=actual { return Err(changed()); }
        let epoch=self.refresh_snapshot_epoch()?;
        let snapshot=self.snapshot(identity)?;
        let digest=snapshot.get("raw_sha256").and_then(Val::as_str).ok_or_else(unavailable)?;
        let token=util::sha256_hex(format!("{}:{identity}:{revision}:{index}:{digest}:text-ink-v1",account_of(&self.store)?).as_bytes());
        if expected.is_some_and(|value| value!=token) { return Err(changed()); }
        let key=InkLayerPageKey{note_id:identity.into(),revision,page_index:index,snapshot_digest:util::sha256(token.as_bytes())};
        let mut cache=match self.ink_layers.try_lock() { Ok(value)=>value,Err(std::sync::TryLockError::WouldBlock)=>return Ok(None),Err(std::sync::TryLockError::Poisoned(error))=>error.into_inner() };
        let base_key=InkLayerCacheKey::TextBase(key.clone());let svg_key=InkLayerCacheKey::TextSvg(key.clone());
        let size_key=InkLayerCacheKey::TextSize(key.clone());
        let guard=||if self.source(identity)?==source&&self.snapshot_epoch()?==epoch {Ok(())} else {Err(changed())};
        let mut prepared=None;
        if cache.get(&base_key).is_none()||cache.get(&svg_key).is_none()||cache.get(&size_key).is_none() {
            let page=crate::ink::prepare::text_ink_snapshot_page(&snapshot,index).map_err(|_|unavailable())?.ok_or_else(unavailable)?;
            let mut found=BTreeMap::new();
            if page.over_pdf {
                let payload=payload_from_snapshot(&snapshot)?;
                let pdf=pdf::inspect_pdf_backgrounds(&payload)?.into_iter().find(|p|p.page_index==index).ok_or_else(unavailable)?;
                let media=rs_store::media_store::MediaStore::open(&self.store.directory).map_err(store_fail)?;
                let directory=self.directory.join("pdf-files");let account=account_of(&self.store)?;
                let file=match pdf::cached_pdf_shared(&pdf,&directory,&account,&media)? {
                    Some(file)=>file,None if self.connected()=>pdf::fetch_pdf_shared(&pdf,self.env.as_ref(),&directory,&account,Some(&guard),&media)?,None=>return Err(unavailable()),
                };
                found.insert(crate::ink::prepare::PDF_UNDERLAY.into(),pdf::render_pdf_background(&file,&pdf)?);
            }
            let mut urls=None;
            for sha in crate::ink::composite_media(&page.body) {
                if found.contains_key(&sha) {continue;}
                if let Some(bytes)=self.media_cached(&sha)? {found.insert(sha,bytes);continue;}
                if !self.connected() {continue;}
                let urls=urls.get_or_insert_with(||self.media_urls(&snapshot));
                if let Some(url)=urls.get(&sha) {if let Ok(bytes)=self.fetch_media(identity,&sha,url,&guard) {if util::sha256_hex(&bytes)==sha {let _=self.store_media(&sha,&bytes,&guard);found.insert(sha,bytes);}}}
            }
            let (base,svg,complete)=self.ink.split_text_prepared(&page,&|sha|found.get(sha).cloned()).ok_or_else(unavailable)??;
            if !complete {return Err(unavailable());} guard()?;
            let mut size=Vec::with_capacity(8);size.extend_from_slice(&(page.width as u32).to_le_bytes());size.extend_from_slice(&(page.height as u32).to_le_bytes());
            if !cache.insert(base_key.clone(),Arc::from(base))||!cache.insert(svg_key.clone(),Arc::from(svg))||!cache.insert(size_key.clone(),Arc::from(size)) {return Err(unavailable());}
            prepared=Some((page.width as u32,page.height as u32));
        }
        let (width,height)=match prepared {Some(size)=>size,None=>{let bytes=cache.get(&size_key).ok_or_else(unavailable)?;if bytes.len()!=8{return Err(unavailable());}(u32::from_le_bytes(bytes[0..4].try_into().map_err(|_|unavailable())?),u32::from_le_bytes(bytes[4..8].try_into().map_err(|_|unavailable())?))}};
        let tile=match requested {Some((half,x,y))=>Some(InkTile::new(width,height,InkTileScale::from_half_units(half).ok_or_else(unavailable)?,x,y).ok_or_else(unavailable)?),None=>None};
        let bytes=if let Some(tile)=tile {
            let tile_key=InkLayerCacheKey::TextTile{page:key,tile};
            if let Some(value)=cache.get(&tile_key) {value.to_vec()} else {
                let svg=cache.get(&svg_key).ok_or_else(unavailable)?;
                let pixels=rs_render::raster::rasterize_ink_tile(&svg,tile).map_err(|_|unavailable())?;
                let png=rs_render::pixels::rgba_png(512,512,&pixels).map_err(|_|unavailable())?;
                if !cache.insert(tile_key,Arc::from(png.clone())) {return Err(unavailable());} png
            }
        } else {cache.get(&base_key).ok_or_else(unavailable)?.to_vec()};
        guard()?;Ok(Some((bytes,token)))
    }

    fn text_composite(&self, identity: &str, index: i64, revision: i64, fulltext: bool) -> Result<Option<Vec<u8>>, Fail> {
        let unavailable = || Fail::store("preview_unavailable", "Bu sayfanın metinsiz görüntüsü kullanılamıyor.", 404);
        let changed = || Fail::store("preview_source_changed", "Not değişti.", 409);
        let source = self.source(identity)?;
        let actual = source.0.get(4).and_then(Val::as_i64).ok_or_else(unavailable)?;
        let revision = if revision == -1 { actual } else { revision };
        if actual != revision { return Err(changed()); }
        let epoch = self.refresh_snapshot_epoch()?;
        let snapshot = self.snapshot(identity)?;
        let model = if fulltext {
            let current = self.store.cached_editor(identity, Some(&Val::Int(revision))).map_err(store_fail)?.ok_or_else(changed)?;
            Some(current.get("model").cloned().filter(|m| !m.is_null()).ok_or_else(changed)?)
        } else { None };
        let digest = snapshot.get("raw_sha256").and_then(Val::as_str).ok_or_else(unavailable)?;
        let token = util::sha256(format!("{}:{identity}:{revision}:{index}:{digest}:text-composite-v1", account_of(&self.store)?).as_bytes());
        let key_page = InkLayerPageKey { note_id: identity.into(), revision, page_index: index, snapshot_digest: token };
        let key = if fulltext { InkLayerCacheKey::FullText(key_page) } else { InkLayerCacheKey::NoText(key_page) };
        let mut cache = match self.ink_layers.try_lock() {
            Ok(cache) => cache,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        };
        if let Some(hit) = cache.get(&key) { return Ok(Some(hit.to_vec())); }
        let page = (if let Some(model) = &model { crate::ink::prepare::fulltext_snapshot_page(&snapshot, index, model) } else { crate::ink::prepare::notext_snapshot_page(&snapshot, index) }).map_err(|_| unavailable())?.ok_or_else(unavailable)?;
        // A partial composite must never silently hide saved page objects
        // while the editor substitutes it for Samsung's complete canvas.
        let body = rs_store::val::loads(std::str::from_utf8(&page.body).map_err(|_| unavailable())?).map_err(|_| unavailable())?;
        if matches!(body.get("notices"), Some(Val::List(notices)) if notices.iter().any(|notice| notice.as_str() == Some(crate::ink::objects::NOTICE_HIDDEN))) {
            return Err(unavailable());
        }
        let mut found = BTreeMap::new();
        let guard = || if self.source(identity)? == source && self.snapshot_epoch()? == epoch { Ok(()) } else { Err(changed()) };
        if page.over_pdf {
            let payload = payload_from_snapshot(&snapshot)?;
            let pdf = pdf::inspect_pdf_backgrounds(&payload)?.into_iter().find(|p| p.page_index == index).ok_or_else(unavailable)?;
            let media = rs_store::media_store::MediaStore::open(&self.store.directory).map_err(store_fail)?;
            let directory = self.directory.join("pdf-files");
            let account = account_of(&self.store)?;
            let file = match pdf::cached_pdf_shared(&pdf, &directory, &account, &media)? {
                Some(file) => file,
                None if self.connected() => pdf::fetch_pdf_shared(&pdf, self.env.as_ref(), &directory, &account, Some(&guard), &media)?,
                None => return Err(unavailable()),
            };
            found.insert(crate::ink::prepare::PDF_UNDERLAY.to_string(), pdf::render_pdf_background(&file, &pdf)?);
        }
        let mut urls = None;
        for sha in crate::ink::composite_media(&page.body) {
            if found.contains_key(&sha) { continue; }
            if let Some(bytes) = self.media_cached(&sha)? { found.insert(sha, bytes); continue; }
            if !self.connected() { continue; }
            let urls = urls.get_or_insert_with(|| self.media_urls(&snapshot));
            if let Some(url) = urls.get(&sha) {
                if let Ok(bytes) = self.fetch_media(identity, &sha, url, &guard) {
                    if util::sha256_hex(&bytes) == sha {
                        let _ = self.store_media(&sha, &bytes, &guard);
                        found.insert(sha, bytes);
                    }
                }
            }
        }
        let (png, complete) = self.ink.render_prepared(&page, &|| page.body.clone(), &|sha| found.get(sha).cloned()).ok_or_else(unavailable)??;
        if !complete { return Err(unavailable()); }
        if self.source(identity)? != source || self.snapshot_epoch()? != epoch { return Err(changed()); }
        if !cache.insert(key, Arc::from(png.clone())) { return Err(unavailable()); }
        Ok(Some(png))
    }

    fn ink_layer(&self, identity: &str, page_index: i64, revision: i64, requested: Option<(u8,u32,u32)>, expected: Option<&str>) -> Result<Option<(Vec<u8>,String)>, Fail> {
        use rs_render::raster::{InkTile, InkTileScale};
        let unavailable = || Fail::store("preview_unavailable", "Bu sayfanın çizim katmanı kullanılamıyor.", 404);
        let changed = || Fail::store("preview_source_changed", "Not değişti.", 409);
        let entry = self.entry(identity)?;
        let epoch = self.snapshot_epoch()?;
        let actual_revision = entry.source.0.get(4).and_then(Val::as_i64).ok_or_else(unavailable)?;
        let revision = if revision == -1 { actual_revision } else { revision };
        if actual_revision != revision { return Err(changed()); }
        let candidate = entry.candidates.get(&page_index).ok_or_else(unavailable)?;
        let (local, pdf) = match candidate {
            Candidate::Ink(c) => (c, None),
            Candidate::Pdf(c) if c.overlay.is_none() => (c.local_ink.as_ref().ok_or_else(unavailable)?, Some(c)),
            _ => return Err(unavailable()),
        };
        let tile = match requested {
            Some((half, x, y)) => {
                let scale = InkTileScale::from_half_units(half).ok_or_else(unavailable)?;
                Some(InkTile::new(local.width.try_into().map_err(|_| unavailable())?, local.height.try_into().map_err(|_| unavailable())?, scale, x, y).ok_or_else(unavailable)?)
            }
            None => None,
        };
        let digest = local.sha256.as_str();
        if self.inventory_worker.is_some() && entry.source.0.get(6).and_then(Val::as_str) != Some(digest) { return Err(changed()); }
        let legacy_snapshot = if self.inventory_worker.is_none() {
            let snapshot = self.snapshot(identity)?;
            if snapshot.get("raw_sha256").and_then(Val::as_str) != Some(digest) { return Err(changed()); }
            Some(snapshot)
        } else { None };
        let token = util::sha256_hex(format!("{}:{identity}:{revision}:{page_index}:{digest}", account_of(&self.store)?).as_bytes());
        if expected.is_some_and(|value| value != token) { return Err(changed()); }
        let page = InkLayerPageKey { note_id: identity.to_string(), revision, page_index, snapshot_digest: util::sha256(token.as_bytes()) };
        let mut cache = match self.ink_layers.try_lock() {
            Ok(cache) => cache,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        };
        let base_key = InkLayerCacheKey::Base(page.clone());
        let svg_key = InkLayerCacheKey::Svg(page.clone());
        if cache.get(&base_key).is_none() || cache.get(&svg_key).is_none() {
            let mut found = BTreeMap::new();
            if let Some(pdf) = pdf {
                let media = rs_store::media_store::MediaStore::open(&self.store.directory).map_err(store_fail)?;
                let file = pdf::cached_pdf_shared(pdf, &self.directory.join("pdf-files"), &account_of(&self.store)?, &media)?.ok_or_else(unavailable)?;
                found.insert(crate::ink::prepare::PDF_UNDERLAY.to_string(), pdf::render_pdf_background(&file, pdf)?);
            }
            let mut prepared = None;
            if self.inventory_worker.is_some() && self.ink.supports_prepared() {
                let material = crate::container_input::read_material(&self.store, identity, page_index, digest).map_err(|error| {
                    if error.code == "preview_source_changed" { error } else { Fail::store("preview_storage_corrupt", "Sayfa verisi doğrulanamadı.", 422) }
                })?;
                if let Some(material) = material {
                    if let Ok(mut pages) = crate::ink::prepare::prepare_container_page(&material, true) {
                        if pages.len() == 1 {
                            let page = pages.remove(0);
                            if (page.index,page.width,page.height,page.composite,page.notice.as_ref(),page.over_pdf)
                                == (local.page_index,local.width,local.height,local.composite,local.notice.as_ref(),pdf.is_some()) {
                                prepared = Some(page);
                            }
                        }
                    }
                }
            }
            let (base, svg, complete) = if let Some(page) = prepared {
                for sha in crate::ink::composite_media(&page.body) {
                    if let Some(bytes) = self.media_cached(&sha)? { found.insert(sha, bytes); }
                }
                self.ink.split_prepared(&page, &|sha| found.get(sha).cloned()).ok_or_else(unavailable)?.map_err(|_| unavailable())?
            } else {
                let snapshot = match legacy_snapshot { Some(snapshot) => snapshot, None => self.snapshot(identity)? };
                if snapshot.get("raw_sha256").and_then(Val::as_str) != Some(digest) { return Err(changed()); }
                for sha in self.ink.page_media(&snapshot, page_index) {
                    if let Some(bytes) = self.media_cached(&sha)? { found.insert(sha, bytes); }
                }
                self.ink.split_page(&snapshot, page_index, &|sha| found.get(sha).cloned()).ok_or_else(unavailable)?.map_err(|_| unavailable())?
            };
            if !complete { return Err(unavailable()); }
            if self.source(identity)? != entry.source || self.snapshot_epoch()? != epoch { return Err(changed()); }
            if !cache.insert(base_key.clone(), Arc::from(base)) || !cache.insert(svg_key.clone(), Arc::from(svg)) { return Err(unavailable()); }
        }
        let result = if let Some(tile) = tile {
            let key = InkLayerCacheKey::Tile { page, tile };
            if let Some(bytes) = cache.get(&key) { bytes.to_vec() } else {
                let svg = cache.get(&svg_key).ok_or_else(unavailable)?;
                let pixels = rs_render::raster::rasterize_ink_tile(&svg, tile).map_err(|_| unavailable())?;
                let png = rs_render::pixels::rgba_png(512, 512, &pixels).map_err(|_| unavailable())?;
                if !cache.insert(key, Arc::from(png.clone())) { return Err(unavailable()); }
                png
            }
        } else { cache.get(&base_key).ok_or_else(unavailable)?.to_vec() };
        if self.source(identity)? != entry.source || self.snapshot_epoch()? != epoch { return Err(changed()); }
        Ok(Some((result,token)))
    }

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
            let sql = if self.schema7 { "SELECT id,remote_commit,loaded,deleted_remote,revision,modified_ms,raw_sha256 FROM notes WHERE id=?" }
                else { "SELECT id,remote_commit,loaded,deleted_remote,revision,modified_ms FROM notes WHERE id=?" };
            db.query(sql, &rs_store::p![identity])
        }).map_err(store_fail)?;
        let Some(row) = row.one() else { return Err(Fail::store("not_found", "Not bulunamadı.", 404)) };
        let mut values: Vec<Val> = (0..6).map(|i| row.at(i)).collect();
        if self.schema7 { values.push(row.at(6)); }
        values.push(self.env.session_path().map_or(Val::Null, |p| Val::s(p.to_string_lossy())));
        values.push(Val::Bool(self.env.offline()));
        Ok(Source(values))
    }

    fn connected(&self) -> bool {
        self.env.session_path().is_some() && !self.env.offline()
    }

    fn retry_inventory(&self, identity: &str) -> Result<Val, Fail> {
        let Some(worker) = &self.inventory_worker else { return self.manifest(identity); };
        let rows = self.store.with_db(|db| {
            use rs_store::db::Sql;
            db.query("SELECT loaded,deleted_remote,trashed,container_sha256 FROM notes WHERE id=?", &rs_store::p![identity])
        }).map_err(store_fail)?;
        let row = rows.one().ok_or_else(|| Fail::store("not_found", "Not bulunamadı.", 404))?;
        if !row.truthy("loaded") || row.truthy("deleted_remote") || row.truthy("trashed") || row.is_null("container_sha256") {
            return self.manifest(identity);
        }
        match self.store.preview_inventory(identity, crate::PREVIEW_INVENTORY_RECIPE).map_err(store_fail)? {
            Some(rs_store::preview_inventory::InventoryOutcome::Ready(_) | rs_store::preview_inventory::InventoryOutcome::Unsupported) => return self.manifest(identity),
            Some(rs_store::preview_inventory::InventoryOutcome::Failed) => {
                self.store.retry_preview_inventory(identity, crate::PREVIEW_INVENTORY_RECIPE).map_err(store_fail)?;
            }
            None => {},
        }
        if !worker.wake_retry() { return Err(Fail::store("preview_inventory_failed", "Sayfa bilgileri hazırlanamadı.", 503)); }
        Ok(Val::dict(vec![("pending", Val::Bool(true)), ("pages", Val::List(vec![])), ("providerAvailable", Val::Bool(false))]))
    }

    fn inventory_pending(&self, identity: &str) -> Result<bool, Fail> {
        let Some(worker) = &self.inventory_worker else { return Ok(false); };
        let rows = self.store.with_db(|db| {
            use rs_store::db::Sql;
            db.query("SELECT loaded,deleted_remote,trashed,container_sha256 FROM notes WHERE id=?", &rs_store::p![identity])
        }).map_err(store_fail)?;
        let row = rows.one().ok_or_else(|| Fail::store("not_found", "Not bulunamadı.", 404))?;
        if !row.truthy("loaded") || row.truthy("deleted_remote") || row.truthy("trashed") || row.is_null("container_sha256") { return Ok(false); }
        match self.store.preview_inventory(identity, crate::PREVIEW_INVENTORY_RECIPE).map_err(store_fail)? {
            None => {
                if worker.last_error().is_some() || !worker.wake() { return Err(Fail::store("preview_inventory_failed", "Sayfa bilgileri hazırlanamadı.", 503)); }
                Ok(true)
            }
            Some(rs_store::preview_inventory::InventoryOutcome::Failed) => Err(Fail::store("preview_inventory_failed", "Sayfa bilgileri hazırlanamadı.", 503)),
            _ => Ok(false),
        }
    }

    fn entry(&self, identity: &str) -> Result<Arc<Entry>, Fail> {
        self.refresh_snapshot_epoch()?;
        if let Some(hook) = &self.entry_hook {
            return hook(identity).map(Arc::new);
        }
        if self.inventory_pending(identity)? { return Err(Fail::store("preview_pending", "Sayfa bilgileri hazırlanıyor.", 503)); }
        let source = self.source(identity)?;
        {
            let state = self.lock();
            if let Some((_, e)) = state.entries.iter().find(|(k, _)| k == identity) {
                if e.source == source {
                    return Ok(e.clone());
                }
            }
        }
        let mut candidates: BTreeMap<i64, Candidate> = BTreeMap::new();
        let mut pages: Vec<Val> = Vec::new();
        let mut used_static = false;
        if self.inventory_worker.is_some() {
            if let Some(rs_store::preview_inventory::InventoryOutcome::Ready(inventory)) = self.store.preview_inventory(identity, crate::PREVIEW_INVENTORY_RECIPE).map_err(store_fail)? {
                if let Some(projection) = crate::container_input::render_index(&self.store, identity)? {
                    let digest = source.0.get(6).and_then(Val::as_str).ok_or_else(|| Fail::store("preview_storage_corrupt", "Sayfa verisi doğrulanamadı.", 422))?;
                    let urls = projection.url_ids.iter().map(|id| (id.clone(), INDEXED_URL.to_string())).collect();
                    self.inventory_static(&inventory, digest, &urls, &mut candidates, &mut pages)?;
                    used_static = true;
                } else if let Some(global) = crate::container_input::read_global(&self.store, identity)? {
                    if let Some(urls) = Self::material_urls(&global) {
                        self.inventory_static(&inventory, &global.index.raw_sha256, &urls, &mut candidates, &mut pages)?;
                        used_static = true;
                    }
                }
            }
        }
        if !used_static {
            let row = self.store.raw(identity).map_err(store_fail)?;
            let base = row.get("base_document").cloned().unwrap_or(Val::Null);
            if base.truthy() && row.get("loaded").is_some_and(Val::truthy) && !row.get("deleted_remote").is_some_and(Val::truthy) {
                if let Err(e) = self.inventory(&base, &mut candidates, &mut pages) {
                    let tolerated = e.is_document() || e.kind == Kind::Ink || e.is_value() || e.is_key() || e.is_type();
                    if !tolerated { return Err(e); }
                    pages.clear();
                }
            }
        }
        if self.source(identity)? != source { return Err(Fail::store("preview_source_changed", "Not değişti.", 409)); }
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

    /// Combine source-bound facts with live account, provider and cache state.
    fn inventory_static(&self, inventory: &rs_store::preview_inventory::Inventory, digest: &str,
                        urls: &BTreeMap<String, String>, candidates: &mut BTreeMap<i64, Candidate>, pages: &mut Vec<Val>) -> Result<(), Fail> {
        let mut canvases: Vec<_> = inventory.canvases.iter().map(|c| CanvasPreviewCandidate {
            page_index: c.page_index, key: c.key, width: c.width, height: c.height, is_dark_mode: c.dark,
            bind_number: c.bind_number.clone(), sha256: c.sha256.clone(), download_url: urls.get(&c.sha256).cloned(),
        }).collect();
        canvases.sort_by_key(|c| (c.is_dark_mode, -(c.width * c.height)));
        for candidate in canvases {
            let index = candidate.page_index as i64;
            if candidate.download_url.as_deref().is_some_and(|u| !u.is_empty()) && candidate.width * candidate.height <= MAX_PIXELS && !candidates.contains_key(&index) {
                candidates.insert(index, Candidate::Canvas(candidate));
            }
        }
        let mut pdf_pages: Vec<(i64, Vec<PdfBackground>)> = Vec::new();
        for p in &inventory.pdfs {
            let candidate = PdfBackground { page_index: p.page_index as i64, pdf_page_index: p.pdf_page_index,
                width: p.width, height: p.height, rect: p.rect, annotation_objects: p.annotation_objects,
                sha256: p.sha256.clone(), download_url: urls.get(&p.sha256).cloned(), overlay: None, local_ink: None };
            match pdf_pages.iter_mut().find(|(i, _)| *i == candidate.page_index) {
                Some((_, list)) => list.push(candidate), None => pdf_pages.push((candidate.page_index, vec![candidate])),
            }
        }
        let can_decode_overlay = !pdf_pages.is_empty() && codec::provider_available(&self.root);
        for (index, placements) in &pdf_pages {
            if placements.len() == 1 && placements[0].download_url.as_deref().is_some_and(|u| !u.is_empty()) {
                let mut background = placements[0].clone();
                if let Some(Candidate::Canvas(overlay)) = candidates.get(index) {
                    if background.annotation_objects != 0 && !overlay.is_dark_mode && (overlay.width, overlay.height) == (background.width, background.height) {
                        let overlay = overlay.clone();
                        if (self.connected() && can_decode_overlay) || self.path(&Candidate::Canvas(overlay.clone()))?.is_file() { background.overlay = Some(overlay); }
                    }
                }
                candidates.insert(*index, Candidate::Pdf(background));
            }
        }
        let in_pdf = |index: i64| pdf_pages.iter().any(|(i, _)| *i == index);
        let unsupported = |index: i64| index >= 0 && inventory.unsupported_pdf_pages.contains(&(index as usize));
        let pdf_without_layer = candidates.values().any(|c| matches!(c, Candidate::Pdf(p) if p.annotation_objects != 0));
        let renderer = self.ink.provider_available();
        let mut over_pdf = Vec::new();
        if inventory.ink_supported && (candidates.len() < inventory.pages.len() || pdf_without_layer) {
            for rendered in &inventory.ink_pages {
                if !renderer && !rendered.composite { continue; }
                let index = rendered.page_index as i64;
                if rendered.over_pdf { over_pdf.push(rendered); continue; }
                if !candidates.contains_key(&index) && !in_pdf(index) && !unsupported(index) {
                    candidates.insert(index, Candidate::Ink(InkGeometryPreview { page_index: index,
                        width: rendered.width, height: rendered.height, sha256: digest.to_owned(),
                        is_dark_mode: rendered.dark, composite: rendered.composite, notice: rendered.notice.clone() }));
                }
            }
        }
        for page in over_pdf {
            if let Some(Candidate::Pdf(c)) = candidates.get_mut(&(page.page_index as i64)) {
                if c.annotation_objects != 0 && (c.width, c.height) == (page.width, page.height) {
                    c.local_ink = Some(InkGeometryPreview { page_index: page.page_index as i64, width: page.width,
                        height: page.height, sha256: digest.to_owned(), is_dark_mode: false, composite: true, notice: page.notice.clone() });
                }
            }
        }
        for index in 0..inventory.pages.len() as i64 {
            let item = candidates.get(&index); let size = &inventory.pages[index as usize];
            let mut page = vec![("index", Val::Int(index)),
                ("width", item.map_or_else(|| size.width.map_or(Val::Null, Val::Int), |c| Val::Int(c.width()))),
                ("height", item.map_or_else(|| size.height.map_or(Val::Null, Val::Int), |c| Val::Int(c.height()))),
                ("available", Val::Bool(item.is_some())),
                ("darkMode", Val::Bool(match item { Some(Candidate::Pdf(_)) => false, Some(Candidate::Ink(c)) => c.is_dark_mode, _ => false }))];
            match item {
                Some(Candidate::Pdf(c)) => {
                    let notes = c.overlay.is_some() || c.local_ink.is_some();
                    page.push(("previewKind", Val::s(if notes { "pdf-with-notes" } else { "pdf-background" })));
                    page.push(("complete", Val::Bool(c.complete())));
                    let notice = match &c.local_ink { Some(local) if !c.complete() && c.overlay.is_none() => local.notice.as_deref().map_or(Val::Null, Val::s), _ if c.complete() => Val::Null, _ => Val::s(PDF_PARTIAL) };
                    page.push(("previewNotice", notice));
                }
                Some(Candidate::Ink(c)) if c.composite => {
                    page.push(("previewKind", Val::s("page-reconstructed"))); page.push(("complete", Val::Bool(c.notice.is_none())));
                    page.push(("previewNotice", c.notice.as_deref().map_or(Val::Null, Val::s)));
                }
                Some(Candidate::Ink(_)) => { page.push(("previewKind", Val::s("ink-reconstructed"))); page.push(("complete", Val::Bool(true))); page.push(("previewNotice", Val::Null)); }
                _ if in_pdf(index) || unsupported(index) => { page.push(("complete", Val::Bool(false))); page.push(("previewNotice", Val::s(PDF_UNSUPPORTED))); }
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
        if self.inventory_pending(identity)? {
            return Ok(Val::dict(vec![("pending", Val::Bool(true)), ("pages", Val::List(vec![])), ("providerAvailable", Val::Bool(false))]));
        }
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
        if this.inventory_pending(identity)? { return Ok(None); }
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
        let stand_in = stand_in_path(candidate, &path);
        if let Some(stand_in) = &stand_in {
            if let Some(raw) = read_cached(stand_in, MAX_PNG as u64)? {
                if crate::png_valid(&raw, candidate.width(), candidate.height()) {
                    let mut state = this.lock();
                    state.discard_stale_job(identity, &key, &entry.source, candidate);
                    let retry = state.retry_standin(identity, &key, &entry.source, candidate, Instant::now());
                    if retry && this.connected() && state.jobs.iter().filter(|(_, j)| matches!(j.state, JobState::Pending | JobState::Running)).count() < MAX_QUEUED {
                        state.job_sources.insert(key.clone(), (entry.source.clone(), candidate.clone()));
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
        state.discard_stale_job(identity, &key, &entry.source, candidate);
        if let Some(job) = state.job(&key).cloned() {
            match job.state {
                JobState::Pending | JobState::Running => {
                    if priority < job.priority && (job.state == JobState::Pending || priority <= 0) {
                        // Re-queue at the higher priority; the stale entry is skipped.
                        if let Some(slot) = state.job_mut(&key) {
                            slot.priority = priority;
                            slot.state = JobState::Pending;
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
            Candidate::Pdf(c) => {
                let media = rs_store::media_store::MediaStore::open(&this.store.directory).map_err(store_fail)?;
                pdf::cached_pdf_shared(c, &this.directory.join("pdf-files"), &account_of(&this.store)?, &media)?
            },
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
                let (old_key, _) = state.jobs.remove(at);
                state.job_sources.remove(&old_key);
                state.retry_counts.remove(&old_key);
            }
        }
        state.job_sources.insert(key.clone(), (entry.source.clone(), candidate.clone()));
        state.set_job(&key, Job { state: JobState::Pending, error: None, priority, local_pdf: local_pdf.clone(), retry_after: None, served: None, offline: false });
        this.queue.put(this.queue_item(&key, identity, &entry.source, candidate, &path, priority, local_pdf));
        Inner::spawn_workers(this, &mut state);
        Ok(None)
    }

    fn spawn_workers(this: &Arc<Inner>, state: &mut State) {
        // One additional worker is reserved for foreground requests, even when
        // the configured background pool has only one worker.
        while state.workers < this.workers + 1 {
            let number = state.workers;
            state.workers += 1;
            this.running.fetch_add(1, AtomicOrdering::SeqCst);
            let inner = this.clone();
            let spawned = std::thread::Builder::new().name(format!("notes-page-preview-{number}")).spawn(move || {
                inner.worker(number == 0);
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
        crate::ink::measure(crate::ink::CACHE_WRITE, || {
            if !crate::png_valid(png, width, height) {
                return Err(preview("Sayfa görüntüsünün boyutu doğrulanamadı."));
            }
            util::write_private(path, png, 8, true, guard)
        })
    }

    /// `_loop()`
    fn worker(self: &Arc<Self>, foreground_only: bool) {
        while !self.stop.load(AtomicOrdering::SeqCst) {
            let Some(item) = self.queue.get(Duration::from_millis(500), foreground_only) else { continue };
            {
                let mut state = self.lock();
                let same_source = state.job_sources.get(&item.key).is_some_and(|(source, candidate)| *source == item.source && *candidate == item.candidate);
                match state.job_mut(&item.key) {
                    Some(job) if same_source && job.state == JobState::Pending && job.priority == item.priority => job.state = JobState::Running,
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
            crate::ink::take_stage_times();
            crate::ink::take_detail_times();
            let mut stages = Stages::default();
            let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.process(&item, &mut stages)))
                .unwrap_or_else(|_| Err(Fail::os("preview worker panicked")));
            if result.is_err() && !self.stop.load(AtomicOrdering::SeqCst)
                && (self.env.session_path() != item.session
                    || self.source(&item.identity).is_ok_and(|source| source != item.source))
            {
                result = Err(Fail::store("preview_source_changed", "Not veya hesap değişti.", 409));
            }
            if matches!(&item.candidate, Candidate::Pdf(_)) {
                if let Err(e) = &mut result {
                    if e.kind == Kind::InkRender && e.code.is_empty() { e.code = "pdf_ink_render".into(); }
                }
            }
            (stages.renderer, stages.png) = crate::ink::take_stage_times();
            stages.detail = crate::ink::take_detail_times();
            timing(&item, started, &result, &stages);
            let deterministic_pdf_error = matches!(&item.candidate, Candidate::Pdf(_))
                && result.as_ref().is_err_and(deterministic_pdf_failure)
                && !self.stop.load(AtomicOrdering::SeqCst)
                && self.env.session_path() == item.session
                && self.source(&item.identity).is_ok_and(|source| source == item.source);
            let retry_standin = if item.priority >= 5 && matches!(&item.candidate, Candidate::Pdf(_)) && result.is_err() {
                stand_in_path(&item.candidate, &item.path).filter(|path| {
                    read_cached(path, MAX_PNG as u64).ok().flatten()
                        .is_some_and(|raw| crate::png_valid(&raw, item.candidate.width(), item.candidate.height()))
                })
            } else { None };
            {
                let mut state = self.lock();
                // A foreground request can supersede a running background job.
                // Its completion must not replace the newer job record.
                if !state.job(&item.key).is_some_and(|job| job.state == JobState::Running && job.priority == item.priority)
                    || !state.job_sources.get(&item.key).is_some_and(|(source, candidate)| *source == item.source && *candidate == item.candidate) {
                    drop(state);
                    self.queue.task_done();
                    continue;
                }
                if result.as_ref().is_err_and(|e| e.code == "preview_source_changed") {
                    state.pop_job(&item.key);
                    state.entries.retain(|(id, _)| id != &item.identity);
                    self.finished.notify_all();
                    drop(state);
                    let page = match &item.candidate {
                        Candidate::Canvas(c) => c.page_index as i64,
                        Candidate::Pdf(c) => c.page_index,
                        Candidate::Ink(c) => c.page_index,
                    };
                    if !self.stop.load(AtomicOrdering::SeqCst) {
                        let _ = Inner::request_once(self, &item.identity, page, item.priority);
                    }
                    self.queue.task_done();
                    continue;
                }
                if deterministic_pdf_error { remember_pdf_failure(&mut state, &item); }
                let start_full = item.priority <= 0 && self.connected() && matches!(&item.candidate, Candidate::Canvas(_))
                    && matches!(&result, Ok(Some(_)));
                let job = match result {
                    Ok(None) => { state.retry_counts.remove(&item.key); Job { state: JobState::Ready, error: None, priority: 0, local_pdf: None, retry_after: None, served: None, offline: false } },
                    // Served by a stand-in: retry the full image later only.
                    Ok(Some(served)) => {
                        let delay = next_retry_delay(&mut state, &item.key);
                        Job { state: JobState::Ready, error: None, priority: 0, local_pdf: None,
                              retry_after: Some(Instant::now() + delay), served: Some(served), offline: false }
                    },
                    Err(_) if retry_standin.is_some() => {
                        let delay = next_retry_delay(&mut state, &item.key);
                        Job { state: JobState::Ready, error: None, priority: 0, local_pdf: None,
                              retry_after: Some(Instant::now() + delay), served: retry_standin, offline: false }
                    }
                    Err(e) => {
                        let known = e.kind == Kind::Preview || e.kind == Kind::PdfBackground || e.is_api() || e.is_store();
                        let offline = e.is_store() && e.code == "preview_offline";
                        Job { state: JobState::Error, error: Some(if known { e.message } else { GENERIC_FAILURE.to_string() }),
                              priority: 0, local_pdf: None, retry_after: Some(Instant::now() + Duration::from_secs(5)), served: None, offline }
                    }
                };
                state.set_job(&item.key, job);
                if start_full {
                    if let Candidate::Canvas(c) = &item.candidate {
                        if !state.decode_failures.contains(&decode_failure_key(&item.key, c))
                            && !state.download_failures.contains(&download_failure_key(&item.key, c))
                            && state.make_room(5)
                        {
                            if let Some(job) = state.job_mut(&item.key) { job.state = JobState::Pending; job.priority = 5; }
                            self.queue.put(self.queue_item(&item.key, &item.identity, &item.source, &item.candidate, &item.path, 5, None));
                        }
                    }
                }
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
        let media = rs_store::media_store::MediaStore::open(&self.store.directory).map_err(store_fail)?;
        if let Some(path) = pdf::cached_pdf_shared(candidate, &directory, &account, &media)? { guard()?; return Ok(path); }
        let mut resolved = candidate.clone();
        resolved.download_url = self.resolve_indexed_url(identity, &candidate.sha256, candidate.download_url.as_deref())?;
        guard()?;
        match pdf::fetch_pdf_shared(&resolved, self.env.as_ref(), &directory, &account, Some(guard), &media) {
            Err(e) if pdf::is_link_expired(&e) => {
                let fresh = self.env.refresh_note(identity)?;
                self.invalidate_snapshot(identity);
                let parsed = crate::payload::payload_from_snapshot(&fresh)?;
                let renewed = pdf::inspect_pdf_backgrounds(&parsed)?
                    .into_iter()
                    .find(|c| c.page_index == candidate.page_index && c.sha256 == candidate.sha256);
                let Some(renewed) = renewed.filter(|c| c.download_url.as_deref().is_some_and(|u| !u.is_empty())) else {
                    return Err(pdf_error("PDF sayfası notta değişti; sayfayı yeniden açabilirsin."));
                };
                guard()?;
                pdf::fetch_pdf_shared(&renewed, self.env.as_ref(), &directory, &account, Some(guard), &media)
            }
            other => other,
        }
    }

    fn process(&self, item: &QueueItem, stages: &mut Stages) -> Result<Option<PathBuf>, Fail> {
        let candidate = &item.candidate;
        // Local pages and canvas pages (drawn locally when offline) run offline.
        let local_ok = matches!(candidate, Candidate::Ink(_) | Candidate::Canvas(_));
        let guard = || -> Result<(), Fail> {
            if self.stop.load(AtomicOrdering::SeqCst) || self.env.session_path() != item.session
                || (self.env.offline() && item.local_pdf.is_none() && !local_ok)
                || self.source(&item.identity)? != item.source
                || self.path(candidate)? != item.path
            {
                return Err(Fail::store("preview_source_changed", "Not veya hesap değişti.", 409));
            }
            Ok(())
        };
        guard()?;
        let (width, height) = (candidate.width(), candidate.height());
        let png = match candidate {
            Candidate::Ink(c) => {
                let (png, complete) = match self.render_container_over(&item.identity, c, &guard, None, true)? {
                    Some(value) => value,
                    None => {
                        let snapshot = self.snapshot(&item.identity)?;
                        if !snapshot.get("raw_sha256").is_some_and(|v| py_eq(v, &Val::s(c.sha256.clone()))) {
                            return Err(Fail::store("preview_source_changed", "Not değişti.", 409));
                        }
                        self.render_local(&item.identity, c, &snapshot, &guard, true, item.priority)?
                    }
                };
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
                    Some(overlay) => self.pdf_native_layer(&item.identity, c, overlay, &png, &guard).map(Some),
                    None => Ok(None),
                };
                match native {
                    Ok(Some(layered)) => png = layered,
                    Err(e) => {
                        if e.code == "preview_source_changed" { return Err(e); }
                        guard()?;
                        let deterministic_overlay = deterministic_pdf_overlay_failure(&e);
                        if deterministic_overlay { remember_pdf_failure(&mut self.lock(), item); }
                        // Samsung's layer is unavailable: the local layer stands in.
                        let Some(local) = &c.local_ink else { return Err(e) };
                        let rendered = match self.render_container_over(&item.identity, local, &guard, Some(png.clone()), true) {
                            Ok(Some(value)) => Ok(value),
                            Ok(None) => {
                                let snapshot = self.snapshot(&item.identity)?;
                                if !snapshot.get("raw_sha256").is_some_and(|v| py_eq(v, &Val::s(local.sha256.clone()))) {
                                    return Err(Fail::store("preview_source_changed", "Not değişti.", 409));
                                }
                                self.render_local_over(&item.identity, local, &snapshot, &guard, Some(png), true, item.priority)
                            }
                            Err(error) => Err(error),
                        };
                        let (layered, _) = match rendered {
                            Ok(rendered) => rendered,
                            Err(error) => { guard()?; return Err(overlay_fallback_error(error, deterministic_overlay)); }
                        };
                        let stand_in = local_path(&item.path);
                        guard()?;
                        Inner::cache_png(&stand_in, &layered, width, height, &guard)?;
                        return Ok(Some(stand_in));
                    }
                    Ok(None) => {}
                }
                if let (None, Some(local)) = (&c.overlay, &c.local_ink) {
                    // The page's own objects drawn locally over the PDF page.
                    let (layered, complete) = match self.render_container_over(&item.identity, local, &guard, Some(png.clone()), true)? {
                        Some(value) => value,
                        None => {
                            let snapshot = self.snapshot(&item.identity)?;
                            if !snapshot.get("raw_sha256").is_some_and(|v| py_eq(v, &Val::s(local.sha256.clone()))) {
                                return Err(Fail::store("preview_source_changed", "Not değişti.", 409));
                            }
                            self.render_local_over(&item.identity, local, &snapshot, &guard, Some(png), true, item.priority)?
                        }
                    };
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
                // The page on screen must not wait for an unavailable network.
                // Media missing locally remains a placeholder until background work.
                if item.priority <= 0 {
                    let start = Instant::now();
                    let local = self.canvas_fallback(item, c);
                    stages.local = start.elapsed();
                    match local {
                        Ok(Some(path)) => return Ok(Some(path)),
                        Err(e) if matches!(e.code.as_str(), "preview_source_changed" | "preview_storage_corrupt") => return Err(e),
                        _ => guard()?,
                    }
                }
                let native = (|| -> Result<Vec<u8>, Fail> {
                    if !self.connected() {
                        return Err(Fail::store("preview_offline", "Bu sayfa henüz indirilmemiş. Bağlantı kurduğunda tekrar açabilirsin.", 503));
                    }
                    if self.lock().decode_failures.contains(&decode_failure_key(&item.key, c))
                        || self.lock().download_failures.contains(&download_failure_key(&item.key, c)) {
                        return Err(preview("Sayfa önbelleği çözümlenemedi."));
                    }
                    let start = Instant::now();
                    let mut resolved = c.clone();
                    resolved.download_url = self.resolve_indexed_url(&item.identity, &c.sha256, c.download_url.as_deref())?;
                    guard()?;
                    let fetched = (self.fetcher)(&resolved, self.env.as_ref(), &guard);
                    stages.download = start.elapsed();
                    let raw = match fetched {
                        Ok(raw) => raw,
                        Err(e) if e.code == "preview_cache_permanent" => {
                            guard()?;
                            self.lock().download_failures.insert(download_failure_key(&item.key, c));
                            return Err(e);
                        }
                        Err(e) => return Err(e),
                    };
                    guard()?;
                    let start = Instant::now();
                    let decoded = (self.decoder)(&raw, (width, height));
                    stages.decode = start.elapsed();
                    let png = match decoded {
                        Ok(png) => png,
                        Err(e) => {
                            guard()?;
                            self.lock().decode_failures.insert(decode_failure_key(&item.key, c));
                            return Err(e);
                        }
                    };
                    if c.is_dark_mode { pdf::light_from_dark(&png, width, height) } else { Ok(png) }
                })();
                match native {
                    Ok(png) => png,
                    Err(e) => {
                        // Samsung's page picture is unavailable (offline, link,
                        // size, or a cache variant the decoder refuses): draw
                        // the page locally when the note allows it.
                        let stopped = self.stop.load(AtomicOrdering::SeqCst);
                        if stopped { return Err(e); }
                        guard()?;
                        let existing = local_path(&item.path);
                        if !self.lock().partial_canvas.contains(&item.key) {
                            if let Some(raw) = read_cached(&existing, MAX_PNG as u64)? {
                                if crate::png_valid(&raw, c.width, c.height) { guard()?; return Ok(Some(existing)); }
                            }
                        }
                        let start = Instant::now();
                        let fallback = self.canvas_fallback(item, c);
                        stages.local = start.elapsed();
                        match fallback {
                            Ok(Some(path)) => return Ok(Some(path)),
                            Err(changed) if matches!(changed.code.as_str(), "preview_source_changed" | "preview_storage_corrupt") => return Err(changed),
                            _ => { guard()?; return Err(e); },
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
    fn pdf_native_layer(&self, identity: &str, c: &PdfBackground, overlay: &CanvasPreviewCandidate, png: &[u8], guard: &dyn Fn() -> Result<(), Fail>) -> Result<Vec<u8>, Fail> {
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
                if overlay.download_url.as_deref().is_none_or(str::is_empty) {
                    let mut error = preview("Bu sayfanın çizim katmanı bulunamadı.");
                    error.code = "pdf_overlay_missing".into();
                    return Err(error);
                }
                let mut resolved = overlay.clone();
                resolved.download_url = self.resolve_indexed_url(identity, &overlay.sha256, overlay.download_url.as_deref())?;
                guard()?;
                let raw = (self.fetcher)(&resolved, self.env.as_ref(), guard)?;
                guard()?;
                let layer = match (self.decoder)(&raw, (c.width, c.height)) {
                    Ok(layer) => layer,
                    Err(mut e) => {
                        guard()?;
                        if !e.is_os() && !e.is_api() && !e.is_store() {
                            e.code = "pdf_overlay_decode".into();
                        }
                        return Err(e);
                    }
                };
                if !crate::png_valid(&layer, c.width, c.height) {
                    let mut error = preview("Sayfa görüntüsünün boyutu doğrulanamadı.");
                    error.code = "pdf_overlay_decode".into();
                    return Err(error);
                }
                // Keep the verified native layer as well as the composite: the
                // offline inventory then selects the same complete recipe after
                // a restart.
                Inner::cache_png(&layer_path, &layer, c.width, c.height, guard)?;
                layer
            }
        };
        guard()?;
        pdf::compose_pdf_canvas(png, &layer, c.width, c.height).map_err(|mut e| {
            e.code = "pdf_overlay_compose".into();
            e
        })
    }

    /// This connection only observes commits; StoreCore opens separate
    /// connections for its operations. A commit may replace a snapshot even
    /// when the note's visible source fields and commit id stay unchanged.
    fn snapshot_epoch(&self) -> Result<i64, Fail> {
        use rs_store::db::Sql;
        let rows = self.db_monitor.query("PRAGMA data_version", &[]).map_err(store_fail)?;
        rows.one().and_then(|row| row.at(0).as_i64())
            .ok_or_else(|| Fail::os("snapshot version unavailable"))
    }

    fn refresh_snapshot_epoch(&self) -> Result<i64, Fail> {
        let epoch = self.snapshot_epoch()?;
        let mut state = self.lock();
        if state.snapshot_epoch != Some(epoch) {
            // A sync can change stored bytes while keeping the visible note
            // revision and commit unchanged. Discard both derived caches.
            state.snapshots.clear();
            state.entries.clear();
            state.snapshot_epoch = Some(epoch);
        }
        Ok(epoch)
    }

    fn snapshot(&self, identity: &str) -> Result<Arc<Val>, Fail> {
        crate::ink::measure(crate::ink::SNAPSHOT, || {
            let source = self.source(identity)?;
            let epoch = self.refresh_snapshot_epoch()?;
            {
                let mut state = self.lock();
                if let Some(at) = state.snapshots.iter().position(|(id, old, _, _, _)| id == identity && *old == source) {
                    let entry = state.snapshots.remove(at);
                    let hit = entry.2.clone();
                    state.snapshots.push(entry);
                    return Ok(hit);
                }
            }
            let row = self.store.raw(identity).map_err(store_fail)?;
            let text = row.get("base_document").and_then(Val::as_str);
            let text = text.ok_or_else(|| Fail::type_("the JSON object must be str, bytes or bytearray, not NoneType"))?;
            let value = Arc::new(notes_host::snapshot::loads(text)?);
            if self.source(identity)? != source || self.snapshot_epoch()? != epoch {
                return Err(Fail::store("preview_source_changed", "Not değişti.", 409));
            }
            // Include overhead for the parsed strings and structure; keep at
            // most four snapshots and do not pin a single oversized document.
            let size = text.len().saturating_mul(2);
            const SNAPSHOT_BUDGET: usize = 128 * 1024 * 1024;
            if size <= SNAPSHOT_BUDGET {
                let mut state = self.lock();
                if state.snapshot_epoch != Some(epoch) {
                    return Err(Fail::store("preview_source_changed", "Not değişti.", 409));
                }
                state.snapshots.retain(|(id, _, _, _, _)| id != identity);
                state.snapshots.push((identity.to_string(), source, value.clone(), size, None));
                while state.snapshots.len() > 4 || state.snapshots.iter().map(|e| e.3).sum::<usize>() > SNAPSHOT_BUDGET {
                    state.snapshots.remove(0);
                }
            }
            Ok(value)
        })
    }

    fn invalidate_snapshot(&self, identity: &str) {
        self.lock().snapshots.retain(|(id, _, _, _, _)| id != identity);
    }

    /// Download links depend on the same immutable snapshot as the page
    /// bodies. Parse them once, only when a page actually needs uncached media.
    fn media_urls(&self, snapshot: &Arc<Val>) -> Arc<BTreeMap<String, String>> {
        {
            let state = self.lock();
            if let Some(hit) = state.snapshots.iter().find(|e| Arc::ptr_eq(&e.2, snapshot)).and_then(|e| e.4.clone()) {
                return hit;
            }
        }
        let urls: BTreeMap<String, String> = crate::ink::measure(crate::ink::PAYLOAD, ||
            crate::payload::payload_from_snapshot(snapshot).map(|p| {
                p.result.download_urls.iter().map(|d| (d.id.to_lowercase(), d.url.clone())).collect()
            }).unwrap_or_default());
        let size: usize = urls.iter().map(|(key, value)| key.len().saturating_add(value.len()).saturating_add(128)).sum();
        let urls = Arc::new(urls);
        if size <= 8 * 1024 * 1024 {
            let mut state = self.lock();
            if let Some(entry) = state.snapshots.iter_mut().find(|e| Arc::ptr_eq(&e.2, snapshot)) {
                entry.4 = Some(urls.clone());
            }
        }
        urls
    }

    /// Signed URLs come from the authenticated global entries. If both contain
    /// URL records, their original ordering is not represented by this partial
    /// view, so use the full snapshot path instead of guessing.
    fn material_urls(global: &rs_store::note_container::GlobalPageContext) -> Option<BTreeMap<String, String>> {
        let note = crate::payload::parse_payload(global.note_xml.clone(), "application/xml").ok()?;
        let separate = if global.download_urls.is_empty() { Vec::new() } else {
            let mut xml = b"<CompareNoteView>".to_vec();
            xml.extend_from_slice(&global.download_urls);
            xml.extend_from_slice(b"</CompareNoteView>");
            crate::payload::parse_payload(xml, "application/xml").ok()?.result.download_urls
        };
        if !note.result.download_urls.is_empty() && !separate.is_empty() { return None; }
        Some(note.result.download_urls.into_iter().chain(separate).map(|d| (d.id.to_lowercase(), d.url)).collect())
    }

    fn resolve_indexed_url(&self, identity: &str, sha: &str, current: Option<&str>) -> Result<Option<String>, Fail> {
        if current != Some(INDEXED_URL) { return Ok(current.map(str::to_owned)); }
        let Some(global) = crate::container_input::read_global(&self.store, identity)? else { return Ok(None) };
        Ok(Self::material_urls(&global).and_then(|urls| urls.get(sha).cloned()))
    }

    /// Prepare and render a verified page without loading other page entries.
    /// None requests the established full-snapshot path; integrity errors are
    /// propagated. Existing guards are run before and after all side effects.
    fn render_container_over(&self, identity: &str, c: &InkGeometryPreview, guard: &dyn Fn() -> Result<(), Fail>,
                             underlay: Option<Vec<u8>>, download_media: bool) -> Result<Option<(Vec<u8>, bool)>, Fail> {
        if !self.schema7 || !self.ink.supports_prepared() { return Ok(None); }
        if !matches!(self.store.preview_inventory(identity, crate::PREVIEW_INVENTORY_RECIPE).map_err(store_fail)?,
                     Some(rs_store::preview_inventory::InventoryOutcome::Ready(_))) { return Ok(None); }
        guard()?;
        let material = match crate::container_input::read_material(&self.store, identity, c.page_index, &c.sha256) {
            Ok(material) => material,
            Err(error) if error.code == "preview_source_changed" || error.code == "page_not_found" => return Err(error),
            Err(_) => return Err(Fail::store("preview_storage_corrupt", "Sayfa verisi doğrulanamadı.", 422)),
        };
        let Some(material) = material else { return Ok(None); };
        let Ok(mut pages) = crate::ink::prepare::prepare_container_page(&material, true) else { return Ok(None); };
        if pages.len() != 1 { return Ok(None); }
        let page = pages.remove(0);
        if (page.index, page.width, page.height, page.dark_mode, page.composite, page.notice.as_ref(), page.over_pdf)
            != (c.page_index, c.width, c.height, c.is_dark_mode, c.composite, c.notice.as_ref(), underlay.is_some()) {
            return Ok(None);
        }
        let wanted = if c.composite { crate::ink::composite_media(&page.body) } else { Vec::new() };
        let mut found = BTreeMap::new();
        if let Some(png) = underlay { found.insert(crate::ink::prepare::PDF_UNDERLAY.to_string(), png); }
        let mut urls = None;
        for sha in wanted {
            if found.contains_key(&sha) || sha.len() != 64 { continue; }
            if let Some(bytes) = self.media_cached(&sha)? { found.insert(sha, bytes); continue; }
            if !download_media || !self.connected() { continue; }
            if urls.is_none() {
                let global;
                let source = if material.projected_resource.is_some() {
                    global = crate::container_input::read_global(&self.store, identity)?;
                    let Some(global) = global.as_ref() else { return Ok(None) };
                    global
                } else { &material.global };
                let Some(parsed) = Self::material_urls(source) else { return Ok(None); };
                urls = Some(parsed);
            }
            let Some(url) = urls.as_ref().and_then(|u| u.get(&sha)).cloned() else { continue };
            guard()?;
            if let Ok(bytes) = self.fetch_media(identity, &sha, &url, guard) {
                if util::sha256_hex(&bytes) == sha {
                    let _ = self.store_media(&sha, &bytes, guard);
                    guard()?;
                    found.insert(sha, bytes);
                }
            }
        }
        guard()?;
        let result = self.ink.render_prepared(&page, &|| crate::ink::prepare::forced_container_composite(&material),
                                              &|sha| found.get(sha).cloned());
        guard()?;
        match result { Some(result) => result.map(Some), None => Ok(None) }
    }

    /// A local page (renderer-only ink or composite) with its pictures:
    /// verified media cache first, then (connected) a download through the
    /// account transport. Returns the PNG and whether it is complete.
    fn render_local(&self, identity: &str, c: &InkGeometryPreview, snapshot: &Arc<Val>, guard: &dyn Fn() -> Result<(), Fail>, download_media: bool, priority: i64) -> Result<(Vec<u8>, bool), Fail> {
        self.render_local_over(identity, c, snapshot, guard, None, download_media, priority)
    }

    /// `render_local` with the PDF page image a PDF page's layer is drawn on.
    fn render_local_over(&self, identity: &str, c: &InkGeometryPreview, snapshot: &Arc<Val>, guard: &dyn Fn() -> Result<(), Fail>,
                         underlay: Option<Vec<u8>>, download_media: bool, priority: i64) -> Result<(Vec<u8>, bool), Fail> {
        let wanted = if c.composite { self.ink.page_media_priority(snapshot, c.page_index, priority) } else { Vec::new() };
        let media_started = Instant::now();
        let mut found: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        if let Some(png) = underlay {
            found.insert(crate::ink::prepare::PDF_UNDERLAY.to_string(), png);
        }
        let mut urls: Option<Arc<BTreeMap<String, String>>> = None;
        for sha in wanted {
            if found.contains_key(&sha) || sha.len() != 64 {
                continue;
            }
            if let Some(bytes) = self.media_cached(&sha)? {
                found.insert(sha, bytes);
                continue;
            }
            if !download_media || !self.connected() {
                continue;
            }
            if urls.is_none() {
                urls = Some(self.media_urls(snapshot));
            }
            let Some(url) = urls.as_ref().and_then(|u| u.get(&sha)).cloned() else { continue };
            guard()?;
            if let Ok(bytes) = self.fetch_media(identity, &sha, &url, guard) {
                if util::sha256_hex(&bytes) == sha {
                    let _ = self.store_media(&sha, &bytes, guard);
                    guard()?;
                    found.insert(sha, bytes);
                }
            }
        }
        crate::ink::record_elapsed(crate::ink::MEDIA, media_started.elapsed());
        guard()?;
        self.ink.render_page_priority(snapshot, c.page_index, &|sha| found.get(sha).cloned(), priority)
    }

    fn media_path(&self, sha256: &str) -> Result<PathBuf, Fail> {
        let account = account_of(&self.store)?;
        let key = util::sha256_hex(format!("{account}:{sha256}:media-v1").as_bytes());
        Ok(private_directory(&self.directory.join("media"))?.join(format!("{key}.bin")))
    }

    /// A downloaded picture, only if its bytes still hash to the content id.
    fn media_cached(&self, sha256: &str) -> Result<Option<Vec<u8>>, Fail> {
        let media = rs_store::media_store::MediaStore::open(&self.store.directory).map_err(store_fail)?;
        if let Some(bytes) = media.read(sha256, rs_render::compose::MAX_IMAGE_BYTES as u64).map_err(store_fail)? {
            return Ok(Some(bytes));
        }
        let path = self.media_path(sha256)?;
        match read_cached(&path, rs_render::compose::MAX_IMAGE_BYTES as u64)? {
            Some(raw) if util::sha256_hex(&raw) == sha256 => {
                media.put(sha256, &raw).map_err(store_fail)?;
                Ok(Some(raw))
            },
            _ => Ok(None),
        }
    }

    fn store_media(&self, sha256: &str, bytes: &[u8], guard: &dyn Fn() -> Result<(), Fail>) -> Result<(), Fail> {
        guard()?;
        let media = rs_store::media_store::MediaStore::open(&self.store.directory).map_err(store_fail)?;
        media.put(sha256, bytes).map_err(store_fail)?;
        guard()?;
        Ok(())
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
        if self.env.refresh_note(identity).is_ok() {
            // A refresh can replace signed links without changing the note
            // commit or visible revision used by Source.
            self.invalidate_snapshot(identity);
            true
        } else {
            false
        }
    }

    /// The local page of a canvas-cache page, scaled to the canvas size and
    /// cached next to it (`<key>.local.png`). None when the note's local
    /// renderer does not admit the page.
    fn canvas_fallback(&self, item: &QueueItem, c: &CanvasPreviewCandidate) -> Result<Option<PathBuf>, Fail> {
        let guard = || -> Result<(), Fail> {
            if self.stop.load(AtomicOrdering::SeqCst) || self.source(&item.identity)? != item.source {
                return Err(Fail::store("preview_source_changed", "Not veya hesap değişti.", 409));
            }
            Ok(())
        };
        if self.inventory_worker.is_some() {
            if let Some(rs_store::preview_inventory::InventoryOutcome::Ready(inventory)) = self.store.preview_inventory(&item.identity, crate::PREVIEW_INVENTORY_RECIPE).map_err(store_fail)? {
                let index = c.page_index as i64;
                if let (Some(digest), Some(page)) = (item.source.0.get(6).and_then(Val::as_str), inventory.ink_pages.iter().find(|p| inventory.ink_supported && p.page_index == c.page_index && !p.over_pdf)) {
                    let local = InkGeometryPreview { page_index: index, width: page.width, height: page.height,
                        sha256: digest.to_owned(), is_dark_mode: page.dark, composite: page.composite, notice: page.notice.clone() };
                    if let Some((png, complete)) = self.render_container_over(&item.identity, &local, &guard, None, item.priority > 0)? {
                        let png = if (page.width, page.height) == (c.width, c.height) { png } else { rescale_png(&png, (page.width, page.height), (c.width, c.height))? };
                        let path = local_path(&item.path);
                        guard()?;
                        Inner::cache_png(&path, &png, c.width, c.height, &guard)?;
                        let mut state = self.lock();
                        if complete { state.partial_canvas.remove(&item.key); } else { state.partial_canvas.insert(item.key.clone()); }
                        return Ok(Some(path));
                    }
                }
            }
        }
        let snapshot = self.snapshot(&item.identity)?;
        let Some(digest) = snapshot.get("raw_sha256").and_then(Val::as_str).map(str::to_string) else { return Ok(None) };
        let index = c.page_index as i64;
        let Some(page) = crate::ink::measure(crate::ink::INVENTORY_STAGE, || self.ink_inventory(&snapshot, &digest))?.into_iter().find(|p| p.index == index && !p.over_pdf) else { return Ok(None) };
        let local = InkGeometryPreview {
            page_index: index, width: page.width, height: page.height, sha256: digest,
            is_dark_mode: page.dark_mode.unwrap_or(false), composite: page.composite, notice: page.notice.clone(),
        };
        let (png, complete) = self.render_local(&item.identity, &local, &snapshot, &guard, item.priority > 0, item.priority)?;
        let png = if (page.width, page.height) == (c.width, c.height) { png } else { rescale_png(&png, (page.width, page.height), (c.width, c.height))? };
        let path = local_path(&item.path);
        guard()?;
        Inner::cache_png(&path, &png, c.width, c.height, &guard)?;
        let mut state = self.lock();
        if complete { state.partial_canvas.remove(&item.key); } else { state.partial_canvas.insert(item.key.clone()); }
        Ok(Some(path))
    }
}

fn next_retry_delay(state: &mut State, key: &str) -> Duration {
    let count = state.retry_counts.entry(key.to_string()).or_default();
    let delay = RETRY_FULL.saturating_mul(1u32 << (*count).min(6));
    *count = count.saturating_add(1);
    delay
}

fn stand_in_path(candidate: &Candidate, path: &Path) -> Option<PathBuf> {
    match candidate {
        Candidate::Canvas(_) => Some(local_path(path)),
        Candidate::Pdf(c) if c.overlay.is_some() && c.local_ink.is_some() => Some(local_path(path)),
        Candidate::Pdf(c) if c.local_ink.is_some() => Some(partial_path(path)),
        Candidate::Ink(_) => Some(partial_path(path)),
        _ => None,
    }
}

fn pdf_failure_key(key: &str, candidate: &Candidate) -> String {
    format!("{key}:{}:{}", candidate.width(), candidate.height())
}

fn deterministic_pdf_overlay_failure(error: &Fail) -> bool {
    matches!(error.code.as_str(), "pdf_overlay_missing" | "pdf_overlay_decode" | "pdf_overlay_compose")
}

fn overlay_fallback_error(mut error: Fail, deterministic_overlay: bool) -> Fail {
    if !deterministic_overlay && error.kind == Kind::InkRender {
        // A local renderer failure does not make the native overlay's network
        // error permanent: that overlay may become available on a later retry.
        error.code = "pdf_overlay_network".into();
    }
    error
}

fn deterministic_pdf_failure(error: &Fail) -> bool {
    deterministic_pdf_overlay_failure(error) || error.code == "pdf_render_refused"
        || (error.kind == Kind::InkRender && !matches!(error.code.as_str(), "ink_prepare_timeout" | "ink_prepare_unavailable" | "ink_renderer_timeout" | "ink_renderer_unavailable" | "pdf_overlay_network"))
}

fn pdf_candidate_fingerprint(candidate: &Candidate) -> [u8; 32] {
    util::sha256(format!("{candidate:?}").as_bytes())
}

fn remember_pdf_failure(state: &mut State, item: &QueueItem) {
    let key = pdf_failure_key(&item.key, &item.candidate);
    let fingerprint = pdf_candidate_fingerprint(&item.candidate);
    let failures = state.pdf_failures.entry(item.identity.clone()).or_default();
    failures.retain(|(old_key, source, old)| *source == item.source && !(old_key == &key && *old == fingerprint));
    failures.push((key, item.source.clone(), fingerprint));
}

fn download_failure_key(key: &str, c: &CanvasPreviewCandidate) -> String {
    format!("{key}:{}", util::sha256_hex(c.download_url.as_deref().unwrap_or("").as_bytes()))
}

fn decode_failure_key(key: &str, c: &CanvasPreviewCandidate) -> String {
    format!("{key}:{}:{}", c.width, c.height)
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
pub(crate) fn page_sizes(payload: &crate::payload::Parsed) -> Vec<Option<(i64, i64)>> {
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

    #[test]
    fn static_inventory_recreates_full_dynamic_candidates() {
        use crate::test_common::{store, TempDir, TestEnv};
        let dir = TempDir::new();
        let manager = PreviewManager::new(store(dir.path()), TestEnv::new(None, true)).unwrap();
        let ink = rs_store::val::loads(include_str!("../tests/fixtures/ink_cases.json")).unwrap();
        let objects = rs_store::val::loads(include_str!("../tests/fixtures/objects_note.json")).unwrap();
        let floating = rs_store::val::loads(include_str!("../tests/fixtures/floating_note.json")).unwrap();
        let pdf_xml = crate::test_common::pdf_xml(None, "", "false");
        let pdf_snapshot = crate::test_common::snapshot(pdf_xml.as_bytes(), "application/xml");
        for snapshot in [objects.get("snapshot").unwrap(), floating.get("snapshot").unwrap(), ink.get("pdf_list").unwrap().get("snapshot").unwrap(), ink.get("rich_flow").unwrap().get("snapshot").unwrap(), &pdf_snapshot] {
            let static_data = crate::inventory_worker::compute_static(snapshot).unwrap();
            let raw = crate::payload::payload_from_snapshot(snapshot).unwrap();
            let urls: BTreeMap<String, String> = raw.result.download_urls.iter().map(|d| (d.id.to_lowercase(), d.url.clone())).collect();
            let mut old_candidates = BTreeMap::new(); let mut old_pages = Vec::new();
            let encoded = rs_store::db::dumps_utf8(snapshot).unwrap();
            manager.inner.inventory(&Val::s(encoded), &mut old_candidates, &mut old_pages).unwrap();
            let mut new_candidates = BTreeMap::new(); let mut new_pages = Vec::new();
            manager.inner.inventory_static(&static_data, snapshot.get("raw_sha256").unwrap().as_str().unwrap(), &urls, &mut new_candidates, &mut new_pages).unwrap();
            assert_eq!(old_candidates, new_candidates);
            assert_eq!(old_pages, new_pages);
            let indexed_urls = urls.keys().map(|key| (key.clone(), INDEXED_URL.to_owned())).collect();
            let mut indexed_candidates = BTreeMap::new(); let mut indexed_pages = Vec::new();
            manager.inner.inventory_static(&static_data, snapshot.get("raw_sha256").unwrap().as_str().unwrap(), &indexed_urls, &mut indexed_candidates, &mut indexed_pages).unwrap();
            for candidate in indexed_candidates.values_mut() {
                match candidate {
                    Candidate::Canvas(c) => c.download_url = urls.get(&c.sha256).cloned(),
                    Candidate::Pdf(p) => { p.download_url = urls.get(&p.sha256).cloned(); if let Some(c) = &mut p.overlay { c.download_url = urls.get(&c.sha256).cloned(); } },
                    Candidate::Ink(_) => {},
                }
            }
            assert_eq!(old_candidates, indexed_candidates);
            assert_eq!(old_pages, indexed_pages);
        }
        manager.close();
    }

    #[test]
    fn indexed_canvas_links_are_resolved_only_on_demand() {
        use crate::test_common::{note_with_document, store, TempDir, TestEnv};
        let input = rs_store::val::loads(include_str!("../tests/fixtures/objects_note.json")).unwrap();
        let fixture = crate::container_input::tests::canonical_fixture(input.get("snapshot").unwrap());
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
        crate::container_input::tests::publish_index(&store, &key, &fixture);
        let urls: BTreeMap<String, String> = payload_from_snapshot(&fixture).unwrap().result.download_urls.into_iter().map(|u| (u.id.to_lowercase(), u.url)).collect();
        let manager = PreviewManager::new(store.clone(), TestEnv::new(None, true)).unwrap();
        let trace = rs_store::read_trace::Session::for_container(dir.path(), &key.container_sha256);
        let entry = manager.entry(&identity).unwrap();
        assert!(trace.finish().is_empty(), "candidate selection should not read ZIP entries");
        let canvases: Vec<_> = entry.candidates.values().filter_map(|candidate| if let Candidate::Canvas(c) = candidate { Some(c) } else { None }).collect();
        assert!(!canvases.is_empty());
        let canvas_index = canvases[0].page_index;
        for canvas in canvases {
            assert_eq!(canvas.download_url.as_deref(), Some(INDEXED_URL));
            let resolved = manager.inner.resolve_indexed_url(&identity, &canvas.sha256, canvas.download_url.as_deref()).unwrap();
            assert_eq!(resolved.as_ref(), urls.get(&canvas.sha256));
        }
        let page_id = store.with_db(|db| { use rs_store::db::Sql; let rows = db.query("SELECT page_id FROM note_pages WHERE note_id=? AND idx=?", &rs_store::p![&identity, canvas_index as i64])?; Ok(rows.one().unwrap().text("page_id").unwrap().to_owned()) }).unwrap();
        let trace = rs_store::read_trace::Session::for_container(dir.path(), &key.container_sha256);
        assert!(manager.request(&identity, canvas_index as i64, 0, 30.0).unwrap().is_some());
        let seen = trace.finish();
        assert!(seen.contains(&format!("{page_id}.page")) && seen.contains(&format!("cloud/pages/{page_id}.xml")), "{seen:?}");
        assert!(seen.iter().all(|name| name == &format!("{page_id}.page") || name == &format!("cloud/pages/{page_id}.xml")), "{seen:?}");
        manager.close();
        manager.join(Duration::from_secs(2));
    }

    #[test]
    fn indexed_pdf_link_matches_full_snapshot_without_reading_pages() {
        use crate::test_common::{note_with_document, store, TempDir, TestEnv};
        let all = rs_store::val::loads(include_str!("../tests/fixtures/ink_cases.json")).unwrap();
        let original = all.get("pdf_list").unwrap().get("snapshot").unwrap();
        let old = notes_host::snapshot::from_dict(original).unwrap();
        let digest = crate::util::sha256_hex(b"synthetic pdf identity");
        let url = "https://example.invalid/pdf?signature=synthetic";
        fn replace(bytes: Vec<u8>, from: &str, to: &str) -> Vec<u8> {
            let start = bytes.windows(from.len()).position(|w| w == from.as_bytes()).expect("synthetic XML marker");
            let mut result = bytes[..start].to_vec(); result.extend_from_slice(to.as_bytes()); result.extend_from_slice(&bytes[start + from.len()..]); result
        }
        let raw = replace(old.raw_body, "<contentFileList />", &format!("<contentFileList><contentFile id=\"{digest}\" bindNumber=\"7\" /></contentFileList>"));
        let raw = replace(raw, "<pdfDataList />", &format!("<pdfDataList><pdfData pageIndex=\"0\" hash=\"{digest}\"><pdfRect l=\"2\" t=\"3\" r=\"22\" b=\"13\" /></pdfData></pdfDataList>"));
        let raw = replace(raw, "</CompareNoteView>", &format!("<downloadUrls><downloadUrl><id>{digest}</id><url>{url}</url></downloadUrl></downloadUrls></CompareNoteView>"));
        let snapshot = notes_host::snapshot::to_dict(&notes_host::snapshot::parse_document(&raw, &old.content_type, old.server_id.as_deref()).unwrap());
        let fixture = crate::container_input::tests::canonical_fixture(&snapshot);
        let dir = TempDir::new();
        let store = store(dir.path());
        let identity = note_with_document(&store, &fixture);
        rs_store::snapshot_migration::migrate_v7(dir.path(), &store.path, &notes_host::areas::MigrationProjectionVerifier).unwrap();
        let key = store.pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, 1).unwrap().remove(0);
        crate::container_input::tests::publish_index(&store, &key, &fixture);
        let manager = PreviewManager::new(store, TestEnv::new(None, true)).unwrap();
        let trace = rs_store::read_trace::Session::for_container(dir.path(), &key.container_sha256);
        let entry = manager.entry(&identity).unwrap();
        assert!(trace.finish().is_empty());
        let pdf = entry.candidates.values().find_map(|c| if let Candidate::Pdf(p) = c { Some(p) } else { None }).expect("PDF candidate");
        assert_eq!(pdf.download_url.as_deref(), Some(INDEXED_URL));
        assert_eq!(manager.inner.resolve_indexed_url(&identity, &pdf.sha256, pdf.download_url.as_deref()).unwrap().as_deref(), Some(url));
        manager.close();
        manager.join(Duration::from_secs(2));
    }

    fn job(state: JobState, priority: i64) -> Job {
        Job { state, error: None, priority, local_pdf: None, retry_after: None, served: None, offline: false }
    }

    #[test]
    fn pdf_failure_memo_blocks_due_retry_only_for_matching_source_and_candidate() {
        let candidate = Candidate::Pdf(PdfBackground {
            page_index: 0, pdf_page_index: 0, width: 30, height: 20, rect: (0, 0, 30, 20), annotation_objects: 1,
            sha256: "a".repeat(64), download_url: None, overlay: None, local_ink: None,
        });
        let source = Source(vec![Val::s("source-1")]);
        let item = QueueItem { priority: 5, sequence: 0, key: "synthetic-key".into(), identity: "synthetic".into(),
            source: source.clone(), candidate: candidate.clone(), path: PathBuf::from("synthetic.png"), session: None,
            local_pdf: None, queued: Instant::now() };
        let mut state = State::default();
        let mut ready = job(JobState::Ready, 0);
        ready.retry_after = Some(Instant::now() - Duration::from_secs(1));
        state.set_job(&item.key, ready);
        assert!(state.retry_standin(&item.identity, &item.key, &source, &candidate, Instant::now()));
        remember_pdf_failure(&mut state, &item);
        assert!(!state.retry_standin(&item.identity, &item.key, &source, &candidate, Instant::now()), "expired delay must not restart a deterministic failure");
        for index in 0..=4 * MAX_QUEUED {
            let another = QueueItem { priority: 5, sequence: index as u64, key: format!("key-{index}"),
                identity: format!("note-{index}"), source: source.clone(), candidate: candidate.clone(),
                path: PathBuf::from("synthetic.png"), session: None, local_pdf: None, queued: Instant::now() };
            remember_pdf_failure(&mut state, &another);
        }
        assert!(!state.retry_standin(&item.identity, &item.key, &source, &candidate, Instant::now()),
                "unrelated failures must not evict a deterministic result");
        assert!(state.retry_standin(&item.identity, &item.key, &Source(vec![Val::s("source-2")]), &candidate, Instant::now()));
        let mut changed = candidate.clone();
        if let Candidate::Pdf(pdf) = &mut changed { pdf.width = 31; }
        assert!(state.retry_standin(&item.identity, &item.key, &source, &changed, Instant::now()));
        let mut network = Fail::new(Kind::PdfBackground, "synthetic network error");
        network.code = "link_expired".into();
        assert!(!deterministic_pdf_failure(&network));
        assert!(!deterministic_pdf_failure(&Fail::store("preview_source_changed", "changed", 409)));
        let mut timeout = Fail::new(Kind::InkRender, "timeout"); timeout.code = "ink_prepare_timeout".into();
        assert!(!deterministic_pdf_failure(&timeout));
        let transient_fallback = overlay_fallback_error(Fail::new(Kind::InkRender, "renderer failed"), false);
        assert_eq!(transient_fallback.code, "pdf_overlay_network");
        assert!(!deterministic_pdf_failure(&transient_fallback));
        assert!(deterministic_pdf_failure(&overlay_fallback_error(Fail::new(Kind::InkRender, "renderer failed"), true)));
        let mut decoder = Fail::new(Kind::Preview, "bad image"); decoder.code = "pdf_overlay_decode".into();
        assert!(deterministic_pdf_failure(&decoder));
        decoder.code = "pdf_overlay_missing".into();
        assert!(deterministic_pdf_failure(&decoder));
    }

    #[test]
    fn bad_pdf_overlay_is_not_downloaded_again_after_backoff_expires() {
        use crate::ink::{InkBackend, InkPage};
        use crate::test_common::{candidate, note_with_document, pdf_fixture, png, snapshot, store, TempDir, TestEnv};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Local;
        impl InkBackend for Local {
            fn provider_available(&self) -> bool { true }
            fn inspect_render_pages(&self, _: &Val) -> Vec<InkPage> { vec![] }
            fn render_ink_page(&self, _: &Val, _: i64) -> Result<Vec<u8>, Fail> { Ok(png(30, 20, &[255; 30 * 20 * 4])) }
        }
        if !pdf::pdf_provider_available() { return; }
        let dir = TempDir::new(); let store = store(dir.path());
        let snap = snapshot(b"synthetic note", "application/xml"); let id = note_with_document(&store, &snap);
        let mut overlay = candidate(b"invalid synthetic overlay"); overlay.width = 30; overlay.height = 20;
        let page = PdfBackground { page_index: 0, pdf_page_index: 0, width: 30, height: 20, rect: (2, 3, 22, 13), annotation_objects: 1,
            sha256: crate::test_common::sha256_hex(&pdf_fixture()), download_url: Some(crate::test_common::URL.into()),
            overlay: Some(overlay), local_ink: Some(InkGeometryPreview { page_index: 0, width: 30, height: 20,
                sha256: snap.get("raw_sha256").unwrap().as_str().unwrap().into(), is_dark_mode: false, composite: false, notice: None }) };
        let candidate = Candidate::Pdf(page.clone());
        let source = Arc::new(Mutex::new(Source(vec![Val::s("old")])));
        let (entry_source, current, selected) = (source.clone(), source.clone(), candidate.clone());
        let calls = Arc::new(AtomicUsize::new(0)); let count = calls.clone();
        let manager = PreviewManager::with_options(store, TestEnv::new(Some(dir.path()), false), PreviewOptions {
            ink: Some(Arc::new(Local)),
            fetcher: Some(Arc::new(move |_, _, _| { count.fetch_add(1, Ordering::SeqCst); Ok(b"invalid synthetic overlay".to_vec()) })),
            decoder: Some(Arc::new(|_, _| Err(preview("unsupported synthetic overlay")))),
            entry_hook: Some(Arc::new(move |_| Ok(Entry { source: entry_source.lock().unwrap().clone(), pages: vec![],
                candidates: BTreeMap::from([(0, selected.clone())]) }))),
            source_hook: Some(Arc::new(move |_| Ok(current.lock().unwrap().clone()))), ..Default::default()
        }).unwrap();
        pdf::fetch_pdf_with(&page, &|| Ok(vec![]), &manager.directory().join("pdf-files"), "offline", None,
            &|_, _, path, _, _| { std::fs::write(path, pdf_fixture()).unwrap(); Ok((200, None)) }).unwrap();
        let path = manager.path(&candidate).unwrap(); let key = path.file_name().unwrap().to_string_lossy().into_owned();
        let standin = png(30, 20, &[255; 30 * 20 * 4]); std::fs::write(path.with_extension("local.png"), &standin).unwrap();
        assert_eq!(manager.request(&id, 0, 0, 0.0).unwrap(), Some(standin.clone()));
        assert!(manager.wait_idle(Some(Duration::from_secs(10))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        {
            let mut state = manager.inner.lock();
            assert!(state.pdf_failures.contains_key(&id));
            state.job_mut(&key).unwrap().retry_after = Some(Instant::now() - Duration::from_secs(1));
        }
        assert_eq!(manager.request(&id, 0, 0, 0.0).unwrap(), Some(standin.clone()));
        assert!(manager.wait_idle(Some(Duration::from_secs(5))));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "deterministic decode failure must not be retried");
        *source.lock().unwrap() = Source(vec![Val::s("new")]);
        assert_eq!(manager.request(&id, 0, 0, 0.0).unwrap(), Some(standin));
        assert!(manager.wait_idle(Some(Duration::from_secs(10))));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a new source must permit retry");
        manager.close();
    }

    #[test]
    fn network_overlay_failure_and_ink_refusal_still_retry() {
        use crate::ink::{InkBackend, InkPage};
        use crate::test_common::{candidate, note_with_document, pdf_fixture, png, snapshot, store, TempDir, TestEnv};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Refused;
        impl InkBackend for Refused {
            fn provider_available(&self) -> bool { true }
            fn inspect_render_pages(&self, _: &Val) -> Vec<InkPage> { vec![] }
            fn render_ink_page(&self, _: &Val, _: i64) -> Result<Vec<u8>, Fail> {
                Err(Fail::new(Kind::InkRender, "local renderer refused"))
            }
        }
        if !pdf::pdf_provider_available() { return; }
        let dir = TempDir::new(); let store = store(dir.path());
        let snap = snapshot(b"synthetic note", "application/xml"); let id = note_with_document(&store, &snap);
        let mut overlay = candidate(b"synthetic overlay"); overlay.width = 30; overlay.height = 20;
        let page = PdfBackground { page_index: 0, pdf_page_index: 0, width: 30, height: 20, rect: (2, 3, 22, 13), annotation_objects: 1,
            sha256: crate::test_common::sha256_hex(&pdf_fixture()), download_url: Some(crate::test_common::URL.into()),
            overlay: Some(overlay), local_ink: Some(InkGeometryPreview { page_index: 0, width: 30, height: 20,
                sha256: snap.get("raw_sha256").unwrap().as_str().unwrap().into(), is_dark_mode: false, composite: false, notice: None }) };
        let candidate = Candidate::Pdf(page.clone());
        let entry = Entry { source: Source(vec![Val::s("synthetic")]), pages: vec![], candidates: BTreeMap::from([(0, candidate.clone())]) };
        let (e1, e2) = (entry.clone(), entry.clone());
        let calls = Arc::new(AtomicUsize::new(0)); let count = calls.clone();
        let manager = PreviewManager::with_options(store, TestEnv::new(Some(dir.path()), false), PreviewOptions {
            ink: Some(Arc::new(Refused)),
            fetcher: Some(Arc::new(move |_, _, _| {
                if count.fetch_add(1, Ordering::SeqCst) == 0 { Err(Fail::os("synthetic network")) }
                else { Ok(b"synthetic overlay".to_vec()) }
            })),
            decoder: Some(Arc::new(|_, _| Ok(png(30, 20, &[0; 30 * 20 * 4])))),
            entry_hook: Some(Arc::new(move |_| Ok(e1.clone()))),
            source_hook: Some(Arc::new(move |_| Ok(e2.source.clone()))), ..Default::default()
        }).unwrap();
        pdf::fetch_pdf_with(&page, &|| Ok(vec![]), &manager.directory().join("pdf-files"), "offline", None,
            &|_, _, path, _, _| { std::fs::write(path, pdf_fixture()).unwrap(); Ok((200, None)) }).unwrap();
        let path = manager.path(&candidate).unwrap(); let key = path.file_name().unwrap().to_string_lossy().into_owned();
        let standin = png(30, 20, &[0xff; 30 * 20 * 4]); std::fs::write(path.with_extension("local.png"), &standin).unwrap();
        assert_eq!(manager.request(&id, 0, 0, 0.0).unwrap(), Some(standin.clone()));
        assert!(manager.wait_idle(Some(Duration::from_secs(10))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        {
            let mut state = manager.inner.lock();
            assert!(!state.pdf_failures.contains_key(&id), "network failure must not become deterministic");
            let job = state.job_mut(&key).unwrap();
            assert_eq!(job.state, JobState::Ready);
            job.retry_after = Some(Instant::now() - Duration::from_secs(1));
        }
        assert_eq!(manager.request(&id, 0, 0, 0.0).unwrap(), Some(standin));
        assert!(manager.wait_idle(Some(Duration::from_secs(10))));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "network recovery should retry the native overlay");
        assert!(path.exists(), "recovered overlay should produce a full image");
        manager.close();
    }

    #[test]
    fn legacy_image_cache_is_verified_and_shared() {
        use crate::test_common::{sha256_hex, store, TempDir, TestEnv};
        let dir = TempDir::new();
        let store = store(dir.path());
        let manager = PreviewManager::new(store.clone(), TestEnv::new(None, true)).unwrap();
        let bytes = b"synthetic image bytes";
        let hash = sha256_hex(bytes);
        let legacy = manager.inner.media_path(&hash).unwrap();
        util::write_private(&legacy, bytes, 8, false, &|| Ok(())).unwrap();
        assert_eq!(manager.inner.media_cached(&hash).unwrap().unwrap(), bytes);
        let shared = rs_store::media_store::MediaStore::open(&store.directory).unwrap();
        assert!(shared.path_verified(&hash).unwrap().is_some());
        std::fs::remove_file(legacy).unwrap();
        assert_eq!(manager.inner.media_cached(&hash).unwrap().unwrap(), bytes);
        assert!(manager.inner.store_media(&hash, b"wrong bytes", &|| Ok(())).is_err());
        manager.close();
    }

    #[test]
    fn transient_retries_back_off_and_reset_for_a_new_source() {
        let mut state = State::default();
        assert_eq!(next_retry_delay(&mut state, "pdf"), RETRY_FULL);
        assert_eq!(next_retry_delay(&mut state, "pdf"), RETRY_FULL * 2);
        assert_eq!(next_retry_delay(&mut state, "pdf"), RETRY_FULL * 4);
        let old = Source(vec![Val::s("old")]);
        let new = Source(vec![Val::s("new")]);
        let candidate = Candidate::Pdf(PdfBackground { page_index: 0, pdf_page_index: 0, width: 1, height: 1,
            rect: (0, 0, 1, 1), annotation_objects: 1, sha256: "a".repeat(64), download_url: None, overlay: None, local_ink: None });
        state.job_sources.insert("pdf".into(), (old, candidate.clone()));
        state.set_job("pdf", job(JobState::Ready, 0));
        state.discard_stale_job("synthetic", "pdf", &new, &candidate);
        assert_eq!(next_retry_delay(&mut state, "pdf"), RETRY_FULL);
    }

    #[test]
    fn timing_outcome_reports_only_a_safe_error_code() {
        let mut error = Fail::new(Kind::InkRender, "private note content");
        error.code = "pdf_ink_render".into();
        assert_eq!(timing_outcome(&Err(error.clone())), "error:InkRender:pdf_ink_render");
        error.code = "secret\"content".into();
        assert_eq!(timing_outcome(&Err(error)), "error:InkRender:invalid_code");
    }

    #[test]
    fn refreshing_links_invalidates_snapshot_even_when_source_is_unchanged() {
        struct Refresh { store: Arc<StoreCore>, document: String }
        impl PreviewEnv for Refresh {
            fn session_path(&self) -> Option<PathBuf> { Some(PathBuf::from("synthetic-session")) }
            fn offline(&self) -> bool { false }
            fn headers(&self) -> Result<Vec<(String, String)>, Fail> { Ok(vec![]) }
            fn refresh_note(&self, identity: &str) -> Result<Val, Fail> {
                self.store.with_db(|db| {
                    use rs_store::db::Sql;
                    db.exec("UPDATE notes SET base_document=? WHERE id=?", &rs_store::p![self.document.as_str(), identity])?;
                    Ok(())
                }).map_err(store_fail)?;
                Ok(Val::Null)
            }
        }
        let dir = std::env::temp_dir().join(format!("preview-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Arc::new(StoreCore::open(&dir).unwrap());
        let note = store.create(&Val::s("Synthetic"), &Val::s("")).unwrap();
        let id = note.get("id").unwrap().as_str().unwrap().to_string();
        {
            use rs_store::db::Sql;
            store.with_db(|db| { db.exec("UPDATE notes SET base_document=? WHERE id=?", &rs_store::p![r#"{"url":"old"}"#, id.as_str()])?; Ok(()) }).unwrap();
        }
        let env = Arc::new(Refresh { store: store.clone(), document: r#"{"url":"new"}"#.to_string() });
        let manager = PreviewManager::with_options(store, env, PreviewOptions {
            source_hook: Some(Arc::new(|_| Ok(Source(vec![Val::s("same-commit")])))), ..Default::default()
        }).unwrap();
        assert_eq!(manager.inner.snapshot(&id).unwrap().get("url"), Some(&Val::s("old")));
        assert!(manager.inner.renew_links(&id));
        assert_eq!(manager.inner.snapshot(&id).unwrap().get("url"), Some(&Val::s("new")));
        manager.close();
        let _ = std::fs::remove_dir_all(dir);
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
#[derive(Default)]
struct Stages { download: Duration, decode: Duration, local: Duration, renderer: Duration, png: Duration, detail: [Duration; 19] }

fn timing_error_code(code: &str) -> &str {
    if code.len() <= 64 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        code
    } else {
        "invalid_code"
    }
}

fn timing_outcome(result: &Result<Option<PathBuf>, Fail>) -> String {
    match result {
        Ok(None) => "ready".to_string(),
        Ok(Some(_)) => "stand-in".to_string(),
        Err(e) => format!("error:{:?}:{}", e.kind, timing_error_code(&e.code)),
    }
}

fn timing(item: &QueueItem, started: Instant, result: &Result<Option<PathBuf>, Fail>, stages: &Stages) {
    let Some(file) = std::env::var_os("NOTES_PREVIEW_TIMING") else { return };
    let kind = match &item.candidate {
        Candidate::Canvas(_) => "canvas",
        Candidate::Pdf(_) => "pdf",
        Candidate::Ink(_) => "ink",
    };
    let outcome = timing_outcome(result);
    let line = format!("{{\"kind\":\"{kind}\",\"priority\":{},\"waitMs\":{},\"workMs\":{},\"downloadMs\":{},\"decodeMs\":{},\"localMs\":{},\"rendererMs\":{},\"pngEncodeMs\":{},\"snapshotMs\":{},\"inventoryMs\":{},\"bodyMs\":{},\"mediaMs\":{},\"compositeMs\":{},\"payloadMs\":{},\"cacheWriteMs\":{},\"rasterMs\":{},\"identityMs\":{},\"childSpawnMs\":{},\"childReadyMs\":{},\"childReadMs\":{},\"childJsonMs\":{},\"childSnapshotMs\":{},\"childVerifyMs\":{},\"childStrokeDecodeMs\":{},\"childInventoryMs\":{},\"childPageBuildMs\":{},\"childWriteMs\":{},\"outcome\":\"{outcome}\"}}\n",
        item.priority, started.duration_since(item.queued).as_millis(), started.elapsed().as_millis(),
        stages.download.as_millis(), stages.decode.as_millis(), stages.local.as_millis(), stages.renderer.as_millis(), stages.png.as_millis(),
        stages.detail[0].as_millis(), stages.detail[1].as_millis(), stages.detail[2].as_millis(),
        stages.detail[3].as_millis(), stages.detail[4].as_millis(), stages.detail[5].as_millis(),
        stages.detail[6].as_millis(), stages.detail[7].as_millis(), stages.detail[8].as_millis(),
        stages.detail[9].as_millis(), stages.detail[10].as_millis(), stages.detail[11].as_millis(), stages.detail[12].as_millis(),
        stages.detail[13].as_millis(), stages.detail[14].as_millis(), stages.detail[15].as_millis(), stages.detail[16].as_millis(),
        stages.detail[17].as_millis(), stages.detail[18].as_millis());
    use std::io::Write;
    if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = out.write_all(line.as_bytes());
    }
}
