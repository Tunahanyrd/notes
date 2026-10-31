//! Port of `samsung_notes/ink_store.py`: revision-bound ink drafts and
//! checkpointed, immutable stroke/erase batches.
//!
//! Cloud writes remain one verified stroke per operation. A batch resumes at
//! its durable checkpoint; a crash before that checkpoint reuses the same
//! operation UUID and the Cloud adapter's receipt.
use crate::db::*;
use crate::host::{Host, HIGHLIGHTER_ALPHA, HIGHLIGHTER_MAX_SIZE, HIGHLIGHTER_MIN_SIZE, HIGHLIGHTER_RECIPE, MAX_PEN_SIZE, MIN_PEN_SIZE};
use crate::p;
use crate::storage::StoreCore;
use crate::val::{py_eq, Val};
#[cfg(feature = "python")]
use pyo3::prelude::*;
use rusqlite::types::Value as SqlVal;
use std::collections::HashSet;

pub const MAX_BATCH_STROKES: usize = 64;
pub const MAX_BATCH_POINTS: usize = 20_000;

fn finite_number(v: &Val) -> bool {
    match v {
        Val::Int(_) | Val::BigInt(_) => true,
        Val::Float(f) => f.is_finite(),
        _ => false,
    }
}

fn num(v: &Val) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

/// `lo <= v <= hi` for Python numbers.
fn between(v: &Val, lo: f64, hi: &Val) -> bool {
    let x = num(v);
    match hi.as_f64() {
        Some(h) => lo <= x && x <= h,
        None => false,
    }
}

/// validate_batch(strokes, projection, snapshot, stamp)
pub fn validate_batch<H: Host>(h: &H, strokes: &Val, projection: &Val, snapshot: &H::Snapshot, stamp: i64) -> Result<Val> {
    let strokes = match strokes {
        Val::List(items) if (1..=MAX_BATCH_STROKES).contains(&items.len()) => items,
        _ => return fail("invalid_ink", "Tek kayıtta 1–64 çizgi gönderilebilir.", 400),
    };
    let pages: Vec<&Val> = match key(projection, "pages")? {
        Val::List(items) => items.iter().collect(),
        other => return Err(Error::Type(format!("'{}' object is not iterable", other.type_name()))),
    };
    let mut page_ids = Vec::with_capacity(pages.len());
    for p in &pages {
        page_ids.push(key(p, "id")?.clone());
    }
    if strokes.len() == 1 && matches!(strokes[0], Val::Dict(_)) && strokes[0].get("tool").is_some_and(|t| py_eq(t, &Val::s("highlighter"))) {
        let page = |id: &str| page_ids.iter().rposition(|p| py_eq(p, &Val::s(id))).map(|at| pages[at]);
        return validate_highlighter_stroke(h, &strokes[0], &page, snapshot, stamp);
    }
    if strokes.len() == 1 && matches!(strokes[0], Val::Dict(_)) && strokes[0].get("sourceStrokeId").is_none() {
        // dict(id -> page): the last page with this id wins.
        let page = |id: &str| page_ids.iter().rposition(|p| py_eq(p, &Val::s(id))).map(|at| pages[at]);
        return validate_first_stroke(h, &strokes[0], &page, snapshot, stamp);
    }
    let mut validated = Vec::new();
    let mut total = 0usize;
    // A stroke may name an explicit colour and page-unit size. If any stroke in
    // the batch differs from its template, the whole batch uses recipe 3 with
    // explicit values for every stroke; otherwise recipe 2 is kept unchanged.
    let mut recipe = 2;
    for stroke in strokes {
        let keys: HashSet<&str> = stroke.keys().into_iter().collect();
        let basic: HashSet<&str> = ["pageId", "sourceStrokeId", "points"].into_iter().collect();
        let styled: HashSet<&str> = ["pageId", "sourceStrokeId", "points", "colorARGB", "size"].into_iter().collect();
        if !matches!(stroke, Val::Dict(_)) || (keys != basic && keys != styled) {
            return fail("invalid_ink", "Çizginin sayfa veya kalem bilgisi eksik.", 400);
        }
        let (Val::Str(page_id), Val::Str(source_id)) = (key(stroke, "pageId")?, key(stroke, "sourceStrokeId")?) else {
            return fail("invalid_ink", "Çizginin sayfa veya kalem bilgisi geçersiz.", 400);
        };
        // dict(id -> page): the last page with this id wins.
        let page = page_ids.iter().rposition(|id| py_eq(id, &Val::s(page_id))).map(|at| pages[at]);
        let mut source = None;
        if let Some(page) = page {
            if let Val::List(items) = key(page, "strokes")? {
                for s in items {
                    if py_eq(key(s, "id")?, &Val::s(source_id)) {
                        source = Some(s);
                        break;
                    }
                }
            }
        }
        let (Some(page), Some(source)) = (page, source) else {
            return fail("invalid_ink_source", "Kalem seçilen sayfaya ait değil.", 400);
        };
        if matches!(source.get("template"), Some(Val::Bool(false))) {
            return fail("invalid_ink_source", "Kalem seçilen sayfaya ait değil.", 400);
        }
        if stroke.get("colorARGB").is_some() {
            let (color, size) = (key(stroke, "colorARGB")?, key(stroke, "size")?);
            let color_ok = matches!(color, Val::Int(c) if (0..=0xffff_ffff).contains(c) && c >> 24 == 0xff);
            let size_ok = matches!(size, Val::Int(_) | Val::BigInt(_) | Val::Float(_))
                && finite_number(size)
                && MIN_PEN_SIZE <= num(size)
                && num(size) <= MAX_PEN_SIZE;
            if !color_ok || !size_ok {
                return fail("invalid_ink", "Kalem rengi veya kalınlığı desteklenen aralıkta değil.", 400);
            }
            if !py_eq(color, key(source, "colorARGB")?) || !py_eq(size, key(source, "size")?) {
                recipe = 3;
            }
        }
        let points = match key(stroke, "points")? {
            Val::List(items) if (1..=MAX_BATCH_POINTS).contains(&items.len()) => items,
            _ => return fail("invalid_ink", "Çizginin nokta sayısı desteklenmiyor.", 400),
        };
        total += points.len();
        if total > MAX_BATCH_POINTS {
            return fail("ink_too_large", "Tek kayıttaki çizim çok büyük; çizimi bölerek kaydedin.", 400);
        }
        let point_keys: HashSet<&str> = ["x", "y", "pressure", "timeMs"].into_iter().collect();
        for pt in points {
            let ok = matches!(pt, Val::Dict(_))
                && pt.keys().into_iter().collect::<HashSet<_>>() == point_keys
                && ["x", "y", "pressure"].iter().all(|k| finite_number(pt.get(k).unwrap()))
                && between(pt.get("x").unwrap(), 0.0, key(page, "width")?)
                && between(pt.get("y").unwrap(), 0.0, key(page, "height")?)
                && between(pt.get("pressure").unwrap(), 0.0, &Val::Int(1))
                && matches!(pt.get("timeMs").unwrap(), Val::Int(_) | Val::BigInt(_));
            if !ok {
                return fail("invalid_ink", "Çizginin noktaları desteklenen aralıkta değil.", 400);
            }
        }
        let source_color = key(source, "colorARGB")?;
        let source_size = key(source, "size")?;
        validated.push(Val::dict(vec![
            ("id", Val::Str(uuid4())),
            ("pageId", key(page, "id")?.clone()),
            ("sourceStrokeId", key(source, "id")?.clone()),
            ("kind", key(source, "kind")?.clone()),
            ("colorARGB", stroke.get("colorARGB").unwrap_or(source_color).clone()),
            ("size", stroke.get("size").unwrap_or(source_size).clone()),
            ("points", Val::List(points.clone())),
        ]));
    }
    // The authoritative writer validates timing, native geometry and the
    // full document profile before anything enters the durable queue.
    for stroke in &validated {
        let page_id = match h.prepare_ink_append(snapshot, recipe, stroke, stamp) {
            Ok(page_id) => page_id,
            Err(f) if f.ink || f.value || f.type_ || f.overflow => {
                return fail("unsupported_ink", "Bu çizgi henüz güvenle kaydedilemiyor.", 422)
            }
            Err(f) => return Err(f.error),
        };
        if !py_eq(&page_id, key(stroke, "pageId")?) {
            return fail("invalid_ink_source", "Çizginin sayfası uyuşmuyor.", 400);
        }
    }
    Ok(Val::dict(vec![("schemaVersion", Val::Int(1)), ("recipeVersion", Val::Int(recipe)), ("strokes", Val::List(validated))]))
}

