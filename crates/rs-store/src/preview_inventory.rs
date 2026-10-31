//! Static preview metadata, computed outside the store and committed with CAS.
//! No signed URLs, cached availability, raw page bodies, or renderer dependencies
//! belong here. The committed container pointer is the durable work queue.
use crate::db::{Error, Result, Sql};
use crate::p;
use crate::storage::StoreCore;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS preview_inventories (
    container_sha256 TEXT NOT NULL, raw_sha256 TEXT NOT NULL, recipe TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('ready','unsupported','failed')), payload TEXT, render_blob TEXT, render_blob_sha256 TEXT,
    PRIMARY KEY(container_sha256,recipe))";
const MAX_PAGES: usize = 4096;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_RENDER_BLOB: usize = 16 * 1024 * 1024;

// A small process-local hint queue, not durable state. An opened note should
// not have to wait for an entire previously selected batch of background work.
const MAX_DEMAND_HINTS: usize = 64;
struct DemandHint { directory: PathBuf, recipe: String, note_id: String }
static DEMAND_HINTS: Mutex<VecDeque<DemandHint>> = Mutex::new(VecDeque::new());

fn hint(directory: &Path, recipe: &str, note_id: &str) {
    if note_id.len() > 1024 { return; }
    let mut hints = DEMAND_HINTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if hints.iter().any(|h| h.directory == directory && h.recipe == recipe && h.note_id == note_id) { return; }
    while hints.len() >= MAX_DEMAND_HINTS { hints.pop_front(); }
    hints.push_back(DemandHint { directory: directory.to_path_buf(), recipe: recipe.to_string(), note_id: note_id.to_string() });
}

fn demanded(directory: &Path, recipe: &str) -> Option<String> {
    let hints = DEMAND_HINTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    hints.iter().find(|h| h.directory == directory && h.recipe == recipe).map(|h| h.note_id.clone())
}

