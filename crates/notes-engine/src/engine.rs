//! `samsung_notes.engine.Engine` in pure Rust: durable local editing, the
//! guarded Cloud outbox and the incremental library sync.
//!
//! Method bodies follow engine.py line by line (same store calls in the same
//! order, same error classes and Turkish messages, same `except` decisions);
//! comments name the Python method each function mirrors.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use notes_host::host::PureHost;
use notes_host::snapshot::{self, Snapshot};
use notes_host::{areas, Fail, Val};
use rs_store::db::Sql;
use rs_store::storage::StoreCore;

use crate::cloud::{Cloud, CloudFactory, Metadata};
use crate::previews::{Previews, Thumbnails};
use crate::sessions::Sessions;
use crate::util::{is_str, key, key_str, loads, meta_or, Event, R};

/// `Engine.MAX_PDF_BYTES`
pub const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
/// `Engine.DOWNLOAD_WORKERS`
pub const DOWNLOAD_WORKERS: usize = 4;

#[derive(Clone, Debug)]
pub struct SyncState {
    pub state: String,
    pub error: Option<String>,
    pub processed: i64,
    pub total: Option<i64>,
}

/// `self._api()` cache (`api_instance`, `api_session`) shared with the
/// preview environment.
pub struct ApiCache {
    pub sessions: Arc<Sessions>,
    factory: Arc<dyn CloudFactory>,
    work_dir: PathBuf,
    current: Mutex<(Option<Arc<dyn Cloud>>, Option<PathBuf>)>,
}

impl ApiCache {
    /// `Engine._api()`
    pub fn api(&self) -> R<Arc<dyn Cloud>> {
        let path = self.sessions.path();
        let Some(path) = path.filter(|_| !self.sessions.offline()) else {
            return Err(Fail::store("not_connected", "Samsung hesabına giriş yap. Yerel notların korunuyor.", 401));
        };
        let mut current = self.current.lock().unwrap();
        if current.0.is_none() || current.1.as_deref() != Some(path.as_path()) {
            let api = self.factory.open(&path, &self.work_dir)?;
            *current = (Some(api), Some(path));
        }
        Ok(current.0.clone().unwrap())
    }
    /// `self.api_session`
    pub fn session(&self) -> Option<PathBuf> {
        self.current.lock().unwrap().1.clone()
    }
    pub fn metadata(&self, api: &Arc<dyn Cloud>) -> Option<Arc<dyn Metadata>> {
        self.factory.metadata(api)
    }
}

/// Builds `PreviewManager(engine)` / `ThumbnailManager(engine)`.
pub trait PreviewFactory: Send + Sync {
    /// `PreviewManager(self)` then `ThumbnailManager(self)` (same order of
    /// directory creation as `Engine.__init__`).
    fn build(&self, store: &Arc<StoreCore>, api: &Arc<ApiCache>) -> R<(Arc<dyn Previews>, Arc<dyn Thumbnails>)>;
}

pub struct Options {
    pub offline: bool,
    pub session: Option<PathBuf>,
    pub auto_sync: bool,
    pub interval: f64,
    pub cloud: Arc<dyn CloudFactory>,
    pub previews: Arc<dyn PreviewFactory>,
    pub login: Arc<dyn crate::sessions::Login>,
}

pub struct Engine {
    pub store: Arc<StoreCore>,
    pub host: PureHost,
    pub sessions: Arc<Sessions>,
    pub apis: Arc<ApiCache>,
    pub auto_sync: bool,
    pub interval: f64,
    /// `self.lock` guarding state/error/processed/total.
    pub sync: Mutex<SyncState>,
    sync_lock: Mutex<()>,
    pub wake: Event,
    pub stop: Event,
    pub previews: Arc<dyn Previews>,
    pub thumbnails: Arc<dyn Thumbnails>,
    worker: Mutex<Option<JoinHandle<()>>>,
    warmup: Mutex<Option<JoinHandle<()>>>,
    /// `self._template_cache`: note id -> (revision, entries, contentFile ids).
    template_cache: Mutex<std::collections::HashMap<String, TemplateCacheEntry>>,
}

type TemplateCacheEntry = (Val, Vec<Val>, Vec<Option<String>>);

fn s(v: &str) -> Val {
    Val::s(v)
}

fn opt(v: Option<String>) -> Val {
    v.map(Val::Str).unwrap_or(Val::Null)
}

/// `{**d, 'capabilities': {key: False for key in d['capabilities']}}`
fn all_capabilities_false(projection: &Val) -> R<Val> {
    let caps = key(projection, "capabilities")?;
    let Val::Dict(items) = caps else { return Err(Fail::type_("'capabilities' is not a dict")) };
    let mut out = projection.clone();
    out.set("capabilities", Val::Dict(items.iter().map(|(k, _)| (k.clone(), Val::Bool(false))).collect()));
    Ok(out)
}

fn with_revision(projection: &Val, revision: &Val) -> Val {
    let mut out = projection.clone();
    out.set("revision", revision.clone());
    out
}

impl Engine {
    /// `Engine(directory, offline=, session=, auto_sync=, api_factory=, interval=, metadata_factory=)`
    pub fn new(directory: &Path, options: Options) -> R<Arc<Engine>> {
        let store = Arc::new(StoreCore::open(directory)?);
        let sessions = Sessions::new(Arc::clone(&store), options.offline, options.session.as_deref(), Arc::clone(&options.login))?;
        let apis = Arc::new(ApiCache {
            sessions: Arc::clone(&sessions),
            factory: options.cloud,
            work_dir: store.directory.join("cloud"),
            current: Mutex::new((None, None)),
        });
        let initial = SyncState { state: if options.offline { "offline" } else { "idle" }.into(), error: None, processed: 0, total: None };
        // _bootstrap_cache runs before the preview managers exist.
        bootstrap_cache(&store, &sessions)?;
        let (previews, thumbnails) = options.previews.build(&store, &apis)?;
        let engine = Arc::new(Engine {
            store,
            host: PureHost::new(),
            sessions: Arc::clone(&sessions),
            apis,
            auto_sync: options.auto_sync,
            interval: options.interval.max(10.0),
            sync: Mutex::new(initial),
            sync_lock: Mutex::new(()),
            wake: Event::new(),
            stop: Event::new(),
            previews,
            thumbnails,
            worker: Mutex::new(None),
            warmup: Mutex::new(None),
            template_cache: Mutex::new(std::collections::HashMap::new()),
        });
        let weak: Weak<Engine> = Arc::downgrade(&engine);
        sessions.set_on_connected(Box::new(move || {
            if let Some(engine) = weak.upgrade() {
                engine.request_sync();
            }
        }));
        let weak = Arc::downgrade(&engine);
        let worker = std::thread::Builder::new().name("notes-sync".into()).spawn(move || run_loop(weak)).map_err(Fail::from)?;
        *engine.worker.lock().unwrap() = Some(worker);
        if options.auto_sync && sessions.path().is_some() {
            engine.request_sync();
        }
        Ok(engine)
    }

    fn api(&self) -> R<Arc<dyn Cloud>> {
        self.apis.api()
    }

    /// `self.stop.is_set() or self.sessions.path != self.api_session`
    fn interrupted(&self) -> bool {
        self.stop.is_set() || self.sessions.path() != self.apis.session()
    }

    fn folder_initialized(&self) -> R<bool> {
        Ok(meta_or(&self.store, "folder_initialized", Val::Bool(false))?.truthy())
    }

    fn bump_processed(&self) {
        self.sync.lock().unwrap().processed += 1;
    }

    // ------------------------------------------------------------ API methods

    /// `Engine.status()`
    pub fn status(&self) -> R<Val> {
        let mut connected = false;
        if self.sessions.path().is_some() && !self.sessions.offline() {
            let attempt = self.api().and_then(|api| api.auth_status()).and_then(|auth| {
                let valid = key(&auth, "valid")?.clone();
                Ok(if valid.truthy() { valid } else { key(&auth, "renewable")?.clone() })
            });
            match attempt {
                Ok(v) => connected = v.truthy(),
                Err(e) if e.is_api() || e.is_os_value_key_type() => {}
                Err(e) => return Err(e),
            }
        }
        let counts = self.store.counts()?;
        let state = self.sync.lock().unwrap().clone();
        let initialized = || meta_or(&self.store, "folder_initialized", Val::Bool(false));
        Ok(Val::dict(vec![
            ("connected", Val::Bool(connected)),
            ("accountLabel", if connected { s("Samsung hesabı") } else { Val::Null }),
            ("loginPending", Val::Bool(self.sessions.pending())),
            ("loginError", opt(self.sessions.error())),
            (
                "sync",
                Val::dict(vec![
                    ("state", Val::s(state.state)),
                    ("lastSyncedAt", meta_or(&self.store, "last_synced_at", Val::Null)?),
                    ("pending", key(&counts, "pending")?.clone()),
                    ("conflicts", key(&counts, "conflicts")?.clone()),
                    (
                        "progress",
                        Val::dict(vec![("processed", Val::Int(state.processed)), ("total", state.total.map(Val::Int).unwrap_or(Val::Null))]),
                    ),
                    ("error", opt(state.error)),
                ]),
            ),
            (
                "capabilities",
                Val::dict(vec![
                    ("textEditing", Val::Bool(true)),
                    ("richTextEditing", Val::Bool(true)),
                    ("cloudDelete", Val::Bool(false)),
                    ("attachments", Val::Bool(false)),
                    ("cloudTrash", initialized()?),
                    ("cloudFavorites", initialized()?),
                    ("cloudFolders", initialized()?),
                ]),
            ),
        ]))
    }