/// Recipe 4: one stroke on a page that has none yet (no template to copy).
/// It is queued alone; later strokes on that page use the new stroke as their
/// template after the readback, through the ordinary recipes.
fn validate_first_stroke<'a, H: Host>(
    h: &H,
    stroke: &Val,
    page: &dyn Fn(&str) -> Option<&'a Val>,
    snapshot: &H::Snapshot,
    stamp: i64,
) -> Result<Val> {
    let keys: HashSet<&str> = stroke.keys().into_iter().collect();
    let want: HashSet<&str> = ["pageId", "points", "colorARGB", "size"].into_iter().collect();
    let page_id = match (keys == want, stroke.get("pageId")) {
        (true, Some(Val::Str(p))) => p.clone(),
        _ => return fail("invalid_ink", "Çizginin sayfa veya kalem bilgisi eksik.", 400),
    };
    // pages.get(pageId, {}).get('strokes')
    if let Some(p) = page(&page_id) {
        let has = match p {
            Val::Dict(_) => p.get_or_null("strokes").truthy(),
            other => return Err(Error::Attribute(format!("'{}' object has no attribute 'get'", other.type_name()))),
        };
        if has {
            return fail("invalid_ink_source", "Bu sayfada çizgi var; kalem seçilerek eklenmeli.", 400);
        }
    }
    let (color, size, points) = (key(stroke, "colorARGB")?, key(stroke, "size")?, key(stroke, "points")?);
    let color_ok = matches!(color, Val::Int(c) if (0..=0xffff_ffff).contains(c) && c >> 24 == 0xff);
    let size_ok = matches!(size, Val::Int(_) | Val::BigInt(_) | Val::Float(_))
        && finite_number(size)
        && MIN_PEN_SIZE <= num(size)
        && num(size) <= MAX_PEN_SIZE;
    if !color_ok || !size_ok {
        return fail("invalid_ink", "Kalem rengi veya kalınlığı desteklenen aralıkta değil.", 400);
    }
    let point_keys: HashSet<&str> = ["x", "y", "pressure", "timeMs"].into_iter().collect();
    let points_ok = match points {
        Val::List(items) if (1..=MAX_BATCH_POINTS).contains(&items.len()) => items.iter().all(|pt| {
            matches!(pt, Val::Dict(_))
                && pt.keys().into_iter().collect::<HashSet<_>>() == point_keys
                && ["x", "y", "pressure"].iter().all(|k| finite_number(pt.get(k).unwrap()))
                && between(pt.get("pressure").unwrap(), 0.0, &Val::Int(1))
                && matches!(pt.get("timeMs").unwrap(), Val::Int(_) | Val::BigInt(_))
        }),
        _ => false,
    };
    if !points_ok {
        return fail("invalid_ink", "Çizginin noktaları desteklenen aralıkta değil.", 400);
    }
    let size = num(size);
    let color = color.as_i64().unwrap();
    let id = uuid4();
    let validated = Val::dict(vec![
        ("id", Val::s(&id)),
        ("pageId", Val::s(&page_id)),
        ("kind", Val::s("fountain")),
        ("colorARGB", Val::Int(color)),
        ("size", Val::Float(size)),
        ("points", points.clone()),
    ]);
    // Page bounds, page profile and the encoder are checked by the writer itself.
    match h.prepare_first_stroke(snapshot, &page_id, points, color, size, stamp, &id) {
        Ok(()) => {}
        Err(f) if f.ink || f.value || f.type_ || f.overflow => {
            return fail("unsupported_ink", "Bu sayfaya ilk çizgi henüz güvenle eklenemiyor.", 422)
        }
        Err(f) => return Err(f.error),
    }
    Ok(Val::dict(vec![("schemaVersion", Val::Int(1)), ("recipeVersion", Val::Int(4)), ("strokes", Val::List(vec![validated]))]))
}

