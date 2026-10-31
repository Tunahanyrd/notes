//! Mark immutable containers from SQLite and durable operation receipts.
//! Both enumeration and sweeping must run under the document publication lock.
use crate::db::{loads_sql, Error, Result, Sql};
use crate::document_store::{
    CollectionReport, DoctorReport, DocumentRef, DocumentStore, RawDocumentRef,
};
use crate::snapshot_store::raw_reference;
use crate::storage::StoreCore;
use crate::val::Val;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::Duration;

const RECEIPT_LIMIT: u64 = 96 * 1024 * 1024;

fn checked_reference(value: &Val) -> Result<RawDocumentRef> {
    let reference = raw_reference(value)?;
    if reference.sha256.len() != 64
        || !reference
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Value("invalid container reference".into()));
    }
    Ok(reference)
}

fn private_dir(path: &Path) -> Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if !meta.file_type().is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(Error::Value("unsafe operation directory".into()));
    }
    Ok(true)
}

fn receipt_references(directory: &Path, references: &mut Vec<RawDocumentRef>) -> Result<()> {
    let cloud = directory.join("cloud");
    if !private_dir(&cloud)? {
        return Ok(());
    }
    for name in ["operations", "metadata-operations"] {
        let root = cloud.join(name);
        if !private_dir(&root)? {
            continue;
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_symlink() {
                return Err(Error::Value("unsafe operation path".into()));
            }
            if !ty.is_dir() {
                continue;
            }
            let path = entry.path();
            if !private_dir(&path)? {
                return Err(Error::Value("operation directory disappeared".into()));
            }
            let receipt = path.join("result.json");
            let mut file = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&receipt)
            {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let meta = file.metadata()?;
            // SAFETY: getuid has no preconditions.
            let uid = unsafe { libc::getuid() };
            if !meta.is_file()
                || meta.uid() != uid
                || meta.mode() & 0o077 != 0
                || meta.len() > RECEIPT_LIMIT
            {
                return Err(Error::Value("invalid operation receipt".into()));
            }
            let mut bytes = Vec::new();
            (&mut file)
                .take(RECEIPT_LIMIT + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > RECEIPT_LIMIT {
                return Err(Error::Value("operation receipt too large".into()));
            }
            let json = std::str::from_utf8(&bytes)
                .map_err(|_| Error::Value("invalid operation receipt".into()))?;
            let value = crate::val::loads(json)?;
            if !matches!(value, Val::Dict(_)) {
                return Err(Error::Value("invalid operation receipt".into()));
            }
            if let Some(snapshot) = value.get("snapshot") {
                if snapshot.get("container_sha256").is_some() {
                    references.push(checked_reference(snapshot)?);
                } else if !snapshot
                    .get("raw_body")
                    .is_some_and(|v| v.as_str().is_some())
                {
                    return Err(Error::Value("invalid receipt snapshot".into()));
                }
            }
        }
    }
    Ok(())
}

fn references(store: &StoreCore) -> Result<Vec<RawDocumentRef>> {
    let mut references = store.with_db(|db| {
        if !StoreCore::schema7(db)? { return Err(Error::Value("container collection requires schema 7".into())); }
        let sql = "SELECT container_metadata AS snapshot FROM notes WHERE container_metadata IS NOT NULL
            UNION ALL SELECT remote_container_metadata FROM notes WHERE remote_container_metadata IS NOT NULL
            UNION ALL SELECT container_metadata FROM note_container_revisions
            UNION ALL SELECT snapshot FROM jobs WHERE snapshot IS NOT NULL AND state NOT IN ('done','cancelled')
            UNION ALL SELECT snapshot FROM organization_jobs WHERE state NOT IN ('done','cancelled') AND NOT (state='uncertain' AND snapshot='{}' AND COALESCE(error,'')='Eski işlemin kaynak sürümü kayıtlı değil; yeniden gönderilmedi.')
            UNION ALL SELECT i.current_snapshot FROM ink_jobs i JOIN jobs j ON j.id=i.job_id WHERE j.state NOT IN ('done','cancelled')
            UNION ALL SELECT i.current_snapshot FROM ink_erase_jobs i JOIN jobs j ON j.id=i.job_id WHERE j.state NOT IN ('done','cancelled')
            UNION ALL SELECT i.current_snapshot FROM ink_move_jobs i JOIN jobs j ON j.id=i.job_id WHERE j.state NOT IN ('done','cancelled')";
        let rows = db.query(sql, &[])?;
        rows.iter().map(|row| checked_reference(&loads_sql(row.raw("snapshot"))?)).collect::<Result<Vec<_>>>()
    })?;
    receipt_references(&store.directory, &mut references)?;
    Ok(references)
}

// Enumerated under the publication lock. Invalid references abort collection.
fn blob_references(store: &StoreCore) -> Result<Vec<DocumentRef>> {
    store.with_db(|db| {
        if !StoreCore::schema7(db)? {
            return Err(Error::Value("blob collection requires schema 7".into()));
        }
        let rows = db.query(
            "SELECT render_blob,render_blob_sha256 FROM preview_inventories WHERE render_blob IS NOT NULL OR render_blob_sha256 IS NOT NULL",
            &[],
        )?;
        rows.iter()
            .map(|row| {
                let value = row
                    .text("render_blob")
                    .ok_or_else(|| Error::Value("invalid preview render blob reference".into()))?;
                let reference = crate::preview_inventory::decode_blob_ref(value)?;
                if row.text("render_blob_sha256") != Some(reference.source_sha256.as_str()) {
                    return Err(Error::Value("preview render blob digest mismatch".into()));
                }
                Ok(reference)
            })
            .collect()
    })
}

impl StoreCore {
    /// Run conservative collection at most once a day. Return errors to the
    /// caller for logging without changing already acknowledged jobs.
    pub fn maintain_containers(&self) -> Result<Option<CollectionReport>> {
        const DAY_MS: i64 = 24 * 60 * 60 * 1000;
        if !self.with_db(|db| StoreCore::schema7(db))? {
            return Ok(None);
        }
        let now = crate::db::now_ms();
        let last = self.meta("schema7_gc_last_ms")?.and_then(|v| v.as_i64());
        if last.is_some_and(|last| now.saturating_sub(last) < DAY_MS) {
            return Ok(None);
        }
        let report = self.collect_containers(Duration::from_secs(24 * 60 * 60))?;
        self.set_meta("schema7_gc_last_ms", &Val::Int(now))?;
        Ok(Some(report))
    }

    /// Verify all live containers, including operation receipts and retained revisions.
    pub fn doctor_containers(&self) -> Result<DoctorReport> {
        let docs = DocumentStore::open(&self.directory)?;
        let mut report = docs.doctor_raw(|| references(self))?;
        let blobs = docs.doctor(|| blob_references(self))?;
        report.referenced += blobs.referenced;
        report.missing += blobs.missing;
        report.corrupt += blobs.corrupt;
        report.unreferenced += blobs.unreferenced;
        report.temporary += blobs.temporary;
        Ok(report)
    }

    /// Remove only old unreferenced containers. The caller chooses the grace period.
    pub fn collect_containers(&self, grace: Duration) -> Result<CollectionReport> {
        let docs = DocumentStore::open(&self.directory)?;
        let mut report = docs.collect_raw(grace, || {
            let marked = references(self)?;
            // Validate every blob mark before any raw container can be removed.
            blob_references(self)?;
            // The publication lock held by collect_raw prevents note pointer
            // changes while the derived-cache mark set is being applied.
            self.with_db(|db| {
                db.exec("BEGIN IMMEDIATE", &[])?;
                db.exec("CREATE TEMP TABLE preview_inventory_live(hash TEXT PRIMARY KEY)", &[])?;
                for reference in &marked {
                    db.exec("INSERT OR IGNORE INTO preview_inventory_live VALUES (?)", &crate::p![&reference.sha256])?;
                }
                db.exec("DELETE FROM preview_inventories WHERE NOT EXISTS (SELECT 1 FROM preview_inventory_live WHERE hash=container_sha256)", &[])?;
                Ok(())
            })?;
            Ok(marked)
        })?;
        let blobs = docs.collect(grace, || blob_references(self))?;
        report.removed += blobs.removed;
        report.temporary_removed += blobs.temporary_removed;
        Ok(report)
    }
}