    /// `Engine.request_sync()`
    pub fn request_sync(&self) -> bool {
        if self.sessions.offline() || self.sessions.path().is_none() {
            return false;
        }
        let already = self.wake.is_set() || self.sync.lock().unwrap().state == "syncing";
        self.wake.set();
        !already
    }

    pub fn create(&self, title: &Val, body: &Val) -> R<Val> {
        let result = self.store.create(title, body)?;
        self.request_sync();
        Ok(result)
    }

    pub fn save(&self, identity: &str, title: &Val, body: &Val, revision: &Val) -> R<Val> {
        let result = self.store.save(&self.host, identity, title, body, revision)?;
        self.request_sync();
        Ok(result)
    }

    pub fn organize(&self, identity: &str, changes: &Val) -> R<Val> {
        if !self.folder_initialized()? {
            return Err(Fail::store("metadata_not_ready", "Önce bulut kütüphanesinin indirilmesini bekle.", 409));
        }
        let result = self.store.organize_cloud(&self.host, identity, changes)?;
        self.request_sync();
        Ok(result)
    }

    pub fn folder_change(&self, identity: Option<&str>, changes: &Val) -> R<Val> {
        if !self.folder_initialized()? {
            return Err(Fail::store("metadata_not_ready", "Önce bulut klasörlerinin indirilmesini bekle.", 409));
        }
        let result = match identity {
            None => {
                if !changes.keys().iter().all(|k| *k == "name" || *k == "parentId") {
                    return Err(Fail::store400("invalid_change", "Geçersiz klasör isteği."));
                }
                self.store.create_folder(&changes.get_or_null("name"), &changes.get_or_null("parentId"))?
            }
            Some(id) => self.store.update_folder(&s(id), changes)?,
        };
        self.request_sync();
        Ok(result)
    }

    pub fn resolve_folder(&self, identity: &str, strategy: &Val) -> R<Val> {
        if !is_str(strategy, "use_remote") {
            return Err(Fail::store("invalid_strategy", "Buluttaki klasör durumunu yeniden al.", 409));
        }
        // The read must succeed before a queued/uncertain intent is abandoned.
        let api = self.api()?;
        let metadata = self.apis.metadata(&api).ok_or_else(|| Fail::type_("'NoneType' object is not callable"))?;
        let inventory = metadata.inventory()?;
        Ok(self.store.accept_remote_folder(&s(identity), &inventory)?.unwrap_or(Val::Null))
    }

    /// `Engine.get(identity)`
    pub fn get(&self, identity: &str) -> R<Val> {
        let mut note = self.store.get(identity)?;
        if !key(&note, "contentLoaded")?.truthy() && is_str(key(&note, "source")?, "cloud") && self.sessions.path().is_some() {
            let row = self.store.raw(identity)?;
            let server_id = key(&row, "server_id")?.clone();
            let fetched = self.api().and_then(|api| api.get_note(&server_id));
            match fetched {
                Ok(snapshot) => {
                    self.store.apply_document(&self.host, &server_id, &snapshot::to_dict(&snapshot), None)?;
                }
                Err(e) if e.is_api() => self.store.content_error(identity, &Val::s(e.message.clone()))?,
                Err(e) => return Err(e),
            }
            note = self.store.get(identity)?;
        }
        // Polling an open note must not deserialize its potentially large
        // binary snapshot. Inspect it only once for each loaded Cloud version.
        let needs_capability = self.store.with_db(|db| {
            let cur = db.query(
                "SELECT loaded=1 AND base_document IS NOT NULL\n                AND title_editable IS NULL FROM notes WHERE id=?",
                &rs_store::p![identity],
            )?;
            Ok(cur.scalar().map(rs_store::db::sql_truthy))
        })?;
        let Some(needs_capability) = needs_capability else {
            return Err(Fail::type_("'NoneType' object is not subscriptable"));
        };
        if needs_capability {
            let row = self.store.raw(identity)?;
            let document = loads(key(&row, "base_document")?)?;
            let attempt = (|| -> R<bool> {
                let projection = areas::editor_projection(&snapshot::from_dict(&document)?)?;
                let supported = key(key(&projection, "capabilities")?, "titleEdit")?.truthy();
                let state = key(&row, "sync_state")?;
                if is_str(state, "synced") || is_str(state, "local") {
                    self.store.cache_editor(identity, key(&note, "revision")?, &projection)?;
                }
                Ok(supported)
            })();
            let supported = match attempt {
                Ok(v) => v,
                Err(e) if e.is_document() || e.is_value() || e.is_key() || e.is_type() => false,
                Err(e) => return Err(e),
            };
            self.store.title_capability(identity, supported, &document.get_or_null("raw_sha256"))?;
            note = self.store.get(identity)?;
        }
        Ok(note)
    }

    pub fn save_title(&self, identity: &str, title: &Val, revision: &Val) -> R<Val> {
        self.get(identity)?; // Establish operation-specific capability once.
        let result = self.store.save_title(&self.host, identity, title, revision)?;
        self.request_sync();
        Ok(result)
    }

    /// `Engine.export_file(identity, fmt)`
    pub fn export_file(&self, identity: &str, fmt: &Val) -> R<Val> {
        use rs_engine::engine_core as core;
        let note = self.get(identity)?;
        let fmt = if is_str(fmt, "pdf") {
            "pdf"
        } else if is_str(fmt, "txt") {
            "txt"
        } else {
            return Err(Fail::store("invalid_format", "Dosya türü desteklenmiyor.", 400));
        };
        let home = rs_engine::server::home_dir();
        let folder = core::download_folder(&home);
        let title = key_str(&note, "title")?;
        let source = if title.is_empty() { "Not" } else { title };
        let replaced: String = source.chars().map(|c| if is_word_char(c) || " .()-".contains(c) { c } else { '_' }).collect();
        let base: String = core::py_strip(&replaced).chars().take(80).collect();
        let base = if base.is_empty() { "Not".to_string() } else { base };
        let file_name = |target: &std::path::Path| target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if fmt == "txt" {
            let text = format!("{}\n\n{}", title, key_str(&note, "body")?);
            let target = core::write_free_target(&folder, &base, "txt", &mut text.as_bytes())?;
            let name = file_name(&target);
            return Ok(Val::dict(vec![
                ("path", Val::s(target.to_string_lossy())),
                ("name", Val::s(name)),
                ("pages", Val::Int(0)),
                ("skipped", Val::Int(0)),
            ]));
        }
        if !notes_platform::tools::available(notes_platform::tools::Tool::Magick) {
            // Linux: unchanged text (PATH lookup); macOS appends `brew install imagemagick`.
            let message = notes_platform::tools::with_install_hint("PDF oluşturmak için ImageMagick gerekli.", notes_platform::tools::Tool::Magick);
            return Err(Fail::store("pdf_unavailable", &message, 503));
        }
        let manifest = self.previews.manifest(identity)?;
        let mut pages = Vec::new();
        if let Val::List(items) = key(&manifest, "pages")? {
            for page in items {
                if page.get("available").map(Val::truthy).unwrap_or(false) {
                    pages.push(page.clone());
                }
            }
        }
        if pages.is_empty() {
            return Err(Fail::store("no_pages", "Bu notta dışa aktarılabilecek sayfa görüntüsü yok.", 422));
        }
        let work = core::TempDir::new("notes-export-")?;
        let (mut files, mut skipped) = (Vec::new(), 0i64);
        for page in &pages {
            let index = key(page, "index")?.as_i64().ok_or_else(|| Fail::type_("page index"))?;
            match self.previews.request(identity, index, 0, 90.0)? {
                None => skipped += 1,
                Some(raw) => {
                    let path = work.0.join(format!("{:05}.png", index));
                    std::fs::write(&path, raw)?;
                    files.push(path);
                }
            }
        }
        if files.is_empty() {
            return Err(Fail::store("no_pages", "Sayfa görüntüleri hazırlanamadı.", 503));
        }
        // magick stores the output basename as the PDF /Title: name the private
        // work file like the reference's partial (`.<target>.part`).
        let predicted = core::free_target(&folder, &base, "pdf");
        let produced = work.0.join(core::pdf_work_name(&file_name(&predicted)));
        if !core::images_to_pdf_at(&files, &produced) {
            return Err(Fail::store("pdf_failed", "PDF oluşturulamadı.", 500));
        }
        let target = core::write_free_target(&folder, &base, "pdf", &mut std::fs::File::open(&produced)?)?;
        let name = file_name(&target);
        Ok(Val::dict(vec![
            ("path", Val::s(target.to_string_lossy())),
            ("name", Val::s(name)),
            ("pages", Val::Int(files.len() as i64)),
            ("skipped", Val::Int(skipped)),
        ]))
    }

    pub fn page_ops(&self, identity: &str) -> R<Val> {
        self.get(identity)?;
        Ok(self.store.page_ops(&self.host, identity)?)
    }

