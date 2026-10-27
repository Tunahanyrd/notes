//! Port of `samsung_notes/organization_store.py`: durable Cloud organization
//! intents (favorite/trash/folder) and the folder graph.
use crate::db::*;
use crate::host::Host;
use crate::p;
use crate::storage::{public, py_int, StoreCore};
use crate::val::{py_eq, Val};
#[cfg(feature = "python")]
use pyo3::prelude::*;
use rusqlite::types::Value as SqlVal;
use std::collections::{HashMap, HashSet};

pub const SCHEMA_VERSION: i64 = 6;

const SCHEMA: &str = "
        CREATE TABLE IF NOT EXISTS folder_nodes (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL, parent_id TEXT NOT NULL,
            name TEXT NOT NULL, state TEXT NOT NULL, modified_ms INTEGER NOT NULL,
            trashed_ms INTEGER NOT NULL, raw_json TEXT NOT NULL,
            sync_state TEXT NOT NULL DEFAULT 'synced', error TEXT
        );
        CREATE INDEX IF NOT EXISTS folder_parents ON folder_nodes(parent_id,kind,state);
        CREATE TABLE IF NOT EXISTS organization_jobs (
            id TEXT PRIMARY KEY, note_id TEXT NOT NULL REFERENCES notes(id),
            changes TEXT NOT NULL, baseline_node TEXT, snapshot TEXT NOT NULL,
            local_revision INTEGER NOT NULL, state TEXT NOT NULL,
            error TEXT, created_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS organization_active ON organization_jobs(state,created_ms);
        CREATE TABLE IF NOT EXISTS folder_jobs (
            id TEXT PRIMARY KEY, folder_id TEXT NOT NULL, kind TEXT NOT NULL,
            changes TEXT NOT NULL, baseline_node TEXT, state TEXT NOT NULL,
            error TEXT, created_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS editor_documents (
            note_id TEXT PRIMARY KEY REFERENCES notes(id),
            revision INTEGER NOT NULL, projection TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS editor_jobs (
            job_id TEXT PRIMARY KEY REFERENCES jobs(id), model TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ink_documents (
            note_id TEXT PRIMARY KEY REFERENCES notes(id),
            revision INTEGER NOT NULL, projection TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ink_jobs (
            job_id TEXT PRIMARY KEY REFERENCES jobs(id), model TEXT NOT NULL,
            next_index INTEGER NOT NULL, current_snapshot TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS pdf_page_jobs (
            job_id TEXT PRIMARY KEY REFERENCES jobs(id), file_sha256 TEXT NOT NULL, file_name TEXT NOT NULL,
            page_sizes TEXT NOT NULL, page_indices TEXT NOT NULL, template_type INTEGER
        );
        CREATE TABLE IF NOT EXISTS ink_erase_jobs (
            job_id TEXT PRIMARY KEY REFERENCES jobs(id), model TEXT NOT NULL,
            next_index INTEGER NOT NULL, current_snapshot TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ink_move_jobs (
            job_id TEXT PRIMARY KEY REFERENCES jobs(id), model TEXT NOT NULL,
            next_index INTEGER NOT NULL, current_snapshot TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS page_op_jobs (
            job_id TEXT PRIMARY KEY REFERENCES jobs(id), op TEXT NOT NULL, spec TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS page_style_jobs (
            job_id TEXT PRIMARY KEY REFERENCES jobs(id), color INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ink_drafts (
            note_id TEXT PRIMARY KEY REFERENCES notes(id), revision INTEGER NOT NULL,
            strokes TEXT NOT NULL, updated_ms INTEGER NOT NULL
        );
    ";

fn fail409<T>(code: &str, message: &str) -> Result<T> {
    fail(code, message, 409)
}

/// Read only the validated snapshot's NoteResource attributes.
pub fn favorite_from_document(document: &Val) -> Result<Option<bool>> {
    use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
    use base64::engine::DecodePaddingMode;
    use base64::Engine as _;
    const STRICT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
    );
    let encoded = match document {
        Val::Dict(_) => document.get_or_null("resource_xml"),
        other => return Err(Error::Attribute(format!("'{}' object has no attribute 'get'", other.type_name()))),
    };
    if !encoded.truthy() {
        return Ok(None);
    }
    let input: &[u8] = match &encoded {
        Val::Str(s) if s.is_ascii() => s.as_bytes(),
        Val::Bytes(b) => b,
        _ => return Ok(None),
    };
    let Ok(raw) = STRICT.decode(input) else { return Ok(None) };
    if raw.len() > 32 * 1024 * 1024 || raw.windows(2).any(|w| w == b"<!") {
        return Ok(None);
    }
    let Ok(text) = std::str::from_utf8(&raw) else { return Ok(None) };
    let Ok(doc) = roxmltree::Document::parse(text) else { return Ok(None) };
    let attrs: Vec<_> = doc
        .root_element()
        .children()
        .filter(|n| n.is_element() && n.tag_name().name().to_lowercase() == "attributes")
        .collect();
    if attrs.len() != 1 {
        return Ok(None);
    }
    let value = attrs[0].attribute("isFavorite").unwrap_or("false").to_lowercase();
    Ok(match value.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    })
}

fn table_columns(db: &dyn Sql, table: &str) -> Result<HashSet<String>> {
    Ok(db
        .query(&format!("PRAGMA table_info({})", table), &[])?
        .rows
        .iter()
        .filter_map(|r| match r.get(1) {
            Some(SqlVal::Text(s)) => Some(s.clone()),
            _ => None,
        })
        .collect())
}

/// Schema upgrade; every ALTER, version update and CREATE stays under one
/// BEGIN IMMEDIATE transaction (the caller commits).
pub fn migrate(db: &dyn Sql) -> Result<()> {
    db.exec("BEGIN IMMEDIATE", &[])?;
    let row = db.one("SELECT value FROM metadata WHERE key='schema_version'", &[])?;
    let Some(row) = row else { return Err(Error::Type("'NoneType' object is not subscriptable".into())) };
    let version = match &row[0] {
        SqlVal::Text(s) => Some(s.as_str()),
        _ => None,
    };
    let set_version = |db: &dyn Sql| -> Result<()> {
        db.exec("UPDATE metadata SET value=? WHERE key='schema_version'", &p![SCHEMA_VERSION.to_string()])?;
        Ok(())
    };
    match version {
        Some("1") => {
            for definition in [
                "folder_id TEXT",
                "cloud_folder_id TEXT",
                "cloud_favorite INTEGER NOT NULL DEFAULT 0",
                "cloud_trashed INTEGER NOT NULL DEFAULT 0",
                "org_state TEXT NOT NULL DEFAULT 'synced'",
                "org_error TEXT",
                "title_editable INTEGER",
            ] {
                db.exec(&format!("ALTER TABLE notes ADD COLUMN {}", definition), &[])?;
            }
            // Preserve old local-only choices for recovery. They are NOT Cloud jobs.
            db.exec("CREATE TABLE legacy_local_organization AS SELECT id,favorite,trashed FROM notes", &[])?;
            db.exec("UPDATE notes SET cloud_favorite=favorite,cloud_trashed=trashed", &[])?;
            set_version(db)?;
        }
        Some("2" | "3") => {
            let columns = table_columns(db, "folder_nodes")?;
            if !columns.is_empty() && !columns.contains("sync_state") {
                db.exec("ALTER TABLE folder_nodes ADD COLUMN sync_state TEXT NOT NULL DEFAULT 'synced'", &[])?;
                db.exec("ALTER TABLE folder_nodes ADD COLUMN error TEXT", &[])?;
            }
            let columns = table_columns(db, "organization_jobs")?;
            if !columns.is_empty() && !columns.contains("snapshot") {
                db.exec("ALTER TABLE organization_jobs ADD COLUMN snapshot TEXT NOT NULL DEFAULT '{}'", &[])?;
                db.exec("UPDATE organization_jobs SET state='uncertain',error='Eski işlemin kaynak sürümü kayıtlı değil; yeniden gönderilmedi.' WHERE state IN ('queued','running')", &[])?;
                db.exec("UPDATE notes SET org_state='uncertain',org_error='Eski işlem yeniden gönderilmedi; buluttaki durumu kontrol et.' WHERE id IN (SELECT note_id FROM organization_jobs WHERE state='uncertain')", &[])?;
            }
            if !table_columns(db, "notes")?.contains("title_editable") {
                db.exec("ALTER TABLE notes ADD COLUMN title_editable INTEGER", &[])?;
            }
            set_version(db)?;
        }
        Some("4" | "5") => set_version(db)?,
        Some("6") => {}
        _ => return Err(Error::Value("unsupported library schema version".into())),
    }
    for statement in SCHEMA.split(';') {
        if !statement.trim().is_empty() {
            db.exec(statement, &[])?;
        }
    }
    Ok(())
}

/// Samsung's own folder order: explicit order ascending, then newest first.
fn folder_rank(raw: &Val, id: &str) -> Result<(i64, i64, String)> {
    let order = match raw.get("order") {
        Some(v) if v.truthy() => py_int(v).unwrap_or(0),
        _ => 0,
    };
    let created = match raw.get("created_time") {
        Some(v) if v.truthy() => py_int(v)?,
        _ => 0,
    };
    Ok((order, -created, id.to_string()))
}

/// Hashable key for Python dict semantics over JSON ids.
fn id_key(v: &Val) -> String {
    match v {
        Val::Str(s) => format!("s{}", s),
        other => format!("j{}", crate::val::dumps_ascii(other).unwrap_or_default()),
    }
}

impl StoreCore {
    pub fn organization_node(&self, resource_id: &Val) -> Result<Option<Val>> {
        let id = b(resource_id)?;
        let row = self.with_db(|db| db.one("SELECT raw_json FROM folder_nodes WHERE id=? AND kind=?", &vec![id, text("note")]))?;
        match row {
            Some(r) => Ok(Some(loads_sql(&r[0])?)),
            None => Ok(None),
        }
    }

    pub fn folders(&self) -> Result<Val> {
        let (rows, counts, all, trash) = self.with_db(|db| {
            let rows = db.query("SELECT * FROM folder_nodes WHERE kind='folder' AND state IN ('normal','trashed')", &[])?;
            let counts = db.query("SELECT folder_id,COUNT(*) FROM notes WHERE trashed=0 AND deleted_remote=0 GROUP BY folder_id", &[])?;
            let all = db.exec("SELECT COUNT(*) FROM notes WHERE trashed=0 AND deleted_remote=0", &[])?.rows[0][0].clone();
            let trash = db.exec("SELECT COUNT(*) FROM notes WHERE trashed=1 AND deleted_remote=0", &[])?.rows[0][0].clone();
            Ok((rows, counts, all, trash))
        })?;
        let counts: HashMap<String, SqlVal> =
            counts.rows.iter().map(|r| (id_key(&sql_to_val(&r[0])), r[1].clone())).collect();
        let mut ranked = Vec::new();
        for row in rows.iter() {
            let raw = loads_sql(row.raw("raw_json"))?;
            let rank = folder_rank(&raw, row.text("id").unwrap_or(""))?;
            ranked.push((rank, raw, row));
        }
        ranked.sort_by(|a, b| a.0.cmp(&b.0));
        let known: HashSet<&str> = ranked.iter().filter_map(|r| r.2.text("id")).collect();
        let ids: Vec<String> = ranked.iter().map(|r| r.2.text("id").unwrap_or("").to_string()).collect();
        let mut parents: HashMap<String, Option<String>> = HashMap::new();
        for r in &ranked {
            let parent = r.2.text("parent_id").filter(|p| known.contains(p)).map(|s| s.to_string());
            parents.insert(r.2.text("id").unwrap_or("").to_string(), parent);
        }
        // Damaged/cyclic ancestry must not make navigation recurse indefinitely.
        for identity in &ids {
            let mut cursor = Some(identity.clone());
            let mut seen = HashSet::new();
            while let Some(c) = cursor {
                if seen.contains(&c) {
                    parents.insert(identity.clone(), None);
                    break;
                }
                seen.insert(c.clone());
                cursor = parents.get(&c).cloned().flatten();
            }
        }
        let mut folders = Vec::new();
        let mut trashed = Vec::new();
        for (rank, raw, row) in &ranked {
            let id = row.text("id").unwrap_or("");
            let color = raw.get_or_null("color");
            let item = Val::dict(vec![
                ("id", row.val("id")),
                ("name", row.val("name")),
                ("parentId", Val::opt_str(parents.get(id).cloned().flatten().as_deref())),
                ("color", if color.truthy() { color } else { Val::Null }),
                ("noteCount", counts.get(&id_key(&row.val("id"))).map(sql_to_val).unwrap_or(Val::Int(0))),
                ("state", row.val("state")),
                // Default device folders (order 999999) are hidden by Samsung clients when empty.
                ("system", Val::Bool(rank.0 >= 999999)),
                // Samsung Cloud's trashedTime (ms); trash keeps items 30 days.
                ("trashedAt", match (row.text("state"), row.val("trashed_ms")) {
                    (Some("trashed"), Val::Int(ms)) if ms > 0 => Val::Int(ms),
                    _ => Val::Null,
                }),
                ("syncState", row.val("sync_state")),
                ("error", row.val("error")),
            ]);
            match row.text("state") {
                Some("normal") => folders.push(item),
                Some("trashed") => trashed.push(item),
                _ => {}
            }
        }
        let initialized = self.meta("folder_initialized")?.unwrap_or(Val::Bool(false));
        Ok(Val::dict(vec![
            ("folders", Val::List(folders)),
            ("trash", Val::List(trashed)),
            ("initialized", initialized),
            ("counts", Val::dict(vec![("all", sql_to_val(&all)), ("trash", sql_to_val(&trash))])),
        ]))
    }

    pub fn apply_folder_graph(&self, inventory: &Val) -> Result<()> {
        let replace_graph = inventory.get_or_null("has_resource").truthy();
        let mut inventory = inventory.clone();
        if !replace_graph {
            // Content may have arrived after this graph. Even an unchanged
            // folder feed must bind newly downloaded notes to cached nodes.
            let cur = self.with_db(|db| db.query("SELECT raw_json FROM folder_nodes", &[]))?;
            let nodes = cur.rows.iter().map(|r| loads_sql(&r[0])).collect::<Result<Vec<_>>>()?;
            if !matches!(inventory, Val::Dict(_)) {
                return Err(Error::Type("cannot convert dictionary update sequence element #0 to a sequence".into()));
            }
            inventory.set("has_resource", Val::Bool(true));
            inventory.set("nodes", Val::List(nodes));
        }
        let nodes = match key(&inventory, "nodes")? {
            Val::List(n) => n.clone(),
            other => return Err(Error::Type(format!("'{}' object is not iterable", other.type_name()))),
        };
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for (i, node) in nodes.iter().enumerate() {
            by_id.insert(id_key(key(node, "id")?), i);
        }
        if by_id.len() != nodes.len() {
            return fail409("invalid_metadata", "Klasör yanıtında tekrarlanan kayıt var.");
        }
        let last_change_point = key(&inventory, "last_change_point").cloned();
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            if replace_graph {
                let pending = db.query("SELECT * FROM folder_nodes WHERE kind='folder' AND sync_state!='synced'", &[])?;
                db.exec("DELETE FROM folder_nodes", &[])?;
                for n in &nodes {
                    let params = vec![
                        b(key(n, "id")?)?,
                        b(key(n, "type")?)?,
                        b(key(n, "parent_id")?)?,
                        b(key(n, "name")?)?,
                        b(key(n, "state")?)?,
                        b(key(n, "sync_modified_time")?)?,
                        b(key(n, "trashed_time")?)?,
                        text(&dumps_utf8(n)?),
                    ];
                    db.exec(
                        "INSERT INTO folder_nodes (id,kind,parent_id,name,state,modified_ms,trashed_ms,raw_json) VALUES (?,?,?,?,?,?,?,?)",
                        &params,
                    )?;
                }
                for row in &pending.rows {
                    db.exec("INSERT OR REPLACE INTO folder_nodes VALUES (?,?,?,?,?,?,?,?,?,?)", row)?;
                }
            }
            let rows = db.query(
                "SELECT id,resource_id,trashed,folder_id,cloud_folder_id,cloud_trashed,org_state,sync_state FROM notes",
                &[],
            )?;
            for row in rows.iter() {
                let Some(&at) = by_id.get(&id_key(&row.val("resource_id"))) else { continue };
                let node = &nodes[at];
                if !py_eq(key(node, "type")?, &Val::s("note")) {
                    continue;
                }
                if py_eq(key(node, "state")?, &Val::s("deleted")) {
                    // Content feed owns permanent deletion and dirty-note protection.
                    continue;
                }
                let parent = key(node, "parent_id")?;
                let folder = match by_id.get(&id_key(parent)) {
                    Some(&p) if py_eq(key(&nodes[p], "type")?, &Val::s("folder")) => parent.clone(),
                    _ => Val::Null,
                };
                let trash = py_eq(key(node, "state")?, &Val::s("trashed")) as i64;
                if !(sql_eq(row.raw("cloud_folder_id"), &folder) && sql_eq(row.raw("cloud_trashed"), &Val::Int(trash))) {
                    db.exec(
                        "UPDATE notes SET cloud_folder_id=?,cloud_trashed=? WHERE id=?",
                        &vec![b(&folder)?, int(trash), row.raw("id").clone()],
                    )?;
                }
                if trash == 1 && !matches!(row.text("sync_state"), Some("synced" | "local")) && !row.truthy("trashed") {
                    let message = "Not başka bir cihazda çöp kutusuna taşındı. Gönderilmemiş metnin burada korunuyor.";
                    db.exec("UPDATE notes SET sync_state='conflict',error=? WHERE id=?", &vec![text(message), row.raw("id").clone()])?;
                    db.exec(
                        "UPDATE jobs SET state='conflict',error=? WHERE note_id=? AND state IN ('queued','running')",
                        &vec![text(message), row.raw("id").clone()],
                    )?;
                    continue;
                }
                if row.text("org_state") == Some("synced") {
                    let changed = !sql_eq(row.raw("folder_id"), &folder) || !sql_eq(row.raw("trashed"), &Val::Int(trash));
                    if changed {
                        db.exec(
                            "UPDATE notes SET folder_id=?,trashed=?,revision=revision+1 WHERE id=?",
                            &vec![b(&folder)?, int(trash), row.raw("id").clone()],
                        )?;
                    }
                }
            }
            let cursor = last_change_point?;
            for (k, v) in [("folder_cursor", cursor), ("folder_initialized", Val::Bool(true))] {
                db.exec(
                    "INSERT INTO metadata VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE metadata.value<>excluded.value",
                    &p![k, dumps_ascii(&v)?],
                )?;
            }
            Ok(())
        })
    }

    pub fn organize_cloud<H: Host>(&self, h: &H, identity: &str, changes: &Val) -> Result<Val> {
        let Val::Dict(items) = changes else {
            return Err(Error::Type(format!("'{}' object is not iterable", changes.type_name())));
        };
        let mut changes: Vec<(String, Val)> = items.clone();
        let revision = match changes.iter().position(|(k, _)| k == "expectedRevision") {
            Some(at) => changes.remove(at).1,
            None => Val::Null,
        };
        if changes.len() != 1 || !changes.iter().all(|(k, _)| matches!(k.as_str(), "favorite" | "trashed" | "folderId")) {
            return fail("invalid_change", "Her istekte tek bir not işlemi gönder.", 400);
        }
        let (field, value) = changes.remove(0);
        let bad = match field.as_str() {
            "favorite" | "trashed" => !matches!(value, Val::Bool(_)),
            _ => match &value {
                Val::Null => false,
                Val::Str(s) => !(1..=1024).contains(&s.chars().count()),
                _ => true,
            },
        };
        if bad {
            return fail("invalid_change", "Geçersiz not işlemi.", 400);
        }
        if !revision.is_null() && !revision.is_exact_int() {
            return fail("revision_required", "Not sürümü geçersiz.", 400);
        }
        let early = self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let Some(row) = cur.one() else { return fail("not_found", "Not bulunamadı.", 404) };
            if !revision.is_null() && !sql_eq(row.raw("revision"), &revision) {
                return fail409("revision_conflict", "Not başka bir yerde değişti. Güncel sürümü aç.");
            }
            h.ensure_no_active_ink(db, identity)?;
            if !row.truthy("server_id") || !row.truthy("loaded") {
                return fail409("cloud_note_pending", "Önce notun buluta eşitlenmesini bekle.");
            }
            if row.truthy("deleted_remote") {
                return fail409("remote_deleted", "Bu not buluttan kalıcı olarak silinmiş.");
            }
            if !matches!(row.text("sync_state"), Some("synced" | "local")) || matches!(row.text("org_state"), Some("pending" | "uncertain")) {
                return fail409("sync_pending", "Önce bu notun bekleyen eşitlemesini tamamla.");
            }
            if field == "folderId" && !value.is_null() {
                let folder = db.one(
                    "SELECT 1 FROM folder_nodes WHERE id=? AND kind='folder' AND state='normal' AND sync_state='synced'",
                    &vec![b(&value)?],
                )?;
                if folder.is_none() {
                    return fail("folder_missing", "Seçilen klasör artık bulunmuyor.", 404);
                }
            }
            let column = match field.as_str() {
                "favorite" => "favorite",
                "trashed" => "trashed",
                _ => "folder_id",
            };
            if sql_eq(row.raw(column), &value) && row.text("org_state") == Some("synced") {
                return Ok(Some(public(&row, true)?));
            }
            let node = db.one("SELECT raw_json FROM folder_nodes WHERE id=?", &vec![row.raw("resource_id").clone()])?;
            let job = uuid4();
            let wire = if field == "folderId" {
                Val::dict(vec![("folder_id", if value.is_null() { Val::s("root") } else { value.clone() })])
            } else {
                Val::dict(vec![(field.as_str(), value.clone())])
            };
            db.exec(
                "INSERT INTO organization_jobs (id,note_id,changes,baseline_node,snapshot,local_revision,state,error,created_ms) VALUES (?,?,?,?,?,?,?,?,?)",
                &vec![
                    text(&job),
                    text(identity),
                    text(&dumps_ascii(&wire)?),
                    node.map(|n| n[0].clone()).unwrap_or(SqlVal::Null),
                    row.raw("base_document").clone(),
                    int(row.int("revision") + 1),
                    text("queued"),
                    null(),
                    int(now_ms()),
                ],
            )?;
            db.exec(
                &format!("UPDATE notes SET {}=?,org_state='pending',org_error=NULL,revision=revision+1 WHERE id=?", column),
                &vec![b(&value)?, text(identity)],
            )?;
            Ok(None)
        })?;
        match early {
            Some(v) => Ok(v),
            None => self.get(identity),
        }
    }

    /// Queue the missing root node only for a proven successful local create.
    pub fn register_note_graph(&self, identity: &Val) -> Result<()> {
        let id = b(identity)?;
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &[id.clone()])?;
            let Some(row) = cur.one() else { return Ok(()) };
            if !row.truthy("server_id") || row.text("org_state") != Some("synced") || row.text("sync_state") != Some("synced") {
                return Ok(());
            }
            if db.one("SELECT 1 FROM folder_nodes WHERE id=?", &vec![row.raw("resource_id").clone()])?.is_some() {
                return Ok(());
            }
            let created = db.one(
                "SELECT 1 FROM jobs WHERE note_id=? AND kind IN ('create','create_editor') AND state='done'",
                &[id.clone()],
            )?;
            if created.is_none() {
                return Ok(());
            }
            db.exec(
                "INSERT INTO organization_jobs (id,note_id,changes,baseline_node,snapshot,local_revision,state,error,created_ms) VALUES (?,?,?,?,?,?,?,?,?)",
                &vec![
                    text(&uuid4()),
                    id.clone(),
                    text("{\"folder_id\":\"root\"}"),
                    null(),
                    row.raw("base_document").clone(),
                    row.raw("revision").clone(),
                    text("queued"),
                    null(),
                    int(now_ms()),
                ],
            )?;
            db.exec("UPDATE notes SET org_state='pending',org_error=NULL WHERE id=?", &[id.clone()])?;
            Ok(())
        })
    }

    pub fn next_organization_job(&self) -> Result<Option<Val>> {
        let cur = self.with_db(|db| {
            db.query("SELECT * FROM organization_jobs WHERE state IN ('queued','running') ORDER BY created_ms,id LIMIT 1", &[])
        })?;
        Ok(cur.one().map(|r| r.to_dict()))
    }

    fn public_state(state: &Val) -> Result<&'static str> {
        Ok(match state.as_str() {
            Some("queued") | Some("running") => "pending",
            Some("failed") => "error",
            Some("uncertain") => "uncertain",
            Some("conflict") => "conflict",
            Some("done") => "synced",
            _ => {
                let shown = match state {
                    Val::Str(s) => py_repr_str(s),
                    other => dumps_ascii(other).unwrap_or_default(),
                };
                return Err(Error::Key(shown));
            }
        })
    }

    pub fn organization_state(&self, job: &Val, state: &Val, message: &Val) -> Result<()> {
        let (id, note) = (b(key(job, "id")?)?, key(job, "note_id")?.clone());
        let (st, msg) = (b(state)?, b(message)?);
        self.with_db(|db| {
            db.exec("UPDATE organization_jobs SET state=?,error=? WHERE id=?", &vec![st, msg.clone(), id])?;
            let public = Self::public_state(state)?;
            db.exec("UPDATE notes SET org_state=?,org_error=? WHERE id=?", &vec![text(public), msg, b(&note)?])?;
            Ok(())
        })
    }

    pub fn organization_completed(&self, job: &Val) -> Result<()> {
        // Caller first applies verified remote snapshots. Only acknowledge the
        // exact queued intent; publication receipt is kept by CloudMetadata.
        let (id, note) = (b(key(job, "id")?)?, b(key(job, "note_id")?)?);
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let current = db.one("SELECT state FROM organization_jobs WHERE id=? AND note_id=?", &vec![id.clone(), note.clone()])?;
            match current.as_ref().map(|r| &r[0]) {
                Some(SqlVal::Text(s)) if s == "queued" || s == "running" => {}
                _ => return Ok(()),
            }
            db.exec("UPDATE organization_jobs SET state='done',error=NULL WHERE id=?", &[id])?;
            db.exec(
                "UPDATE notes SET org_state='synced',org_error=NULL,favorite=cloud_favorite,trashed=cloud_trashed,folder_id=cloud_folder_id WHERE id=?",
                &[note],
            )?;
            Ok(())
        })
    }

    pub fn apply_organization_node(&self, node: &Val) -> Result<()> {
        self.with_db(|db| write_node(db, node))
    }

    pub fn folder_completed(&self, job: &Val, node: &Val) -> Result<()> {
        let (id, folder) = (b(key(job, "id")?)?, b(key(job, "folder_id")?)?);
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let current = db.one("SELECT state FROM folder_jobs WHERE id=? AND folder_id=?", &vec![id.clone(), folder])?;
            match current.as_ref().map(|r| &r[0]) {
                Some(SqlVal::Text(s)) if s == "queued" || s == "running" => {}
                _ => return Ok(()),
            }
            write_node(db, node)?;
            db.exec("UPDATE folder_jobs SET state='done',error=NULL WHERE id=?", &[id])?;
            Ok(())
        })
    }

    pub fn apply_favorite(&self, identity: &str, document: &Val) -> Result<()> {
        let Some(value) = favorite_from_document(document)? else { return Ok(()) };
        self.with_db(|db| {
            db.exec("UPDATE notes SET cloud_favorite=? WHERE id=?", &p![value, identity])?;
            db.exec("UPDATE notes SET favorite=? WHERE id=? AND org_state='synced'", &p![value, identity])?;
            Ok(())
        })
    }

    pub fn folder(&self, identity: &Val) -> Result<Val> {
        let values = self.folders()?;
        for group in ["folders", "trash"] {
            if let Some(Val::List(items)) = values.get(group) {
                for item in items {
                    if py_eq(&item.get_or_null("id"), identity) {
                        return Ok(item.clone());
                    }
                }
            }
        }
        fail("folder_missing", "Klasör bulunamadı.", 404)
    }

    pub fn create_folder(&self, name: &Val, parent: &Val) -> Result<Val> {
        let name = folder_name(name)?;
        let identity = uuid4();
        let now = now_ms();
        self.check_folder_parent(parent, None)?;
        let parent_id = if parent.truthy() { parent.clone() } else { Val::s("root") };
        let wire = Val::dict(vec![("name", Val::s(&name)), ("parent_id", parent_id.clone())]);
        let node = Val::dict(vec![
            ("id", Val::s(&identity)),
            ("type", Val::s("folder")),
            ("parent_id", parent_id.clone()),
            ("name", Val::s(&name)),
            ("state", Val::s("normal")),
            ("sync_modified_time", Val::Int(0)),
            ("created_time", Val::Int(now)),
            ("modified_time", Val::Int(now)),
            ("trashed_time", Val::Int(0)),
            ("restore_path", Val::s("")),
            ("color", Val::s("")),
            ("order", Val::s("")),
            ("raw_xml", Val::s("")),
        ]);
        self.with_db(|db| {
            db.exec(
                "INSERT INTO folder_nodes VALUES (?,?,?,?,?,?,?,?,?,?)",
                &vec![
                    text(&identity),
                    text("folder"),
                    b(&parent_id)?,
                    text(&name),
                    text("normal"),
                    int(now),
                    int(0),
                    text(&dumps_utf8(&node)?),
                    text("pending"),
                    null(),
                ],
            )?;
            db.exec(
                "INSERT INTO folder_jobs VALUES (?,?,?,?,?,?,?,?)",
                &p![uuid4(), &identity, "create", dumps_ascii(&wire)?, null(), "queued", null(), now],
            )?;
            Ok(())
        })?;
        self.folder(&Val::Str(identity))
    }

    pub fn check_folder_parent(&self, parent: &Val, identity: Option<&Val>) -> Result<()> {
        if parent.is_null() {
            return Ok(());
        }
        let parent = match parent {
            Val::Str(s) if (1..=1024).contains(&s.chars().count()) => s.clone(),
            _ => return fail("invalid_parent", "Geçersiz üst klasör.", 400),
        };
        let cur = self.with_db(|db| db.query("SELECT id,parent_id,sync_state FROM folder_nodes WHERE kind='folder' AND state='normal'", &[]))?;
        let rows: HashMap<String, (Option<String>, Option<String>)> = cur
            .iter()
            .map(|r| {
                (
                    r.text("id").unwrap_or("").to_string(),
                    (r.text("parent_id").map(String::from), r.text("sync_state").map(String::from)),
                )
            })
            .collect();
        match rows.get(&parent) {
            Some((_, Some(state))) if state == "synced" => {}
            _ => return fail409("folder_missing", "Üst klasör henüz bulutta hazır değil."),
        }
        let mut seen: HashSet<String> = HashSet::new();
        if let Some(id) = identity.filter(|i| i.truthy()) {
            seen.insert(id_key(id));
        }
        let mut cursor = Some(parent);
        while let Some(c) = cursor {
            let Some((parent_id, _)) = rows.get(&c) else { break };
            let k = id_key(&Val::Str(c.clone()));
            if seen.contains(&k) {
                return fail409("folder_cycle", "Klasörü kendisinin veya alt klasörünün içine taşıyamazsın.");
            }
            seen.insert(k);
            cursor = parent_id.clone();
        }
        Ok(())
    }

    pub fn update_folder(&self, identity: &Val, changes: &Val) -> Result<Val> {
        let items = match changes {
            Val::Dict(items) if items.len() == 1 && matches!(items[0].0.as_str(), "name" | "parentId" | "trashed") => items,
            _ => return fail("invalid_change", "Her istekte tek bir klasör işlemi gönder.", 400),
        };
        let (k, mut value) = items[0].clone();
        if k == "name" {
            value = Val::Str(folder_name(&value)?);
        }
        if k == "trashed" && !matches!(value, Val::Bool(_)) {
            return fail("invalid_change", "Geçersiz çöp kutusu tercihi.", 400);
        }
        if k == "parentId" {
            self.check_folder_parent(&value, Some(identity))?;
        }
        let id = b(identity)?;
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM folder_nodes WHERE id=? AND kind='folder' AND state IN ('normal','trashed')", &[id.clone()])?;
            let Some(row) = cur.one() else { return fail("folder_missing", "Klasör bulunamadı.", 404) };
            if row.text("sync_state") != Some("synced") {
                return fail409("sync_pending", "Klasörün bekleyen işlemini tamamlamasını bekle.");
            }
            if k == "trashed" && value.truthy() && db.one("SELECT 1 FROM folder_nodes WHERE parent_id=? AND state='normal' LIMIT 1", &[id.clone()])?.is_some() {
                return fail409(
                    "folder_not_empty",
                    "Önce klasördeki notları ve alt klasörleri taşı. Şimdilik yalnız boş klasörler çöp kutusuna gönderilebilir.",
                );
            }
            let wire_key = if k == "parentId" { "parent_id" } else { k.as_str() };
            let wire_value = if k == "parentId" {
                if value.truthy() {
                    value.clone()
                } else {
                    Val::s("root")
                }
            } else {
                value.clone()
            };
            db.exec(
                "INSERT INTO folder_jobs VALUES (?,?,?,?,?,?,?,?)",
                &vec![
                    text(&uuid4()),
                    id.clone(),
                    text("update"),
                    text(&dumps_ascii(&Val::dict(vec![(wire_key, wire_value.clone())]))?),
                    row.raw("raw_json").clone(),
                    text("queued"),
                    null(),
                    int(now_ms()),
                ],
            )?;
            let column = match k.as_str() {
                "name" => "name",
                "parentId" => "parent_id",
                _ => "state",
            };
            let shown = if k == "trashed" { Val::s(if value.truthy() { "trashed" } else { "normal" }) } else { wire_value };
            db.exec(
                &format!("UPDATE folder_nodes SET {}=?,sync_state='pending',error=NULL WHERE id=?", column),
                &vec![b(&shown)?, id.clone()],
            )?;
            Ok(())
        })?;
        self.folder(identity)
    }

    pub fn next_folder_job(&self) -> Result<Option<Val>> {
        let cur = self.with_db(|db| {
            db.query("SELECT * FROM folder_jobs WHERE state IN ('queued','running') ORDER BY created_ms,id LIMIT 1", &[])
        })?;
        Ok(cur.one().map(|r| r.to_dict()))
    }

    pub fn folder_job_state(&self, job: &Val, state: &Val, message: &Val) -> Result<()> {
        let (id, folder) = (b(key(job, "id")?)?, key(job, "folder_id")?.clone());
        let (st, msg) = (b(state)?, b(message)?);
        self.with_db(|db| {
            db.exec("UPDATE folder_jobs SET state=?,error=? WHERE id=?", &vec![st, msg.clone(), id])?;
            let public = Self::public_state(state)?;
            db.exec("UPDATE folder_nodes SET sync_state=?,error=? WHERE id=?", &vec![text(public), msg, b(&folder)?])?;
            Ok(())
        })
    }

    pub fn accept_remote_organization(&self, identity: &str) -> Result<Val> {
        self.with_db(|db| {
            db.exec("UPDATE organization_jobs SET state='cancelled' WHERE note_id=? AND state!='done'", &p![identity])?;
            db.exec(
                "UPDATE notes SET org_state='synced',org_error=NULL,favorite=cloud_favorite,trashed=cloud_trashed,folder_id=cloud_folder_id,revision=revision+1 WHERE id=?",
                &p![identity],
            )?;
            Ok(())
        })?;
        self.get(identity)
    }

    pub fn accept_remote_folder(&self, identity: &Val, inventory: &Val) -> Result<Option<Val>> {
        let mut found = None;
        if let Val::List(nodes) = key(inventory, "nodes")? {
            for n in nodes {
                if py_eq(key(n, "id")?, identity) && py_eq(key(n, "type")?, &Val::s("folder")) {
                    found = Some(n.clone());
                    break;
                }
            }
        }
        let id = b(identity)?;
        self.with_db(|db| {
            db.exec("UPDATE folder_jobs SET state='cancelled' WHERE folder_id=? AND state!='done'", &[id.clone()])?;
            db.exec("DELETE FROM folder_nodes WHERE id=?", &[id.clone()])?;
            Ok(())
        })?;
        if let Some(node) = &found {
            self.apply_organization_node(node)?;
        }
        match found {
            Some(node) if !py_eq(key(&node, "state")?, &Val::s("deleted")) => Ok(Some(self.folder(identity)?)),
            _ => Ok(None),
        }
    }
}