/// Recipe 5: one highlighter (Marker4) stroke, queued alone, on any page
/// the highlighter writer accepts (with or without existing strokes).
fn validate_highlighter_stroke<'a, H: Host>(
    h: &H,
    stroke: &Val,
    page: &dyn Fn(&str) -> Option<&'a Val>,
    snapshot: &H::Snapshot,
    stamp: i64,
) -> Result<Val> {
    let keys: HashSet<&str> = stroke.keys().into_iter().collect();
    let want: HashSet<&str> = ["tool", "pageId", "points", "colorARGB", "size"].into_iter().collect();
    let page_id = match (keys == want, stroke.get("pageId")) {
        (true, Some(Val::Str(p))) => p.clone(),
        _ => return fail("invalid_ink", "Fosforlu kalem çizgisinin sayfa veya renk bilgisi eksik.", 400),
    };
    if page(&page_id).is_none() {
        return fail("invalid_ink_source", "Fosforlu kalem çizgisinin sayfası bulunamadı.", 400);
    }
    let (color, size, points) = (key(stroke, "colorARGB")?, key(stroke, "size")?, key(stroke, "points")?);
    let color_ok = matches!(color, Val::Int(c) if (0..=0xffff_ffff).contains(c) && c >> 24 == HIGHLIGHTER_ALPHA);
    let size_ok = matches!(size, Val::Int(_) | Val::BigInt(_) | Val::Float(_))
        && finite_number(size)
        && HIGHLIGHTER_MIN_SIZE <= num(size)
        && num(size) <= HIGHLIGHTER_MAX_SIZE;
    if !color_ok || !size_ok {
        return fail("invalid_ink", "Fosforlu kalem rengi veya kalınlığı desteklenen aralıkta değil.", 400);
    }
    let point_keys: HashSet<&str> = ["x", "y", "pressure", "timeMs"].into_iter().collect();
    let points_ok = match points {
        Val::List(items) if (1..=MAX_BATCH_POINTS).contains(&items.len()) => items.iter().all(|pt| {
            matches!(pt, Val::Dict(_))
                && pt.keys().into_iter().collect::<HashSet<_>>() == point_keys
                && ["x", "y", "pressure"].iter().all(|k| finite_number(pt.get(k).unwrap()))
                && between(pt.get("pressure").unwrap(), 0.0, &Val::Int(1))
                && matches!(pt.get("timeMs").unwrap(), Val::Int(_) | Val::BigInt(_))
        }),
        _ => false,
    };
    if !points_ok {
        return fail("invalid_ink", "Çizginin noktaları desteklenen aralıkta değil.", 400);
    }
    let size = num(size);
    let color = color.as_i64().unwrap();
    let id = uuid4();
    let validated = Val::dict(vec![
        ("id", Val::s(&id)),
        ("pageId", Val::s(&page_id)),
        ("kind", Val::s("highlighter")),
        ("colorARGB", Val::Int(color)),
        ("size", Val::Float(size)),
        ("points", points.clone()),
    ]);
    match h.prepare_highlighter_stroke(snapshot, &page_id, points, color, size, stamp, &id) {
        Ok(()) => {}
        Err(f) if f.ink || f.value || f.type_ || f.overflow => {
            return fail("unsupported_ink", "Bu sayfaya fosforlu kalem çizgisi henüz güvenle eklenemiyor.", 422)
        }
        Err(f) => return Err(f.error),
    }
    Ok(Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        ("recipeVersion", Val::Int(HIGHLIGHTER_RECIPE)),
        ("strokes", Val::List(vec![validated])),
    ]))
}

/// InkStore.ensure_no_active_ink(db, identity)
pub fn ensure_no_active_ink(db: &dyn Sql, identity: &str) -> Result<()> {
    if db
        .one(
            "SELECT 1 FROM jobs WHERE note_id=? AND kind IN ('append_ink','erase_ink','move_ink') AND state NOT IN ('done','cancelled')",
            &p![identity],
        )?
        .is_some()
    {
        return fail("sync_pending", "Önce bu notun bekleyen çizim eşitlemesini tamamla.", 409);
    }
    Ok(())
}

/// A verified receipt may be checkpointed after a stop, never followed by
/// another publication or an acknowledgement that clears that stop.
pub fn guard_ink_job_tx(db: &dyn Sql, job: &Val) -> Result<()> {
    let (id, note) = (b(key(job, "id")?)?, b(key(job, "note_id")?)?);
    let current = db.one("SELECT state FROM jobs WHERE id=? AND note_id=? AND kind=?", &vec![id, note.clone(), text("append_ink")])?;
    let cur = db.query("SELECT sync_state,org_state,trashed,deleted_remote FROM notes WHERE id=?", &[note])?;
    let active = matches!(current.as_ref().map(|r| &r[0]), Some(SqlVal::Text(s)) if s == "queued" || s == "running");
    let ok = active
        && match cur.one() {
            None => false,
            Some(n) => {
                matches!(n.text("sync_state"), Some("pending" | "synced"))
                    && n.text("org_state") == Some("synced")
                    && !n.truthy("trashed")
                    && !n.truthy("deleted_remote")
            }
        };
    if !ok {
        return fail("ink_job_stopped", "Çizimin eşitleme durumu değişti; kalan gönderimler durduruldu.", 409);
    }
    Ok(())
}

fn hashable(v: &Val) -> Result<String> {
    match v {
        Val::List(_) | Val::Dict(_) => Err(Error::Type(format!("unhashable type: '{}'", v.type_name()))),
        Val::Str(s) => Ok(format!("s{}", s)),
        other => Ok(format!("j{}", dumps_ascii(other)?)),
    }
}