    /// `Engine.shapes(identity)`: shapes DTO; addShape only on a synced note.
    pub fn shapes(&self, identity: &str) -> R<Val> {
        let note = self.get(identity)?;
        let revision = key(&note, "revision")?.clone();
        let row = self.store.raw(identity)?;
        let base = key(&row, "base_document")?;
        if !base.truthy() {
            return Ok(Val::dict(vec![
                ("schemaVersion", Val::Int(1)),
                ("kinds", Val::List(vec![])),
                ("pages", Val::List(vec![])),
                ("reason", s("Notun çizim verisi henüz hazır değil.")),
                ("rendering", Val::dict(vec![("nativeEquivalent", Val::Bool(false)), ("mode", s("geometry-only"))])),
                ("revision", revision),
            ]));
        }
        let mut projection = areas::project_shapes(&loads(base)?)?;
        if key(&note, "trashed")?.truthy()
            || key(&self.store.raw_state(identity)?, "deleted_remote")?.truthy()
            || !is_str(key(&note, "syncState")?, "synced")
            || !is_str(key(key(&note, "metadataSync")?, "state")?, "synced")
        {
            let pages = match key(&projection, "pages")? {
                Val::List(items) => items
                    .iter()
                    .map(|p| {
                        let mut p = p.clone();
                        p.set("addShape", Val::Bool(false));
                        p
                    })
                    .collect(),
                _ => vec![],
            };
            projection.set("pages", Val::List(pages));
        }
        Ok(with_revision(&projection, &revision))
    }

    pub fn save_page_op(&self, identity: &str, op: &Val, spec: &Val, revision: &Val) -> R<Val> {
        self.get(identity)?;
        let result = self.store.save_page_op(&self.host, identity, op, spec, revision)?;
        self.request_sync();
        Ok(result)
    }

    pub fn page_style(&self, identity: &str) -> R<Val> {
        self.get(identity)?;
        Ok(self.store.page_style(&self.host, identity)?)
    }

    pub fn save_page_background(&self, identity: &str, color: &Val, revision: &Val) -> R<Val> {
        self.get(identity)?;
        let result = self.store.save_page_background(&self.host, identity, color, revision)?;
        self.request_sync();
        Ok(result)
    }

    fn pdf_staging(&self) -> R<PathBuf> {
        Ok(rs_store::storage::private_directory(&self.store.directory.join("pdf-uploads"))?)
    }

    /// `Engine.add_pdf(identity, pdf, file_name, revision, page_indices=, template_type=)`
    pub fn add_pdf(&self, identity: &str, pdf: &[u8], file_name: &str, revision: &Val, page_indices: &Val, template_type: &Val) -> R<Val> {
        use rs_engine::engine_core as core;
        self.get(identity)?;
        if !pdf.starts_with(b"%PDF-") || pdf.len() as u64 > MAX_PDF_BYTES {
            return Err(Fail::store("invalid_pdf", "Dosya PDF değil ya da çok büyük.", 400));
        }
        let name = core::pdf_file_name(file_name);
        let sha = core::sha256_hex(pdf);
        let target = self.pdf_staging()?.join(format!("{}.pdf", sha));
        core::stage_file(&target, pdf)?;
        let sizes = match rs_ops::page_template::pdf_page_sizes(&target.to_string_lossy(), Duration::from_secs(20)) {
            Ok(v) => v,
            Err(e) => {
                let f = ops_fail(e);
                if f.is_document() {
                    return Err(Fail::store("invalid_pdf", &f.message, 422));
                }
                return Err(f);
            }
        };
        let page_sizes = Val::List(sizes.iter().map(|(w, h)| Val::List(vec![Val::Float(*w), Val::Float(*h)])).collect());
        let result = self.store.save_pdf_pages(&self.host, identity, revision, &Val::s(sha), &Val::s(name), &page_sizes, page_indices, template_type)?;
        self.request_sync();
        Ok(result)
    }

    /// `Engine.templates(note_id=None)`: per-note scan cached by revision;
    /// with `note_id`, entries get `inNote`.
    pub fn templates(&self, note_id: Option<&str>) -> R<Val> {
        let rows = self.store.with_db(|db| {
            db.query(
                "SELECT id,revision,json_extract(base_document,'$.resource_xml') AS xml FROM notes WHERE base_document IS NOT NULL AND trashed=0 AND deleted_remote=0",
                &[],
            )
        })?;
        let mut found: Vec<Val> = Vec::new();
        let mut files_in_note: Vec<Option<String>> = Vec::new();
        for row in rows.iter() {
            let id = row.val("id");
            let revision = row.val("revision");
            let key_id = id.as_str().unwrap_or("").to_string();
            let cached = {
                let cache = self.template_cache.lock().unwrap();
                cache.get(&key_id).filter(|c| rs_store::val::py_eq(&c.0, &revision)).cloned()
            };
            let cached = match cached {
                Some(c) => c,
                None => {
                    let (entries, ids) = scan_templates(&row.val("xml"))?;
                    let value = (revision.clone(), entries, ids);
                    self.template_cache.lock().unwrap().insert(key_id.clone(), value.clone());
                    value
                }
            };
            if note_id.is_some() && id.as_str() == note_id {
                files_in_note = cached.2.clone();
            }
            for entry in &cached.1 {
                let sha = entry.get_or_null("id");
                if !found.iter().any(|f| rs_store::val::py_eq(&f.get_or_null("id"), &sha)) {
                    let mut e = entry.clone();
                    e.set("sourceNoteId", id.clone());
                    found.push(e);
                }
            }
        }
        // sorted(..., key=lambda t: t['name']) is stable.
        found.sort_by(|a, b| a.get("name").and_then(Val::as_str).unwrap_or("").cmp(b.get("name").and_then(Val::as_str).unwrap_or("")));
        if note_id.is_some() {
            for t in found.iter_mut() {
                let sha = t.get("id").and_then(Val::as_str).map(str::to_string);
                let in_note = files_in_note.iter().any(|f| *f == sha);
                t.set("inNote", Val::Bool(in_note));
            }
        }
        Ok(Val::dict(vec![("templates", Val::List(found))]))
    }

    /// `Engine.apply_template(identity, template_id, revision)`
    pub fn apply_template(&self, identity: &str, template_id: &Val, revision: &Val) -> R<Val> {
        let templates = self.templates(None)?;
        let template = match key(&templates, "templates")? {
            Val::List(items) => items.iter().find(|t| t.get("id").map(|v| rs_store::val::py_eq(v, template_id)).unwrap_or(false)).cloned(),
            _ => None,
        };
        let Some(template) = template else { return Err(Fail::store("template_not_found", "Şablon bulunamadı.", 404)) };
        let source = self.store.raw(key_str(&template, "sourceNoteId")?)?;
        let document = loads(key(&source, "base_document")?)?;
        let sha = key_str(&template, "id")?.to_string();
        let account = match meta_or(&self.store, "account_key", Val::s("offline"))? {
            Val::Str(s) => s,
            other => return Err(Fail::type_(format!("unsupported account key {}", other.type_name()))),
        };
        let mut path = self.previews.template_pdf(&document, &sha, &account, false)?;
        if path.is_none() {
            if self.sessions.path().is_none() || self.sessions.offline() {
                return Err(Fail::store("template_offline", "Şablon dosyası henüz indirilmemiş; bağlantı gerekli.", 503));
            }
            self.api()?;
            path = self.previews.template_pdf(&document, &sha, &account, true)?;
        }
        let path = path.ok_or_else(|| Fail::store("template_unavailable", "Şablon dosyası bu sürümde okunamadı.", 422))?;
        let raw = std::fs::read(&path)?;
        self.add_pdf(identity, &raw, key_str(&template, "fileName")?, revision, key(&template, "pageIndices")?, key(&template, "templateType")?)
    }

    /// `Engine.editor(identity)`
    pub fn editor(&self, identity: &str) -> R<Val> {
        let note = self.get(identity)?;
        let revision = key(&note, "revision")?.clone();
        let mut projection = self.store.cached_editor(identity, Some(&revision))?;
        if projection.is_none() {
            let row = self.store.raw(identity)?;
            let state = key(&row, "sync_state")?;
            let dirty = !(is_str(state, "synced") || is_str(state, "local"));
            let prior = if dirty { self.store.cached_editor(identity, None)? } else { None };
            let reuse = match &prior {
                Some(p) if p.truthy() => {
                    key(key(p, "capabilities")?, "textEdit")?.truthy()
                        && p.get("model").map(|m| !m.is_null()).unwrap_or(false)
                        && rs_store::val::py_eq(key(key(p, "model")?, "text")?, key(&row, "body")?)
                }
                _ => false,
            };
            let value = if reuse {
                // Organization-only revision changes must not discard a
                // newer local rich model while an older job is in flight.
                prior.unwrap()
            } else if key(&row, "editable")?.truthy() && (dirty || !key(&row, "base_document")?.truthy()) {
                // A plain PUT may have changed this body after the last
                // projection. Never bind the old Cloud body to its new revision.
                let model = areas::normalize_model(&Val::dict(vec![
                    ("schemaVersion", Val::Int(1)),
                    ("text", key(&note, "body")?.clone()),
                    ("spans", Val::List(vec![])),
                    ("paragraphs", Val::List(vec![])),
                ]))?;
                Val::dict(vec![
                    ("schemaVersion", Val::Int(1)),
                    (
                        "capabilities",
                        Val::dict(vec![
                            ("titleEdit", Val::Bool(true)),
                            ("textEdit", Val::Bool(true)),
                            ("richStyle", Val::Bool(true)),
                            ("inkEdit", Val::Bool(false)),
                        ]),
                    ),
                    ("reason", Val::Null),
                    ("model", model),
                ])
            } else if key(&row, "base_document")?.truthy() {
                let mut projection = areas::editor_projection(&snapshot::from_dict(&loads(key(&row, "base_document")?)?)?)?;
                if dirty && key(key(&projection, "capabilities")?, "textEdit")?.truthy() {
                    let mut caps = key(&projection, "capabilities")?.clone();
                    caps.set("textEdit", Val::Bool(false));
                    caps.set("richStyle", Val::Bool(false));
                    projection.set("capabilities", caps);
                    projection.set("reason", s("Yerel düzenlemenin biçim bilgisi eksik; bulut metni düzenlemenin yerine kullanılmadı."));
                }
                projection
            } else {
                Val::dict(vec![
                    ("schemaVersion", Val::Int(1)),
                    (
                        "capabilities",
                        Val::dict(vec![
                            ("titleEdit", Val::Bool(false)),
                            ("textEdit", Val::Bool(false)),
                            ("richStyle", Val::Bool(false)),
                            ("inkEdit", Val::Bool(false)),
                        ]),
                    ),
                    ("model", Val::Null),
                    ("reason", s("Not içeriği henüz hazır değil.")),
                ])
            };
            self.store.cache_editor(identity, &revision, &value)?;
            projection = Some(value);
        }
        let mut projection = projection.unwrap();
        if key(&note, "trashed")?.truthy() || key(&self.store.raw_state(identity)?, "deleted_remote")?.truthy() {
            projection = all_capabilities_false(&projection)?;
        }
        Ok(with_revision(&projection, &revision))
    }