fn take_demand(directory: &Path, recipe: &str) -> Option<String> {
    let mut hints = DEMAND_HINTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let i = hints.iter().position(|h| h.directory == directory && h.recipe == recipe)?;
    Some(hints.remove(i)?.note_id)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlobRef {
    source_sha256: String,
    stored_sha256: String,
    source_size: u64,
    stored_size: u64,
}
impl BlobRef {
    fn document(&self) -> Result<crate::document_store::DocumentRef> {
        if !digest(&self.source_sha256)
            || !digest(&self.stored_sha256)
            || self.source_size > MAX_RENDER_BLOB as u64
            || self.stored_size > 20 * 1024 * 1024
        {
            return Err(Error::Value("invalid preview render blob reference".into()));
        }
        Ok(crate::document_store::DocumentRef {
            source_sha256: self.source_sha256.clone(),
            stored_sha256: self.stored_sha256.clone(),
            source_size: self.source_size,
            stored_size: self.stored_size,
        })
    }
}
pub(crate) fn decode_blob_ref(value: &str) -> Result<crate::document_store::DocumentRef> {
    if value.len() > 1024 {
        return Err(Error::Value("invalid preview render blob reference".into()));
    }
    serde_json::from_str::<BlobRef>(value)
        .map_err(|_| Error::Value("invalid preview render blob reference".into()))?
        .document()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryKey {
    pub note_id: String,
    pub container_sha256: String,
    pub raw_sha256: String,
    pub recipe: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageSize {
    pub width: Option<i64>,
    pub height: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
    pub page_index: usize,
    pub key: i64,
    pub width: i64,
    pub height: i64,
    pub dark: bool,
    pub bind_number: String,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pdf {
    pub page_index: usize,
    pub pdf_page_index: i64,
    pub width: i64,
    pub height: i64,
    pub rect: (i64, i64, i64, i64),
    pub annotation_objects: i64,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InkPage {
    pub page_index: usize,
    pub width: i64,
    pub height: i64,
    pub dark: bool,
    pub composite: bool,
    pub notice: Option<String>,
    pub over_pdf: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub pages: Vec<PageSize>,
    pub canvases: Vec<Canvas>,
    pub pdfs: Vec<Pdf>,
    pub unsupported_pdf_pages: Vec<usize>,
    pub ink_pages: Vec<InkPage>,
    /// False means full-note ink preparation deterministically refused it.
    pub ink_supported: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InventoryOutcome {
    Ready(Inventory),
    Unsupported,
    /// A bounded number of transient attempts failed; explicit retry required.
    Failed,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_source_metadata(
    container: &str,
    raw: &str,
    size: &rusqlite::types::Value,
    metadata: &rusqlite::types::Value,
) -> Result<crate::val::Val> {
    let value = crate::db::loads_sql(metadata)?;
    let reference = crate::snapshot_store::raw_reference(&value)?;
    let stored_size =
        i64::try_from(reference.size).map_err(|_| Error::Value("invalid container size".into()))?;
    if reference.sha256 != container
        || *size != crate::db::int(stored_size)
        || value.get("raw_sha256").and_then(crate::val::Val::as_str) != Some(raw)
    {
        return Err(Error::Value(
            "preview inventory source metadata mismatch".into(),
        ));
    }
    Ok(value)
}

fn recipe(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.bytes().any(|c| !c.is_ascii_graphic()) {
        return Err(Error::Value("invalid preview inventory recipe".into()));
    }
    Ok(())
}
fn encode_inventory(inventory: &Inventory) -> Result<String> {
    struct Limited(Vec<u8>);
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .0
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > MAX_BYTES)
            {
                return Err(std::io::Error::other(
                    "preview inventory exceeds size limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    validate(inventory, inventory.pages.len())?;
    let mut output = Limited(Vec::new());
    serde_json::to_writer(&mut output, inventory).map_err(|e| Error::Value(e.to_string()))?;
    String::from_utf8(output.0)
        .map_err(|_| Error::Value("invalid preview inventory encoding".into()))
}

fn validate(inventory: &Inventory, pages: usize) -> Result<()> {
    let valid_size = |w: i64, h: i64| w > 0 && h > 0 && w <= 32768 && h <= 32768;
    if pages > MAX_PAGES
        || inventory.pages.len() != pages
        || inventory.canvases.len() > MAX_PAGES * 64
        || inventory.pdfs.len() > MAX_PAGES * 64
        || inventory.ink_pages.len() > pages
        || inventory.unsupported_pdf_pages.len() > pages
    {
        return Err(Error::Value("invalid preview inventory size".into()));
    }
    if inventory.pages.iter().any(|p| match (p.width, p.height) {
        (Some(w), Some(h)) => !valid_size(w, h),
        (None, None) => false,
        _ => true,
    }) || inventory.canvases.iter().any(|c| {
        c.page_index >= pages
            || !valid_size(c.width, c.height)
            || !digest(&c.sha256)
            || c.bind_number.len() > 256
            || c.bind_number.chars().any(char::is_control)
            || c.bind_number.contains("://")
    }) || inventory.pdfs.iter().any(|p| {
        p.page_index >= pages
            || p.pdf_page_index < 0
            || p.annotation_objects < 0
            || !valid_size(p.width, p.height)
            || !digest(&p.sha256)
    }) || inventory.ink_pages.iter().any(|p| {
        p.page_index >= pages
            || !valid_size(p.width, p.height)
            || (p.over_pdf && !p.composite)
            || p.notice
                .as_ref()
                .is_some_and(|s| s.len() > 4096 || s.to_ascii_lowercase().contains("://"))
    }) {
        return Err(Error::Value("invalid preview inventory entry".into()));
    }
    let mut ink = HashSet::new();
    if inventory
        .ink_pages
        .iter()
        .any(|p| !ink.insert(p.page_index))
    {
        return Err(Error::Value("duplicate preview ink page".into()));
    }
    let mut unsupported = HashSet::new();
    if inventory
        .unsupported_pdf_pages
        .iter()
        .any(|i| *i >= pages || !unsupported.insert(*i))
    {
        return Err(Error::Value("invalid unsupported PDF page".into()));
    }
    Ok(())
}

impl StoreCore {
    /// Return a bounded batch of missing indexes. No container is opened and
    /// the caller schedules work only on publication or an explicit retry.
    pub fn pending_preview_inventories(
        &self,
        recipe_id: &str,
        limit: usize,
    ) -> Result<Vec<InventoryKey>> {
        recipe(recipe_id)?;
        if limit == 0 || limit > 128 {
            return Err(Error::Value("invalid preview inventory batch size".into()));
        }
        self.with_db(|db| {
            if !StoreCore::schema7(db)? { return Ok(Vec::new()); }
            while let Some(id) = take_demand(&self.directory, recipe_id) {
                let preferred = db.query("SELECT n.id,n.container_sha256,n.raw_sha256 FROM notes n WHERE n.id=? AND n.container_sha256 IS NOT NULL AND n.raw_sha256 IS NOT NULL AND n.loaded=1 AND n.deleted_remote=0 AND n.trashed=0 AND NOT EXISTS (SELECT 1 FROM preview_inventories i WHERE i.container_sha256=n.container_sha256 AND i.raw_sha256=n.raw_sha256 AND i.recipe=?)", &p![&id, recipe_id])?;
                if let Some(row) = preferred.one() {
                    let hash = row.text("container_sha256").ok_or_else(|| Error::Value("invalid container reference".into()))?;
                    let raw = row.text("raw_sha256").ok_or_else(|| Error::Value("invalid raw reference".into()))?;
                    if !digest(hash) || !digest(raw) { return Err(Error::Value("invalid inventory source digest".into())); }
                    return Ok(vec![InventoryKey { note_id: id, container_sha256: hash.into(), raw_sha256: raw.into(), recipe: recipe_id.into() }]);
                }
            }
            let rows = db.query("SELECT n.id,n.container_sha256,n.raw_sha256 FROM notes n WHERE n.container_sha256 IS NOT NULL AND n.raw_sha256 IS NOT NULL AND n.loaded=1 AND n.deleted_remote=0 AND n.trashed=0 AND NOT EXISTS (SELECT 1 FROM preview_inventories i WHERE i.container_sha256=n.container_sha256 AND i.raw_sha256=n.raw_sha256 AND i.recipe=?) AND n.id=(SELECT MIN(n2.id) FROM notes n2 WHERE n2.container_sha256=n.container_sha256 AND n2.raw_sha256=n.raw_sha256 AND n2.loaded=1 AND n2.deleted_remote=0 AND n2.trashed=0) ORDER BY n.id LIMIT ?", &p![recipe_id, limit as i64])?;
            rows.iter().map(|row| {
                let note_id = row.text("id").ok_or_else(|| Error::Value("invalid note ID".into()))?;
                let hash = row.text("container_sha256").ok_or_else(|| Error::Value("invalid container reference".into()))?;
                let raw = row.text("raw_sha256").ok_or_else(|| Error::Value("invalid raw reference".into()))?;
                if !digest(hash) || !digest(raw) { return Err(Error::Value("invalid inventory source digest".into())); }
                Ok(InventoryKey { note_id: note_id.to_string(), container_sha256: hash.to_string(), raw_sha256: raw.to_string(), recipe: recipe_id.to_string() })
            }).collect()
        })
    }

    /// Resolve a worker's source without retaining it in the snapshot cache.
    /// Pin the immutable file under the publication lock, then release the
    /// lock before reconstructing. An unlinked file remains readable through
    /// its open descriptor; publication still checks the source with CAS.
    pub fn preview_inventory_source(&self, key: &InventoryKey) -> Result<Option<crate::val::Val>> {
        recipe(&key.recipe)?;
        if !digest(&key.container_sha256) || !digest(&key.raw_sha256) {
            return Err(Error::Value("invalid inventory source digest".into()));
        }
        if demanded(&self.directory, &key.recipe).is_some_and(|id| id != key.note_id) {
            return Ok(None);
        }
        let store = crate::document_store::DocumentStore::open(&self.directory)?;
        let pinned = store.with_write(|writer| {
            self.with_db(|db| {
                if !StoreCore::schema7(db)? {
                    return Ok(None);
                }
                let row = db.one(
                    "SELECT container_sha256,raw_sha256,container_size,container_metadata FROM notes WHERE id=?",
                    &p![&key.note_id],
                )?;
                let Some(row) = row else { return Ok(None) };
                if row[0] != crate::db::text(&key.container_sha256)
                    || row[1] != crate::db::text(&key.raw_sha256)
                {
                    return Ok(None);
                }
                let reference = validate_source_metadata(&key.container_sha256, &key.raw_sha256, &row[2], &row[3])?;
                let file = writer.open_raw(&crate::snapshot_store::raw_reference(&reference)?)?;
                Ok(Some((reference, file)))
            })
        })?;
        pinned.map(|(reference, file)| crate::snapshot_store::load_snapshot_from_file(&reference, file)).transpose()
    }

    /// Return only an inventory bound to the note's current immutable source.
    pub fn preview_inventory(
        &self,
        note_id: &str,
        recipe_id: &str,
    ) -> Result<Option<InventoryOutcome>> {
        recipe(recipe_id)?;
        let result = self.with_db(|db| {
            if !StoreCore::schema7(db)? { return Ok(None); }
            let rows = db.query("SELECT i.state,i.payload,n.container_sha256,n.raw_sha256,n.container_size,n.container_metadata,(SELECT COUNT(*) FROM note_pages p WHERE p.note_id=n.id) AS pages FROM notes n JOIN preview_inventories i ON i.container_sha256=n.container_sha256 AND i.raw_sha256=n.raw_sha256 AND i.recipe=? WHERE n.id=?", &p![recipe_id, note_id])?;
            let Some(row) = rows.one() else { return Ok(None); };
            let container = row.text("container_sha256").ok_or_else(|| Error::Value("invalid container reference".into()))?;
            let raw = row.text("raw_sha256").ok_or_else(|| Error::Value("invalid raw reference".into()))?;
            validate_source_metadata(container, raw, row.raw("container_size"), row.raw("container_metadata"))?;
            match row.text("state") {
                Some("unsupported") if row.is_null("payload") => Ok(Some(InventoryOutcome::Unsupported)),
                Some("failed") if row.is_null("payload") => Ok(Some(InventoryOutcome::Failed)),
                Some("ready") => {
                    let payload = row.text("payload").filter(|s| s.len() <= MAX_BYTES).ok_or_else(|| Error::Value("invalid preview inventory payload".into()))?;
                    let inventory: Inventory = serde_json::from_str(payload).map_err(|_| Error::Value("invalid preview inventory payload".into()))?;
                    let count = usize::try_from(row.int("pages")).map_err(|_| Error::Value("invalid preview page count".into()))?;
                    validate(&inventory, count)?;
                    Ok(Some(InventoryOutcome::Ready(inventory)))
                }
                _ => Err(Error::Value("invalid preview inventory state".into())),
            }
        })?;
        if result.is_none() { hint(&self.directory, recipe_id, note_id); }
        Ok(result)
    }

    /// Explicit user retry only: reset a terminal transient failure if it
    /// still belongs to the note's current source. Normal polling never resets it.
    pub fn retry_preview_inventory(&self, note_id: &str, recipe_id: &str) -> Result<bool> {
        recipe(recipe_id)?;
        self.with_db(|db| {
            if !StoreCore::schema7(db)? { return Ok(false); }
            Ok(db.exec("DELETE FROM preview_inventories WHERE recipe=? AND state='failed' AND container_sha256=(SELECT container_sha256 FROM notes WHERE id=?)", &p![recipe_id, note_id])?.rowcount == 1)
        })
    }

    /// Commit only while the note still points to the exact source used by the
    /// worker. A failed computation writes nothing and stays pending. A false
    /// result means the worker lost the race with a newer note revision.
    pub fn publish_preview_inventory(
        &self,
        key: &InventoryKey,
        value: &InventoryOutcome,
    ) -> Result<bool> {
        self.publish_preview_inventory_with_blob(key, value, None)
    }

    /// Publish derived data and commit its source-bound pointer under one lock.
    pub fn publish_preview_inventory_with_blob(
        &self,
        key: &InventoryKey,
        value: &InventoryOutcome,
        blob: Option<&[u8]>,
    ) -> Result<bool> {
        recipe(&key.recipe)?;
        if !digest(&key.container_sha256) || !digest(&key.raw_sha256) {
            return Err(Error::Value("invalid inventory source digest".into()));
        }
        if blob.is_some_and(|b| b.len() > MAX_RENDER_BLOB)
            || (blob.is_some() && !matches!(value, InventoryOutcome::Ready(_)))
        {
            return Err(Error::Value("invalid preview render blob".into()));
        }
        let payload = match value {
            InventoryOutcome::Ready(inventory) => Some(encode_inventory(inventory)?),
            InventoryOutcome::Unsupported | InventoryOutcome::Failed => None,
        };
        let docs = crate::document_store::DocumentStore::open(&self.directory)?;
        docs.with_write(|writer| {
        let blob_ref = blob.map(|bytes| writer.put(bytes)).transpose()?.map(|r| BlobRef {
            source_sha256: r.source_sha256, stored_sha256: r.stored_sha256,
            source_size: r.source_size, stored_size: r.stored_size,
        });
        let blob_sha256 = blob_ref.as_ref().map(|r| r.source_sha256.clone());
        let blob_json = blob_ref.map(|r| serde_json::to_string(&r).map_err(|e| Error::Value(e.to_string()))).transpose()?;
        self.with_db(|db| {
            if !StoreCore::schema7(db)? { return Ok(false); }
            db.exec("BEGIN IMMEDIATE", &[])?;
            let rows = db.query("SELECT container_sha256,raw_sha256,container_size,container_metadata,(SELECT COUNT(*) FROM note_pages WHERE note_id=notes.id) AS pages FROM notes WHERE id=?", &p![&key.note_id])?;
            let Some(row) = rows.one() else { return Ok(false); };
            if row.text("container_sha256") != Some(&key.container_sha256) || row.text("raw_sha256") != Some(&key.raw_sha256) { return Ok(false); }
            validate_source_metadata(&key.container_sha256, &key.raw_sha256, row.raw("container_size"), row.raw("container_metadata"))?;
            if let InventoryOutcome::Ready(inventory) = value {
                let count = usize::try_from(row.int("pages")).map_err(|_| Error::Value("invalid preview page count".into()))?;
                validate(inventory, count)?;
            }
            let state = match value { InventoryOutcome::Ready(_) => "ready", InventoryOutcome::Unsupported => "unsupported", InventoryOutcome::Failed => "failed" };
            let result = db.exec("INSERT OR IGNORE INTO preview_inventories(container_sha256,raw_sha256,recipe,state,payload,render_blob,render_blob_sha256) VALUES (?,?,?,?,?,?,?)", &[
                crate::db::text(&key.container_sha256), crate::db::text(&key.raw_sha256), crate::db::text(&key.recipe), crate::db::text(state), payload.as_deref().map(crate::db::text).unwrap_or_else(crate::db::null), blob_json.as_deref().map(crate::db::text).unwrap_or_else(crate::db::null), blob_sha256.as_deref().map(crate::db::text).unwrap_or_else(crate::db::null),
            ])?;
            if result.rowcount == 0 {
                let existing = db.one("SELECT raw_sha256,state,payload,render_blob,render_blob_sha256 FROM preview_inventories WHERE container_sha256=? AND recipe=?", &p![&key.container_sha256, &key.recipe])?.ok_or_else(|| Error::Value("missing preview inventory".into()))?;
                if existing[0] != crate::db::text(&key.raw_sha256) || existing[1] != crate::db::text(state) || existing[2] != payload.as_deref().map(crate::db::text).unwrap_or_else(crate::db::null) || existing[3] != blob_json.as_deref().map(crate::db::text).unwrap_or_else(crate::db::null) || existing[4] != blob_sha256.as_deref().map(crate::db::text).unwrap_or_else(crate::db::null) {
                    return Err(Error::Value("preview inventory differs for the same source and recipe".into()));
                }
            }
            Ok(true)
        })
        })
    }

    /// A cached entry is reused only when the current pointer and file identity
    /// still match. A cache miss returns verified bytes under the same lock.
    pub fn preview_render_blob_cached(
        &self,
        note_id: &str,
        recipe_id: &str,
        cached: Option<(
            &crate::document_store::DocumentRef,
            &crate::document_store::DocumentFingerprint,
        )>,
    ) -> Result<
        Option<(
            crate::document_store::DocumentRef,
            crate::document_store::DocumentFingerprint,
            Option<Vec<u8>>,
        )>,
    > {
        recipe(recipe_id)?;
        let docs = crate::document_store::DocumentStore::open(&self.directory)?;
        docs.with_write(|writer| self.with_db(|db| {
            if !StoreCore::schema7(db)? { return Ok(None); }
            let rows = db.query("SELECT i.render_blob,i.render_blob_sha256,n.container_sha256,n.raw_sha256,n.container_size,n.container_metadata FROM notes n JOIN preview_inventories i ON i.container_sha256=n.container_sha256 AND i.raw_sha256=n.raw_sha256 AND i.recipe=? AND i.state='ready' WHERE n.id=?", &p![recipe_id,note_id])?;
            let Some(row) = rows.one() else { return Ok(None) };
            let container = row.text("container_sha256").ok_or_else(|| Error::Value("invalid container reference".into()))?;
            let raw = row.text("raw_sha256").ok_or_else(|| Error::Value("invalid raw reference".into()))?;
            validate_source_metadata(container, raw, row.raw("container_size"), row.raw("container_metadata"))?;
            if row.is_null("render_blob") && row.is_null("render_blob_sha256") { return Ok(None); }
            let blob = row.text("render_blob").ok_or_else(|| Error::Value("invalid preview render blob reference".into()))?;
            let reference = decode_blob_ref(blob)?;
            // These independent fields detect accidental or single-field changes;
            // coordinated SQLite changes are outside this integrity boundary.
            if row.text("render_blob_sha256") != Some(reference.source_sha256.as_str()) {
                return Err(Error::Value("preview render blob digest mismatch".into()));
            }
            let fingerprint = writer.fingerprint(&reference)?;
            if cached.is_some_and(|(r, f)| *r == reference && *f == fingerprint) {
                return Ok(Some((reference, fingerprint, None)));
            }
            let bytes = writer.read(&reference)?;
            if writer.fingerprint(&reference)? != fingerprint {
                return Err(Error::Value("preview render blob changed while reading".into()));
            }
            Ok(Some((reference, fingerprint, Some(bytes))))
        }))
    }

    /// Read and verify the current immutable projection without a cache.
    pub fn preview_render_blob(&self, note_id: &str, recipe_id: &str) -> Result<Option<Vec<u8>>> {
        self.preview_render_blob_cached(note_id, recipe_id, None)?
            .map(|(_, _, bytes)| {
                bytes.ok_or_else(|| Error::Value("missing preview render blob bytes".into()))
            })
            .transpose()
    }
}