fn validate_draft(revision: &Val, strokes: &Val) -> Result<String> {
        let invalid = || fail("invalid_ink", "Çizim taslağı geçersiz.", 400);
        let list = match strokes {
            Val::List(items) if revision.is_exact_int() && items.len() <= MAX_BATCH_STROKES => items,
            _ => return invalid(),
        };
        let allowed: HashSet<&str> = ["id", "pageId", "sourceStrokeId", "kind", "colorARGB", "size", "points", "pageSize"].into_iter().collect();
        let required = ["id", "pageId", "sourceStrokeId", "points"];
        let mut points = 0usize;
        let mut ids = HashSet::new();
        for stroke in list {
            let keys: HashSet<&str> = stroke.keys().into_iter().collect();
            let ok = matches!(stroke, Val::Dict(_))
                && required.iter().all(|k| keys.contains(k))
                && keys.is_subset(&allowed)
                && stroke.get("id").and_then(Val::as_str).is_some_and(|id| !id.is_empty() && ids.insert(id.to_string()))
                && matches!(stroke.get("points"), Some(Val::List(_)));
            if !ok {
                return invalid();
            }
            if let Some(Val::List(p)) = stroke.get("points") {
                points += p.len();
            }
        }
        let encoded = crate::val::dumps(strokes, false, false)?;
        if points > MAX_BATCH_POINTS || (encoded.len() > 8 * 1024 * 1024 && encoded.chars().count() > 8 * 1024 * 1024) {
            return fail("ink_too_large", "Çizim taslağı çok büyük.", 400);
        }
        Ok(encoded)
}

// A queued first stroke becomes the template for subsequent lines on the
// same formerly empty page. Its generated id belongs to the immutable job.
fn bind_first_draft(db: &DbHandle, identity: &str, revision: &Val, strokes: &Val) -> Result<Val> {
    let Val::Int(rev) = revision else { return Ok(strokes.clone()) };
    let Some(next) = rev.checked_add(1) else { return Ok(strokes.clone()) };
    let Some(row) = db.one("SELECT ink_jobs.model FROM ink_jobs JOIN jobs ON jobs.id=ink_jobs.job_id WHERE jobs.note_id=? AND jobs.kind='append_ink' AND jobs.local_revision=? AND jobs.state<>'cancelled' ORDER BY jobs.created_ms DESC LIMIT 1", &p![identity, next])? else { return Ok(strokes.clone()) };
    let model=loads_sql(&row[0])?;
    if model.get("recipeVersion") != Some(&Val::Int(4)) { return Ok(strokes.clone()); }
    let Some(Val::List(queued))=model.get("strokes") else { return Ok(strokes.clone()) };
    let Some(first)=queued.first() else { return Ok(strokes.clone()) };
    let (Some(page), Some(id))=(first.get("pageId"),first.get("id")) else { return Ok(strokes.clone()) };
    let Val::List(items)=strokes else { return Ok(strokes.clone()) };
    let mut output=items.clone();
    for item in &mut output {
        if item.get("sourceStrokeId").and_then(Val::as_str)==Some("") && item.get("kind").and_then(Val::as_str)!=Some("highlighter") && item.get("pageId").is_some_and(|p| py_eq(p,page)) {
            item.set("sourceStrokeId",id.clone());
        }
    }
    Ok(Val::List(output))
}