    pub fn create_editor(&self, title: &Val, model: &Val) -> R<Val> {
        // A new note has no object a text anchor (objectSpan) could name; the
        // upload would be refused later, so refuse the model up front.
        if areas::normalize_model(model)?.get("objects").is_some() {
            return Err(Fail::store("anchored_object", "Metnine resim yerleşmiş bir not yeni not olarak oluşturulamaz.", 422));
        }
        let result = self.store.create_editor(&self.host, title, model)?;
        self.request_sync();
        Ok(result)
    }

    pub fn save_editor(&self, identity: &str, title: &Val, model: &Val, revision: &Val) -> R<Val> {
        self.editor(identity)?;
        let result = self.store.save_editor(&self.host, identity, title, model, revision)?;
        self.request_sync();
        Ok(result)
    }

    /// `Engine.ink(identity)`
    pub fn ink(&self, identity: &str) -> R<Val> {
        let note = self.get(identity)?;
        let revision = key(&note, "revision")?.clone();
        let mut projection = self.store.cached_ink(identity, &revision)?;
        if let Some(p) = &projection {
            let recipe_ok = p.get("recipe").map(|r| is_str(r, areas::PROJECTION_RECIPE)).unwrap_or(false);
            let pending = p.get("pendingStrokes").map(Val::truthy).unwrap_or(false);
            if !recipe_ok && !pending {
                projection = None; // computed under older gate rules
            }
        }
        if projection.is_none() {
            let row = self.store.raw(identity)?;
            let base = key(&row, "base_document")?;
            let mut value = if base.truthy() {
                areas::project_ink(&loads(base)?, false)?
            } else {
                Val::dict(vec![
                    ("schemaVersion", Val::Int(1)),
                    ("pages", Val::List(vec![])),
                    ("capabilities", Val::dict(vec![("appendInk", Val::Bool(false))])),
                    ("reason", s("Notun çizim verisi henüz hazır değil.")),
                    ("rendering", Val::dict(vec![("nativeEquivalent", Val::Bool(false)), ("mode", s("geometry-only"))])),
                ])
            };
            let (pending, erase_job) = self.store.with_db(|db| {
                let pending = db.one(
                    "SELECT ink_jobs.model FROM ink_jobs JOIN jobs ON jobs.id=ink_jobs.job_id\n                    WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') ORDER BY jobs.created_ms DESC LIMIT 1",
                    &rs_store::p![identity],
                )?;
                let erase = db.one(
                    "SELECT ink_erase_jobs.model FROM ink_erase_jobs JOIN jobs ON jobs.id=ink_erase_jobs.job_id\n                    WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') ORDER BY jobs.created_ms DESC LIMIT 1",
                    &rs_store::p![identity],
                )?;
                Ok((pending, erase))
            })?;
            let pending_strokes = match pending {
                Some(row) => key(&loads(&rs_store::db::sql_to_val(&row[0]))?, "strokes")?.clone(),
                None => Val::List(vec![]),
            };
            let pending_erase = match erase_job {
                Some(row) => {
                    let model = loads(&rs_store::db::sql_to_val(&row[0]))?;
                    let groups = match key(&model, "groups")? {
                        Val::List(g) => g.clone(),
                        other => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
                    };
                    let mut out = Vec::new();
                    for g in &groups {
                        out.push(Val::dict(vec![("pageId", key(g, "pageId")?.clone()), ("strokeIds", key(g, "strokeIds")?.clone())]));
                    }
                    Val::List(out)
                }
                None => Val::List(vec![]),
            };
            value.set("pendingStrokes", pending_strokes);
            value.set("pendingErase", pending_erase);
            let move_job = self.store.with_db(|db| {
                db.one(
                    "SELECT ink_move_jobs.model FROM ink_move_jobs JOIN jobs ON jobs.id=ink_move_jobs.job_id\n                    WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') ORDER BY jobs.created_ms DESC LIMIT 1",
                    &rs_store::p![identity],
                )
            })?;
            if let Some(row) = move_job {
                // key only present while a move waits for its Cloud write
                let model = loads(&rs_store::db::sql_to_val(&row[0]))?;
                let groups = match key(&model, "groups")? {
                    Val::List(g) => g.clone(),
                    other => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
                };
                let mut out = Vec::new();
                for g in &groups {
                    out.push(Val::dict(vec![
                        ("pageId", key(g, "pageId")?.clone()),
                        ("strokeIds", key(g, "strokeIds")?.clone()),
                        ("dx", key(g, "dx")?.clone()),
                        ("dy", key(g, "dy")?.clone()),
                    ]));
                }
                value.set("pendingMove", Val::List(out));
            }
            value.set("recipe", s(areas::PROJECTION_RECIPE));
            self.store.cache_ink(identity, &revision, &value)?;
            projection = Some(value);
        }
        let mut projection = projection.unwrap();
        if key(&note, "trashed")?.truthy()
            || key(&self.store.raw_state(identity)?, "deleted_remote")?.truthy()
            || !is_str(key(&note, "syncState")?, "synced")
            || !is_str(key(key(&note, "metadataSync")?, "state")?, "synced")
        {
            projection = all_capabilities_false(&projection)?;
        }
        Ok(with_revision(&projection, &revision))
    }

    pub fn ink_draft(&self, identity: &str) -> R<Val> {
        Ok(self.store.ink_draft(identity)?)
    }

    pub fn save_ink_draft(&self, identity: &str, revision: &Val, strokes: &Val) -> R<Val> {
        Ok(self.store.save_ink_draft(identity, revision, strokes)?)
    }

    pub fn append_ink_draft(&self, identity: &str, revision: &Val, strokes: &Val) -> R<Val> {
        Ok(self.store.append_ink_draft(identity, revision, strokes)?)
    }

    pub fn save_ink(&self, identity: &str, strokes: &Val, revision: &Val) -> R<Val> {
        self.ink(identity)?;
        let result = self.store.save_ink(&self.host, identity, strokes, revision)?;
        self.request_sync();
        Ok(result)
    }

    pub fn save_ink_from_draft(&self, identity: &str, strokes: &Val, revision: &Val, draft_ids: &Val) -> R<Val> {
        self.ink(identity)?;
        let result = self.store.save_ink_from_draft(&self.host, identity, strokes, revision, draft_ids)?;
        self.request_sync();
        Ok(result)
    }

    pub fn save_ink_move(&self, identity: &str, moves: &Val, revision: &Val) -> R<Val> {
        self.ink(identity)?;
        let result = self.store.save_ink_move(&self.host, identity, moves, revision)?;
        self.request_sync();
        Ok(result)
    }

    pub fn save_ink_erase(&self, identity: &str, erase: &Val, revision: &Val) -> R<Val> {
        self.ink(identity)?;
        let result = self.store.save_ink_erase(&self.host, identity, erase, revision)?;
        self.request_sync();
        Ok(result)
    }

    pub fn ink_geometry(&self, identity: &str, page_id: &str, revision: &Val) -> R<Val> {
        if !rs_store::val::py_eq(key(&self.get(identity)?, "revision")?, revision) {
            return Err(Fail::store("revision_conflict", "Not değişti.", 409));
        }
        let row = self.store.raw(identity)?;
        let base = key(&row, "base_document")?;
        if !base.truthy() {
            return Err(Fail::store("not_ready", "Notun çizim verisi henüz hazır değil.", 409));
        }
        areas::project_page_geometry(&loads(base)?, page_id)
    }

