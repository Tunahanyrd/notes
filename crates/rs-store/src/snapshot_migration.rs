//! Schema 6 to 7 conversion. Caller must hold the engine lock and have made
//! and checked an online SQLite backup before invoking this function.
use crate::db::{dumps_utf8, loads_sql, with_db, Error, Result, Sql};
use crate::document_store::DocumentStore;
use crate::p;
use crate::snapshot_store::{raw_reference, store_snapshot_with_pages};
use crate::val::Val;
use rusqlite::OptionalExtension;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use sha2::{Digest, Sha256};
use std::io::{BufWriter, Write};

/// The host owns projection semantics. The recipe must change whenever those
/// semantics or the serialization used here changes; a restart with a
/// different recipe cannot complete an interrupted migration.
pub trait ProjectionVerifier {
    fn recipe(&self) -> String;
    fn project(&self, snapshot: &Val) -> Result<(Val, Val)>;
    /// Hash projections independently; implementations may override this to
    /// avoid holding multiple large derived values at the same time.
    fn project_hashes(&self, snapshot: &Val) -> Result<(String, String)> {
        let (editor, ink) = self.project(snapshot)?;
        Ok((projection_value_hash(&editor)?, projection_value_hash(&ink)?))
    }
    /// A host may use an exact snapshot identity in place of two costly
    /// deterministic projection evaluations. Older pending baselines remain
    /// readable and are still checked with their original projection recipe.
    fn accepts_snapshot_identity(&self) -> bool { false }
}

fn projection_hashes(verifier: &dyn ProjectionVerifier, snapshot: &Val) -> Result<(String, String)> {
    if verifier.accepts_snapshot_identity() { Ok((snapshot_metadata_identity(snapshot)?, "snapshot-identity-v2".into())) }
    else { verifier.project_hashes(snapshot) }
}

// Resource XML is checked against the raw multipart before publication.
// Its exact regeneration therefore follows from the raw digest and the
// unchanged parser recipe; hashing the remaining fields binds all metadata.
fn snapshot_metadata_identity(snapshot: &Val) -> Result<String> {
    let Val::Dict(items) = snapshot else { return Err(Error::Value("invalid snapshot identity".into())); };
    let fields = items.iter().filter(|(key,_)| !matches!(key.as_str(), "raw_body" | "resource_xml" | "container_sha256" | "container_size" | "context_sha256" | "manifest_sha256"))
        .cloned().collect();
    snapshot_identity(&Val::Dict(fields))
}

fn verify_raw_reference(store: &DocumentStore, value: &Val) -> Result<()> {
    let reference = raw_reference(value)?;
    let file = store.open_raw(&reference)?;
    if file.metadata()?.len() != reference.size { return Err(Error::Value("container size mismatch".into())); }
    let rebuilt = crate::note_container::reconstruct(file).map_err(Error::Value)?;
    let expected = value.get("raw_sha256").and_then(Val::as_str).ok_or_else(|| Error::Value("missing snapshot digest".into()))?;
    let content_type = value.get("content_type").and_then(Val::as_str).ok_or_else(|| Error::Value("missing snapshot content type".into()))?;
    if format!("{:x}", Sha256::digest(&rebuilt.raw_body)) != expected || rebuilt.content_type != content_type {
        return Err(Error::Value("reconstructed snapshot verification failed".into()));
    }
    Ok(())
}

fn snapshot_identity(snapshot: &Val) -> Result<String> {
    let Val::Dict(items) = snapshot else { return Err(Error::Value("invalid snapshot identity".into())); };
    let mut fields: Vec<_> = items.iter().collect();
    fields.sort_by(|a,b| a.0.cmp(&b.0));
    if fields.windows(2).any(|w| w[0].0 == w[1].0) { return Err(Error::Value("duplicate snapshot field".into())); }
    let mut hash = ProjectionHasher::new();
    hash.literal(b"[")?;
    for (i, (key, value)) in fields.into_iter().enumerate() {
        if i != 0 { hash.literal(b", ")?; }
        hash.literal(b"[")?;
        hash.value(&Val::s(key))?;
        hash.literal(b", ")?;
        hash.value(value)?;
        hash.literal(b"]")?;
    }
    hash.literal(b"]")?;
    hash.finish()
}

fn page_index_hash<'a>(revision: i64, pages: impl IntoIterator<Item = (&'a str, &'a str, &'a str, bool)>) -> Result<String> {
    let mut hash = ProjectionHasher::new(); hash.literal(b"[")?;
    for (index, (id, page, xml, ink)) in pages.into_iter().enumerate() {
        if index != 0 { hash.literal(b", ")?; }
        hash.value(&Val::List(vec![Val::s(id), Val::Int(index as i64), Val::s(page), Val::s(xml), Val::Bool(ink), Val::Int(revision)]))?;
    }
    hash.literal(b"]")?; hash.finish()
}

struct DigestWriter(Sha256);
impl Write for DigestWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> { self.0.update(data); Ok(data.len()) }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

/// Hash the exact `dumps_utf8` representation without materializing its JSON.
pub struct ProjectionHasher(BufWriter<DigestWriter>);
impl ProjectionHasher {
    pub fn new() -> Self { Self(BufWriter::new(DigestWriter(Sha256::new()))) }
    pub fn literal(&mut self, value: &[u8]) -> Result<()> { self.0.write_all(value)?; Ok(()) }
    fn string(&mut self, text: &str) -> Result<()> {
        self.literal(b"\"")?;
        let mut start = 0;
        for (i, c) in text.char_indices() {
            let escaped = match c {
                '"' => Some("\\\""), '\\' => Some("\\\\"), '\n' => Some("\\n"), '\r' => Some("\\r"),
                '\t' => Some("\\t"), '\u{8}' => Some("\\b"), '\u{c}' => Some("\\f"),
                c if (c as u32) < 0x20 => Some(""), _ => None,
            };
            if let Some(escaped) = escaped {
                self.literal(&text.as_bytes()[start..i])?;
                if escaped.is_empty() { write!(&mut self.0, "\\u{:04x}", c as u32)?; } else { self.literal(escaped.as_bytes())?; }
                start = i + c.len_utf8();
            }
        }
        self.literal(&text.as_bytes()[start..])?;
        self.literal(b"\"")
    }
    pub fn value(&mut self, value: &Val) -> Result<()> {
        match value {
            Val::Str(s) => self.string(s),
            Val::List(items) => {
                self.literal(b"[")?;
                for (i, item) in items.iter().enumerate() { if i != 0 { self.literal(b", ")?; } self.value(item)?; }
                self.literal(b"]")
            }
            Val::Dict(items) => {
                self.literal(b"{")?;
                for (i, (key, item)) in items.iter().enumerate() { if i != 0 { self.literal(b", ")?; } self.string(key)?; self.literal(b": ")?; self.value(item)?; }
                self.literal(b"}")
            }
            // Scalars have bounded output except BigInt, which is already a
            // canonical string and can be written directly.
            Val::BigInt(n) => self.literal(n.as_bytes()),
            _ => self.literal(dumps_utf8(value)?.as_bytes()),
        }
    }
    pub fn finish(mut self) -> Result<String> {
        self.0.flush()?;
        Ok(format!("{:x}", self.0.into_inner().map_err(|e| Error::Io(e.into_error()))?.0.finalize()))
    }
}
impl Default for ProjectionHasher { fn default() -> Self { Self::new() } }
pub fn projection_value_hash(value: &Val) -> Result<String> {
    let mut hash = ProjectionHasher::new(); hash.value(value)?; hash.finish()
}