impl StoreCore {
    /// Bind a still-running job to an already observed stop condition.
    pub fn suspend_ink_job(&self, job: &Val) -> Result<()> {
        let (id, note) = (b(key(job, "id")?)?, b(key(job, "note_id")?)?);
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let current = db.one("SELECT state FROM jobs WHERE id=?", &[id.clone()])?;
            match current.as_ref().map(|r| &r[0]) {
                Some(SqlVal::Text(s)) if s == "queued" || s == "running" => {}
                _ => return Ok(()),
            }
            let cur = db.query("SELECT sync_state,error FROM notes WHERE id=?", &[note.clone()])?;
            let Some(row) = cur.one() else { return Err(Error::Type("'NoneType' object is not subscriptable".into())) };
            let sync_state = row.text("sync_state");
            let state = match sync_state {
                Some("uncertain") => "uncertain",
                Some("error") => "failed",
                _ => "conflict",
            };
            let message = if row.truthy("error") {
                row.raw("error").clone()
            } else {
                text("Çizimin eşitleme durumu değişti; kalan çizgiler korunuyor.")
            };
            db.exec("UPDATE jobs SET state=?,error=? WHERE id=?", &vec![text(state), message.clone(), id])?;
            if !matches!(sync_state, Some("conflict" | "uncertain" | "error")) {
                db.exec("UPDATE notes SET sync_state='conflict',error=? WHERE id=?", &vec![message, note])?;
            }
            Ok(())
        })
    }

    pub fn cached_ink(&self, identity: &str, revision: &Val) -> Result<Option<Val>> {
        let rev = b(revision)?;
        let row = self.with_db(|db| db.one("SELECT projection FROM ink_documents WHERE note_id=? AND revision=?", &vec![text(identity), rev]))?;
        match row {
            Some(r) => Ok(Some(loads_sql(&r[0])?)),
            None => Ok(None),
        }
    }

    pub fn cache_ink(&self, identity: &str, revision: &Val, projection: &Val) -> Result<()> {
        let encoded = dumps_utf8(projection)?;
        let rev = b(revision)?;
        self.with_db(|db| {
            db.exec(
                "INSERT INTO ink_documents (note_id,revision,projection)
                SELECT id,revision,? FROM notes WHERE id=? AND revision=?
                ON CONFLICT(note_id) DO UPDATE SET revision=excluded.revision,projection=excluded.projection
                WHERE ink_documents.revision<>excluded.revision OR ink_documents.projection<>excluded.projection",
                &vec![text(&encoded), text(identity), rev],
            )?;
            Ok(())
        })
    }

    pub fn save_ink_erase<H: Host>(&self, h: &H, identity: &str, erase: &Val, expected_revision: &Val) -> Result<Val> {
        self.with_container_read(|| {
        if !expected_revision.is_exact_int() {
            return fail("revision_required", "Not sürümü eksik.", 400);
        }
        let invalid = || fail("invalid_erase", "Silinecek çizgiler geçersiz.", 400);
        let erase = match erase {
            Val::List(items) if (1..=16).contains(&items.len()) => items,
            _ => return invalid(),
        };
        let want: HashSet<&str> = ["pageId", "layerId", "strokeIds"].into_iter().collect();
        for g in erase {
            let ok = matches!(g, Val::Dict(_))
                && g.keys().into_iter().collect::<HashSet<_>>() == want
                && matches!(g.get("strokeIds"), Some(Val::List(_)));
            if !ok {
                return invalid();
            }
        }
        let groups: Vec<Val> = erase
            .iter()
            .map(|g| {
                Val::dict(vec![
                    ("pageId", g.get_or_null("pageId")),
                    ("layerId", g.get_or_null("layerId")),
                    ("strokeIds", g.get_or_null("strokeIds")),
                ])
            })
            .collect();
        let total: usize = groups.iter().map(|g| if let Some(Val::List(l)) = g.get("strokeIds") { l.len() } else { 0 }).sum();
        let mut pairs = HashSet::new();
        for g in &groups {
            pairs.insert((hashable(&g.get_or_null("pageId"))?, hashable(&g.get_or_null("layerId"))?));
        }
        if total > 64 || pairs.len() != groups.len() {
            return invalid();
        }
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let Some(row) = cur.one() else { return fail("not_found", "Not bulunamadı.", 404) };
            if !sql_eq(row.raw("revision"), expected_revision) {
                return fail("revision_conflict", "Not değişti; silme işaretleri korunuyor.", 409);
            }
            if row.truthy("deleted_remote") || row.truthy("trashed") || !row.truthy("server_id") || matches!(Self::base_ref(&row), SqlVal::Null) {
                return fail("read_only", "Bu notta çizgi silinemiyor.", 422);
            }
            if row.text("sync_state") != Some("synced")
                || row.text("org_state") != Some("synced")
                || db.one("SELECT 1 FROM jobs WHERE note_id=? AND state IN ('queued','running','uncertain')", &p![identity])?.is_some()
            {
                return fail("sync_pending", "Önce bu notun eşitlenmesini bekleyin.", 409);
            }
            for g in &groups {
                let base = self.load_snapshot_sql(Self::base_ref(&row))?;
                match h.prepare_ink_delete(&base, key(g, "pageId")?, key(g, "layerId")?, key(g, "strokeIds")?) {
                    Ok(()) => {}
                    Err(f) if f.ink || f.document || f.value || f.type_ => {
                        return fail("unsupported_erase", "Bu çizgiler güvenle silinemiyor.", 422)
                    }
                    Err(f) => return Err(f.error),
                }
            }
            let revision = row.int("revision") + 1;
            let job = uuid4();
            db.exec(
                "INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?)",
                &p![&job, identity, "erase_ink", revision, row.raw("title"), row.raw("body"), Self::base_ref(&row), "queued", null(), now_ms()],
            )?;
            let model = Val::dict(vec![("schemaVersion", Val::Int(1)), ("groups", Val::List(groups.clone()))]);
            db.exec(
                "INSERT INTO ink_erase_jobs VALUES (?,?,?,?)",
                &p![&job, dumps_ascii(&model)?, 0, Self::base_ref(&row)],
            )?;
            db.exec(
                "UPDATE notes SET revision=?,modified_ms=?,sync_state='pending',error=NULL WHERE id=?",
                &p![revision, now_ms(), identity],
            )?;
            Ok(())
        })?;
        self.get(identity)
        })
    }

    /// InkStore.save_ink_move: queue a whole-stroke move (one Cloud write per
    /// page/layer group, every stroke of a group shifted by the same dx/dy).
    pub fn save_ink_move<H: Host>(&self, h: &H, identity: &str, moves: &Val, expected_revision: &Val) -> Result<Val> {
        self.with_container_read(|| {
        if !expected_revision.is_exact_int() {
            return fail("revision_required", "Not sürümü eksik.", 400);
        }
        let invalid = || fail("invalid_move", "Taşınacak çizgiler geçersiz.", 400);
        let moves = match moves {
            Val::List(items) if (1..=16).contains(&items.len()) => items,
            _ => return invalid(),
        };
        let want: HashSet<&str> = ["pageId", "layerId", "strokeIds", "dx", "dy"].into_iter().collect();
        let exact_number = |v: Option<&Val>| matches!(v, Some(Val::Int(_)) | Some(Val::BigInt(_))) || matches!(v, Some(Val::Float(f)) if f.is_finite());
        for g in moves {
            let ok = matches!(g, Val::Dict(_))
                && g.keys().into_iter().collect::<HashSet<_>>() == want
                && matches!(g.get("strokeIds"), Some(Val::List(_)))
                && exact_number(g.get("dx"))
                && exact_number(g.get("dy"));
            if !ok {
                return invalid();
            }
        }
        let groups: Vec<Val> = moves
            .iter()
            .map(|g| {
                Val::dict(vec![
                    ("pageId", g.get_or_null("pageId")),
                    ("layerId", g.get_or_null("layerId")),
                    ("strokeIds", g.get_or_null("strokeIds")),
                    ("dx", g.get_or_null("dx")),
                    ("dy", g.get_or_null("dy")),
                ])
            })
            .collect();
        let total: usize = groups.iter().map(|g| if let Some(Val::List(l)) = g.get("strokeIds") { l.len() } else { 0 }).sum();
        let mut pairs = HashSet::new();
        for g in &groups {
            pairs.insert((hashable(&g.get_or_null("pageId"))?, hashable(&g.get_or_null("layerId"))?));
        }
        if total > 64 || pairs.len() != groups.len() {
            return invalid();
        }
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let Some(row) = cur.one() else { return fail("not_found", "Not bulunamadı.", 404) };
            if !sql_eq(row.raw("revision"), expected_revision) {
                return fail("revision_conflict", "Not değişti; çizgiler taşınmadı.", 409);
            }
            if row.truthy("deleted_remote") || row.truthy("trashed") || !row.truthy("server_id") || matches!(Self::base_ref(&row), SqlVal::Null) {
                return fail("read_only", "Bu notta çizgi taşınamıyor.", 422);
            }
            if row.text("sync_state") != Some("synced")
                || row.text("org_state") != Some("synced")
                || db.one("SELECT 1 FROM jobs WHERE note_id=? AND state IN ('queued','running','uncertain')", &p![identity])?.is_some()
            {
                return fail("sync_pending", "Önce bu notun eşitlenmesini bekleyin.", 409);
            }
            for g in &groups {
                let base = self.load_snapshot_sql(Self::base_ref(&row))?;
                let dx = g.get("dx").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                let dy = g.get("dy").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                match h.prepare_ink_move(&base, key(g, "pageId")?, key(g, "layerId")?, key(g, "strokeIds")?, dx, dy) {
                    Ok(()) => {}
                    Err(f) if f.ink || f.document || f.value || f.type_ => {
                        return fail("unsupported_move", "Bu çizgiler güvenle taşınamıyor.", 422)
                    }
                    Err(f) => return Err(f.error),
                }
            }
            let revision = row.int("revision") + 1;
            let job = uuid4();
            db.exec(
                "INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?)",
                &p![&job, identity, "move_ink", revision, row.raw("title"), row.raw("body"), Self::base_ref(&row), "queued", null(), now_ms()],
            )?;
            let model = Val::dict(vec![("schemaVersion", Val::Int(1)), ("groups", Val::List(groups.clone()))]);
            db.exec(
                "INSERT INTO ink_move_jobs VALUES (?,?,?,?)",
                &p![&job, dumps_ascii(&model)?, 0, Self::base_ref(&row)],
            )?;
            db.exec(
                "UPDATE notes SET revision=?,modified_ms=?,sync_state='pending',error=NULL WHERE id=?",
                &p![revision, now_ms(), identity],
            )?;
            Ok(())
        })?;
        self.get(identity)
        })
    }

    pub fn ink_move_job(&self, job: &Val) -> Result<(Val, i64, Val)> {
        let (model, index, snapshot) = self.batch_row("ink_move_jobs", job, ("missing_move", "Taşıma bilgisi bulunamadı; tekrar gönderilmedi."))?;
        let ok = py_eq(&model.get_or_null("schemaVersion"), &Val::Int(1)) && {
            let n = match key(&model, "groups")? {
                Val::List(l) => l.len() as i64,
                _ => -1,
            };
            0 <= index && index <= n
        };
        if !ok {
            return fail("invalid_move_job", "Kaydedilmiş taşıma işinin sürümü desteklenmiyor.", 409);
        }
        Ok((model, index, snapshot))
    }

    pub fn checkpoint_ink_move(&self, job: &Val, index: &Val, snapshot: &Val) -> Result<()> {
        self.checkpoint("ink_move_jobs", job, index, snapshot, ("move_checkpoint_changed", "Taşıma sırası değişti; gönderim durduruldu."))
    }

    fn batch_row(&self, table: &str, job: &Val, missing: (&str, &str)) -> Result<(Val, i64, Val)> {
        self.with_container_read(|| {
        let id = b(key(job, "id")?)?;
        let cur = self.with_db(|db| db.query(&format!("SELECT * FROM {} WHERE job_id=?", table), &[id]))?;
        let Some(row) = cur.one() else { return fail(missing.0, missing.1, 409) };
        Ok((loads_sql(row.raw("model"))?, row.int("next_index"), self.load_snapshot_sql(row.raw("current_snapshot"))?))
        })
    }

    pub fn ink_erase_job(&self, job: &Val) -> Result<(Val, i64, Val)> {
        let (model, index, snapshot) = self.batch_row("ink_erase_jobs", job, ("missing_erase", "Silme bilgisi bulunamadı; tekrar gönderilmedi."))?;
        let ok = py_eq(&model.get_or_null("schemaVersion"), &Val::Int(1)) && {
            let n = match key(&model, "groups")? {
                Val::List(l) => l.len() as i64,
                _ => -1,
            };
            0 <= index && index <= n
        };
        if !ok {
            return fail("invalid_erase_job", "Kaydedilmiş silme işinin sürümü desteklenmiyor.", 409);
        }
        Ok((model, index, snapshot))
    }

    fn checkpoint(&self, table: &str, job: &Val, index: &Val, snapshot: &Val, changed: (&str, &str)) -> Result<()> {
        let Val::Int(i) = index else {
            return Err(Error::Type(format!("can only concatenate {} (not \"int\") to {}", index.type_name(), index.type_name())));
        };
        let id = b(key(job, "id")?)?;
        let write = |encoded: &str| self.with_db(|db| {
            let n = db
                .exec(
                    &format!("UPDATE {} SET next_index=?,current_snapshot=? WHERE job_id=? AND next_index=?", table),
                    &vec![int(i + 1), text(encoded), id.clone(), int(*i)],
                )?
                .rowcount;
            if n != 1 {
                return fail(changed.0, changed.1, 409);
            }
            Ok(())
        });
        if self.with_db(|db| Self::schema7(db))? {
            let store = crate::document_store::DocumentStore::open(&self.directory)?;
            store.with_write(|writer| {
                let reference = crate::snapshot_store::store_snapshot(writer, snapshot, None)?;
                write(&dumps_utf8(&reference)?)
            })
        } else {
            write(&dumps_utf8(snapshot)?)
        }
    }

    pub fn checkpoint_ink_erase(&self, job: &Val, index: &Val, snapshot: &Val) -> Result<()> {
        self.checkpoint("ink_erase_jobs", job, index, snapshot, ("erase_checkpoint_changed", "Silme sırası değişti; gönderim durduruldu."))
    }

    /// Unsaved strokes kept on this device only; never sent to Cloud.
    pub fn ink_draft(&self, identity: &str) -> Result<Val> {
        self.raw(identity)?;
        let row = self.with_db(|db| db.one("SELECT revision,strokes FROM ink_drafts WHERE note_id=?", &p![identity]))?;
        Ok(match row {
            Some(r) => Val::dict(vec![("revision", sql_to_val(&r[0])), ("strokes", loads_sql(&r[1])?)]),
            None => Val::dict(vec![("revision", Val::Null), ("strokes", Val::List(vec![]))]),
        })
    }

    pub fn save_ink_draft(&self, identity: &str, revision: &Val, strokes: &Val) -> Result<Val> {
        self.raw(identity)?;
        validate_draft(revision, strokes)?;
        let rev = b(revision)?;
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let bound=bind_first_draft(db,identity,revision,strokes)?;
            let encoded=validate_draft(revision,&bound)?;
            let empty = matches!(&bound, Val::List(items) if items.is_empty());
            if empty {
                db.exec("DELETE FROM ink_drafts WHERE note_id=?", &p![identity])?;
            } else {
                db.exec(
                    "INSERT INTO ink_drafts (note_id,revision,strokes,updated_ms) VALUES (?,?,?,?)
                    ON CONFLICT(note_id) DO UPDATE SET revision=excluded.revision,strokes=excluded.strokes,updated_ms=excluded.updated_ms",
                    &vec![text(identity), rev, text(&encoded), int(now_ms())],
                )?;
            }
            Ok(())
        })?;
        self.ink_draft(identity)
    }

    /// Add new strokes without replacing a concurrent outbox transfer or draft.
    pub fn append_ink_draft(&self, identity: &str, revision: &Val, strokes: &Val) -> Result<Val> {
        self.raw(identity)?;
        validate_draft(revision, strokes)?;
        if matches!(strokes, Val::List(items) if items.is_empty()) { return fail("invalid_ink", "Çizim taslağı geçersiz.", 400); }
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let bound=bind_first_draft(db,identity,revision,strokes)?;
            let Val::List(added) = &bound else { unreachable!() };
            let existing = db.one("SELECT revision,strokes FROM ink_drafts WHERE note_id=?", &p![identity])?;
            let (mut list, saved_revision) = if let Some(row) = existing {
                let Val::List(list) = loads_sql(&row[1])? else { return fail("invalid_ink", "Çizim taslağı geçersiz.", 400); };
                (list, sql_to_val(&row[0]))
            } else { (vec![], revision.clone()) };
            for stroke in added {
                let id = key(stroke, "id")?;
                if let Some(previous) = list.iter().find(|item| item.get("id").is_some_and(|v| py_eq(v,id))) {
                    if !py_eq(previous,stroke) { return fail("revision_conflict", "Çizim taslağı değişti.", 409); }
                } else { list.push(stroke.clone()); }
            }
            let merged_revision = match (revision, &saved_revision) {
                (Val::Int(a), Val::Int(b)) => Val::Int((*a).min(*b)),
                _ => return fail("invalid_ink", "Çizim taslağı geçersiz.", 400),
            };
            let encoded = validate_draft(&merged_revision, &Val::List(list))?;
            db.exec("INSERT INTO ink_drafts (note_id,revision,strokes,updated_ms) VALUES (?,?,?,?) ON CONFLICT(note_id) DO UPDATE SET revision=excluded.revision,strokes=excluded.strokes,updated_ms=excluded.updated_ms", &vec![text(identity), b(&merged_revision)?, text(&encoded), int(now_ms())])?;
            Ok(())
        })?;
        self.ink_draft(identity)
    }

    pub fn save_ink<H: Host>(&self, h: &H, identity: &str, strokes: &Val, expected_revision: &Val) -> Result<Val> {
        self.save_ink_impl(h, identity, strokes, expected_revision, None)
    }

    /// Atomically transfer exactly the named device-draft strokes to the immutable batch.
    pub fn save_ink_from_draft<H: Host>(&self, h: &H, identity: &str, strokes: &Val, expected_revision: &Val, draft_ids: &Val) -> Result<Val> {
        self.save_ink_impl(h, identity, strokes, expected_revision, Some(draft_ids))
    }

    fn save_ink_impl<H: Host>(&self, h: &H, identity: &str, strokes: &Val, expected_revision: &Val, draft_ids: Option<&Val>) -> Result<Val> {
        if !expected_revision.is_exact_int() {
            return fail("revision_required", "Not sürümü eksik.", 400);
        }
        let row = self.raw(identity)?;
        let projection = self.cached_ink(identity, expected_revision)?;
        if !py_eq(&row.get_or_null("revision"), expected_revision) {
            return fail("revision_conflict", "Not değişti; çiziminiz korunuyor.", 409);
        }
        if row.get_or_null("sync_state").as_str() != Some("synced") || row.get_or_null("org_state").as_str() != Some("synced") {
            return fail("sync_pending", "Önce bu notun eşitlenmesini bekleyin.", 409);
        }
        // A first stroke needs no template; its writer checks the page itself.
        let first = matches!(strokes, Val::List(items) if items.len() == 1
            && matches!(items[0], Val::Dict(_)) && items[0].get("sourceStrokeId").is_none());
        let append = match &projection {
            Some(p) if p.truthy() => key(key(p, "capabilities")?, "appendInk")?.truthy() || first,
            _ => false,
        };
        let base = row.get_or_null("base_document");
        let base_reference = match row.get("container_metadata") {
            Some(value) if value.truthy() => value.clone(),
            _ => base.clone(),
        };
        if row.get_or_null("trashed").truthy()
            || row.get_or_null("deleted_remote").truthy()
            || !base.truthy()
            || !row.get_or_null("server_id").truthy()
            || !append
        {
            return fail("read_only", "Bu notta kalem düzenleme henüz desteklenmiyor.", 422);
        }
        let projection = projection.unwrap();
        let stamp = now_ms();
        let document = crate::val::loads(base.as_str().unwrap_or(""))?;
        let snapshot = h.snapshot_from_dict(&document)?;
        let model = validate_batch(h, strokes, &projection, &snapshot, stamp)?;
        let job_id = uuid4();
        let revision = match expected_revision {
            Val::Int(i) => i + 1,
            _ => i64::MAX,
        };
        self.with_db(|db| {
            db.exec("BEGIN IMMEDIATE", &[])?;
            let cur = db.query("SELECT * FROM notes WHERE id=?", &p![identity])?;
            let current = cur.one().ok_or_else(|| Error::Type("'NoneType' object is not subscriptable".into()))?;
            // Projection/validation ran outside the transaction. Compare the
            // snapshot too: a refreshed Cloud commit must never borrow its gate.
            if !sql_eq(current.raw("revision"), expected_revision)
                || !sql_eq(Self::base_ref(&current), &base_reference)
                || current.text("sync_state") != Some("synced")
                || current.text("org_state") != Some("synced")
                || current.truthy("trashed")
                || current.truthy("deleted_remote")
            {
                return fail("revision_conflict", "Not değişti; çiziminiz korunuyor.", 409);
            }
            if db.one("SELECT 1 FROM jobs WHERE note_id=? AND state IN ('queued','running','uncertain')", &p![identity])?.is_some() {
                return fail("sync_pending", "Önce bu notun eşitlenmesini bekleyin.", 409);
            }
            db.exec(
                "INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?)",
                &vec![
                    text(&job_id),
                    text(identity),
                    text("append_ink"),
                    int(revision),
                    b(&row.get_or_null("title"))?,
                    b(&row.get_or_null("body"))?,
                    b(&base_reference)?,
                    text("queued"),
                    null(),
                    int(stamp),
                ],
            )?;
            db.exec("INSERT INTO ink_jobs VALUES (?,?,?,?)", &vec![text(&job_id), text(&dumps_utf8(&model)?), int(0), b(&base_reference)?])?;
            db.exec(
                "UPDATE notes SET revision=?,modified_ms=?,sync_state='pending',error=NULL WHERE id=?",
                &p![revision, stamp, identity],
            )?;
            // Verify the exact draft snapshot under the same write transaction.
            // A newer stroke is retained; a changed/missing selected stroke is
            // refused rather than publishing different content under its id.
            if let Some(ids) = draft_ids {
                let (Val::List(ids), Val::List(submitted)) = (ids, strokes) else {
                    return fail("invalid_ink", "Çizim taslağı geçersiz.", 400);
                };
                let mut selected = HashSet::new();
                if ids.len() != submitted.len() || ids.is_empty() || ids.iter().any(|id| id.as_str().filter(|s| !s.is_empty()).is_none_or(|id| !selected.insert(id.to_string()))) {
                    return fail("invalid_ink", "Çizim taslağı geçersiz.", 400);
                }
                let Some(stored) = db.one("SELECT revision,strokes FROM ink_drafts WHERE note_id=?", &p![identity])? else {
                    return fail("revision_conflict", "Çizim taslağı değişti.", 409);
                };
                if !sql_eq(&stored[0], expected_revision) {
                    return fail("revision_conflict", "Çizim taslağı değişti.", 409);
                }
                let Val::List(existing) = loads_sql(&stored[1])? else {
                    return fail("invalid_ink", "Çizim taslağı geçersiz.", 400);
                };
                let Val::List(validated) = key(&model,"strokes")? else { return fail("invalid_ink", "Çizim taslağı geçersiz.", 400) };
                if validated.len()!=ids.len() { return fail("invalid_ink", "Çizim taslağı geçersiz.", 400); }
                for ((id, request), saved) in ids.iter().zip(submitted).zip(validated) {
                    let Some(draft) = existing.iter().find(|item| item.get("id").is_some_and(|v| py_eq(v, id))) else {
                        return fail("revision_conflict", "Çizim taslağı değişti.", 409);
                    };
                    let matches = ["pageId", "points"].iter().all(|k| draft.get(k).zip(request.get(k)).is_some_and(|(a,b)| py_eq(a,b)))
                        && request.get("sourceStrokeId").map_or_else(|| draft.get("sourceStrokeId").and_then(Val::as_str)==Some(""), |v| draft.get("sourceStrokeId").is_some_and(|d| py_eq(d,v)))
                        && ["kind", "colorARGB", "size"].iter().all(|k| draft.get(k).zip(saved.get(k)).is_some_and(|(a,b)| py_eq(a,b)))
                        && (request.get("tool").and_then(Val::as_str)==Some("highlighter")) == (draft.get("kind").and_then(Val::as_str)==Some("highlighter"));
                    if !matches { return fail("revision_conflict", "Çizim taslağı değişti.", 409); }
                }
                let rest: Vec<Val> = existing.into_iter().filter(|item| item.get("id").and_then(Val::as_str).is_none_or(|id| !selected.contains(id))).collect();
                let Val::List(rest)=bind_first_draft(db,identity,expected_revision,&Val::List(rest))? else { unreachable!() };
                if rest.is_empty() {
                    db.exec("DELETE FROM ink_drafts WHERE note_id=?", &p![identity])?;
                } else {
                    db.exec("UPDATE ink_drafts SET strokes=?,updated_ms=? WHERE note_id=?", &p![dumps_utf8(&Val::List(rest))?, stamp, identity])?;
                }
            } else {
                // Legacy API clients do not supply draft ids.
                db.exec("DELETE FROM ink_drafts WHERE note_id=?", &p![identity])?;
            }
            let mut desired = projection.clone();
            desired.set("pendingStrokes", key(&model, "strokes")?.clone());
            desired.set("capabilities", Val::dict(vec![("appendInk", Val::Bool(false))]));
            desired.set("reason", Val::s("Çizim kaydedildi; eşitlenmesi bekleniyor."));
            db.exec(
                "UPDATE ink_documents SET revision=?,projection=? WHERE note_id=?",
                &p![revision, dumps_utf8(&desired)?, identity],
            )?;
            Ok(())
        })?;
        self.get(identity)
    }

    pub fn ink_job(&self, job: &Val) -> Result<(Val, i64, Val)> {
        let (model, index, snapshot) =
            self.batch_row("ink_jobs", job, ("missing_ink", "Kaydedilmiş çizim bilgisi bulunamadı; tekrar gönderilmedi."))?;
        let one = Val::Int(1);
        let recipe = model.get("recipeVersion").unwrap_or(&one);
        let ok = py_eq(&model.get_or_null("schemaVersion"), &one)
            && matches!(recipe, Val::Int(1..=5))
            && {
                let n = match key(&model, "strokes")? {
                    Val::List(l) => l.len() as i64,
                    _ => -1,
                };
                0 <= index && index <= n
            };
        if !ok {
            return fail("invalid_ink_job", "Kaydedilmiş çizimin sürümü desteklenmiyor.", 409);
        }
        Ok((model, index, snapshot))
    }

    pub fn checkpoint_ink(&self, job: &Val, index: &Val, snapshot: &Val) -> Result<()> {
        self.checkpoint("ink_jobs", job, index, snapshot, ("ink_checkpoint_changed", "Çizimin eşitleme sırası değişti; gönderim durduruldu."))
    }
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(crate::pystore::samples_from_points, m)?)?;
    m.add_function(wrap_pyfunction!(crate::pystore::validate_batch, m)?)?;
    crate::pystore::add_marker(m, "InkStore", "samsung_notes.ink_store")?;
    rs_common::export(m, &["samples_from_points", "validate_batch", "InkStore"])
}