    /// Saved pens of the note's template strokes for the live pen preview
    /// (`GET /api/notes/:id/ink-pens`). A note the page renderer cannot
    /// prepare has no pens; the UI then keeps its simple preview.
    pub fn ink_pens(&self, identity: &str, revision: &Val) -> R<Val> {
        if !rs_store::val::py_eq(key(&self.get(identity)?, "revision")?, revision) {
            return Err(Fail::store("revision_conflict", "Not değişti.", 409));
        }
        let row = self.store.raw(identity)?;
        let base = key(&row, "base_document")?;
        let pens = if base.truthy() {
            notes_preview::ink::prepare::pen_templates(&loads(base)?).unwrap_or_else(|_| "{}".into())
        } else {
            "{}".into()
        };
        Ok(Val::dict(vec![("schemaVersion", Val::Int(1)), ("revision", revision.clone()), ("pens", loads(&Val::Str(pens))?)]))
    }

    /// `Engine.resolve(identity, strategy)`
    pub fn resolve(&self, identity: &str, strategy: &Val) -> R<Val> {
        let row = self.store.raw(identity)?;
        let org = key(&row, "org_state")?;
        if is_str(org, "conflict") || is_str(org, "uncertain") || is_str(org, "error") {
            if !is_str(strategy, "use_remote") {
                return Err(Fail::store("invalid_strategy", "Bu işlem için buluttaki durumu yeniden al.", 409));
            }
            let api = self.api()?;
            let metadata = self.apis.metadata(&api).ok_or_else(|| Fail::type_("'NoneType' object is not callable"))?;
            let inventory = metadata.inventory()?;
            let server_id = key(&row, "server_id")?.clone();
            let snapshot = api.get_note(&server_id)?;
            self.store.apply_folder_graph(&inventory)?;
            self.store.apply_document(&self.host, &server_id, &snapshot::to_dict(&snapshot), None)?;
            return Ok(self.store.accept_remote_organization(identity)?);
        }
        // Refresh remote before a resolution, never silently discard the local
        // draft based on a stale cached version.
        if key(&row, "server_id")?.truthy() && !key(&row, "deleted_remote")?.truthy() {
            let server_id = key(&row, "server_id")?.clone();
            let refreshed = self.api().and_then(|api| api.get_note(&server_id)).and_then(|snapshot| {
                self.store.apply_document(&self.host, &server_id, &snapshot::to_dict(&snapshot), None).map_err(Fail::from)
            });
            if let Err(e) = refreshed {
                if e.is_api() || e.is_store() {
                    if is_str(strategy, "use_remote") {
                        return Err(Fail::store("remote_unavailable", "Güncel bulut sürümü alınamadı; yerel metnin korunuyor.", 409));
                    }
                } else {
                    return Err(e);
                }
            }
        }
        let result = self.store.resolve(&self.host, identity, strategy)?;
        self.request_sync();
        Ok(result)
    }

    // ------------------------------------------------------------ outbox

    fn guard_ink_job(&self, job: &Val) -> R<()> {
        Ok(self.store.with_db(|db| rs_store::ink_store::guard_ink_job_tx(db, job))?)
    }

    fn operation_id(job: &Val, name: &str) -> R<String> {
        let id = key_str(job, "id")?;
        rs_engine::engine_core::uuid5(id, name).map_err(Fail::value)
    }

    /// `Engine._push_ink_erase(api, row, job)`
    fn push_ink_erase(&self, api: &Arc<dyn Cloud>, row: &Val, job: &Val) -> R<Val> {
        let (model, cursor, mut snapshot) = self.store.ink_erase_job(job)?;
        let groups = match key(&model, "groups")? {
            Val::List(g) => g.clone(),
            other => return Err(Fail::type_(format!("object of type '{}' has no len()", other.type_name()))),
        };
        let server_id = key(row, "server_id")?.clone();
        for index in (cursor.max(0) as usize)..groups.len() {
            if self.interrupted() {
                return Err(Fail::api("cloud_unavailable", "Silme kaydedildi; eşitlemeye daha sonra devam edilecek."));
            }
            let g = &groups[index];
            let operation_id = Self::operation_id(job, &format!("erase-{}", index))?;
            let result = api.erase_ink_strokes(
                &server_id,
                &snapshot::from_dict(&snapshot)?,
                key(g, "pageId")?,
                key(g, "layerId")?,
                key(g, "strokeIds")?,
                &operation_id,
            )?;
            if !matches!(result.get("verified"), Some(Val::Bool(true))) {
                return Err(Fail::ambiguous("write_uncertain", "Silmenin bulut kaydı henüz doğrulanamadı."));
            }
            snapshot = key(&result, "snapshot")?.clone();
            self.store.checkpoint_ink_erase(job, &Val::Int(index as i64), &snapshot)?;
        }
        Ok(Val::dict(vec![
            ("verified", Val::Bool(true)),
            ("server_id", server_id),
            ("local_id", key(&snapshot, "local_id")?.clone()),
            ("snapshot", snapshot),
        ]))
    }

    /// `Engine._push_ink_move(api, row, job)`
    fn push_ink_move(&self, api: &Arc<dyn Cloud>, row: &Val, job: &Val) -> R<Val> {
        let (model, cursor, mut snapshot) = self.store.ink_move_job(job)?;
        let groups = match key(&model, "groups")? {
            Val::List(g) => g.clone(),
            other => return Err(Fail::type_(format!("object of type '{}' has no len()", other.type_name()))),
        };
        let server_id = key(row, "server_id")?.clone();
        for index in (cursor.max(0) as usize)..groups.len() {
            if self.interrupted() {
                return Err(Fail::api("cloud_unavailable", "Taşıma kaydedildi; eşitlemeye daha sonra devam edilecek."));
            }
            let g = &groups[index];
            let operation_id = Self::operation_id(job, &format!("move-{}", index))?;
            let result = api.move_ink_strokes(
                &server_id,
                &snapshot::from_dict(&snapshot)?,
                key(g, "pageId")?,
                key(g, "layerId")?,
                key(g, "strokeIds")?,
                key(g, "dx")?,
                key(g, "dy")?,
                &operation_id,
            )?;
            if !matches!(result.get("verified"), Some(Val::Bool(true))) {
                return Err(Fail::ambiguous("write_uncertain", "Taşımanın bulut kaydı henüz doğrulanamadı."));
            }
            snapshot = key(&result, "snapshot")?.clone();
            self.store.checkpoint_ink_move(job, &Val::Int(index as i64), &snapshot)?;
        }
        Ok(Val::dict(vec![
            ("verified", Val::Bool(true)),
            ("server_id", server_id),
            ("local_id", key(&snapshot, "local_id")?.clone()),
            ("snapshot", snapshot),
        ]))
    }

    /// `Engine._push_ink_batch(api, row, job)`
    fn push_ink_batch(&self, api: &Arc<dyn Cloud>, row: &Val, job: &Val) -> R<Val> {
        let (model, cursor, mut snapshot) = self.store.ink_job(job)?;
        self.guard_ink_job(job)?;
        let strokes = match key(&model, "strokes")? {
            Val::List(g) => g.clone(),
            other => return Err(Fail::type_(format!("object of type '{}' has no len()", other.type_name()))),
        };
        let server_id = key(row, "server_id")?.clone();
        for index in (cursor.max(0) as usize)..strokes.len() {
            self.guard_ink_job(job)?;
            if self.interrupted() {
                return Err(Fail::api("cloud_unavailable", "Çizim kaydedildi; eşitlemeye daha sonra devam edilecek."));
            }
            let stroke = &strokes[index];
            let recipe = model.get("recipeVersion").cloned().unwrap_or(Val::Int(1));
            let operation_id = Self::operation_id(job, &index.to_string())?;
            let current = snapshot::from_dict(&snapshot)?;
            let result = if rs_store::val::py_eq(&recipe, &Val::Int(4)) || rs_store::val::py_eq(&recipe, &Val::Int(5)) {
                // Recipe 4: first stroke on a stroke-free page; recipe 5:
                // highlighter stroke. Same scratch-stroke transaction.
                let (page_id, points, color, size, id) =
                    (key(stroke, "pageId")?, key(stroke, "points")?, key(stroke, "colorARGB")?, key(stroke, "size")?, key(stroke, "id")?);
                if rs_store::val::py_eq(&recipe, &Val::Int(4)) {
                    api.append_first_stroke(&server_id, &current, page_id, points, color, size, id, &operation_id)?
                } else {
                    api.append_highlighter_stroke(&server_id, &current, page_id, points, color, size, id, &operation_id)?
                }
            } else {
                let recipe_n = if rs_store::val::py_eq(&recipe, &Val::Int(2)) {
                    2
                } else if rs_store::val::py_eq(&recipe, &Val::Int(3)) {
                    3
                } else {
                    1
                };
                let style = if recipe_n == 3 { Some((key(stroke, "colorARGB")?, key(stroke, "size")?)) } else { None };
                api.append_ink_stroke(
                    recipe_n,
                    &server_id,
                    &current,
                    key(stroke, "sourceStrokeId")?,
                    key(stroke, "points")?,
                    key(stroke, "id")?,
                    &operation_id,
                    style,
                )?
            };
            if !matches!(result.get("verified"), Some(Val::Bool(true))) {
                return Err(Fail::ambiguous("write_uncertain", "Çizimin bulut kaydı henüz doğrulanamadı."));
            }
            snapshot = key(&result, "snapshot")?.clone();
            self.store.checkpoint_ink(job, &Val::Int(index as i64), &snapshot)?;
            self.guard_ink_job(job)?;
        }
        Ok(Val::dict(vec![
            ("verified", Val::Bool(true)),
            ("server_id", server_id),
            ("local_id", key(&snapshot, "local_id")?.clone()),
            ("snapshot", snapshot),
        ]))
    }

