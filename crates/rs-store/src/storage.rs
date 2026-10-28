//! Port of `samsung_notes/storage.py`: private durable library and outbox.
//! Local edits and Cloud acknowledgements are distinct states.
use crate::db::*;
use crate::host::{Fault, Host};
use crate::p;
use crate::val::{casefold, py_eq, Val};
#[cfg(feature = "python")]
use pyo3::prelude::*;
use rusqlite::types::Value as SqlVal;
use std::path::{Path, PathBuf};

pub const PUBLIC_COLUMNS: &str = "id,server_id,title,body,created_ms,modified_ms,revision,sync_state,\
editable,reason,favorite,trashed,loaded,error,deleted_remote,\
folder_id,org_state,org_error,title_editable";

const BASE_SCHEMA: &str = "
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS notes (
                    id TEXT PRIMARY KEY, server_id TEXT UNIQUE, resource_id TEXT,
                    title TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '',
                    created_ms INTEGER NOT NULL, modified_ms INTEGER NOT NULL,
                    revision INTEGER NOT NULL DEFAULT 1, sync_state TEXT NOT NULL,
                    editable INTEGER NOT NULL DEFAULT 0, reason TEXT,
                    favorite INTEGER NOT NULL DEFAULT 0, trashed INTEGER NOT NULL DEFAULT 0,
                    loaded INTEGER NOT NULL DEFAULT 0, error TEXT,
                    base_document TEXT, remote_document TEXT, remote_commit TEXT,
                    deleted_remote INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY, note_id TEXT NOT NULL REFERENCES notes(id),
                    kind TEXT NOT NULL, local_revision INTEGER NOT NULL,
                    title TEXT NOT NULL, body TEXT NOT NULL, snapshot TEXT,
                    state TEXT NOT NULL, error TEXT, created_ms INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS jobs_active ON jobs(state,created_ms);
                INSERT OR IGNORE INTO metadata VALUES ('schema_version','1');
            ";

/// 2**62, the offline dry-run timestamp.
pub const DRY_RUN_MS: i64 = 1 << 62;

/// Create (0700) and verify a private, user-owned directory.
pub fn private_directory(path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            match std::fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(e) => return Err(e.into()),
    }
    let meta = std::fs::metadata(path)?;
    // SAFETY: getuid takes no arguments, has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if path.is_symlink() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(Error::Value("application directory must be private and owned by the user".into()));
    }
    Ok(path.to_path_buf())
}

pub fn normalize_commit(value: Option<&str>) -> String {
    let value = value.unwrap_or("");
    match value.find("_lc") {
        Some(at) if at > 0 => value[..at].to_string(),
        _ => value.to_string(),
    }
}

/// normalize_commit for an arbitrary JSON value (`value or ''`).
pub fn normalize_commit_val(v: &Val) -> Result<String> {
    if !v.truthy() {
        return Ok(String::new());
    }
    match v {
        Val::Str(s) => Ok(normalize_commit(Some(s))),
        other => Err(Error::Type(format!("'{}' object has no attribute 'find'", other.type_name()))),
    }
}

/// Row-like access used by `public` (sqlite rows, dicts or Python mappings).
pub trait RowGet {
    fn get(&self, key: &str) -> Result<Val>;
}

impl RowGet for Row<'_> {
    fn get(&self, key: &str) -> Result<Val> {
        self.try_raw(key).map(sql_to_val).ok_or_else(|| Error::Key("No item with that key".into()))
    }
}

impl RowGet for Val {
    fn get(&self, k: &str) -> Result<Val> {
        key(self, k).cloned()
    }
}

/// Store.public(row, detail)
pub fn public(row: &dyn RowGet, detail: bool) -> Result<Val> {
    let g = |k: &str| row.get(k);
    let body = g("body")?;
    let preview = match &body {
        Val::Str(s) => Val::Str(s.chars().take(220).collect()),
        other => return Err(Error::Type(format!("'{}' object is not subscriptable", other.type_name()))),
    };
    let trashed = g("trashed")?.truthy();
    let deleted = g("deleted_remote")?.truthy();
    let editable = g("editable")?;
    let mut result = Val::dict(vec![
        ("id", g("id")?),
        ("title", g("title")?),
        ("preview", preview),
        ("createdAt", g("created_ms")?),
        ("modifiedAt", g("modified_ms")?),
        ("revision", g("revision")?),
        ("syncState", g("sync_state")?),
        ("editable", Val::Bool(editable.truthy() && !deleted && !trashed)),
        ("readOnlyReason", g("reason")?),
        ("favorite", Val::Bool(g("favorite")?.truthy())),
        ("trashed", Val::Bool(trashed)),
        ("contentLoaded", Val::Bool(g("loaded")?.truthy())),
        ("source", Val::s(if g("server_id")?.truthy() { "cloud" } else { "local" })),
        ("error", g("error")?),
        ("folderId", g("folder_id")?),
        ("metadataSync", Val::dict(vec![("state", g("org_state")?), ("error", g("org_error")?)])),
    ]);
    let title_editable = g("title_editable")?;
    let te = if title_editable.is_null() { editable.truthy() } else { title_editable.truthy() };
    result.set("titleEditable", Val::Bool(te && !trashed && !deleted));
    let org_state = g("org_state")?;
    let sync_state = g("sync_state")?;
    if !py_eq(&org_state, &Val::s("synced")) && matches!(sync_state.as_str(), Some("synced" | "local")) {
        result.set("syncState", org_state);
        result.set("error", g("org_error")?);
    }
    if detail {
        result.set("body", body);
        let remote = g("remote_document")?;
        if remote.truthy() {
            let remote = match &remote {
                Val::Str(s) => crate::val::loads(s)?,
                other => return Err(Error::Type(format!("the JSON object must be str, bytes or bytearray, not {}", other.type_name()))),
            };
            let empty = Val::s("");
            result.set("remoteTitle", get_or(&remote, "title", &empty).clone());
            result.set("remoteBody", get_or(&remote, "body", &empty).clone());
        }
    }
    Ok(result)
}

/// Store.validate_text(title, body)
pub fn validate_text(title: &Val, body: &Val) -> Result<()> {
    let (Val::Str(title), Val::Str(body)) = (title, body) else {
        return fail("invalid_text", "Başlık ve not metin olmalı.", 400);
    };
    let bad = |c: char| {
        let o = c as u32;
        (o < 32 && !matches!(c, '\t' | '\n' | '\r')) || (0xD800..=0xDFFF).contains(&o) || o == 0xFFFE || o == 0xFFFF
    };
    if title.chars().chain(body.chars()).any(bad) {
        return fail("invalid_text", "Metinde desteklenmeyen karakter var.", 400);
    }
    if title.len() > 4096 || body.len() > 256 * 1024 {
        return fail("text_too_large", "Bu not metin düzenleyicisinin boyut sınırını aşıyor.", 413);
    }
    Ok(())
}