/// OrganizationStore._write_node(db, node)
pub fn write_node(db: &dyn Sql, node: &Val) -> Result<()> {
    let params = vec![
        b(key(node, "id")?)?,
        b(key(node, "type")?)?,
        b(key(node, "parent_id")?)?,
        b(key(node, "name")?)?,
        b(key(node, "state")?)?,
        b(key(node, "sync_modified_time")?)?,
        b(key(node, "trashed_time")?)?,
        text(&dumps_utf8(node)?),
    ];
    db.exec(
        "INSERT INTO folder_nodes (id,kind,parent_id,name,state,modified_ms,trashed_ms,raw_json) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET \
         kind=excluded.kind,parent_id=excluded.parent_id,name=excluded.name,state=excluded.state,\
         modified_ms=excluded.modified_ms,trashed_ms=excluded.trashed_ms,raw_json=excluded.raw_json,sync_state='synced',error=NULL",
        &params,
    )?;
    if py_eq(key(node, "type")?, &Val::s("note")) {
        let parent = key(node, "parent_id")?;
        let parent = if py_eq(parent, &Val::s("root")) { SqlVal::Null } else { b(parent)? };
        let trashed = py_eq(key(node, "state")?, &Val::s("trashed")) as i64;
        db.exec(
            "UPDATE notes SET cloud_folder_id=?,cloud_trashed=? WHERE resource_id=?",
            &vec![parent, int(trashed), b(key(node, "id")?)?],
        )?;
    }
    Ok(())
}

/// OrganizationStore._folder_name(value)
pub fn folder_name(value: &Val) -> Result<String> {
    let ok = match value {
        Val::Str(s) => {
            !s.trim().is_empty() && s.len() <= 1024 && !s.chars().any(|c| (c as u32) < 32 || (0xD800..=0xDFFF).contains(&(c as u32)))
        }
        _ => false,
    };
    if !ok {
        return fail("invalid_folder_name", "Geçerli bir klasör adı gir.", 400);
    }
    Ok(value.as_str().unwrap().trim().to_string())
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(crate::pystore::favorite_from_document, m)?)?;
    m.add_function(wrap_pyfunction!(crate::pystore::migrate, m)?)?;
    crate::pystore::add_marker(m, "OrganizationStore", "samsung_notes.organization_store")?;
    rs_common::export(m, &["favorite_from_document", "migrate", "OrganizationStore"])
}