    fn publish_job(&self, api: &Arc<dyn Cloud>, row: &Val, job: &Val) -> R<Val> {
        let kind = key(job, "kind")?.clone();
        if is_str(&kind, "append_ink") {
            return self.push_ink_batch(api, row, job);
        }
        if is_str(&kind, "erase_ink") {
            return self.push_ink_erase(api, row, job);
        }
        if is_str(&kind, "move_ink") {
            return self.push_ink_move(api, row, job);
        }
        if is_str(&kind, "create_editor") {
            let model = self.store.editor_job_model(job)?;
            return api.create_editor(key(job, "title")?, &model, key(job, "id")?, key(row, "resource_id")?);
        }
        if is_str(&kind, "create") {
            return api.create(key(job, "title")?, key(job, "body")?, key(job, "id")?, key(row, "resource_id")?);
        }
        if !key(job, "snapshot")?.truthy() {
            return Err(Fail::api("missing_base", "Düzenlemenin temel sürümü eksik; yerel metin korunuyor."));
        }
        let snapshot = snapshot::from_dict(&loads(key(job, "snapshot")?)?)?;
        let server_id = key(row, "server_id")?;
        let operation_id = key(job, "id")?;
        if is_str(&kind, "title") {
            api.update_title(server_id, &snapshot, key(job, "title")?, operation_id)
        } else if is_str(&kind, "pdf_pages") {
            let spec = self.store.pdf_page_job(job)?;
            let pdf = std::fs::read(self.pdf_staging()?.join(format!("{}.pdf", key_str(&spec, "file_sha256")?)))?;
            api.add_pdf_pages(
                server_id,
                &snapshot,
                &pdf,
                key(&spec, "file_name")?,
                key(&spec, "page_sizes")?,
                key(&spec, "page_indices")?,
                key(&spec, "template_type")?,
                operation_id,
            )
        } else if is_str(&kind, "page_op") {
            let (op, spec) = self.store.page_op_job(job)?;
            if is_str(&op, "shape") {
                api.add_shape(server_id, &snapshot, key(&spec, "pageId")?, key(&spec, "shape")?, operation_id)
            } else if is_str(&op, "add") {
                api.page_op("add", server_id, &snapshot, key(&spec, "afterIndex")?, operation_id)
            } else if is_str(&op, "delete") {
                api.page_op("delete", server_id, &snapshot, key(&spec, "pageIds")?, operation_id)
            } else {
                api.page_op("reorder", server_id, &snapshot, key(&spec, "pageIds")?, operation_id)
            }
        } else if is_str(&kind, "page_style") {
            let color = self.store.page_style_job_color(job)?;
            api.update_page_background(server_id, &snapshot, &color, operation_id)
        } else if is_str(&kind, "update_editor") {
            let model = self.store.editor_job_model(job)?;
            api.update_editor(server_id, &snapshot, key(job, "title")?, &model, operation_id)
        } else {
            api.update(server_id, &snapshot, key(job, "title")?, key(job, "body")?, operation_id)
        }
    }

    /// `Engine._push(api)`
    #[doc(hidden)]
    pub fn push(&self, api: &Arc<dyn Cloud>) -> R<()> {
        for _ in 0..100 {
            if self.interrupted() {
                return Ok(());
            }
            let Some(job) = self.store.next_job()? else { return Ok(()) };
            let row = self.store.raw(key_str(&job, "note_id")?)?;
            if !self.store.job_state(&job, &s("running"), &Val::Null)? {
                // Cancelled or resolved after selection; do not publish it.
                continue;
            }
            let outcome = self.publish_job(api, &row, &job).and_then(|result| {
                if !matches!(result.get("verified"), Some(Val::Bool(true))) {
                    return Err(Fail::ambiguous("write_uncertain", "Bulut kaydı henüz doğrulanamadı."));
                }
                self.store.complete_job(&self.host, &job, &result)?;
                if self.folder_initialized()? {
                    self.store.register_note_graph(key(&job, "note_id")?)?;
                }
                Ok(())
            });
            if let Err(error) = outcome {
                if error.is_conflict() {
                    self.store.job_state(&job, &s("conflict"), &Val::s(error.message.clone()))?;
                    let server_id = key(&row, "server_id")?.clone();
                    match api.get_note(&server_id) {
                        Ok(fresh) => {
                            self.store.apply_document(&self.host, &server_id, &snapshot::to_dict(&fresh), None)?;
                        }
                        Err(e) if e.is_api() => {}
                        Err(e) => return Err(e),
                    }
                } else if error.is_api() {
                    if matches!(error.code.as_str(), "cloud_unavailable" | "reauth_required" | "renew_failed") && !error.ambiguous {
                        // API immutable operation IDs guard the later continuation.
                        self.store.job_state(&job, &s("queued"), &Val::Null)?;
                        return Err(error);
                    }
                    let state = if error.ambiguous { "uncertain" } else { "failed" };
                    self.store.job_state(&job, &s(state), &Val::s(error.message.clone()))?;
                } else if error.is_document() || error.is_store() {
                    if error.is_store() && error.code == "ink_job_stopped" {
                        // A concurrent conflict/resolution owns the job state. Do
                        // not turn cancelled/conflict back into failed or done.
                        self.store.suspend_ink_job(&job)?;
                        continue;
                    }
                    self.store.job_state(&job, &s("failed"), &Val::s(str_of(&error)))?;
                } else if error.is_os_value_key_type() {
                    self.store.job_state(
                        &job,
                        &s("uncertain"),
                        &s("Gönderim sonucu bilinmiyor. Otomatik tekrar durduruldu; yerel metin korunuyor."),
                    )?;
                } else {
                    return Err(error);
                }
            }
            self.bump_processed();
        }
        Ok(())
    }

    fn prefetch_previews(&self) {
        let _ = self.previews.prefetch();
    }

    /// `Engine._fetch_many(api, server_ids)`: results keep input order;
    /// `None` = interrupted, `Some(Err(api error))` recorded per note.
    fn fetch_many(self: &Arc<Self>, api: &Arc<dyn Cloud>, server_ids: &[Val]) -> Vec<Option<R<Snapshot>>> {
        let fetch = |engine: &Engine, api: &Arc<dyn Cloud>, server_id: &Val| -> Option<R<Snapshot>> {
            if engine.interrupted() {
                return None;
            }
            Some(api.get_note(server_id))
        };
        if server_ids.len() <= 1 {
            return server_ids.iter().map(|id| fetch(self, api, id)).collect();
        }
        let workers = DOWNLOAD_WORKERS.min(server_ids.len());
        let next = Arc::new(Mutex::new(0usize));
        let results: Arc<Mutex<Vec<Option<Option<R<Snapshot>>>>>> = Arc::new(Mutex::new((0..server_ids.len()).map(|_| None).collect()));
        let ids: Arc<Vec<Val>> = Arc::new(server_ids.to_vec());
        let mut handles = Vec::new();
        for n in 0..workers {
            let (engine, api, next, results, ids) = (Arc::clone(self), Arc::clone(api), Arc::clone(&next), Arc::clone(&results), Arc::clone(&ids));
            handles.push(
                std::thread::Builder::new()
                    .name(format!("notes-download_{}", n))
                    .spawn(move || loop {
                        let index = {
                            let mut guard = next.lock().unwrap();
                            let i = *guard;
                            *guard += 1;
                            i
                        };
                        if index >= ids.len() {
                            break;
                        }
                        let out = fetch(&engine, &api, &ids[index]);
                        results.lock().unwrap()[index] = Some(out);
                    })
                    .expect("spawn download worker"),
            );
        }
        for h in handles {
            let _ = h.join();
        }
        let mut guard = results.lock().unwrap();
        guard.drain(..).map(|r| r.unwrap_or(None)).collect()
    }

    /// `Engine._apply_fetched(identity, server_id, result)`; non-API errors
    /// from a fetch propagate like ThreadPoolExecutor.map re-raising them.
    fn apply_fetched(&self, identity: &Val, server_id: &Val, result: Option<R<Snapshot>>) -> R<bool> {
        let Some(result) = result else { return Ok(false) };
        match result {
            Err(error) if error.is_api() => {
                let id = identity.as_str().ok_or_else(|| Fail::type_("note id"))?;
                self.store.content_error(id, &Val::s(error.message.clone()))?;
                if error.code == "reauth_required" || error.code == "cloud_unavailable" {
                    return Err(error);
                }
                Ok(true)
            }
            Err(error) => Err(error),
            Ok(snapshot) => {
                self.store.apply_document(&self.host, server_id, &snapshot::to_dict(&snapshot), None)?;
                Ok(true)
            }
        }
    }

    /// `Engine._load_missing(api, limit=20)`
    #[doc(hidden)]
    pub fn load_missing(self: &Arc<Self>, api: &Arc<dyn Cloud>) -> R<()> {
        let rows = self.store.with_db(|db| db.query("SELECT id,server_id FROM notes WHERE loaded=0 AND deleted_remote=0 LIMIT ?", &rs_store::p![20i64]))?;
        let pairs: Vec<(Val, Val)> = rows.iter().map(|r| (r.val("id"), r.val("server_id"))).collect();
        let ids: Vec<Val> = pairs.iter().map(|p| p.1.clone()).collect();
        let results = self.fetch_many(api, &ids);
        first_error(&results)?;
        for ((identity, server_id), result) in pairs.into_iter().zip(results) {
            if !self.apply_fetched(&identity, &server_id, result)? {
                return Ok(());
            }
        }
        Ok(())
    }

