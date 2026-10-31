//! Event-driven, single-worker static preview inventory generation.
//! It keeps no snapshots in its queue or interactive caches. Signed URLs and
//! device/account-dependent availability are never persisted.
use notes_host::{Fail, Kind, Val};
use rs_store::preview_inventory::{
    Canvas, InkPage, Inventory, InventoryKey, InventoryOutcome, PageSize, Pdf,
};
use rs_store::storage::StoreCore;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

static COMPUTATION: Mutex<()> = Mutex::new(());
#[cfg(test)]
pub(crate) fn hold_computation() -> std::sync::MutexGuard<'static, ()> {
    COMPUTATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
const BATCH: usize = 32;
const ATTEMPTS: usize = 3;

// The inventory worker processes at most one note at a time. Return the
// temporary parser arenas to the OS between its expensive phases as well as
// between notes, so a formerly large note does not become the idle footprint.
fn release_temporary_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim has no pointer arguments and is thread-safe.
    unsafe { libc::malloc_trim(0); }
}

/// Pure static metadata; network/provider/local-cache decisions remain dynamic.
pub fn compute_static(snapshot: &Val) -> Result<Inventory, Fail> {
    let payload = crate::payload::payload_from_snapshot(snapshot)?;
    let ink_inventory = crate::payload::inspect_ink_payload(&payload)?;
    if ink_inventory.page_count > 4096 {
        return Err(Fail::value("preview page count exceeds limit"));
    }
    let mut sizes = crate::manager::page_sizes(&payload);
    sizes.resize(ink_inventory.page_count, None);
    sizes.truncate(ink_inventory.page_count);
    let pages = sizes
        .into_iter()
        .map(|size| match size {
            Some((width, height)) => PageSize {
                width: Some(width),
                height: Some(height),
            },
            None => PageSize {
                width: None,
                height: None,
            },
        })
        .collect();
    let canvases = ink_inventory
        .canvas_candidates
        .into_iter()
        .map(|item| Canvas {
            page_index: item.page_index,
            key: item.key,
            width: item.width,
            height: item.height,
            dark: item.is_dark_mode,
            bind_number: item.bind_number,
            sha256: item.sha256,
        })
        .collect();
    let (pdfs, unsupported_pdf_pages) = match crate::pdf::inspect_pdf_backgrounds(&payload) {
        Ok(items) => (
            items
                .into_iter()
                .map(|item| Pdf {
                    page_index: item.page_index as usize,
                    pdf_page_index: item.pdf_page_index,
                    width: item.width,
                    height: item.height,
                    rect: item.rect,
                    annotation_objects: item.annotation_objects,
                    sha256: item.sha256,
                })
                .collect(),
            Vec::new(),
        ),
        Err(error) if error.kind == Kind::PdfBackground => (
            Vec::new(),
            crate::pdf::pdf_page_indices(&payload)?
                .into_iter()
                .collect(),
        ),
        Err(error) => return Err(error),
    };
    drop(payload);
    release_temporary_heap();
    let (ink_pages, ink_supported) = match crate::ink::prepare::inventory_uncached(snapshot) {
        Ok(items) => (
            items
                .into_iter()
                .map(|item| InkPage {
                    page_index: item.index as usize,
                    width: item.width,
                    height: item.height,
                    dark: item.dark_mode,
                    composite: item.composite,
                    notice: item.notice,
                    over_pdf: item.over_pdf,
                })
                .collect(),
            true,
        ),
        Err(_) => (Vec::new(), false),
    };
    Ok(Inventory {
        pages,
        canvases,
        pdfs,
        unsupported_pdf_pages,
        ink_pages,
        ink_supported,
    })
}

