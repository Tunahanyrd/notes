//! Port of `samsung_notes/editor_store.py`: version-bound editor projections
//! and immutable rich-text outbox models.
use crate::db::*;
use crate::host::Host;
use crate::p;
use crate::storage::{public, validate_text, StoreCore};
use crate::val::{py_eq, Val};
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub fn projection(model: &Val) -> Val {
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
        ("model", model.clone()),
        ("reason", Val::Null),
    ])
}

impl StoreCore {
    pub fn cached_editor(&self, identity: &str, revision: Option<&Val>) -> Result<Option<Val>> {
        let row = self.with_db(|db| match revision {
            None => db.one("SELECT projection FROM editor_documents WHERE note_id=?", &p![identity]),
            Some(r) => db.one("SELECT projection FROM editor_documents WHERE note_id=? AND revision=?", &vec![text(identity), b(r)?]),
        })?;
        match row {
            Some(r) => Ok(Some(loads_sql(&r[0])?)),
            None => Ok(None),
        }
    }

    pub fn cache_editor(&self, identity: &str, revision: &Val, projection: &Val) -> Result<()> {
        let encoded = dumps_utf8(projection)?;
        let rev = b(revision)?;
        self.with_db(|db| {
            db.exec(
                "INSERT INTO editor_documents (note_id,revision,projection)
                SELECT id,revision,? FROM notes WHERE id=? AND revision=?
                ON CONFLICT(note_id) DO UPDATE SET revision=excluded.revision,projection=excluded.projection
                WHERE editor_documents.revision<>excluded.revision",
                &vec![text(&encoded), text(identity), rev],
            )?;
            Ok(())
        })
    }

    pub fn create_editor<H: Host>(&self, h: &H, title: &Val, model: &Val) -> Result<Val> {
        let model = h.normalize_model(model)?;
        let body = key(&model, "text")?.clone();
        validate_text(title, &body)?;
        let identity = uuid4();
        let stamp = now_ms();
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            db.exec(
                "INSERT INTO notes (id,resource_id,title,body,created_ms,modified_ms,
                sync_state,editable,title_editable,loaded) VALUES (?,?,?,?,?,?,'pending',0,1,1)",
                &vec![text(&identity), text(&identity), b(title)?, b(&body)?, int(stamp), int(stamp)],
            )?;
            db.exec(
                "INSERT INTO editor_documents VALUES (?,?,?)",
                &p![&identity, 1, dumps_utf8(&projection(&model))?],
            )?;
            Ok(())
        })?;
        self.get(&identity)
    }

    pub fn save_editor<H: Host>(&self, h: &H, identity: &str, title: &Val, model: &Val, expected_revision: &Val) -> Result<Val> {
        let model = h.normalize_model(model)?;
        let body = key(&model, "text")?.clone();
        validate_text(title, &body)?;
        if !expected_revision.is_exact_int() {
            return fail("revision_required", "Not sürümü eksik.", 400);
        }
        let early = self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let Some(row) = cur.one() else { return fail("not_found", "Not bulunamadı.", 404) };
            if !sql_eq(row.raw("revision"), expected_revision) {
                return fail("revision_conflict", "Not başka bir yerde değişti. Düzenlemen korunuyor.", 409);
            }
            let cached = db.one(
                "SELECT projection FROM editor_documents WHERE note_id=? AND revision=?",
                &vec![text(identity), b(expected_revision)?],
            )?;
            let projection = match cached {
                Some(c) => Some(loads_sql(&c[0])?),
                None => None,
            };
            let text_edit = match &projection {
                Some(p) if p.truthy() => key(key(p, "capabilities")?, "textEdit")?.truthy(),
                _ => false,
            };
            if row.truthy("trashed") || row.truthy("deleted_remote") || !text_edit {
                return fail("read_only", "Bu notun içeriği henüz güvenle düzenlenemiyor.", 422);
            }
            h.ensure_no_active_ink(db, identity)?;
            if row.text("org_state") != Some("synced") || matches!(row.text("sync_state"), Some("conflict" | "uncertain" | "error")) {
                return fail("sync_pending", "Önce bu notun eşitleme sorununu çöz.", 409);
            }
            let projection = projection.unwrap();
            if sql_eq(row.raw("title"), title) && py_eq(key(&projection, "model")?, &model) {
                return Ok(Some(public(&row, true)?));
            }
            let revision = row.int("revision") + 1;
            db.exec(
                "UPDATE notes SET title=?,body=?,revision=?,modified_ms=?,sync_state='pending',editable=0,error=NULL WHERE id=?",
                &vec![b(title)?, b(&body)?, int(revision), int(now_ms()), text(identity)],
            )?;
            db.exec(
                "UPDATE editor_documents SET revision=?,projection=? WHERE note_id=?",
                &p![revision, dumps_utf8(&crate::editor_store::projection(&model))?, identity],
            )?;
            Ok(None)
        })?;
        match early {
            Some(v) => Ok(v),
            None => self.get(identity),
        }
    }

    pub fn editor_job_model(&self, job: &Val) -> Result<Val> {
        let id = b(key(job, "id")?)?;
        let row = self.with_db(|db| db.one("SELECT model FROM editor_jobs WHERE job_id=?", &[id]))?;
        match row {
            Some(r) => loads_sql(&r[0]),
            None => fail("missing_editor_model", "Düzenlemenin biçim bilgisi eksik; tekrar gönderilmedi.", 409),
        }
    }
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::pystore::add_marker(m, "EditorStore", "samsung_notes.editor_store")?;
    rs_common::export(m, &["EditorStore"])
}