    /// `Engine._pull_folders(metadata)`
    #[doc(hidden)]
    pub fn pull_folders(&self, metadata: &Arc<dyn Metadata>) -> R<()> {
        let inventory = metadata.folder_changes(&meta_or(&self.store, "folder_cursor", Val::Null)?)?;
        if self.interrupted() {
            return Ok(());
        }
        self.store.apply_folder_graph(&inventory)?;
        if !meta_or(&self.store, "note_inventory_imported", Val::Bool(false))?.truthy() {
            let records = metadata.note_inventory()?;
            if self.interrupted() {
                return Ok(());
            }
            if let Val::List(notes) = key(&records, "notes")? {
                for record in notes {
                    // Tombstones cannot supply missing contents. Existing dirty
                    // documents are protected by the same deletion conflict path.
                    self.store.stage_change(record)?;
                }
            }
            self.store.set_meta("note_inventory_imported", &Val::Bool(true))?;
            // Newly staged notes now receive the graph's placement and trash.
            if inventory.get("has_resource").map(Val::truthy).unwrap_or(false) {
                self.store.apply_folder_graph(&inventory)?;
            }
        }
        // Upgrade only proven local creates missing a graph node. Old local
        // trash/favorite preferences are never transformed into remote writes.
        let created = self.store.with_db(|db| db.query("SELECT DISTINCT note_id FROM jobs WHERE kind IN ('create','create_editor') AND state='done'", &[]))?;
        for row in created.iter() {
            self.store.register_note_graph(&row.val("note_id"))?;
        }
        Ok(())
    }

    fn requeue_or_record<F: Fn(&str, &Val) -> R<()>>(error: Fail, set_state: F, unknown: &str) -> R<()> {
        if error.is_conflict() {
            set_state("conflict", &Val::s(error.message.clone()))
        } else if error.is_api() {
            if matches!(error.code.as_str(), "cloud_unavailable" | "reauth_required" | "renew_failed") && !error.ambiguous {
                set_state("queued", &Val::Null)?;
                return Err(error);
            }
            set_state(if error.ambiguous { "uncertain" } else { "failed" }, &Val::s(error.message.clone()))
        } else if error.is_os_value_key_type() {
            set_state("uncertain", &s(unknown))
        } else {
            Err(error)
        }
    }

    /// `Engine._push_organization(api, metadata)`
    #[doc(hidden)]
    pub fn push_organization(&self, metadata: &Arc<dyn Metadata>) -> R<()> {
        for _ in 0..100 {
            if self.interrupted() {
                return Ok(());
            }
            let Some(job) = self.store.next_organization_job()? else { return Ok(()) };
            self.store.organization_state(&job, &s("running"), &Val::Null)?;
            let row = self.store.raw(key_str(&job, "note_id")?)?;
            let attempt = (|| -> R<()> {
                let snapshot = snapshot::from_dict(&loads(key(&job, "snapshot")?)?)?;
                let baseline = if key(&job, "baseline_node")?.truthy() { loads(key(&job, "baseline_node")?)? } else { Val::Null };
                let result =
                    metadata.update_organization(key(&row, "server_id")?, &snapshot, &loads(key(&job, "changes")?)?, key(&job, "id")?, &baseline)?;
                if !matches!(result.get("verified"), Some(Val::Bool(true))) {
                    return Err(Fail::ambiguous("write_uncertain", "Bulut işleminin sonucu doğrulanamadı."));
                }
                self.store.apply_document(&self.host, key(&row, "server_id")?, key(&result, "snapshot")?, None)?;
                if result.get("node").map(Val::truthy).unwrap_or(false) {
                    self.store.apply_organization_node(key(&result, "node")?)?;
                }
                self.store.organization_completed(&job)?;
                Ok(())
            })();
            if let Err(error) = attempt {
                Self::requeue_or_record(
                    error,
                    |state, message| Ok(self.store.organization_state(&job, &s(state), message)?),
                    "Bulut işleminin sonucu bilinmiyor. Otomatik tekrar durduruldu.",
                )?;
            }
        }
        Ok(())
    }

    /// `Engine._push_folders(metadata)`
    #[doc(hidden)]
    pub fn push_folders(&self, metadata: &Arc<dyn Metadata>) -> R<()> {
        for _ in 0..100 {
            if self.interrupted() {
                return Ok(());
            }
            let Some(job) = self.store.next_folder_job()? else { return Ok(()) };
            self.store.folder_job_state(&job, &s("running"), &Val::Null)?;
            let attempt = (|| -> R<()> {
                let changes = loads(key(&job, "changes")?)?;
                let result = if is_str(key(&job, "kind")?, "create") {
                    metadata.create_folder(key(&changes, "name")?, key(&changes, "parent_id")?, key(&job, "id")?, key(&job, "folder_id")?)?
                } else {
                    metadata.update_folder(key(&job, "folder_id")?, &changes, &loads(key(&job, "baseline_node")?)?, key(&job, "id")?)?
                };
                if !matches!(result.get("verified"), Some(Val::Bool(true))) {
                    return Err(Fail::ambiguous("write_uncertain", "Klasör işleminin sonucu doğrulanamadı."));
                }
                self.store.folder_completed(&job, key(&result, "node")?)?;
                Ok(())
            })();
            if let Err(error) = attempt {
                Self::requeue_or_record(
                    error,
                    |state, message| Ok(self.store.folder_job_state(&job, &s(state), message)?),
                    "Klasör işleminin sonucu bilinmiyor. Yeniden gönderilmedi.",
                )?;
            }
        }
        Ok(())
    }

    /// `Engine._pull(api)`
    #[doc(hidden)]
    pub fn pull(self: &Arc<Self>, api: &Arc<dyn Cloud>) -> R<bool> {
        let stored = meta_or(&self.store, "cursor", Val::Null)?;
        let mut cursor = if stored.truthy() { stored } else { api.initial_cursor() };
        let mut seen: Vec<Val> = Vec::new();
        for _ in 0..500 {
            if self.interrupted() {
                return Ok(false);
            }
            if seen.iter().any(|c| rs_store::val::py_eq(c, &cursor)) {
                return Err(Fail::api("cursor_cycle", "Bulut değişiklik listesi aynı konumu tekrarladı; ilerleme kaydedildi."));
            }
            seen.push(cursor.clone());
            let page = api.changes(&cursor)?;
            let mut downloads: Vec<(Val, Val)> = Vec::new();
            let changes = match key(&page, "changes")? {
                Val::List(c) => c.clone(),
                other => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
            };
            for change in &changes {
                if self.interrupted() {
                    return Ok(false);
                }
                let server_id = key(change, "server_id")?;
                let old = self.store.by_server(server_id)?;
                let identity = self.store.stage_change(change)?;
                if let Some(identity) = identity.filter(|i| !i.is_empty()) {
                    if !matches!(change.get("is_deleted"), Some(Val::Bool(true))) {
                        let old_commit = match &old {
                            Some(o) if key(o, "base_document")?.truthy() => loads(key(o, "base_document")?)?.get_or_null("commit_id"),
                            _ => Val::Null,
                        };
                        let loaded = old.as_ref().map(|o| key(o, "loaded").map(Val::truthy)).transpose()?.unwrap_or(false);
                        if old.is_none()
                            || !loaded
                            || rs_store::storage::normalize_commit_val(&old_commit)? != rs_store::storage::normalize_commit_val(&change.get_or_null("commit_id"))?
                        {
                            downloads.push((Val::s(identity), server_id.clone()));
                            continue;
                        }
                    }
                }
                self.bump_processed();
            }
            // Changed documents of this page download in parallel; the cursor
            // only advances after every one of them is applied or recorded.
            let ids: Vec<Val> = downloads.iter().map(|d| d.1.clone()).collect();
            let results = self.fetch_many(api, &ids);
            first_error(&results)?;
            for ((identity, server_id), result) in downloads.into_iter().zip(results) {
                if !self.apply_fetched(&identity, &server_id, result)? {
                    return Ok(false);
                }
                self.bump_processed();
            }
            cursor = key(&page, "last_change_point")?.clone();
            self.store.set_meta("cursor", &cursor)?;
            if matches!(key(&page, "has_next")?, Val::Bool(false)) {
                return Ok(true);
            }
            if self.wake.is_set() && key(&self.store.counts()?, "pending")?.truthy() {
                return Ok(false);
            }
        }
        // Resume bounded initial imports on the next worker iteration.
        self.wake.set();
        Ok(false)
    }