#[cfg(test)]
fn test_crash_at(point: &str) {
    if std::env::var("NOTES_TEST_MIGRATION_CRASH").ok().as_deref() == Some(point) {
        std::process::exit(86);
    }
}

/// Convert all references in a single transaction after publishing and
/// verifying every container. The publication lock remains held until commit.
/// On any error the schema transaction rolls back; orphan files are harmless
/// and a later run deduplicates them.
pub fn migrate_v7(directory: &Path, database: &Path, verifier: &dyn ProjectionVerifier) -> Result<()> {
    migrate_inner(directory, database, verifier, false, None)
}

/// Report note-scale progress throughout conversion and committed verification.
pub fn migrate_v7_with_progress(directory: &Path, database: &Path, verifier: &dyn ProjectionVerifier, progress: &dyn Fn(u64, u64)) -> Result<()> {
    migrate_inner(directory, database, verifier, false, Some(progress))
}

fn check_disk_space(directory: &Path, database: &Path) -> Result<()> {
    let db_size = std::fs::metadata(database)?.len() as u128;
    let path = CString::new(directory.as_os_str().as_bytes())
        .map_err(|_| Error::Value("invalid library path".into()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is a NUL-terminated path, and stat points to valid storage.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: statvfs returned success and initialized the struct.
    let stat = unsafe { stat.assume_init() };
    let free = stat.f_bavail as u128 * stat.f_frsize as u128;
    if free * 5 < db_size * 6 {
        return Err(Error::Value(
            "insufficient free space for schema 7 migration".into(),
        ));
    }
    Ok(())
}

fn migrate_inner(directory: &Path, database: &Path, verifier: &dyn ProjectionVerifier, fail_before_commit: bool, progress: Option<&dyn Fn(u64,u64)>) -> Result<()> {
    let store = DocumentStore::open(directory)?;
    let finalized = store.with_write(|writer| {
        let transaction_started = std::time::Instant::now();
        let converted = std::cell::Cell::new(false);
        let pending = with_db(database, |db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let version = db.one("SELECT value FROM metadata WHERE key='schema_version'", &[])?;
            match version.as_ref().and_then(|v| v.first()) {
                Some(rusqlite::types::Value::Text(v)) if v == "7" => return Ok(db.one("SELECT 1 FROM metadata WHERE key='schema7_vacuum_pending' AND value='true'", &[])?.is_some()),
                Some(rusqlite::types::Value::Text(v)) if v == "6" => {},
                _ => return Err(Error::Value("schema 7 migration requires schema 6".into())),
            }
            converted.set(true);
            check_disk_space(directory, database)?;
            for table in ["jobs", "organization_jobs", "folder_jobs"] {
                if db.one(&format!("SELECT 1 FROM {table} WHERE state='running' LIMIT 1"), &[])?.is_some() {
                    return Err(Error::Value("cannot migrate while a job is running".into()));
                }
            }

            // Schema changes and each row conversion share one transaction. A
            // failure rolls them back while published files remain harmless
            // orphans for a later idempotent retry.
            for column in ["container_sha256 TEXT", "container_size INTEGER", "raw_sha256 TEXT", "container_metadata TEXT", "remote_container_metadata TEXT"] {
                db.exec(&format!("ALTER TABLE notes ADD COLUMN {column}"), &[])?;
            }
            db.exec("CREATE TABLE note_pages (note_id TEXT NOT NULL REFERENCES notes(id), revision INTEGER NOT NULL, page_id TEXT NOT NULL, idx INTEGER NOT NULL, page_sha256 TEXT NOT NULL, xml_sha256 TEXT NOT NULL, has_ink INTEGER NOT NULL, PRIMARY KEY(note_id,revision,page_id))", &[])?;
            db.exec("CREATE TABLE schema7_projection_baseline(kind TEXT NOT NULL,id TEXT NOT NULL,editor TEXT NOT NULL,ink TEXT NOT NULL,PRIMARY KEY(kind,id))", &[])?;
            db.exec("INSERT INTO metadata(key,value) VALUES('schema7_projection_recipe',?)", &p![verifier.recipe()])?;
            db.exec(crate::preview_inventory::SCHEMA, &[])?;
            db.exec("CREATE TABLE note_container_revisions (note_id TEXT NOT NULL REFERENCES notes(id), revision INTEGER NOT NULL, container_metadata TEXT NOT NULL, PRIMARY KEY(note_id,revision))", &[])?;

            // IDs remain stable throughout the transaction. A keyset query
            // fetches only one snapshot; base and remote are read separately.
            if db.one("SELECT 1 FROM notes WHERE id IS NULL LIMIT 1", &[])?.is_some() {
                return Err(Error::Value("invalid note id".into()));
            }
            let total = db.one("SELECT COUNT(*) FROM notes", &[])?.and_then(|r| match r.first() { Some(rusqlite::types::Value::Integer(n)) => Some(*n as u64), _ => None }).unwrap_or(0);
            if let Some(report) = progress { report(0, total); }
            let mut completed = 0u64;
            let mut previous: Option<String> = None;
            loop {
                let rows = if let Some(last) = &previous {
                    db.query("SELECT id,revision FROM notes WHERE id>? ORDER BY id LIMIT 1", &p![last])?
                } else {
                    db.query("SELECT id,revision FROM notes ORDER BY id LIMIT 1", &[])?
                };
                let Some(row) = rows.one() else { break };
                let id = row.text("id").ok_or_else(|| Error::Value("invalid note id".into()))?.to_owned();
                let revision = row.int("revision");
                drop(rows);
                for column in ["base_document", "remote_document"] {
                    let snapshot = {
                        let rows = db.query(&format!("SELECT {column} AS snapshot FROM notes WHERE id=?"), &p![&id])?;
                        let row = rows.one().ok_or_else(|| Error::Value("missing note during migration".into()))?;
                        if row.is_null("snapshot") { continue; }
                        loads_sql(row.raw("snapshot"))?
                    };
                    let (editor, mut ink) = projection_hashes(verifier, &snapshot)?;
                    let (reference, pages) = store_snapshot_with_pages(writer, &snapshot, None)?;
                    drop(snapshot);
                    if column == "base_document" && ink == "snapshot-identity-v2" {
                        ink.push(':');
                        ink.push_str(&page_index_hash(revision, pages.iter().map(|p| (p.page_id.as_str(), p.page_sha256.as_str(), p.xml_sha256.as_str(), p.has_ink)))?);
                    }
                    db.exec("INSERT INTO schema7_projection_baseline VALUES(?,?,?,?)", &p![column, &id, editor, ink])?;
                    let metadata = dumps_utf8(&reference)?;
                    if column == "base_document" {
                        let raw = reference.get("raw_sha256").and_then(Val::as_str).ok_or_else(|| Error::Value("invalid snapshot hash".into()))?;
                        let document = raw_reference(&reference)?;
                        let size = i64::try_from(document.size).map_err(|_| Error::Value("invalid container size".into()))?;
                        db.exec("UPDATE notes SET container_sha256=?,container_size=?,raw_sha256=?,container_metadata=?,base_document=NULL WHERE id=?", &p![&document.sha256, size, raw, &metadata, &id])?;
                        db.exec("INSERT INTO note_container_revisions(note_id,revision,container_metadata) VALUES (?,?,?)", &p![&id, revision, &metadata])?;
                        for (idx, page) in pages.iter().enumerate() {
                            db.exec("INSERT INTO note_pages(note_id,revision,page_id,idx,page_sha256,xml_sha256,has_ink) VALUES (?,?,?,?,?,?,?)", &p![&id, revision, &page.page_id, idx as i64, &page.page_sha256, &page.xml_sha256, page.has_ink])?;
                        }
                    } else {
                        db.exec("UPDATE notes SET remote_container_metadata=?,remote_document=NULL WHERE id=?", &p![&metadata, &id])?;
                    }
                }
                completed += 1;
                if let Some(report) = progress { report(completed, total); }
                previous = Some(id);
            }
            for (table, column, key) in [
                ("jobs", "snapshot", "id"),
                ("organization_jobs", "snapshot", "id"),
                ("ink_jobs", "current_snapshot", "job_id"),
                ("ink_erase_jobs", "current_snapshot", "job_id"),
                ("ink_move_jobs", "current_snapshot", "job_id"),
            ] {
                if db.one(&format!("SELECT 1 FROM {table} WHERE {key} IS NULL LIMIT 1"), &[])?.is_some() {
                    return Err(Error::Value("invalid job id".into()));
                }
                let active = if table == "organization_jobs" {
                    "state NOT IN ('done','cancelled') AND NOT (state='uncertain' AND snapshot='{}' AND COALESCE(error,'')='Eski işlemin kaynak sürümü kayıtlı değil; yeniden gönderilmedi.')".to_string()
                } else if table == "jobs" {
                    "state NOT IN ('done','cancelled')".to_string()
                } else {
                    format!("NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.id={table}.job_id AND jobs.state IN ('done','cancelled'))")
                };
                let mut previous: Option<String> = None;
                loop {
                    let (lower_bound, parameters) = match &previous {
                        Some(last) => (format!(" AND {key}>?"), p![last]),
                        None => (String::new(), Vec::new()),
                    };
                    let rows = db.query(&format!("SELECT {key} AS identity FROM {table} WHERE {column} IS NOT NULL AND {active}{lower_bound} ORDER BY {key} LIMIT 1"), &parameters)?;
                    let Some(row) = rows.one() else { break };
                    let id = row.text("identity").ok_or_else(|| Error::Value("invalid job id".into()))?.to_owned();
                    drop(rows);
                    let original = {
                        let rows = db.query(&format!("SELECT {column} AS snapshot FROM {table} WHERE {key}=?"), &p![&id])?;
                        let row = rows.one().ok_or_else(|| Error::Value("missing job during migration".into()))?;
                        loads_sql(row.raw("snapshot"))?
                    };
                    let (editor, ink) = projection_hashes(verifier, &original)?;
                    let (snapshot, _) = store_snapshot_with_pages(writer, &original, None)?;
                    drop(original);
                    db.exec("INSERT INTO schema7_projection_baseline VALUES(?,?,?,?)", &p![format!("{table}.{column}"), &id, editor, ink])?;
                    db.exec(&format!("UPDATE {table} SET {column}=? WHERE {key}=?"), &p![dumps_utf8(&snapshot)?, &id])?;
                    previous = Some(id);
                }
            }
            db.exec("UPDATE jobs SET snapshot=NULL WHERE state IN ('done','cancelled')", &[])?;
            db.exec("UPDATE organization_jobs SET snapshot='{}' WHERE state IN ('done','cancelled')", &[])?;
            for table in ["ink_jobs", "ink_erase_jobs", "ink_move_jobs"] {
                db.exec(&format!("UPDATE {table} SET current_snapshot='{{}}' WHERE job_id IN (SELECT id FROM jobs WHERE state IN ('done','cancelled'))"), &[])?;
            }
            if fail_before_commit {
                return Err(Error::Value("injected migration interruption".into()));
            }
            db.exec("INSERT INTO metadata(key,value) VALUES ('schema7_vacuum_pending','true') ON CONFLICT(key) DO UPDATE SET value='true'", &[])?;
            db.exec("UPDATE metadata SET value='7' WHERE key='schema_version'", &[])?;
            #[cfg(test)]
            test_crash_at("before-commit");
            Ok(true)
        })?;
        let detailed = std::env::var_os("NOTES_MIGRATION_PROFILE").is_some();
        if detailed { eprintln!("migration-transaction elapsed_ms={}", transaction_started.elapsed().as_millis()); }
        if !pending { return Ok(false); }
        #[cfg(test)]
        test_crash_at("after-commit");
        let verify_started = std::time::Instant::now();
        verify_committed(&store, database, verifier, progress, converted.get())?;
        if detailed { eprintln!("migration-verify elapsed_ms={}", verify_started.elapsed().as_millis()); }
        let vacuum_started = std::time::Instant::now();
        vacuum_if_pending(database)?;
        if detailed { eprintln!("migration-vacuum elapsed_ms={}", vacuum_started.elapsed().as_millis()); }
        Ok(true)
    })?;
    // The transaction, reconstructed snapshots and projection values are all
    // dropped before asking glibc to return unused heap pages to the OS.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if finalized {
        // SAFETY: malloc_trim has no pointer arguments and is thread-safe.
        unsafe { libc::malloc_trim(0); }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let _ = finalized;
    Ok(())
}

/// Reopen committed references and compare every page index against a fresh
/// lossless reconstruction before replacing or reclaiming the SQLite file.
fn verify_committed(store: &DocumentStore, database: &Path, verifier: &dyn ProjectionVerifier, progress: Option<&dyn Fn(u64,u64)>, already_converted: bool) -> Result<()> {
    struct VerificationTemp(std::path::PathBuf);
    impl Drop for VerificationTemp {
        fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
    }
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let conn = rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let pending: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM metadata WHERE key='schema7_vacuum_pending' AND value='true')", [], |r| r.get(0))?;
    if !pending { return Ok(()); }
    let recipe: Option<String> = conn.query_row("SELECT value FROM metadata WHERE key='schema7_projection_recipe'", [], |r| r.get(0)).optional()?;
    if recipe.as_deref() != Some(verifier.recipe().as_str()) {
        return Err(Error::Value("schema 7 projection baseline recipe missing or incompatible".into()));
    }
    let mut checked = 0i64;
    let mut check = |kind: &str, id: &str, snapshot: &Val| -> Result<()> {
        let expected: Option<(String, String)> = conn.query_row(
            "SELECT editor,ink FROM schema7_projection_baseline WHERE kind=? AND id=?",
            [kind, id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let expected = expected.ok_or_else(|| Error::Value("missing schema 7 projection baseline".into()))?;
        if kind != "base_document" && expected.1.contains(':') { return Err(Error::Value("invalid snapshot baseline".into())); }
        let actual = if expected.1 == "snapshot-identity-v2" || expected.1.starts_with("snapshot-identity-v2:") {
            if !verifier.accepts_snapshot_identity() { return Err(Error::Value("snapshot identity verification unavailable".into())); }
            (snapshot_metadata_identity(snapshot)?, expected.1.clone())
        } else if expected.1 == "snapshot-identity-v1" || expected.1.starts_with("snapshot-identity-v1:") {
            if !verifier.accepts_snapshot_identity() { return Err(Error::Value("snapshot identity verification unavailable".into())); }
            (snapshot_identity(snapshot)?, expected.1.clone())
        } else { verifier.project_hashes(snapshot)? };
        if actual != expected {
            return Err(Error::Value("schema 7 projection verification failed".into()));
        }
        checked += 1;
        Ok(())
    };
    let total = conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0))? as u64;
    if let Some(report) = progress { report(if already_converted { total } else { 0 }, total); }
    let mut processed = 0u64;
    let mut notes = conn.prepare("SELECT id,revision,container_sha256,container_size,raw_sha256,container_metadata,remote_container_metadata FROM notes")?;
    let rows = notes.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?)))?;
    for row in rows {
        let (id, revision, hash, size, raw_hash, metadata, remote) = row?;
        if let Some(encoded) = metadata {
            let value = crate::val::loads(&encoded)?;
            let reference = raw_reference(&value)?;
            if hash.as_deref() != Some(reference.sha256.as_str()) || size != Some(reference.size as i64)
                || raw_hash.as_deref() != value.get("raw_sha256").and_then(Val::as_str) {
                return Err(Error::Value("committed container metadata mismatch".into()));
            }
            let baseline: String = conn.query_row("SELECT ink FROM schema7_projection_baseline WHERE kind='base_document' AND id=?", [&id], |r| r.get(0))?;
            let snapshot = if baseline.starts_with("snapshot-identity-v2:") { verify_raw_reference(store, &value)?; value.clone() } else { crate::snapshot_store::load_snapshot(store, &value)? };
            check("base_document", &id, &snapshot)?;
            if let Some(expected) = baseline.strip_prefix("snapshot-identity-v2:").or_else(|| baseline.strip_prefix("snapshot-identity-v1:")) {
                let mut pages = conn.prepare("SELECT page_id,idx,page_sha256,xml_sha256,has_ink,revision FROM note_pages WHERE note_id=? ORDER BY idx")?;
                let rows = pages.query_map([&id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,bool>(4)?,r.get::<_,i64>(5)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
                if rows.iter().enumerate().any(|(i, row)| row.1 != i as i64 || row.5 != revision)
                    || page_index_hash(revision, rows.iter().map(|p| (p.0.as_str(), p.2.as_str(), p.3.as_str(), p.4)))? != expected {
                    return Err(Error::Value("committed page index mismatch".into()));
                }
            } else {
            let raw = snapshot.get("raw_body").and_then(Val::as_str).ok_or_else(|| Error::Value("invalid snapshot body".into()))?;
            let raw = STANDARD.decode(raw).map_err(|_| Error::Value("invalid snapshot body".into()))?;
            let content_type = snapshot.get("content_type").and_then(Val::as_str).ok_or_else(|| Error::Value("invalid snapshot content type".into()))?;
            // Verify through a temporary seekable file rather than retaining a
            // second complete compressed container in memory.
            let (path, mut output) = store.create_raw_temp()?;
            let temp = VerificationTemp(path);
            let built = crate::note_container::build_to(&raw, content_type, None, &mut output).map_err(Error::Value)?;
            drop(output);
            drop(temp);
            let mut pages = conn.prepare("SELECT page_id,idx,page_sha256,xml_sha256,has_ink,revision FROM note_pages WHERE note_id=? ORDER BY idx")?;
            let index = pages.query_map([&id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,bool>(4)?,r.get::<_,i64>(5)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            if index.len() != built.pages.len() { return Err(Error::Value("committed page index count mismatch".into())); }
            for (i, (page, entry)) in built.pages.iter().zip(&index).enumerate() {
                if entry.0 != page.page_id || entry.1 != i as i64 || entry.2 != page.page_sha256 || entry.3 != page.xml_sha256 || entry.4 != page.has_ink || entry.5 != revision {
                    return Err(Error::Value("committed page index mismatch".into()));
                }
            }
            }
        } else if hash.is_some() || size.is_some() || raw_hash.is_some() {
            return Err(Error::Value("incomplete committed container metadata".into()));
        }
        if let Some(encoded) = remote {
            let value = crate::val::loads(&encoded)?;
            let baseline: String = conn.query_row("SELECT ink FROM schema7_projection_baseline WHERE kind='remote_document' AND id=?", [&id], |r| r.get(0))?;
            let snapshot = if baseline == "snapshot-identity-v2" { verify_raw_reference(store, &value)?; value } else { crate::snapshot_store::load_snapshot(store, &value)? };
            check("remote_document", &id, &snapshot)?;
        }
        processed += 1;
        if !already_converted { if let Some(report) = progress { report(processed, total); } }
    }
    drop(notes);
    for (table, column, key, condition) in [
        ("jobs", "snapshot", "id", "state NOT IN ('done','cancelled')"),
        ("organization_jobs", "snapshot", "id", "state NOT IN ('done','cancelled') AND NOT (state='uncertain' AND snapshot='{}' AND COALESCE(error,'')='Eski işlemin kaynak sürümü kayıtlı değil; yeniden gönderilmedi.')"),
        ("ink_jobs", "current_snapshot", "job_id", "NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.id=ink_jobs.job_id AND jobs.state IN ('done','cancelled'))"),
        ("ink_erase_jobs", "current_snapshot", "job_id", "NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.id=ink_erase_jobs.job_id AND jobs.state IN ('done','cancelled'))"),
        ("ink_move_jobs", "current_snapshot", "job_id", "NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.id=ink_move_jobs.job_id AND jobs.state IN ('done','cancelled'))"),
    ] {
        let mut stmt = conn.prepare(&format!("SELECT {key},{column} FROM {table} WHERE {column} IS NOT NULL AND {condition}"))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?;
        for row in rows {
            let (id, encoded) = row?; let value = crate::val::loads(&encoded)?; let kind = format!("{table}.{column}");
            let baseline: String = conn.query_row("SELECT ink FROM schema7_projection_baseline WHERE kind=? AND id=?", [&kind, &id], |r| r.get(0))?;
            let snapshot = if baseline == "snapshot-identity-v2" { verify_raw_reference(store, &value)?; value } else { crate::snapshot_store::load_snapshot(store, &value)? };
            check(&kind, &id, &snapshot)?;
        }
    }
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM schema7_projection_baseline", [], |row| row.get(0))?;
    if checked != count { return Err(Error::Value("unexpected schema 7 projection baseline".into())); }
    Ok(())
}

/// The checked copy and the pending marker survive either side of the
/// atomic rename. Caller holds both engine.lock and the publication lock.
fn vacuum_if_pending(database: &Path) -> Result<()> {
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let conn = rusqlite::Connection::open(database)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='schema7_vacuum_pending' AND value='true')",
        [], |row| row.get(0),
    )?;
    if !pending { return Ok(()); }
    let (busy, _, _): (i64, i64, i64) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    if busy != 0 { return Err(Error::Value("cannot checkpoint database before VACUUM".into())); }
    let parent = database.parent().ok_or_else(|| Error::Value("invalid database path".into()))?;
    let name = database.file_name().and_then(|s| s.to_str()).ok_or_else(|| Error::Value("invalid database path".into()))?;
    let temp = parent.join(format!(".{name}.schema7-vacuum.tmp"));
    match fs::symlink_metadata(&temp) {
        Ok(meta) => {
            // SAFETY: getuid has no preconditions.
            let uid = unsafe { libc::getuid() };
            if !meta.file_type().is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
                return Err(Error::Value("unsafe pending VACUUM file".into()));
            }
            fs::remove_file(&temp)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.into()),
    }
    let temp_file = OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(&temp)?;
    drop(temp_file);
    conn.execute("VACUUM INTO ?", [temp.to_str().ok_or_else(|| Error::Value("invalid database path".into()))?])?;
    conn.close().map_err(|(_, e)| Error::from(e))?;
    let candidate = rusqlite::Connection::open_with_flags(&temp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = candidate.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let version: String = candidate.query_row("SELECT value FROM metadata WHERE key='schema_version'", [], |row| row.get(0))?;
    let candidate_pending: bool = candidate.query_row("SELECT EXISTS(SELECT 1 FROM metadata WHERE key='schema7_vacuum_pending' AND value='true')", [], |row| row.get(0))?;
    if integrity != "ok" || version != "7" || !candidate_pending {
        return Err(Error::Value("VACUUM copy verification failed".into()));
    }
    candidate.close().map_err(|(_, e)| Error::from(e))?;
    File::open(&temp)?.sync_all()?;
    File::open(parent)?.sync_all()?;
    let mut wal_name = database.as_os_str().to_os_string();
    wal_name.push("-wal");
    match fs::symlink_metadata(Path::new(&wal_name)) {
        Ok(_) => return Err(Error::Value("database WAL remains open during VACUUM".into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.into()),
    }
    let mut shm_name = database.as_os_str().to_os_string();
    shm_name.push("-shm");
    match fs::symlink_metadata(Path::new(&shm_name)) {
        Ok(_) => return Err(Error::Value("database shared memory remains open during VACUUM".into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.into()),
    }
    #[cfg(test)]
    test_crash_at("before-rename");
    fs::rename(&temp, database)?;
    File::open(parent)?.sync_all()?;
    #[cfg(test)]
    test_crash_at("after-rename");
    let conn = rusqlite::Connection::open(database)?;
    conn.execute_batch("BEGIN IMMEDIATE; DROP TABLE schema7_projection_baseline; DELETE FROM metadata WHERE key IN ('schema7_vacuum_pending','schema7_projection_recipe'); COMMIT;")?;
    conn.close().map_err(|(_, e)| Error::from(e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_store::load_snapshot;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rusqlite::{params, Connection};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn streaming_projection_hash_matches_existing_json_bytes() {
        let values = [
            Val::Null, Val::Bool(true), Val::Int(-17), Val::BigInt("1234567890123456789012345".into()),
            Val::Float(-0.0), Val::Float(f64::INFINITY), Val::Float(f64::NAN),
            Val::s("Türkçe 💚 \" \\ \n \t \r \u{1} \u{8} \u{c}"),
            Val::dict(vec![("a\n", Val::List(vec![Val::s("ç"), Val::Float(1.25), Val::Null]))]),
        ];
        for value in values {
            let expected = format!("{:x}", Sha256::digest(dumps_utf8(&value).unwrap().as_bytes()));
            assert_eq!(projection_value_hash(&value).unwrap(), expected);
        }
        assert!(projection_value_hash(&Val::Bytes(vec![1])).is_err());
    }

    #[test]
    fn pinned_raw_snapshot_survives_pointer_change_and_collection() {
        let (temp, original) = fixture();
        let path = temp.0.join("library.sqlite3");
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        let store = crate::storage::StoreCore { directory: temp.0.clone(), path };
        let (row, pinned) = store.pin_raw("n").unwrap();
        assert_eq!(pinned.len(), 2);
        let mut replacement = crate::val::loads(&original).unwrap();
        let new_raw = b"<noteResource>new snapshot</noteResource>";
        let new_resource = rs_doc::doc::parse_document(new_raw, "application/xml", None).unwrap().resource_xml;
        replacement.set("raw_body", Val::s(STANDARD.encode(new_raw)));
        replacement.set("resource_xml", Val::s(STANDARD.encode(new_resource)));
        replacement.set("raw_sha256", Val::s(format!("{:x}", Sha256::digest(new_raw))));
        replacement.set("title", Val::s("replacement"));
        // Simulate a committed pointer replacement while the reader
        // is paused after pinning. This must not wait for reconstruction.
        let docs = DocumentStore::open(&temp.0).unwrap();
        docs.with_write(|writer| {
            let (metadata, _) = store_snapshot_with_pages(writer, &replacement, None)?;
            let reference = raw_reference(&metadata)?;
            let encoded = dumps_utf8(&metadata)?;
            store.with_db(|db| {
            db.exec("DELETE FROM note_pages", &[])?;
            db.exec("DELETE FROM note_container_revisions", &[])?;
            db.exec("DELETE FROM ink_jobs", &[])?;
            db.exec("DELETE FROM ink_erase_jobs", &[])?;
            db.exec("DELETE FROM ink_move_jobs", &[])?;
            db.exec("DELETE FROM jobs", &[])?;
            db.exec("DELETE FROM organization_jobs", &[])?;
            db.exec("UPDATE notes SET container_sha256=?,container_size=?,raw_sha256=?,container_metadata=?,remote_container_metadata=NULL WHERE id='n'", &p![&reference.sha256, reference.size as i64, metadata.get("raw_sha256").unwrap().as_str().unwrap(), encoded])?;
            Ok(())
        }) }).unwrap();
        let collected = store.collect_containers(std::time::Duration::ZERO).unwrap();
        assert!(collected.removed > 0);
        let hydrated = store.hydrate_pinned_raw(row, pinned).unwrap();
        for field in ["base_document", "remote_document"] {
            let actual = crate::val::loads(hydrated.get(field).unwrap().as_str().unwrap()).unwrap();
            assert!(crate::val::py_eq(&actual, &crate::val::loads(&original).unwrap()));
        }
        let current = store.raw("n").unwrap();
        let current = crate::val::loads(current.get("base_document").unwrap().as_str().unwrap()).unwrap();
        assert!(crate::val::py_eq(&current, &replacement));
    }

    struct TestVerifier;
    impl ProjectionVerifier for TestVerifier {
        fn recipe(&self) -> String { "synthetic-test-v1".into() }
        fn project(&self, snapshot: &Val) -> Result<(Val, Val)> {
            Ok((snapshot.get_or_null("raw_sha256").clone(), snapshot.get_or_null("title").clone()))
        }
    }

    struct IdentityVerifier;
    impl ProjectionVerifier for IdentityVerifier {
        fn recipe(&self) -> String { "synthetic-identity-v1".into() }
        fn project(&self, _: &Val) -> Result<(Val, Val)> { panic!("identity verification must not build projections") }
        fn accepts_snapshot_identity(&self) -> bool { true }
    }

    #[test]
    fn snapshot_identity_is_order_independent_and_covers_every_field() {
        let first = Val::dict(vec![("raw_body", Val::s("abc")), ("title", Val::s("before"))]);
        let reversed = Val::dict(vec![("title", Val::s("before")), ("raw_body", Val::s("abc"))]);
        let changed = Val::dict(vec![("raw_body", Val::s("abc")), ("title", Val::s("after"))]);
        assert_eq!(snapshot_identity(&first).unwrap(), snapshot_identity(&reversed).unwrap());
        assert_ne!(snapshot_identity(&first).unwrap(), snapshot_identity(&changed).unwrap());
        let (temp, original) = fixture();
        let path = temp.0.join("library.sqlite3");
        migrate_v7(&temp.0, &path, &IdentityVerifier).unwrap();
        let db = Connection::open(&path).unwrap();
        let encoded: String = db.query_row("SELECT container_metadata FROM notes", [], |r| r.get(0)).unwrap();
        let actual = load_snapshot(&DocumentStore::open(&temp.0).unwrap(), &crate::val::loads(&encoded).unwrap()).unwrap();
        assert_eq!(snapshot_identity(&actual).unwrap(), snapshot_identity(&crate::val::loads(&original).unwrap()).unwrap());
    }

    #[test]
    fn progress_counts_notes_and_never_moves_backwards() {
        let (temp, _) = fixture();
        let reports = std::cell::RefCell::new(Vec::new());
        migrate_v7_with_progress(&temp.0, &temp.0.join("library.sqlite3"), &IdentityVerifier, &|done,total| reports.borrow_mut().push((done,total))).unwrap();
        let reports = reports.into_inner();
        assert_eq!(reports.first(), Some(&(0,1)));
        assert_eq!(reports.last(), Some(&(1,1)));
        assert!(reports.windows(2).all(|w| w[0].0 <= w[1].0));
    }

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("schema7-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (Temp, String) {
        let temp = Temp::new();
        let db = Connection::open(temp.0.join("library.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            INSERT INTO metadata VALUES('schema_version','6');
            CREATE TABLE notes(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, base_document TEXT, remote_document TEXT);
            CREATE TABLE jobs(id TEXT PRIMARY KEY, snapshot TEXT, state TEXT NOT NULL);
            CREATE TABLE organization_jobs(id TEXT PRIMARY KEY, snapshot TEXT NOT NULL, state TEXT NOT NULL, error TEXT);
            CREATE TABLE folder_jobs(id TEXT PRIMARY KEY, state TEXT NOT NULL);
            CREATE TABLE ink_jobs(job_id TEXT PRIMARY KEY, current_snapshot TEXT NOT NULL);
            CREATE TABLE ink_erase_jobs(job_id TEXT PRIMARY KEY, current_snapshot TEXT NOT NULL);
            CREATE TABLE ink_move_jobs(job_id TEXT PRIMARY KEY, current_snapshot TEXT NOT NULL);").unwrap();
        let bytes = b"<noteResource/>";
        let resource = rs_doc::doc::parse_document(bytes, "application/xml", None)
            .unwrap()
            .resource_xml;
        let snapshot = Val::dict(vec![
            ("schemaVersion", Val::Int(1)),
            ("content_type", Val::s("application/xml")),
            ("raw_body", Val::s(&STANDARD.encode(bytes))),
            ("resource_xml", Val::s(&STANDARD.encode(resource))),
            (
                "raw_sha256",
                Val::s(&format!("{:x}", Sha256::digest(bytes))),
            ),
            ("title", Val::s("synthetic")),
        ]);
        let json = dumps_utf8(&snapshot).unwrap();
        db.execute("INSERT INTO notes VALUES('n',3,?,?)", params![json, json])
            .unwrap();
        db.execute("INSERT INTO jobs VALUES('j',?,'queued')", params![json])
            .unwrap();
        db.execute(
            "INSERT INTO organization_jobs(id,snapshot,state) VALUES('o',?,'uncertain')",
            params![json],
        )
        .unwrap();
        db.execute("INSERT INTO ink_jobs VALUES('j',?)", params![json])
            .unwrap();
        db.execute("INSERT INTO ink_erase_jobs VALUES('e',?)", params![json])
            .unwrap();
        db.execute("INSERT INTO ink_move_jobs VALUES('m',?)", params![json])
            .unwrap();
        (temp, json)
    }

    #[test]
    fn subprocess_migration_helper() {
        let Some(directory) = std::env::var_os("NOTES_TEST_MIGRATION_DIRECTORY") else { return };
        let directory = std::path::PathBuf::from(directory);
        if std::env::var_os("NOTES_TEST_IDENTITY_VERIFIER").is_some() {
            migrate_v7(&directory, &directory.join("library.sqlite3"), &IdentityVerifier).unwrap();
        } else {
            migrate_v7(&directory, &directory.join("library.sqlite3"), &TestVerifier).unwrap();
        }
        if std::env::var_os("NOTES_TEST_MIGRATION_RSS").is_some() {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            // SAFETY: RUSAGE_SELF initializes the supplied rusage on success.
            assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) }, 0);
            let usage = unsafe { usage.assume_init() };
            println!("migration-max-rss-kib={}", usage.ru_maxrss);
            #[cfg(target_os = "linux")]
            {
                let status = std::fs::read_to_string("/proc/self/status").unwrap();
                let rss = status.lines().find_map(|line| line.strip_prefix("VmRSS:")).unwrap().split_whitespace().next().unwrap();
                println!("migration-idle-rss-kib={rss}");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn migration_memory_does_not_scale_with_library_snapshot_bytes() {
        fn run(count: usize) -> (i64, i64) {
            let (temp, original) = fixture();
            let path = temp.0.join("library.sqlite3");
            let mut raw = b"<noteResource>".to_vec();
            raw.extend(std::iter::repeat_n(b'a', 256 * 1024));
            raw.extend_from_slice(b"</noteResource>");
            let resource = rs_doc::doc::parse_document(&raw, "application/xml", None).unwrap().resource_xml;
            let mut snapshot = crate::val::loads(&original).unwrap();
            snapshot.set("raw_body", Val::s(&STANDARD.encode(&raw)));
            snapshot.set("resource_xml", Val::s(&STANDARD.encode(resource)));
            snapshot.set("raw_sha256", Val::s(&format!("{:x}", Sha256::digest(&raw))));
            let json = dumps_utf8(&snapshot).unwrap();
            let db = Connection::open(&path).unwrap();
            db.execute("DELETE FROM notes", []).unwrap();
            for i in 0..count {
                db.execute("INSERT INTO notes VALUES(?,3,?,NULL)", params![format!("n-{i:04}"), &json]).unwrap();
            }
            drop(db);
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "snapshot_migration::tests::subprocess_migration_helper", "--nocapture"])
                .env("NOTES_TEST_MIGRATION_DIRECTORY", &temp.0)
                .env("NOTES_TEST_MIGRATION_RSS", "1")
                .output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let stdout = String::from_utf8(output.stdout).unwrap();
            let value = |prefix| stdout.lines().find_map(|line| line.strip_prefix(prefix)).unwrap().parse().unwrap();
            (value("migration-max-rss-kib="), value("migration-idle-rss-kib="))
        }
        let one = run(1);
        let many = run(64);
        eprintln!("one note peak/idle: {}/{} KiB; 64 notes: {}/{} KiB", one.0, one.1, many.0, many.1);
        assert!(many.0 <= one.0 + 32 * 1024, "one note {} KiB; 64 notes {} KiB", one.0, many.0);
    }

    #[test]
    fn process_crashes_at_commit_and_vacuum_boundaries_recover() {
        for point in ["before-commit", "after-commit", "before-rename", "after-rename"] {
            let (temp, original) = fixture();
            let path = temp.0.join("library.sqlite3");
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "snapshot_migration::tests::subprocess_migration_helper"])
                .env("NOTES_TEST_MIGRATION_DIRECTORY", &temp.0)
                .env("NOTES_TEST_MIGRATION_CRASH", point)
                .status().unwrap();
            assert_eq!(status.code(), Some(86), "{point}");
            migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
            let db = Connection::open(&path).unwrap();
            let (version, pending, integrity): (String, i64, String) = db.query_row(
                "SELECT (SELECT value FROM metadata WHERE key='schema_version'),(SELECT count(*) FROM metadata WHERE key='schema7_vacuum_pending'),(SELECT integrity_check FROM pragma_integrity_check LIMIT 1)", [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap();
            assert_eq!((version.as_str(), pending, integrity.as_str()), ("7", 0, "ok"), "{point}");
            let encoded: String = db.query_row("SELECT container_metadata FROM notes WHERE id='n'", [], |r| r.get(0)).unwrap();
            let reference = crate::val::loads(&encoded).unwrap();
            let document = load_snapshot(&DocumentStore::open(&temp.0).unwrap(), &reference).unwrap();
            assert!(crate::val::py_eq(&document, &crate::val::loads(&original).unwrap()), "{point}");
        }
    }

    #[test]
    fn identity_baseline_rejects_changed_page_index_after_interruption() {
        let (temp, _) = fixture();
        let path = temp.0.join("library.sqlite3");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "snapshot_migration::tests::subprocess_migration_helper"])
            .env("NOTES_TEST_MIGRATION_DIRECTORY", &temp.0)
            .env("NOTES_TEST_IDENTITY_VERIFIER", "1")
            .env("NOTES_TEST_MIGRATION_CRASH", "after-commit")
            .status().unwrap();
        assert_eq!(status.code(), Some(86));
        let db = Connection::open(&path).unwrap();
        db.execute("INSERT INTO note_pages VALUES('n',3,'synthetic',0,'bad','bad',0)", []).unwrap();
        drop(db);
        assert!(migrate_v7(&temp.0, &path, &IdentityVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        db.execute("DELETE FROM note_pages WHERE note_id='n' AND page_id='synthetic'", []).unwrap();
        drop(db);
        migrate_v7(&temp.0, &path, &IdentityVerifier).unwrap();
    }

    #[test]
    fn projection_baseline_survives_crash_and_blocks_vacuum_on_mismatch() {
        let (temp, _) = fixture();
        let path = temp.0.join("library.sqlite3");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "snapshot_migration::tests::subprocess_migration_helper"])
            .env("NOTES_TEST_MIGRATION_DIRECTORY", &temp.0)
            .env("NOTES_TEST_MIGRATION_CRASH", "after-commit")
            .status().unwrap();
        assert_eq!(status.code(), Some(86));
        let db = Connection::open(&path).unwrap();
        let saved: String = db.query_row("SELECT editor FROM schema7_projection_baseline WHERE kind='base_document' AND id='n'", [], |r| r.get(0)).unwrap();
        db.execute("UPDATE schema7_projection_baseline SET editor='invalid' WHERE kind='base_document' AND id='n'", []).unwrap();
        drop(db);
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        let pending: String = db.query_row("SELECT value FROM metadata WHERE key='schema7_vacuum_pending'", [], |r| r.get(0)).unwrap();
        assert_eq!(pending, "true");
        db.execute("UPDATE schema7_projection_baseline SET editor=? WHERE kind='base_document' AND id='n'", [&saved]).unwrap();
        db.execute("UPDATE metadata SET value='other' WHERE key='schema7_projection_recipe'", []).unwrap();
        drop(db);
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        db.execute("UPDATE metadata SET value='synthetic-test-v1' WHERE key='schema7_projection_recipe'", []).unwrap();
        drop(db);
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        let db = Connection::open(&path).unwrap();
        let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema7_projection_baseline')", [], |r| r.get(0)).unwrap();
        assert!(!exists);
    }

    #[test]
    fn committed_verification_failure_preserves_database_and_can_retry() {
        let (temp, original) = fixture();
        let path = temp.0.join("library.sqlite3");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "snapshot_migration::tests::subprocess_migration_helper"])
            .env("NOTES_TEST_MIGRATION_DIRECTORY", &temp.0)
            .env("NOTES_TEST_MIGRATION_CRASH", "after-commit")
            .status().unwrap();
        assert_eq!(status.code(), Some(86));
        let db = Connection::open(&path).unwrap();
        let encoded: String = db.query_row("SELECT container_metadata FROM notes", [], |r| r.get(0)).unwrap();
        let reference = raw_reference(&crate::val::loads(&encoded).unwrap()).unwrap();
        drop(db);
        let stored = temp.0.join("sdocx").join(&reference.sha256[..2]).join(&reference.sha256[2..4]).join(format!("{}.sdocx",reference.sha256));
        let backup = stored.with_extension("saved");
        std::fs::rename(&stored, &backup).unwrap();
        std::fs::write(&stored, vec![0; reference.size as usize]).unwrap();
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        let pending: String = db.query_row("SELECT value FROM metadata WHERE key='schema7_vacuum_pending'", [], |r| r.get(0)).unwrap();
        assert_eq!(pending, "true");
        drop(db);
        std::fs::remove_file(&stored).unwrap();
        std::fs::rename(&backup, &stored).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute("INSERT INTO note_pages VALUES('n',3,'unexpected',0,'x','y',0)", []).unwrap();
        drop(db);
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        db.execute("DELETE FROM note_pages", []).unwrap();
        drop(db);
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        let actual = load_snapshot(&DocumentStore::open(&temp.0).unwrap(), &crate::val::loads(&encoded).unwrap()).unwrap();
        assert!(crate::val::py_eq(&actual, &crate::val::loads(&original).unwrap()));
    }

    #[test]
    fn live_sqlite_reader_blocks_replacement_and_retry_recovers() {
        let (temp, _) = fixture();
        let path = temp.0.join("library.sqlite3");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "snapshot_migration::tests::subprocess_migration_helper"])
            .env("NOTES_TEST_MIGRATION_DIRECTORY", &temp.0)
            .env("NOTES_TEST_MIGRATION_CRASH", "after-commit")
            .status().unwrap();
        assert_eq!(status.code(), Some(86));
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: String = reader.query_row("SELECT value FROM metadata WHERE key='schema_version'", [], |r| r.get(0)).unwrap();
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        let db = Connection::open(&path).unwrap();
        let pending: i64 = db.query_row("SELECT count(*) FROM metadata WHERE key='schema7_vacuum_pending'", [], |r| r.get(0)).unwrap();
        assert_eq!(pending, 0);
    }

    #[test]
    fn old_uncertain_organization_job_without_a_snapshot_is_preserved() {
        let (temp, _) = fixture();
        let path = temp.0.join("library.sqlite3");
        let db = Connection::open(&path).unwrap();
        db.execute("INSERT INTO organization_jobs VALUES('legacy','{}','uncertain','Eski işlemin kaynak sürümü kayıtlı değil; yeniden gönderilmedi.')", []).unwrap();
        db.execute("INSERT INTO organization_jobs VALUES('broken','{}','uncertain','different error')", []).unwrap();
        db.execute("INSERT INTO organization_jobs VALUES('broken-null','{}','uncertain',NULL)", []).unwrap();
        drop(db);
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        db.execute("DELETE FROM organization_jobs WHERE id IN ('broken','broken-null')", []).unwrap();
        drop(db);
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        let db = Connection::open(&path).unwrap();
        let (state, snapshot, error): (String, String, String) = db.query_row("SELECT state,snapshot,error FROM organization_jobs WHERE id='legacy'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!((state.as_str(), snapshot.as_str(), error.as_str()), ("uncertain", "{}", "Eski işlemin kaynak sürümü kayıtlı değil; yeniden gönderilmedi."));
    }

    #[test]
    fn interrupted_migration_rolls_back_and_rerun_preserves_every_snapshot() {
        let (temp, original) = fixture();
        let path = temp.0.join("library.sqlite3");
        assert!(migrate_inner(&temp.0, &path, &TestVerifier, true, None).is_err());
        let db = Connection::open(&path).unwrap();
        let version: String = db
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "6");
        let current: String = db
            .query_row("SELECT base_document FROM notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(current, original);
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('notes') WHERE name='container_sha256'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        drop(db);
        let store = DocumentStore::open(&temp.0).unwrap();
        assert_eq!(store.doctor_raw(|| Ok(vec![])).unwrap().unreferenced, 1);
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        let db = Connection::open(&path).unwrap();
        let version: String = db
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "7");
        for query in [
            "SELECT container_metadata FROM notes",
            "SELECT remote_container_metadata FROM notes",
            "SELECT snapshot FROM jobs",
            "SELECT snapshot FROM organization_jobs",
            "SELECT current_snapshot FROM ink_jobs",
            "SELECT current_snapshot FROM ink_erase_jobs",
            "SELECT current_snapshot FROM ink_move_jobs",
        ] {
            let encoded: String = db.query_row(query, [], |r| r.get(0)).unwrap();
            let reference = crate::val::loads(&encoded).unwrap();
            let actual = load_snapshot(&store, &reference).unwrap();
            assert!(crate::val::py_eq(
                &actual,
                &crate::val::loads(&original).unwrap()
            ));
        }
        let old_count: i64 = db.query_row("SELECT count(*) FROM notes WHERE base_document IS NOT NULL OR remote_document IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(old_count, 0);
    }

    #[test]
    fn running_job_aborts_without_changing_schema() {
        let (temp, original) = fixture();
        let path = temp.0.join("library.sqlite3");
        let db = Connection::open(&path).unwrap();
        db.execute("UPDATE jobs SET state='running'", []).unwrap();
        drop(db);
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        let current: String = db
            .query_row("SELECT base_document FROM notes", [], |r| r.get(0))
            .unwrap();
        let version: String = db
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "6");
        assert_eq!(current, original);
    }

    #[test]
    fn invalid_inflight_snapshot_aborts_without_discarding_data() {
        let (temp, original) = fixture();
        let path = temp.0.join("library.sqlite3");
        let db = Connection::open(&path).unwrap();
        db.execute("UPDATE jobs SET snapshot='{}'", []).unwrap();
        drop(db);
        assert!(migrate_v7(&temp.0, &path, &TestVerifier).is_err());
        let db = Connection::open(&path).unwrap();
        let version: String = db
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let base: String = db
            .query_row("SELECT base_document FROM notes", [], |r| r.get(0))
            .unwrap();
        let job: String = db
            .query_row("SELECT snapshot FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            (version.as_str(), base.as_str(), job.as_str()),
            ("6", original.as_str(), "{}")
        );
    }

    #[test]
    fn finished_jobs_release_snapshot_references() {
        let (temp, _) = fixture();
        let path = temp.0.join("library.sqlite3");
        let db = Connection::open(&path).unwrap();
        db.execute("UPDATE jobs SET state='done'", []).unwrap();
        db.execute("UPDATE organization_jobs SET state='done'", [])
            .unwrap();
        drop(db);
        migrate_v7(&temp.0, &path, &TestVerifier).unwrap();
        let db = Connection::open(&path).unwrap();
        let job: Option<String> = db
            .query_row("SELECT snapshot FROM jobs", [], |r| r.get(0))
            .unwrap();
        let ink: String = db
            .query_row("SELECT current_snapshot FROM ink_jobs", [], |r| r.get(0))
            .unwrap();
        let org: String = db
            .query_row("SELECT snapshot FROM organization_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(job, None);
        assert_eq!((ink.as_str(), org.as_str()), ("{}", "{}"));
    }
}