/// Store._apply_favorite_tx(db, identity, document)
pub fn apply_favorite_tx(db: &dyn Sql, identity: &str, document: &Val) -> Result<()> {
    if let Some(value) = crate::organization_store::favorite_from_document(document)? {
        db.exec("UPDATE notes SET cloud_favorite=? WHERE id=?", &p![value, identity])?;
        db.exec("UPDATE notes SET favorite=? WHERE id=? AND org_state='synced'", &p![value, identity])?;
    }
    Ok(())
}

pub fn sid(v: &Val) -> Result<&str> {
    v.as_str().ok_or_else(|| Error::Type(format!("identity must be str, not {}", v.type_name())))
}

/// `type(v) is not int` -> StoreError(revision_required, ...)
fn require_revision(v: &Val, message: &str) -> Result<i64> {
    match v {
        Val::Int(i) => Ok(*i),
        Val::BigInt(_) => Ok(i64::MAX),
        _ => fail("revision_required", message, 400),
    }
}

/// int(x) for bool/int/float/str values.
pub fn py_int(v: &Val) -> Result<i64> {
    match v {
        Val::Bool(b) => Ok(*b as i64),
        Val::Int(i) => Ok(*i),
        Val::Float(f) if f.is_finite() => Ok(f.trunc() as i64),
        Val::Float(_) => Err(Error::Overflow("cannot convert float infinity to integer".into())),
        Val::Str(s) => {
            let t: String = s.trim().chars().filter(|c| *c != '_').collect();
            t.parse::<i64>().map_err(|_| Error::Value(format!("invalid literal for int() with base 10: {}", py_repr_str(s))))
        }
        other => Err(Error::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// float(x) for numeric/str values.
pub fn py_float(v: &Val) -> Result<f64> {
    match v {
        Val::Str(s) => {
            let t = s.trim().to_ascii_lowercase().replace('_', "");
            let t = t.as_str();
            match t {
                "inf" | "+inf" | "infinity" | "+infinity" => Ok(f64::INFINITY),
                "-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
                "nan" | "+nan" | "-nan" => Ok(f64::NAN),
                _ => t.parse::<f64>().map_err(|_| Error::Value(format!("could not convert string to float: {}", py_repr_str(s)))),
            }
        }
        other => other.as_f64().ok_or_else(|| {
            Error::Type(format!("float() argument must be a string or a real number, not '{}'", other.type_name()))
        }),
    }
}

/// Map a collaborator fault caught as `except DocumentError as error`.
fn document_fault(f: Fault, code: &str) -> Error {
    if f.document {
        store_err(code, &f.message, 422)
    } else {
        f.error
    }
}

pub struct StoreCore {
    pub directory: PathBuf,
    pub path: PathBuf,
}

impl StoreCore {
    /// Store.__init__(directory)
    pub fn open(directory: &Path) -> Result<StoreCore> {
        let directory = private_directory(directory)?;
        let path = directory.join("library.sqlite3");
        if path.is_symlink() {
            return Err(Error::Value("database cannot be a symbolic link".into()));
        }
        let store = StoreCore { directory, path };
        store.with_db(|db| {
            db.0.executescript(BASE_SCHEMA)?;
            crate::organization_store::migrate(db)
        })?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&store.path, std::fs::Permissions::from_mode(0o600))?;
        }
        store.refresh_editor_capabilities()?;
        if !store.meta("favorite_snapshot_imported")?.map(|v| v.truthy()).unwrap_or(false) {
            let rows = store.with_db(|db| {
                db.query(
                    "SELECT id,json_object('resource_xml',json_extract(base_document,'$.resource_xml')) AS snapshot FROM notes WHERE base_document IS NOT NULL",
                    &[],
                )
            })?;
            for row in rows.iter() {
                let snapshot = loads_sql(row.raw("snapshot"))?;
                store.apply_favorite(row.text("id").unwrap_or(""), &snapshot)?;
            }
            store.set_meta("favorite_snapshot_imported", &Val::Bool(true))?;
        }
        Ok(store)
    }

    pub fn with_db<T>(&self, f: impl FnOnce(&DbHandle) -> Result<T>) -> Result<T> {
        with_db(&self.path, f)
    }

    /// Editor capabilities are cached per note revision. When the capability
    /// rules change (`rs_editor::edit::PROJECTION_RECIPE`), drop the cached
    /// projections (and refused title flags) of synced Cloud notes so they are
    /// derived again from the stored Cloud document. Local/pending models and
    /// granted title flags are kept.
    fn refresh_editor_capabilities(&self) -> Result<()> {
        const KEY: &str = "editor_projection_recipe";
        let recipe = _editor::edit::PROJECTION_RECIPE;
        if self.meta(KEY)?.as_ref().and_then(Val::as_str) == Some(recipe) {
            return Ok(());
        }
        let encoded = dumps_ascii(&Val::s(recipe))?;
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let synced = "SELECT id FROM notes WHERE sync_state='synced' AND base_document IS NOT NULL";
            db.exec(&format!("DELETE FROM editor_documents WHERE note_id IN ({synced})"), &[])?;
            // The rules only ever widen title editing: re-derive the refused ones.
            db.exec(&format!("UPDATE notes SET title_editable=NULL WHERE title_editable=0 AND id IN ({synced})"), &[])?;
            db.exec("INSERT INTO metadata VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", &p![KEY, encoded])?;
            Ok(())
        })
    }

    pub fn meta(&self, key: &str) -> Result<Option<Val>> {
        self.with_db(|db| match db.one("SELECT value FROM metadata WHERE key=?", &p![key])? {
            Some(row) => Ok(Some(loads_sql(&row[0])?)),
            None => Ok(None),
        })
    }

    pub fn set_meta(&self, key: &str, value: &Val) -> Result<()> {
        let encoded = dumps_ascii(value)?;
        self.with_db(|db| {
            db.exec("INSERT INTO metadata VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", &p![key, encoded])?;
            Ok(())
        })
    }

    pub fn raw(&self, note_id: &str) -> Result<Val> {
        let cur = self.with_db(|db| db.query("SELECT * FROM notes WHERE id=?", &p![note_id]))?;
        match cur.one() {
            Some(row) => Ok(row.to_dict()),
            None => fail("not_found", "Not bulunamadı.", 404),
        }
    }

    pub fn get(&self, note_id: &str) -> Result<Val> {
        // Do not read multi-megabyte raw Cloud snapshots on every UI poll.
        let sql = format!(
            "SELECT {}, CASE WHEN sync_state='conflict' AND remote_document IS NOT NULL \
             THEN json_object('title',json_extract(remote_document,'$.title'),'body',json_extract(remote_document,'$.body')) \
             ELSE NULL END AS remote_document FROM notes WHERE id=?",
            PUBLIC_COLUMNS
        );
        let cur = self.with_db(|db| db.query(&sql, &p![note_id]))?;
        match cur.one() {
            Some(row) => public(&row, true),
            None => fail("not_found", "Not bulunamadı.", 404),
        }
    }

    pub fn list(&self, query: &str, filter: &str, folder: Option<&Val>) -> Result<Vec<Val>> {
        let condition = match filter {
            "all" => "trashed=0",
            "favorites" => "trashed=0 AND favorite=1",
            "conflicts" => "sync_state IN ('conflict','uncertain')",
            "trash" => "trashed=1",
            _ => return fail("bad_filter", "Bu görünüm bulunamadı.", 400),
        };
        let mut sql = format!("SELECT {} FROM notes WHERE {}", PUBLIC_COLUMNS, condition);
        let mut args = Vec::new();
        if let Some(folder) = folder {
            sql.push_str(" AND folder_id=?");
            args.push(b(folder)?);
        }
        sql.push_str(" ORDER BY modified_ms DESC,id");
        let trash = filter == "trash";
        let (cur, trashed_at) = self.with_db(|db| {
            let cur = db.query(&sql, &args)?;
            // Samsung Cloud's trashedTime (ms) of the note node; trash keeps items 30 days.
            let trashed_at = if trash {
                Some(db.query(
                    "SELECT n.id,f.trashed_ms FROM notes n JOIN folder_nodes f ON f.id=n.resource_id \
                     AND f.kind='note' AND f.state='trashed' WHERE n.trashed=1 AND f.trashed_ms>0",
                    &[],
                )?)
            } else {
                None
            };
            Ok((cur, trashed_at))
        })?;
        let times: Vec<(Val, Val)> = match &trashed_at {
            Some(rows) => rows.iter().map(|r| (r.val("id"), r.val("trashed_ms"))).collect(),
            None => Vec::new(),
        };
        let q = casefold(query);
        let mut out = Vec::new();
        for row in cur.iter() {
            if !q.is_empty() {
                let title = casefold(row.text("title").unwrap_or(""));
                let body = casefold(row.text("body").unwrap_or(""));
                if !title.contains(&q) && !body.contains(&q) {
                    continue;
                }
            }
            let mut note = public(&row, false)?;
            if trash {
                let id = row.val("id");
                let at = times.iter().find(|(k, _)| *k == id).map(|(_, v)| v.clone()).unwrap_or(Val::Null);
                note.set("trashedAt", at);
            }
            out.push(note);
        }
        Ok(out)
    }

    pub fn create(&self, title: &Val, body: &Val) -> Result<Val> {
        validate_text(title, body)?;
        let identity = uuid4();
        let now = now_ms();
        let (t, bo) = (title.as_str().unwrap(), body.as_str().unwrap());
        self.with_db(|db| {
            db.exec(
                "INSERT INTO notes (id,resource_id,title,body,created_ms,modified_ms,
                sync_state,editable,loaded) VALUES (?,?,?,?,?,?,'pending',1,1)",
                &p![&identity, &identity, t, bo, now, now],
            )?;
            Ok(())
        })?;
        self.get(&identity)
    }

    pub fn save<H: Host>(&self, h: &H, identity: &str, title: &Val, body: &Val, expected_revision: &Val) -> Result<Val> {
        validate_text(title, body)?;
        let expected = require_revision(expected_revision, "Not sürümü eksik.")?;
        let (title, body) = (title.as_str().unwrap(), body.as_str().unwrap());
        let early = self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let Some(row) = cur.one() else { return fail("not_found", "Not bulunamadı.", 404) };
            if !sql_eq(row.raw("revision"), &Val::Int(expected)) {
                return fail("revision_conflict", "Not başka bir yerde değişti. Düzenlemen korunuyor; güncel sürümü aç.", 409);
            }
            if !row.truthy("editable") || row.truthy("deleted_remote") || row.truthy("trashed") {
                let reason = row.text("reason").filter(|s| !s.is_empty()).unwrap_or("Bu not yalnızca okunabilir.");
                return fail("read_only", reason, 422);
            }
            h.ensure_no_active_ink(db, identity)?;
            if matches!(row.text("sync_state"), Some("conflict" | "uncertain")) {
                return fail("resolve_first", "Önce bu notun eşitleme durumunu çöz.", 409);
            }
            if row.text("org_state") != Some("synced") {
                return fail(
                    "organization_pending",
                    "Önce notun bekleyen bulut işlemini tamamla. Düzenlemen korunuyor.",
                    409,
                );
            }
            if row.text("title") == Some(title) && row.text("body") == Some(body) {
                return Ok(Some(public(&row, true)?));
            }
            let base = loads_opt(row.raw("base_document"))?;
            let active = db
                .one("SELECT 1 FROM jobs WHERE note_id=? AND state IN ('queued','running','uncertain') LIMIT 1", &p![identity])?
                .is_some();
            let state = match &base {
                Some(base) if base.truthy() && !active => {
                    if py_eq(key(base, "title")?, &Val::s(title)) && py_eq(key(base, "body")?, &Val::s(body)) {
                        "synced"
                    } else {
                        "pending"
                    }
                }
                _ => "pending",
            };
            db.exec(
                "UPDATE notes SET title=?,body=?,revision=revision+1,modified_ms=?,sync_state=?,error=NULL WHERE id=?",
                &p![title, body, now_ms(), state, identity],
            )?;
            db.exec("UPDATE jobs SET state='cancelled' WHERE note_id=? AND state='failed'", &p![identity])?;
            Ok(None)
        })?;
        match early {
            Some(v) => Ok(v),
            None => self.get(identity),
        }
    }

    pub fn title_capability(&self, identity: &str, supported: bool, raw_sha256: &Val) -> Result<()> {
        let sha = b(raw_sha256)?;
        self.with_db(|db| {
            db.exec(
                "UPDATE notes SET title_editable=? WHERE id=? AND json_extract(base_document,'$.raw_sha256')=?",
                &p![supported, identity, sha],
            )?;
            Ok(())
        })
    }

    pub fn raw_state(&self, identity: &str) -> Result<Val> {
        let cur = self.with_db(|db| db.query("SELECT deleted_remote FROM notes WHERE id=?", &p![identity]))?;
        match cur.one() {
            Some(row) => Ok(row.to_dict()),
            None => fail("not_found", "Not bulunamadı.", 404),
        }
    }

    pub fn save_title<H: Host>(&self, h: &H, identity: &str, title: &Val, expected_revision: &Val) -> Result<Val> {
        validate_text(title, &Val::s(""))?;
        let title = title.as_str().unwrap();
        if title.contains('\n') || title.contains('\r') {
            return fail("invalid_title", "Başlık tek satır olmalı.", 400);
        }
        let expected = require_revision(expected_revision, "Not sürümü eksik.")?;
        let early = self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let Some(row) = cur.one() else { return fail("not_found", "Not bulunamadı.", 404) };
            if !sql_eq(row.raw("revision"), &Val::Int(expected)) {
                return fail("revision_conflict", "Not başka bir yerde değişti. Başlığın korunuyor.", 409);
            }
            h.ensure_no_active_ink(db, identity)?;
            if !row.truthy("title_editable") || row.truthy("deleted_remote") || row.truthy("trashed") || !row.truthy("server_id") {
                return fail("title_read_only", "Bu notun başlığı henüz güvenle düzenlenemiyor.", 422);
            }
            if row.text("sync_state") != Some("synced") || row.text("org_state") != Some("synced") {
                return fail("sync_pending", "Önce bu notun bekleyen eşitlemesini tamamla.", 409);
            }
            if row.text("title") == Some(title) {
                return Ok(Some(public(&row, true)?));
            }
            let revision = row.int("revision") + 1;
            db.exec(
                "INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?)",
                &p![uuid4(), identity, "title", revision, title, row.raw("body"), row.raw("base_document"), "queued", null(), now_ms()],
            )?;
            db.exec(
                "UPDATE notes SET title=?,revision=?,modified_ms=?,sync_state='pending',error=NULL WHERE id=?",
                &p![title, revision, now_ms(), identity],
            )?;
            Ok(None)
        })?;
        match early {
            Some(v) => Ok(v),
            None => self.get(identity),
        }
    }

    fn ready(row: &Val) -> bool {
        row.get_or_null("server_id").truthy()
            && !row.get_or_null("deleted_remote").truthy()
            && !row.get_or_null("trashed").truthy()
            && row.get_or_null("sync_state").as_str() == Some("synced")
            && row.get_or_null("org_state").as_str() == Some("synced")
    }

    fn base_of(row: &Val) -> Result<Option<Val>> {
        let base = row.get_or_null("base_document");
        if base.truthy() {
            match &base {
                Val::Str(s) => Ok(Some(crate::val::loads(s)?)),
                _ => Err(Error::Type("the JSON object must be str".into())),
            }
        } else {
            Ok(None)
        }
    }

    /// Current page background as the UI sees it; derived from the base snapshot.
    pub fn page_style<H: Host>(&self, h: &H, identity: &str) -> Result<Val> {
        let row = self.raw(identity)?;
        let pending = self.with_db(|db| {
            db.one(
                "SELECT page_style_jobs.color FROM page_style_jobs JOIN jobs ON jobs.id=page_style_jobs.job_id
                WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') ORDER BY jobs.created_ms DESC LIMIT 1",
                &p![identity],
            )
        })?;
        let base = Self::base_of(&row)?;
        let mut state = h.page_background_state(base.as_ref())?;
        if let Some(pending) = pending {
            state.set("backgroundArgb", sql_to_val(&pending[0]));
            state.set("pending", Val::Bool(true));
        }
        if key(&state, "editable")?.truthy() && !Self::ready(&row) {
            state.set("editable", Val::Bool(false));
            state.set("reason", Val::s("Önce bu notun bekleyen eşitlemesi tamamlanmalı."));
        }
        state.set("revision", row.get_or_null("revision"));
        Ok(state)
    }

    /// Shared write gate for snapshot-derived jobs (page style, PDF pages, page ops).
    fn gate_row<'a, H: Host>(
        &self,
        h: &H,
        db: &DbHandle,
        cur: &'a Cursor,
        identity: &str,
        expected: i64,
        read_only: (&str, &str),
    ) -> Result<Row<'a>> {
        let Some(row) = cur.one() else { return fail("not_found", "Not bulunamadı.", 404) };
        if !sql_eq(row.raw("revision"), &Val::Int(expected)) {
            return fail("revision_conflict", "Not başka bir yerde değişti.", 409);
        }
        h.ensure_no_active_ink(db, identity)?;
        if row.truthy("deleted_remote") || row.truthy("trashed") || !row.truthy("server_id") || !row.truthy("base_document") {
            return fail(read_only.0, read_only.1, 422);
        }
        if row.text("sync_state") != Some("synced") || row.text("org_state") != Some("synced") {
            return fail("sync_pending", "Önce bu notun bekleyen eşitlemesini tamamla.", 409);
        }
        Ok(row)
    }

    fn queue_snapshot_job(db: &DbHandle, row: &Row, identity: &str, kind: &str) -> Result<(String, i64)> {
        let revision = row.int("revision") + 1;
        let job = uuid4();
        db.exec(
            "INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?)",
            &p![&job, identity, kind, revision, row.raw("title"), row.raw("body"), row.raw("base_document"), "queued", null(), now_ms()],
        )?;
        Ok((job, revision))
    }

    fn mark_pending(db: &DbHandle, identity: &str, revision: i64) -> Result<()> {
        db.exec(
            "UPDATE notes SET revision=?,modified_ms=?,sync_state='pending',error=NULL WHERE id=?",
            &p![revision, now_ms(), identity],
        )?;
        Ok(())
    }

    pub fn save_page_background<H: Host>(&self, h: &H, identity: &str, color_argb: &Val, expected_revision: &Val) -> Result<Val> {
        let color = match color_argb {
            Val::Int(c) if (0..=0xFFFF_FFFF).contains(c) && c >> 24 == 0xFF => *c,
            _ => return fail("invalid_color", "Sayfa rengi opak bir renk olmalı.", 400),
        };
        let expected = require_revision(expected_revision, "Not sürümü eksik.")?;
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let row = self.gate_row(h, db, &cur, identity, expected, ("style_read_only", "Bu notun sayfa rengi değiştirilemiyor."))?;
            let base = loads_sql(row.raw("base_document"))?;
            // Offline dry run: unsupported notes fail here, before queueing.
            h.prepare_page_background(&base, color).map_err(|f| document_fault(f, "style_read_only"))?;
            let (job, revision) = Self::queue_snapshot_job(db, &row, identity, "page_style")?;
            db.exec("INSERT INTO page_style_jobs VALUES (?,?)", &p![&job, color])?;
            Self::mark_pending(db, identity, revision)
        })?;
        self.get(identity)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_pdf_pages<H: Host>(
        &self,
        h: &H,
        identity: &str,
        expected_revision: &Val,
        file_sha256: &Val,
        file_name: &Val,
        page_sizes: &Val,
        page_indices: &Val,
        template_type: &Val,
    ) -> Result<Val> {
        let expected = require_revision(expected_revision, "Not sürümü eksik.")?;
        let sizes = match page_sizes {
            Val::List(items) => items,
            other => return Err(Error::Type(format!("object of type '{}' has no len()", other.type_name()))),
        };
        let indices = match page_indices {
            Val::Null => Val::List((0..sizes.len() as i64).map(Val::Int).collect()),
            Val::List(items) => Val::List(items.clone()),
            other => return Err(Error::Type(format!("'{}' object is not iterable", other.type_name()))),
        };
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let row = self.gate_row(h, db, &cur, identity, expected, ("pdf_read_only", "Bu nota sayfa eklenemiyor."))?;
            let base = loads_sql(row.raw("base_document"))?;
            h.prepare_pdf_pages(&base, file_sha256, file_name, page_sizes, &indices, template_type)
                .map_err(|f| document_fault(f, "pdf_read_only"))?;
            let (job, revision) = Self::queue_snapshot_job(db, &row, identity, "pdf_pages")?;
            let mut floats = Vec::new();
            for s in sizes {
                let items = match s {
                    Val::List(items) => items,
                    other => return Err(Error::Type(format!("'{}' object is not iterable", other.type_name()))),
                };
                floats.push(Val::List(items.iter().map(|x| py_float(x).map(Val::Float)).collect::<Result<Vec<_>>>()?));
            }
            db.exec(
                "INSERT INTO pdf_page_jobs VALUES (?,?,?,?,?,?)",
                &vec![
                    SqlVal::Text(job.clone()),
                    b(file_sha256)?,
                    b(file_name)?,
                    SqlVal::Text(dumps_ascii(&Val::List(floats))?),
                    SqlVal::Text(dumps_ascii(&indices)?),
                    b(template_type)?,
                ],
            )?;
            Self::mark_pending(db, identity, revision)
        })?;
        self.get(identity)
    }

    pub fn pdf_page_job(&self, job: &Val) -> Result<Val> {
        let id = b(key(job, "id")?)?;
        let cur = self.with_db(|db| db.query("SELECT * FROM pdf_page_jobs WHERE job_id=?", &[id]))?;
        let Some(row) = cur.one() else {
            return fail("missing_pdf_job", "PDF sayfa bilgisi eksik; tekrar gönderilmedi.", 409);
        };
        Ok(Val::dict(vec![
            ("file_sha256", row.val("file_sha256")),
            ("file_name", row.val("file_name")),
            ("page_sizes", loads_sql(row.raw("page_sizes"))?),
            ("page_indices", loads_sql(row.raw("page_indices"))?),
            ("template_type", row.val("template_type")),
        ]))
    }

    pub fn page_ops<H: Host>(&self, h: &H, identity: &str) -> Result<Val> {
        let row = self.raw(identity)?;
        let pending = self.with_db(|db| {
            db.one(
                "SELECT 1 FROM page_op_jobs JOIN jobs ON jobs.id=page_op_jobs.job_id
                WHERE jobs.note_id=? AND jobs.state NOT IN ('done','cancelled') LIMIT 1",
                &p![identity],
            )
        })?;
        let base = Self::base_of(&row)?;
        let mut state = h.page_ops_state(base.as_ref())?;
        if key(&state, "editable")?.truthy() && !Self::ready(&row) {
            state.set("editable", Val::Bool(false));
            state.set("reason", Val::s("Önce bu notun bekleyen eşitlemesi tamamlanmalı."));
        }
        state.set("pending", Val::Bool(pending.is_some()));
        state.set("revision", row.get_or_null("revision"));
        Ok(state)
    }

    pub fn save_page_op<H: Host>(&self, h: &H, identity: &str, op: &Val, spec: &Val, expected_revision: &Val) -> Result<Val> {
        let op_name = match op.as_str() {
            Some(o @ ("add" | "delete" | "reorder" | "shape")) if expected_revision.is_exact_int() => o,
            _ => return fail("invalid_change", "Sayfa işlemi geçersiz.", 400),
        };
        let expected = require_revision(expected_revision, "")?;
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let row = self.gate_row(h, db, &cur, identity, expected, ("pages_read_only", "Bu notun sayfaları değiştirilemiyor."))?;
            let base = loads_sql(row.raw("base_document"))?;
            // offline dry run before queueing
            let mut stored = spec.clone();
            let attempt = (|| -> std::result::Result<(), Fault> {
                match op_name {
                    "shape" => {
                        stored = h.prepare_add_shape(&base, spec)?;
                        Ok(())
                    }
                    "add" => h.prepare_add_page(&base, key(spec, "afterIndex")?),
                    "delete" => h.prepare_delete_page(&base, key(spec, "pageIds")?),
                    _ => h.prepare_reorder_pages(&base, key(spec, "pageIds")?),
                }
            })();
            if let Err(f) = attempt {
                if f.document {
                    return fail("pages_read_only", &f.message, 422);
                } else if f.key || f.type_ {
                    return fail("pages_read_only", "Sayfa işlemi geçersiz.", 422);
                }
                return Err(f.error);
            }
            let (job, revision) = Self::queue_snapshot_job(db, &row, identity, "page_op")?;
            db.exec("INSERT INTO page_op_jobs VALUES (?,?,?)", &p![&job, op_name, dumps_ascii(&stored)?])?;
            Self::mark_pending(db, identity, revision)
        })?;
        self.get(identity)
    }

    pub fn page_op_job(&self, job: &Val) -> Result<(Val, Val)> {
        let id = b(key(job, "id")?)?;
        let row = self.with_db(|db| db.one("SELECT op,spec FROM page_op_jobs WHERE job_id=?", &[id]))?;
        let Some(row) = row else {
            return fail("missing_page_op", "Sayfa işlemi bilgisi eksik; tekrar gönderilmedi.", 409);
        };
        Ok((sql_to_val(&row[0]), loads_sql(&row[1])?))
    }

    pub fn page_style_job_color(&self, job: &Val) -> Result<Val> {
        let id = b(key(job, "id")?)?;
        let row = self.with_db(|db| db.one("SELECT color FROM page_style_jobs WHERE job_id=?", &[id]))?;
        match row {
            Some(row) => Ok(sql_to_val(&row[0])),
            None => fail("missing_style", "Sayfa rengi bilgisi eksik; tekrar gönderilmedi.", 409),
        }
    }

    pub fn counts(&self) -> Result<Val> {
        self.with_db(|db| {
            let n = |sql: &str| -> Result<i64> {
                Ok(match db.exec(sql, &[])?.scalar() {
                    Some(SqlVal::Integer(i)) => *i,
                    _ => 0,
                })
            };
            let total = n("SELECT COUNT(*) FROM notes WHERE trashed=0")?;
            let mut pending = n("SELECT COUNT(*) FROM notes WHERE sync_state IN ('pending','error','uncertain') OR org_state IN ('pending','error','uncertain')")?;
            let mut conflicts = n("SELECT COUNT(*) FROM notes WHERE sync_state IN ('conflict','uncertain') OR org_state IN ('conflict','uncertain')")?;
            pending += n("SELECT COUNT(*) FROM folder_nodes WHERE kind='folder' AND sync_state IN ('pending','error','uncertain')")?;
            conflicts += n("SELECT COUNT(*) FROM folder_nodes WHERE kind='folder' AND sync_state IN ('conflict','uncertain')")?;
            Ok(Val::dict(vec![("pending", Val::Int(pending)), ("conflicts", Val::Int(conflicts)), ("total", Val::Int(total))]))
        })
    }

    pub fn by_server(&self, server_id: &Val) -> Result<Option<Val>> {
        let sid = b(server_id)?;
        let cur = self.with_db(|db| db.query("SELECT * FROM notes WHERE server_id=?", &[sid]))?;
        Ok(cur.one().map(|r| r.to_dict()))
    }

    /// Durably stage metadata before advancing a feed checkpoint.
    pub fn stage_change(&self, change: &Val) -> Result<Option<String>> {
        let server = key(change, "server_id")?.clone();
        let existing = self.by_server(&server)?;
        let deleted = matches!(change.get("is_deleted"), Some(Val::Bool(true)));
        if deleted && existing.is_none() {
            return Ok(None);
        }
        self.with_db(|db| {
            let identity;
            match &existing {
                None => {
                    identity = uuid4();
                    let blocked = change.get_or_null("is_locked").truthy() || change.get_or_null("is_encrypted").truthy();
                    let modified = change.get_or_null("sync_modified_time");
                    let modified = if modified.truthy() { b(&modified)? } else { int(now_ms()) };
                    db.exec(
                        "INSERT INTO notes (id,server_id,resource_id,title,created_ms,modified_ms,
                    sync_state,reason,remote_commit) VALUES (?,?,?,'',?,?,'synced',?,?)",
                        &vec![
                            text(&identity),
                            b(&server)?,
                            b(&change.get_or_null("resource_id"))?,
                            int(now_ms()),
                            modified,
                            text(if blocked { "Bu not kilitli veya şifreli." } else { "İçerik henüz indirilmedi." }),
                            b(&change.get_or_null("commit_id"))?,
                        ],
                    )?;
                }
                Some(existing) => {
                    identity = sid(key(existing, "id")?)?.to_string();
                    if deleted {
                        let dirty = existing.get_or_null("sync_state").as_str() != Some("synced");
                        db.exec(
                            "UPDATE notes SET deleted_remote=1,trashed=?,sync_state=?,reason=? WHERE id=?",
                            &p![
                                if dirty { 0 } else { 1 },
                                if dirty { "conflict" } else { "synced" },
                                "Bu not buluttan silindi. Yerel metni kopya olarak saklayabilirsin.",
                                &identity
                            ],
                        )?;
                        return Ok(Some(identity));
                    }
                    if normalize_commit_val(&existing.get_or_null("remote_commit"))? != normalize_commit_val(&change.get_or_null("commit_id"))? {
                        db.exec(
                            "UPDATE notes SET remote_commit=?,loaded=0 WHERE id=?",
                            &vec![b(&change.get_or_null("commit_id"))?, text(&identity)],
                        )?;
                    }
                }
            }
            // Never overwrite an edited local body from feed metadata.
            Ok(Some(identity))
        })
    }

    pub fn apply_document<H: Host>(&self, h: &H, server_id: &Val, document: &Val, expected_job: Option<&Val>) -> Result<String> {
        let identity = match self.by_server(server_id)? {
            None => {
                let change = Val::dict(vec![
                    ("server_id", server_id.clone()),
                    ("resource_id", key(document, "local_id")?.clone()),
                    ("commit_id", key(document, "commit_id")?.clone()),
                    ("is_deleted", Val::Bool(false)),
                ]);
                self.stage_change(&change)?.unwrap_or_default()
            }
            Some(existing) => sid(key(&existing, "id")?)?.to_string(),
        };
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            if h.apply_document_tx(self, db, &identity, document, expected_job)? {
                h.apply_favorite_tx(self, db, &identity, document)?;
            }
            Ok(())
        })?;
        Ok(identity)
    }

    /// Caller holds BEGIN IMMEDIATE; acknowledgement can share this commit.
    pub fn apply_document_tx<H: Host>(&self, h: &H, db: &dyn Sql, identity: &str, document: &Val, expected_job: Option<&Val>) -> Result<bool> {
        let encoded = dumps_utf8(document)?;
        let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
        let Some(row) = cur.one() else {
            return Err(Error::Type("'NoneType' object is not subscriptable".into()));
        };
        if let Some(job) = expected_job {
            if py_eq(key(job, "kind")?, &Val::s("append_ink")) {
                h.guard_ink_job(self, db, job)?;
            }
        }
        let dirty = !matches!(row.text("sync_state"), Some("synced" | "local"));
        let base = loads_opt(row.raw("base_document"))?;
        let unchanged = match &base {
            Some(base) if base.truthy() => normalize_commit_val(key(base, "commit_id")?)? == normalize_commit_val(key(document, "commit_id")?)?,
            _ => false,
        };
        if dirty && expected_job.is_none() {
            if !unchanged && base.is_some() {
                db.exec(
                    "UPDATE notes SET sync_state='conflict',loaded=1,remote_document=?,remote_commit=?,error=? WHERE id=?",
                    &vec![
                        text(&encoded),
                        b(key(document, "commit_id")?)?,
                        text("Not başka bir cihazda değişti. İki sürüm de korunuyor."),
                        text(identity),
                    ],
                )?;
            } else if unchanged {
                db.exec("UPDATE notes SET loaded=1 WHERE id=?", &p![identity])?;
            }
            return Ok(false);
        }
        // A folder-only revision bump is not a newer ink draft. Never extend
        // that exception to a newer desired title/body or another content job.
        let ink_metadata_only = match expected_job {
            Some(job) if py_eq(key(job, "kind")?, &Val::s("append_ink")) => {
                sql_eq(row.raw("title"), key(job, "title")?)
                    && sql_eq(row.raw("body"), key(job, "body")?)
                    && db
                        .one(
                            "SELECT 1 FROM jobs WHERE note_id=? AND id<>? AND state NOT IN ('done','cancelled')",
                            &vec![text(identity), b(key(job, "id")?)?],
                        )?
                        .is_none()
            }
            _ => false,
        };
        let newer_local = match expected_job {
            Some(job) => !sql_eq(row.raw("revision"), key(job, "local_revision")?) && !ink_metadata_only,
            None => false,
        };
        if !newer_local {
            db.exec("DELETE FROM editor_documents WHERE note_id=?", &p![identity])?;
            db.exec("DELETE FROM ink_documents WHERE note_id=?", &p![identity])?;
        }
        let same_visible = unchanged && {
            let doc_title = key(document, "title")?;
            let doc_body = key(document, "body")?;
            let doc_editable = key(document, "editable")?.truthy();
            let doc_reason = document.get_or_null("read_only_reason");
            sql_eq(row.raw("title"), doc_title)
                && sql_eq(row.raw("body"), doc_body)
                && row.truthy("editable") == doc_editable
                && sql_eq(row.raw("reason"), &doc_reason)
        };
        let local_id = b(key(document, "local_id")?)?;
        let title = if newer_local { row.raw("title").clone() } else { b(key(document, "title")?)? };
        let body = if newer_local { row.raw("body").clone() } else { b(key(document, "body")?)? };
        let created = document.get_or_null("created_ms");
        let created = if created.truthy() { b(&created)? } else { row.raw("created_ms").clone() };
        let modified = if newer_local {
            row.raw("modified_ms").clone()
        } else {
            let m = document.get_or_null("modified_ms");
            if m.truthy() {
                b(&m)?
            } else {
                int(now_ms())
            }
        };
        let bump = if expected_job.map(|j| j.truthy()).unwrap_or(false) || same_visible { 0 } else { 1 };
        let editable = if newer_local { row.raw("editable").clone() } else { int(py_int(key(document, "editable")?)?) };
        db.exec(
            "UPDATE notes SET resource_id=?,title=?,body=?,created_ms=?,modified_ms=?,
            revision=revision+?,sync_state=?,editable=?,reason=?,loaded=1,error=NULL,
            base_document=?,remote_document=NULL,remote_commit=?,deleted_remote=0,
            title_editable=CASE WHEN ? THEN title_editable ELSE NULL END WHERE id=?",
            &vec![
                local_id,
                title,
                body,
                created,
                modified,
                int(bump),
                text(if newer_local { "pending" } else { "synced" }),
                editable,
                b(&document.get_or_null("read_only_reason"))?,
                text(&encoded),
                b(key(document, "commit_id")?)?,
                int(unchanged as i64),
                text(identity),
            ],
        )?;
        Ok(true)
    }

    pub fn content_error(&self, identity: &str, message: &Val) -> Result<()> {
        let m = b(message)?;
        self.with_db(|db| {
            db.exec("UPDATE notes SET error=? WHERE id=?", &vec![m, text(identity)])?;
            Ok(())
        })
    }

    pub fn next_job(&self) -> Result<Option<Val>> {
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM jobs WHERE state IN ('queued','running') ORDER BY created_ms LIMIT 1", &[])?;
            if let Some(row) = cur.one() {
                return Ok(Some(row.to_dict()));
            }
            let cur = db.query(
                "SELECT * FROM notes WHERE sync_state='pending' AND trashed=0 AND
                id NOT IN (SELECT note_id FROM jobs WHERE state IN ('uncertain','failed')) ORDER BY modified_ms LIMIT 1",
                &[],
            )?;
            let Some(row) = cur.one() else { return Ok(None) };
            let server = row.truthy("server_id");
            let mut job = Val::dict(vec![
                ("id", Val::Str(uuid4())),
                ("note_id", row.val("id")),
                ("kind", Val::s(if server { "update" } else { "create" })),
                ("local_revision", row.val("revision")),
                ("title", row.val("title")),
                ("body", row.val("body")),
                ("snapshot", row.val("base_document")),
                ("state", Val::s("queued")),
                ("error", Val::Null),
                ("created_ms", Val::Int(now_ms())),
            ]);
            let editor = db.one(
                "SELECT projection FROM editor_documents WHERE note_id=? AND revision=?",
                &vec![row.raw("id").clone(), row.raw("revision").clone()],
            )?;
            let projection = match editor {
                Some(e) => Some(loads_sql(&e[0])?),
                None => None,
            };
            let mut model = match &projection {
                Some(p) if p.truthy() && key(key(p, "capabilities")?, "textEdit")?.truthy() => p.get("model").cloned(),
                _ => None,
            };
            if let Some(m) = &model {
                if !m.is_null() && !sql_eq(row.raw("body"), &m.get_or_null("text")) {
                    model = None;
                }
            }
            let model = model.filter(|m| !m.is_null());
            if model.is_none() && !row.truthy("editable") {
                return fail("missing_editor_model", "Düzenlemenin biçim bilgisi eksik; gönderim durduruldu.", 409);
            }
            if model.is_some() {
                job.set("kind", Val::s(if server { "update_editor" } else { "create_editor" }));
            }
            let params = match &job {
                Val::Dict(items) => items.iter().enumerate().map(|(i, (_, v))| bind(v, i + 1)).collect::<Result<Vec<_>>>()?,
                _ => unreachable!(),
            };
            db.exec("INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?)", &params)?;
            if let Some(model) = &model {
                db.exec("INSERT INTO editor_jobs VALUES (?,?)", &vec![b(key(&job, "id")?)?, text(&dumps_utf8(model)?)])?;
            }
            Ok(Some(job))
        })
    }

    /// Only an active job may move. A cancellation/resolution that landed
    /// after selection owns the row; never reopen or overwrite it.
    pub fn job_state(&self, job: &Val, state: &Val, message: &Val) -> Result<bool> {
        let (id, note) = (b(key(job, "id")?)?, key(job, "note_id")?.clone());
        let (st, msg) = (b(state)?, b(message)?);
        self.with_db(|db| {
            let claimed = db
                .exec("UPDATE jobs SET state=?,error=? WHERE id=? AND state IN ('queued','running')", &vec![st, msg.clone(), id])?
                .rowcount
                == 1;
            if claimed {
                if let Some(s @ ("uncertain" | "failed" | "conflict")) = state.as_str() {
                    let public = if s == "failed" { "error" } else { s };
                    db.exec("UPDATE notes SET sync_state=?,error=? WHERE id=?", &vec![text(public), msg, b(&note)?])?;
                }
            }
            Ok(claimed)
        })
    }

    /// Snapshot/UI acknowledgement and job completion are one durable
    /// transaction. Cloud receipts/checkpoints remain separately durable.
    pub fn complete_job_tx<H: Host>(&self, h: &H, db: &DbHandle, job: &Val, result: &Val) -> Result<()> {
        db.exec("BEGIN IMMEDIATE", &[])?;
        if py_eq(key(job, "kind")?, &Val::s("append_ink")) {
            h.guard_ink_job(self, db, job)?;
        }
        let note = key(job, "note_id")?;
        db.exec(
            "UPDATE notes SET server_id=?,resource_id=? WHERE id=?",
            &vec![b(key(result, "server_id")?)?, b(key(result, "local_id")?)?, b(note)?],
        )?;
        let note = sid(note)?;
        let snapshot = key(result, "snapshot")?;
        if h.apply_document_tx(self, db, note, snapshot, Some(job))? {
            h.apply_favorite_tx(self, db, note, snapshot)?;
        }
        db.exec("UPDATE jobs SET state='done',error=NULL WHERE id=?", &vec![b(key(job, "id")?)?])?;
        Ok(())
    }

    pub fn complete_job<H: Host>(&self, h: &H, job: &Val, result: &Val) -> Result<()> {
        self.with_db(|db| self.complete_job_tx(h, db, job, result))
    }

    pub fn resolve<H: Host>(&self, h: &H, identity: &str, strategy: &Val) -> Result<Val> {
        let row = self.raw(identity)?;
        if !matches!(row.get_or_null("sync_state").as_str(), Some("conflict" | "uncertain" | "error")) {
            return fail("no_conflict", "Bu notta çözülecek eşitleme sorunu yok.", 409);
        }
        let keep = match strategy.as_str() {
            Some("keep_local_copy") => true,
            Some("use_remote") => false,
            _ => return fail("invalid_strategy", "Geçersiz çözüm seçimi.", 400),
        };
        if keep {
            let ink = self.with_db(|db| {
                db.one(
                    "SELECT 1 FROM jobs WHERE note_id=? AND kind IN ('append_ink','erase_ink','move_ink') AND state NOT IN ('done','cancelled')",
                    &p![identity],
                )
            })?;
            if ink.is_some() {
                return fail("ink_copy_unsupported", "Çizim taslağınız korunuyor; ayrı nota kopyalama henüz desteklenmiyor.", 409);
            }
        }
        let projection = self.cached_editor(identity, Some(&row.get_or_null("revision")))?;
        let title = row.get_or_null("title");
        let copy = match &projection {
            Some(p) if keep && p.truthy() && key(key(p, "capabilities")?, "textEdit")?.truthy() => {
                Some(self.create_editor(h, &title, key(p, "model")?)?)
            }
            _ if keep => Some(self.create(&title, &row.get_or_null("body"))?),
            _ => None,
        };
        self.with_db(|db| {
            db.exec("UPDATE jobs SET state='cancelled' WHERE note_id=? AND state NOT IN ('done','cancelled')", &p![identity])?;
            db.exec("UPDATE notes SET sync_state='synced',error=NULL WHERE id=?", &p![identity])?;
            Ok(())
        })?;
        let server_id = row.get_or_null("server_id");
        let deleted = row.get_or_null("deleted_remote").truthy();
        if deleted {
            self.with_db(|db| {
                db.exec("UPDATE notes SET trashed=1,editable=0 WHERE id=?", &p![identity])?;
                Ok(())
            })?;
        } else if row.get_or_null("remote_document").truthy() {
            let doc = crate::val::loads(row.get_or_null("remote_document").as_str().unwrap_or(""))?;
            h.apply_document(self, &server_id, &doc)?;
        } else if row.get_or_null("base_document").truthy() {
            let doc = crate::val::loads(row.get_or_null("base_document").as_str().unwrap_or(""))?;
            h.apply_document(self, &server_id, &doc)?;
        } else {
            // An uncertain create may exist remotely. Keep the original visibly
            // archived; accepting remote never retries or destroys its local text.
            self.with_db(|db| {
                db.exec(
                    "UPDATE notes SET trashed=1,editable=0,reason=? WHERE id=?",
                    &p!["Önceki gönderim sonucu bilinmiyor; yerel metin korunuyor.", identity],
                )?;
                Ok(())
            })?;
        }
        if server_id.truthy() && !deleted {
            self.with_db(|db| {
                db.exec("UPDATE notes SET trashed=cloud_trashed WHERE id=?", &p![identity])?;
                Ok(())
            })?;
        }
        match copy {
            Some(c) if c.truthy() => Ok(c),
            _ => self.get(identity),
        }
    }
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::pyconv::install(m.py())?;
    m.add_function(wrap_pyfunction!(crate::pystore::now_ms, m)?)?;
    m.add_function(wrap_pyfunction!(crate::pystore::private_directory, m)?)?;
    m.add_function(wrap_pyfunction!(crate::pystore::normalize_commit, m)?)?;
    m.add_class::<crate::pyconv::StoreError>()?;
    m.add_class::<crate::pystore::Store>()?;
    m.add_class::<crate::pyconv::Connection>()?;
    m.add_class::<crate::pyconv::PyCursor>()?;
    m.add_class::<crate::pyconv::PyRow>()?;
    rs_common::export(m, &["now_ms", "private_directory", "normalize_commit", "StoreError", "Store"])
}