    /// `Engine.sync_once()`
    pub fn sync_once(self: &Arc<Self>) -> bool {
        let Ok(_guard) = self.sync_lock.try_lock() else { return false };
        {
            let mut st = self.sync.lock().unwrap();
            *st = SyncState { state: "syncing".into(), error: None, processed: 0, total: None };
        }
        // A panic while syncing a hostile Cloud answer must not end the worker
        // thread or poison `sync_lock` (both would stop sync until restart).
        // SQLite transactions roll back on drop and an interrupted job is
        // resumed exactly as after a restart (claim files prevent a resend).
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.sync_body()))
            .unwrap_or_else(|_| Err(Fail::os("sync worker panicked")));
        match outcome {
            Ok(complete) => complete,
            Err(error) if error.is_api() || error.is_store() => {
                let mut st = self.sync.lock().unwrap();
                st.state = if error.code == "cloud_unavailable" { "offline" } else { "error" }.into();
                st.error = Some(error.message.clone());
                false
            }
            Err(error) if error.is_os_value_key_type() => {
                let mut st = self.sync.lock().unwrap();
                st.state = "error".into();
                st.error = Some("Eşitleme tamamlanamadı. Yerel notlar ve işlem kayıtları korunuyor.".into());
                false
            }
            Err(_) => {
                // Python would let any other exception escape the worker thread
                // (ending it); keep the loop alive but report a generic error.
                let mut st = self.sync.lock().unwrap();
                st.state = "error".into();
                st.error = Some("Eşitleme tamamlanamadı. Yerel notlar ve işlem kayıtları korunuyor.".into());
                false
            }
        }
    }

    fn sync_body(self: &Arc<Self>) -> R<bool> {
        let api = self.api()?;
        let metadata = self.apis.metadata(&api);
        if let Some(m) = &metadata {
            self.pull_folders(m)?;
        }
        self.push(&api)?;
        if let Some(m) = &metadata {
            self.push_folders(m)?;
            self.push_organization(m)?;
        }
        self.load_missing(&api)?;
        let mut complete = self.pull(&api)?;
        if metadata.is_some() {
            self.store.apply_folder_graph(&Val::dict(vec![
                ("has_resource", Val::Bool(false)),
                ("last_change_point", meta_or(&self.store, "folder_cursor", Val::Null)?),
            ]))?;
        }
        let missing = self.store.with_db(|db| {
            let cur = db.query("SELECT COUNT(*) FROM notes WHERE loaded=0 AND deleted_remote=0", &[])?;
            Ok(cur.scalar().map(|v| rs_store::db::sql_to_val(v).as_i64().unwrap_or(0)))
        })?;
        let missing = missing.unwrap_or(0);
        if missing != 0 {
            complete = false;
        }
        if complete {
            self.store.set_meta("last_synced_at", &Val::Int(rs_store::db::now_ms()))?;
            // Warm first-page previews so opening a note is immediate.
            let mut warmup = self.warmup.lock().unwrap();
            let alive = warmup.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
            if !self.stop.is_set() && !alive {
                let engine = Arc::clone(self);
                *warmup = std::thread::Builder::new().name("notes-preview-warmup".into()).spawn(move || engine.prefetch_previews()).ok();
            }
        }
        let mut st = self.sync.lock().unwrap();
        st.state = if missing != 0 { "error" } else { "idle" }.into();
        if missing != 0 {
            st.error = Some(format!(
                "{} not henüz indirilemedi. İndirilen notlar çevrimdışı okunabilir; eşitleme yeniden denenecek.",
                missing
            ));
        }
        Ok(complete)
    }

    /// `Engine.close()`
    pub fn close(&self) {
        self.previews.close();
        self.sessions.closed.set();
        self.stop.set();
        self.wake.set();
        // An in-flight HTTP request may still finish its immutable receipt.
        join_timeout(self.worker.lock().unwrap().take(), Duration::from_secs(2));
        join_timeout(self.warmup.lock().unwrap().take(), Duration::from_secs(2));
        self.previews.join(Duration::from_secs(2));
    }
}

/// `Engine._scan_templates(resource_b64)` -> (entries, contentFile ids).
fn scan_templates(resource_b64: &Val) -> R<(Vec<Val>, Vec<Option<String>>)> {
    use rs_engine::engine_core as core;
    let text = match resource_b64 {
        v if !v.truthy() => String::new(),
        Val::Str(s) => s.clone(),
        Val::Bytes(b) => match std::str::from_utf8(b) {
            Ok(t) if t.is_ascii() => t.to_string(),
            _ => return Ok((vec![], vec![])),
        },
        _ => return Ok((vec![], vec![])),
    };
    if !text.is_ascii() {
        return Ok((vec![], vec![]));
    }
    let Ok(xml) = core::b64decode_lenient(&text) else { return Ok((vec![], vec![])) };
    let (groups, ids) = match core::scan_templates(&xml) {
        core::TemplateScan::Skip => return Ok((vec![], vec![])),
        core::TemplateScan::Error(m) => return Err(Fail::value(m)),
        core::TemplateScan::Found(g, ids) => (g, ids),
    };
    let mut entries = Vec::new();
    for group in groups {
        let Some(file_name) = group.file_name.clone() else { continue };
        if !core::py_isdigit(&group.template_type) {
            continue;
        }
        let template_type = core::py_int(&group.template_type)
            .ok_or_else(|| Fail::value(format!("invalid literal for int() with base 10: '{}'", group.template_type)))?;
        entries.push(Val::dict(vec![
            ("id", opt(group.sha.clone())),
            ("name", Val::s(core::template_display_name(&file_name))),
            ("fileName", Val::s(file_name.clone())),
            ("templateType", Val::Int(template_type)),
            ("pageIndices", Val::List(group.indices.iter().map(|i| Val::Int(*i)).collect())),
            ("pageCount", Val::Int(group.indices.len() as i64)),
        ]));
    }
    Ok((entries, ids))
}

/// ThreadPoolExecutor.map re-raises the first non-API error after all finish.
fn first_error(results: &[Option<R<Snapshot>>]) -> R<()> {
    for r in results.iter().flatten() {
        if let Err(e) = r {
            if !e.is_api() {
                return Err(e.clone());
            }
        }
    }
    Ok(())
}

/// `str(error)` for StoreError (Exception args = (message,)) and DocumentError.
fn str_of(error: &Fail) -> String {
    error.message.clone()
}

/// Python `re` `\w` for one character (Unicode `str.isalnum() or '_'`).
pub fn is_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

fn ops_fail(e: rs_ops::err::Error) -> Fail {
    notes_host::deps::fail_of_ops(e)
}

fn join_timeout(handle: Option<JoinHandle<()>>, timeout: Duration) {
    let Some(handle) = handle else { return };
    let deadline = std::time::Instant::now() + timeout;
    while !handle.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

/// `Engine._bootstrap_cache()`
fn bootstrap_cache(store: &StoreCore, sessions: &Sessions) -> R<()> {
    let Some(session) = sessions.path() else { return Ok(()) };
    if meta_or(store, "research_seed_imported", Val::Bool(false))?.truthy() {
        return Ok(());
    }
    let scan = session.join("cloud/scan");
    let mut pages: Vec<PathBuf> = match std::fs::read_dir(&scan) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                name.starts_with("page-") && name.ends_with(".json")
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    pages.sort();
    for parsed_path in pages {
        let stem = parsed_path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !rs_engine::engine_core::py_isdigit(&stem[5.min(stem.len())..]) {
            continue;
        }
        let raw_path = parsed_path.with_file_name(format!("{}-response.body", stem));
        let step = (|| -> R<bool> {
            if std::fs::metadata(&parsed_path)?.len() > 8 * 1024 * 1024 || std::fs::metadata(&raw_path)?.len() > 2 * 1024 * 1024 {
                return Ok(false);
            }
            let text = String::from_utf8(std::fs::read(&parsed_path)?).map_err(|_| Fail::value("invalid utf-8"))?;
            let page = rs_store::val::loads(&text).map_err(|e| Fail::value(format!("{:?}", e)))?;
            let digest = rs_engine::engine_core::sha256_hex(&std::fs::read(&raw_path)?);
            if !page.get("sourceSha256").map(|v| is_str(v, &digest)).unwrap_or(false) {
                return Ok(false);
            }
            let inner = key(&page, "page")?;
            if let Val::List(changes) = key(inner, "changes")? {
                for change in changes {
                    store.stage_change(change)?;
                }
            }
            store.set_meta("cursor", key(inner, "last_change_point")?)?;
            Ok(true)
        })();
        match step {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) if e.is_os_value_key_type() => break,
            Err(e) => return Err(e),
        }
    }
    let remote = session.join("cloud/sync-probe/remote.json");
    if remote.exists() {
        let step = (|| -> R<()> {
            let text = String::from_utf8(std::fs::read(&remote)?).map_err(|_| Fail::value("invalid utf-8"))?;
            let value = rs_store::val::loads(&text).map_err(|e| Fail::value(format!("{:?}", e)))?;
            if matches!(value.get("createdByThisClient"), Some(Val::Bool(true))) {
                store.stage_change(&Val::dict(vec![
                    ("server_id", key(&value, "serverId")?.clone()),
                    ("resource_id", key(&value, "localNoteId")?.clone()),
                    ("commit_id", key(&value, "commitId")?.clone()),
                    ("is_deleted", Val::Bool(false)),
                ]))?;
            }
            Ok(())
        })();
        if let Err(e) = step {
            if !e.is_os_value_key_type() {
                return Err(e);
            }
        }
    }
    store.set_meta("research_seed_imported", &Val::Bool(true))?;
    Ok(())
}

/// `Engine._loop()`
fn run_loop(engine: Weak<Engine>) {
    loop {
        let Some(e) = engine.upgrade() else { return };
        if e.stop.is_set() {
            return;
        }
        let interval = Duration::from_secs_f64(e.interval);
        let wake = Arc::clone(&e);
        drop(e);
        let triggered = wake.wake.wait(interval);
        wake.wake.clear();
        if wake.stop.is_set() {
            return;
        }
        if wake.sessions.path().is_some() && (triggered || wake.auto_sync) {
            wake.sync_once();
        }
    }
}
