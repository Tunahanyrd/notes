//! Differential conformance against fixtures dumped from the Python
//! implementation (synthetic documents only).
//!
//! `tests/fixtures/cases.json` is always checked; more fixture files can be
//! given in `NOTES_HOST_FIXTURES` (colon-separated). A fixture holds the
//! synthetic inputs and the Python results:
//! * `funcs`: area facades (project_ink, project_page_geometry,
//!   page_background_state, page_ops_state, editor_projection, normalize_model)
//!   — `json.dumps(result, ensure_ascii=False)` must be byte-identical;
//! * `host`: PureHost collaborators — success/failure and the exception class
//!   flags the store's `except` clauses test, plus `str(exception)`;
//! * `templates`: Engine.templates over stored rows;
//! * `store`: a scripted StoreCore + PureHost run compared with the Python
//!   Store on the same documents (results and every table, random UUIDs and
//!   wall-clock timestamps masked).
use std::path::{Path, PathBuf};

use notes_host::areas;
use notes_host::host::PureHost;
use notes_host::snapshot;
use notes_host::{Fail, Kind, Val};
use rs_store::host::Host;
use rs_store::storage::StoreCore;
use rs_store::val::{dumps, loads};

fn fixture_paths() -> Vec<PathBuf> {
    let mut out = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cases.json")];
    if let Ok(extra) = std::env::var("NOTES_HOST_FIXTURES") {
        out.extend(extra.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    out.into_iter().filter(|p| p.exists()).collect()
}

fn load(path: &Path) -> Val {
    loads(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn js(v: &Val) -> String {
    dumps(v, false, false).unwrap()
}

fn flags_of(f: &Fail) -> Val {
    Val::dict(vec![
        ("document", Val::Bool(f.is_document())),
        ("ink", Val::Bool(f.kind == Kind::Ink)),
        ("value", Val::Bool(f.is_value())),
        ("type", Val::Bool(f.is_type())),
        ("key", Val::Bool(f.is_key())),
        ("overflow", Val::Bool(f.kind == Kind::Overflow)),
        ("msg", Val::s(&f.message)),
        ("store", if f.is_store() { Val::List(vec![Val::s(&f.code), Val::s(&f.message), Val::Int(f.status as i64)]) } else { Val::Null }),
    ])
}

/// The comparable part of a Python error record.
fn expected_flags(err: &Val) -> Val {
    let mut d = Vec::new();
    for k in ["document", "ink", "value", "type", "key", "overflow", "msg", "store"] {
        d.push((k, err.get(k).cloned().unwrap_or(Val::Null)));
    }
    Val::dict(d)
}

#[derive(Default)]
struct Tally {
    total: usize,
    ok_match: usize,
    err_match: usize,
    flags_only: usize,
    /// Counted in `ok_match`: documented intended divergences (editor rules v2).
    intended: usize,
    mismatches: Vec<String>,
}

impl Tally {
    /// Compare an outcome; error messages are compared too, a flags-only
    /// match (same classes, different `str`) is counted separately.
    fn check(&mut self, label: &str, expected: &Val, actual: Result<Option<String>, Fail>) {
        self.total += 1;
        match (expected.get("ok"), expected.get("err"), actual) {
            (Some(want), None, Ok(got)) => {
                let want = want.as_str().map(String::from);
                if want == got {
                    self.ok_match += 1;
                } else if (label.starts_with("editor_projection") || label.starts_with("store [\"cache_editor\""))
                    && got.as_deref().and_then(|raw| loads(raw).ok()).is_some_and(|mut value| {
                        let Some(Val::List(areas)) = value.remove("bodyTextAreas") else { return false };
                        !areas.is_empty() && areas.iter().all(|area| area.get("rect").is_some() && area.get("pageIndex").is_some())
                            && Some(js(&value)) == want
                    }) {
                    // v5 adds geometry; compare every original oracle field.
                    self.ok_match += 1;
                    self.intended += 1;
                } else {
                    self.mismatches.push(format!("{label}: value\n  py:   {:?}\n  rust: {:?}", want.map(trunc), got.map(trunc)));
                }
            }
            (None, Some(err), Err(f)) => {
                let want = expected_flags(err);
                let got = flags_of(&f);
                if js(&want) == js(&got) {
                    self.err_match += 1;
                } else {
                    let strip = |v: &Val| {
                        let mut v = v.clone();
                        v.set("msg", Val::Null);
                        js(&v)
                    };
                    if strip(&want) == strip(&got) {
                        self.flags_only += 1;
                        self.mismatches.push(format!("{label}: message only\n  py:   {}\n  rust: {}", js(&want), js(&got)));
                    } else {
                        self.mismatches.push(format!("{label}: error\n  py:   {} ({})\n  rust: {} ({:?})", js(&want), err.get_or_null("cls").as_str().unwrap_or(""), js(&got), f.kind));
                    }
                }
            }
            (_, _, got) => {
                self.mismatches.push(format!("{label}: outcome\n  py:   {}\n  rust: {:?}", trunc(js(expected)), got.map(|o| o.map(trunc))));
            }
        }
    }
}

fn trunc(s: String) -> String {
    if s.chars().count() > 600 {
        format!("{}…", s.chars().take(600).collect::<String>())
    } else {
        s
    }
}

fn doc<'a>(fx: &'a Val, name: &Val) -> &'a Val {
    match name {
        Val::Null => &Val::Null,
        Val::Str(n) => fx.k("docs").get(n).or_else(|| fx.k("bads").get(n)).unwrap_or_else(|| panic!("doc {n}")),
        _ => panic!("doc name"),
    }
}

trait Idx {
    fn idx(&self, i: usize) -> &Val;
}
impl Idx for Val {
    fn idx(&self, i: usize) -> &Val {
        match self {
            Val::List(items) => &items[i],
            _ => panic!("not a list"),
        }
    }
}
trait Key {
    fn k(&self, k: &str) -> &Val;
}
impl Key for Val {
    fn k(&self, k: &str) -> &Val {
        self.get(k).unwrap_or_else(|| panic!("missing key {k}"))
    }
}

fn list(v: &Val) -> &Vec<Val> {
    match v {
        Val::List(items) => items,
        _ => panic!("not a list"),
    }
}

fn ok_val(r: Result<Val, Fail>) -> Result<Option<String>, Fail> {
    r.map(|v| Some(js(&v)))
}

fn run_funcs(fx: &Val, t: &mut Tally) {
    for case in list(fx.k("funcs")) {
        let args = case.k("args");
        let fname = case.k("fn").as_str().unwrap();
        let label = format!("{fname}{}", trunc(js(args)));
        let r = match fname {
            "project_ink" => areas::project_ink(doc(fx, args.idx(0)), args.idx(1).truthy()),
            "page_background_state" => areas::page_background_state(match args.idx(0) {
                Val::Null => None,
                n => Some(doc(fx, n)),
            }),
            "page_ops_state" => areas::page_ops_state(match args.idx(0) {
                Val::Null => None,
                n => Some(doc(fx, n)),
            }),
            "editor_projection_val" => areas::editor_projection_val(doc(fx, args.idx(0))),
            "editor_projection" => snapshot::from_dict(doc(fx, args.idx(0))).and_then(|s| areas::editor_projection(&s)),
            "project_page_geometry" => areas::project_page_geometry_val(doc(fx, args.idx(0)), args.idx(1), areas::MAX_GEOMETRY_POINTS),
            "normalize_model" => {
                let i = args.idx(0).as_i64().unwrap() as usize;
                areas::normalize_model(fx.k("models").idx(i))
            }
            other => panic!("unknown fn {other}"),
        };
        let got = ok_val(r);
        if fname.starts_with("editor_projection") {
            if let (Some(want), Ok(Some(rust))) = (case.k("result").get("ok").and_then(Val::as_str), &got) {
                if editor_v2_intended(want, rust) {
                    t.total += 1;
                    t.ok_match += 1;
                    t.intended += 1;
                    continue;
                }
            }
        }
        t.check(&label, case.k("result"), got);
    }
}

/// Intended divergence from the frozen Python oracle (editor capability
/// rules v2, `rs_editor::edit::PROJECTION_RECIPE`): text objects next to page
/// objects/content files are editable. The oracle blocked them at its
/// object/resource gate; Rust either enables them (same model and title flag)
/// or, for a page/tree inventory mismatch, keeps them blocked with that reason.
fn editor_v2_intended(want: &str, got: &str) -> bool {
    const OLD: [&str; 2] = ["Çizim veya ek nesne içeren notlar salt okunur.", "Ek kaynak içeren notlar salt okunur."];
    let (Ok(w), Ok(g)) = (loads(want), loads(got)) else { return false };
    let caps = |v: &Val, k: &str| v.get("capabilities").and_then(|c| c.get(k)).map(Val::truthy).unwrap_or(false);
    if caps(&w, "textEdit") || !w.get("reason").and_then(Val::as_str).is_some_and(|r| OLD.contains(&r)) {
        return false;
    }
    if caps(&w, "titleEdit") != caps(&g, "titleEdit") || js(&w.get_or_null("model")) != js(&g.get_or_null("model")) {
        return false;
    }
    let enabled = caps(&g, "textEdit") && caps(&g, "richStyle") && g.get_or_null("reason").is_null();
    let inconsistent = !caps(&g, "textEdit") && g.get("reason").and_then(Val::as_str) == Some(rs_doc::doc::EDITOR_INCONSISTENT);
    enabled || inconsistent
}

fn unit(r: rs_store::host::FaultResult<()>) -> Result<Option<String>, Fail> {
    r.map(|_| None).map_err(|f| Fail::from(f.error))
}

fn run_host(fx: &Val, t: &mut Tally) {
    let h = PureHost::new();
    for case in list(fx.k("host")) {
        let a = case.k("args");
        let m = case.k("method").as_str().unwrap();
        let label = format!("{m}{}", trunc(js(a)));
        let d = doc(fx, a.idx(0));
        let r: Result<Option<String>, Fail> = match m {
            "prepare_page_background" => unit(h.prepare_page_background(d, a.idx(1).as_i64().unwrap())),
            "prepare_add_page" => unit(h.prepare_add_page(d, a.idx(1))),
            "prepare_delete_page" => unit(h.prepare_delete_page(d, a.idx(1))),
            "prepare_reorder_pages" => unit(h.prepare_reorder_pages(d, a.idx(1))),
            "prepare_pdf_pages" => unit(h.prepare_pdf_pages(d, a.idx(1), a.idx(2), a.idx(3), a.idx(4), a.idx(5))),
            "prepare_ink_delete" => unit(h.prepare_ink_delete(d, a.idx(1), a.idx(2), a.idx(3))),
            "prepare_first_stroke" => {
                let snap = snapshot::from_dict(d).unwrap();
                let pts = fx.k("points").idx(a.idx(2).as_i64().unwrap() as usize);
                unit(h.prepare_first_stroke(
                    &snap,
                    a.idx(1).as_str().unwrap(),
                    pts,
                    a.idx(3).as_i64().unwrap(),
                    a.idx(4).as_f64().unwrap(),
                    a.idx(5).as_i64().unwrap(),
                    a.idx(6).as_str().unwrap(),
                ))
            }
            "prepare_ink_append" => {
                let snap = snapshot::from_dict(d).unwrap();
                h.prepare_ink_append(&snap, a.idx(1).as_i64().unwrap(), a.idx(2), a.idx(3).as_i64().unwrap())
                    .map(|v| Some(js(&v)))
                    .map_err(|f| Fail::from(f.error))
            }
            other => panic!("unknown host method {other}"),
        };
        t.check(&label, case.k("result"), r);
    }
}

fn run_templates(fx: &Val, t: &mut Tally) {
    let Some(tc) = fx.get("templates") else { return };
    // Full fixtures carry the stored rows; compact ones name the documents
    // (the row text is json.dumps of the snapshot dict).
    let rows: Vec<(String, String)> = match (tc.get("rows"), tc.get("row_docs")) {
        (Some(rows), _) => list(rows).iter().map(|r| (r.idx(0).as_str().unwrap().to_string(), r.idx(1).as_str().unwrap().to_string())).collect(),
        (None, Some(rows)) => list(rows)
            .iter()
            .map(|r| (r.idx(0).as_str().unwrap().to_string(), dumps(fx.k("docs").k(r.idx(1).as_str().unwrap()), true, true).unwrap()))
            .collect(),
        _ => return,
    };
    t.check("templates", &tc.k("result"), ok_val(areas::templates(&rows)));
}

// ---------------------------------------------------------------- store script

/// Mask random UUIDs (synthetic fixture ids keep their `00000000-0000-4000-8000-` prefix)
/// and wall-clock millisecond timestamps.
fn mask(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
    let word = |c: char| c.is_ascii_alphanumeric();
    while i < b.len() {
        if i + 36 <= b.len() && (i == 0 || !word(b[i - 1])) && (i + 36 == b.len() || !word(b[i + 36])) {
            let cand: String = b[i..i + 36].iter().collect();
            let shape = cand.char_indices().all(|(j, c)| if [8, 13, 18, 23].contains(&j) { c == '-' } else { hex(c) });
            if shape {
                out.push_str(if cand.starts_with("00000000-0000-4000-8000-") { &cand } else { "<uuid>" });
                i += 36;
                continue;
            }
        }
        if b[i].is_ascii_digit() && (i == 0 || !word(b[i - 1])) {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let digits: String = b[i..j].iter().collect();
            if (j == b.len() || !word(b[j])) && digits.len() == 13 {
                let v: i64 = digits.parse().unwrap();
                if (1_750_000_000_000..2_100_000_000_000).contains(&v) {
                    out.push_str("<ts>");
                    i = j;
                    continue;
                }
            }
            out.push_str(&digits);
            i = j;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn masked_result(r: &Val) -> Val {
    match r.get("ok") {
        Some(Val::Str(s)) => Val::dict(vec![("ok", Val::s(mask(s)))]),
        _ => r.clone(),
    }
}

fn run_store(fx: &Val, scratch: &Path, t: &mut Tally) {
    let Some(sc) = fx.get("store") else { return };
    let dir = scratch.join("lib");
    let _ = std::fs::remove_dir_all(&dir);
    let store = StoreCore::open(&dir).unwrap();
    let h = PureHost::new();
    let mut ids = Vec::new();
    for (i, name) in list(sc.k("docs")).iter().enumerate() {
        let d = fx.k("docs").k(name.as_str().unwrap());
        ids.push(store.apply_document(&h, &Val::s(format!("store-srv-{i}")), d, None).unwrap());
    }
    for (o, expected) in list(sc.k("ops")).iter().zip(list(sc.k("results"))) {
        let name = o.idx(0).as_str().unwrap();
        let ident = match o.idx(1) {
            Val::Int(i) if name != "create_editor" => ids[*i as usize].clone(),
            _ => String::new(),
        };
        let rev = |tag: &Val| -> Val {
            let r = store.get(&ident).unwrap().get_or_null("revision");
            match (tag.as_str(), &r) {
                (Some("stale"), Val::Int(i)) => Val::Int(i - 1),
                _ => r,
            }
        };
        let r: Result<Val, rs_store::db::Error> = match name {
            "page_style" => store.page_style(&h, &ident),
            "page_ops" => store.page_ops(&h, &ident),
            "get" => store.get(&ident),
            "save_page_background" => store.save_page_background(&h, &ident, o.idx(2), &rev(o.idx(3))),
            "save_page_op" => store.save_page_op(&h, &ident, o.idx(2), o.idx(3), &rev(o.idx(4))),
            "save_pdf_pages" => store.save_pdf_pages(&h, &ident, &rev(o.idx(2)), o.idx(3), o.idx(4), o.idx(5), o.idx(6), o.idx(7)),
            "cache_ink" => (|| {
                let row = store.raw(&ident)?;
                let base = loads(row.k("base_document").as_str().unwrap()).unwrap();
                let mut p = areas::project_ink(&base, false).map_err(rs_store::db::Error::from)?;
                p.set("pendingStrokes", Val::List(vec![]));
                p.set("pendingErase", Val::List(vec![]));
                // The frozen oracle predates `ink-projection-v7:carried-objects`
                // (2026-10-28, pen on notes with pictures/shapes/text boxes) and
                // `ink-projection-v8:floating-objects` (2026-10-28, pen on notes
                // with free-floating pictures), which changed only the recipe
                // string for these synthetic notes.
                assert_eq!(areas::PROJECTION_RECIPE, "ink-projection-v8:floating-objects");
                p.set("recipe", Val::s("ink-projection-v6:highlighter"));
                store.cache_ink(&ident, &row.k("revision"), &p)?;
                Ok(p)
            })(),
            "cache_editor" => (|| {
                let row = store.raw(&ident)?;
                let base = loads(row.k("base_document").as_str().unwrap()).unwrap();
                let snap = snapshot::from_dict(&base).map_err(rs_store::db::Error::from)?;
                let p = areas::editor_projection(&snap).map_err(rs_store::db::Error::from)?;
                store.cache_editor(&ident, &row.k("revision"), &p)?;
                Ok(p)
            })(),
            "save_ink" => store.save_ink(&h, &ident, o.idx(2), &rev(o.idx(3))),
            "save_ink_erase" => store.save_ink_erase(&h, &ident, o.idx(2), &rev(o.idx(3))),
            "save_editor" => store.save_editor(&h, &ident, o.idx(2), o.idx(3), &rev(o.idx(4))),
            "create_editor" => store.create_editor(&h, o.idx(1), o.idx(2)),
            other => panic!("unknown op {other}"),
        };
        let label = format!("store {}", trunc(js(o)));
        t.check(&label, &masked_result(expected), r.map(|v| Some(mask(&js(&v)))).map_err(Fail::from));
    }
    // Every table, masked: full text, or its SHA-256 in compact fixtures.
    let (tables, digests) = match (sc.get("tables"), sc.get("table_sha256")) {
        (Some(t), _) => (t.clone(), false),
        (None, Some(t)) => (t.clone(), true),
        _ => return,
    };
    for (table, want) in match tables {
        Val::Dict(items) => items,
        _ => vec![],
    } {
        let order = if table == "metadata" { "1" } else { "rowid" };
        let rows = store
            .with_db(|db| {
                // `editor_projection_recipe` (editor rules v2) is newer than the oracle.
                let filter = if table == "metadata" { " WHERE key<>'editor_projection_recipe'" } else { "" };
                let cur = rs_store::db::Sql::query(db, &format!("SELECT * FROM \"{table}\"{filter} ORDER BY {order}"), &[])?;
                Ok(cur
                    .iter()
                    .map(|row| match row.to_dict() {
                        Val::Dict(items) => Val::List(
                            items
                                .into_iter()
                                .map(|(_, v)| match v {
                                    Val::Bytes(b) => Val::s(b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
                                    v => v,
                                })
                                .collect(),
                        ),
                        _ => Val::Null,
                    })
                    .collect::<Vec<_>>())
            })
            .unwrap();
        let mut got = mask(&js(&Val::List(rows)));
        let want = if digests {
            got = rs_doc::util::hex(&rs_doc::util::sha256(got.as_bytes()));
            want.as_str().unwrap().to_string()
        } else {
            mask(want.as_str().unwrap())
        };
        t.total += 1;
        if got == want {
            t.ok_match += 1;
        } else {
            t.mismatches.push(format!("table {table}\n  py:   {}\n  rust: {}", trunc(want), trunc(got)));
        }
    }
}

#[test]
fn python_differential() {
    let paths = fixture_paths();
    if paths.is_empty() {
        eprintln!("no fixtures");
        return;
    }
    let mut failed = false;
    for path in paths {
        let fx = load(&path);
        let mut sections = Vec::new();
        for (name, f) in [
            ("funcs", run_funcs as fn(&Val, &mut Tally)),
            ("host", run_host as fn(&Val, &mut Tally)),
            ("templates", run_templates as fn(&Val, &mut Tally)),
        ] {
            let mut t = Tally::default();
            f(&fx, &mut t);
            sections.push((name, t));
        }
        let scratch = std::env::var("NOTES_HOST_SCRATCH").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join(format!(
            "notes-host-diff-{}-{}",
            std::process::id(),
            path.file_stem().unwrap().to_string_lossy()
        ));
        let mut t = Tally::default();
        run_store(&fx, &scratch, &mut t);
        let _ = std::fs::remove_dir_all(&scratch);
        sections.push(("store", t));
        let impl_name = fx.get_or_null("impl");
        for (name, t) in &sections {
            eprintln!(
                "[{} impl={}] {name}: {} cases, {} values equal ({} intended v2 editor divergences), {} errors equal (class+message), {} errors class-only, {} mismatches",
                path.display(),
                impl_name.as_str().unwrap_or("?"),
                t.total,
                t.ok_match,
                t.intended,
                t.err_match,
                t.flags_only,
                t.mismatches.len() - t.flags_only
            );
            for m in t.mismatches.iter().take(40) {
                eprintln!("  {m}");
            }
            if t.mismatches.len() > t.flags_only {
                failed = true;
            }
        }
    }
    assert!(!failed, "differential mismatches (see stderr)");
}