fn one(
    store: &StoreCore,
    key: &InventoryKey,
    stop: &AtomicBool,
) -> Result<bool, rs_store::db::Error> {
    if stop.load(Ordering::Acquire) {
        return Ok(false);
    }
    let Some(snapshot) = store.preview_inventory_source(key)? else {
        return Ok(false);
    };
    release_temporary_heap();
    if stop.load(Ordering::Acquire) {
        return Ok(false);
    }
    let output = match compute_static(&snapshot) {
        Ok(inventory) => InventoryOutcome::Ready(inventory),
        Err(_) => InventoryOutcome::Unsupported,
    };
    release_temporary_heap();
    let blob = if matches!(output, InventoryOutcome::Ready(_)) {
        let parsed = crate::payload::payload_from_snapshot(&snapshot)
            .map_err(|e| rs_store::db::Error::Value(format!("preview payload: {e:?}")))?;
        let ids = parsed
            .result
            .download_urls
            .iter()
            .map(|item| item.id.to_ascii_lowercase())
            .filter(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .collect();
        let resource = notes_host::snapshot::from_dict(&snapshot)
            .map_err(|e| rs_store::db::Error::Value(format!("preview resource: {e:?}")))?;
        rs_store::preview_render_index::capture(store, key, &resource.resource_xml, ids)?
    } else {
        None
    };
    drop(snapshot);
    release_temporary_heap();
    if stop.load(Ordering::Acquire) {
        return Ok(false);
    }
    store.publish_preview_inventory_with_blob(key, &output, blob.as_deref())
}

/// A single background producer. Wakeups coalesce, and each source is tried at
/// most three times per event before a durable Failed state requires explicit
/// user retry. No timer or page-request polling loop is created here.
#[derive(Default)]
struct Health {
    generation: u64,
    error: Option<String>,
}

pub struct InventoryWorker {
    wake: mpsc::SyncSender<()>,
    stop: Arc<AtomicBool>,
    health: Arc<Mutex<Health>>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl InventoryWorker {
    pub fn start(store: Arc<StoreCore>) -> std::io::Result<Self> {
        let (wake, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let health = Arc::new(Mutex::new(Health::default()));
        let worker_stop = stop.clone();
        let worker_health = health.clone();
        let continue_wake = wake.clone();
        let join = std::thread::Builder::new()
            .name("notes-preview-inventory".into())
            .spawn(move || {
                while receiver.recv().is_ok() && !worker_stop.load(Ordering::Acquire) {
                    let _single = COMPUTATION
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let generation = worker_health
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .generation;
                    let mut processed = 0usize;
                    let mut progressed = false;
                    let mut failure = None;
                    while processed < 256 && !worker_stop.load(Ordering::Acquire) {
                        let keys = match store
                            .pending_preview_inventories(crate::PREVIEW_INVENTORY_RECIPE, BATCH)
                        {
                            Ok(keys) => keys,
                            Err(_) => {
                                failure = Some("Preview inventory could not be read".to_string());
                                break;
                            }
                        };
                        if keys.is_empty() {
                            break;
                        }
                        for key in keys {
                            if worker_stop.load(Ordering::Acquire) {
                                break;
                            }
                            processed += 1;
                            let mut success = false;
                            for _ in 0..ATTEMPTS {
                                if worker_stop.load(Ordering::Acquire) {
                                    break;
                                }
                                let result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        one(&store, &key, &worker_stop)
                                    }));
                                if let Ok(Ok(committed)) = result {
                                    progressed |= committed;
                                    success = true;
                                    break;
                                }
                                if !worker_stop.load(Ordering::Acquire) {
                                    std::thread::sleep(std::time::Duration::from_millis(50));
                                }
                            }
                            if !success && !worker_stop.load(Ordering::Acquire) {
                                match store
                                    .publish_preview_inventory(&key, &InventoryOutcome::Failed)
                                {
                                    Ok(committed) => progressed |= committed,
                                    Err(_) => {
                                        failure = Some(
                                            "Preview inventory could not be saved".to_string(),
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                        if failure.is_some() {
                            break;
                        }
                    }
                    if processed >= 256
                        && progressed
                        && failure.is_none()
                        && !worker_stop.load(Ordering::Acquire)
                    {
                        let _ = continue_wake.try_send(());
                    }
                    let mut health = worker_health
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if health.generation == generation {
                        health.error = failure;
                    }
                }
            })?;
        Ok(Self {
            wake,
            stop,
            health,
            join: Mutex::new(Some(join)),
        })
    }

    /// Signal at startup, after snapshot publication and on explicit retry.
    pub fn wake(&self) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => true,
            Err(mpsc::TrySendError::Disconnected(())) => false,
        }
    }

    /// Generic error: no raw note content, paths or signed URLs escape.
    pub fn last_error(&self) -> Option<String> {
        self.health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .error
            .clone()
    }

    /// Explicit retry prevents the preceding pass from restoring stale health.
    pub fn wake_retry(&self) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        {
            let mut health = self
                .health
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            health.generation = health.generation.wrapping_add(1);
            health.error = None;
        }
        if self.wake() {
            true
        } else {
            self.health
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .error = Some("Preview inventory worker unavailable".into());
            false
        }
    }

    /// Stop scheduling and wait up to `timeout`; false means a currently
    /// running computation remains alive and must be joined later.
    pub fn join_for(&self, timeout: std::time::Duration) -> bool {
        self.stop();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let mut guard = self
                .join
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.as_ref() {
                None => return true,
                Some(handle) if handle.is_finished() => {
                    let handle = guard.take().unwrap();
                    drop(guard);
                    return handle.join().is_ok();
                }
                _ => {}
            }
            drop(guard);
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(10)));
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
    }
}
impl Drop for InventoryWorker {
    fn drop(&mut self) {
        self.stop();
    }
}
