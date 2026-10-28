//! Structural page census over a PRIVATE COPY of a library (SQLite online
//! backup). Prints counts only: no note ids, titles, text, geometry or URLs.
//!
//! Usage: page_census <store-copy-dir> [<live page-previews dir, read-only>]
//!
//! For every page of every loaded note it runs the real `PreviewManager`
//! manifest (offline, no network) and buckets the page by its preview source
//! (canvas cache cached on disk / needs a download, PDF, local renderer) or,
//! when no source exists, by the structural reason the local renderer refuses
//! it. The optional second argument is only `stat`ed to tell whether a canvas
//! cache was already downloaded.
use notes_preview::{Candidate, Fail, PreviewEnv, PreviewManager, Val};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Offline;
impl PreviewEnv for Offline {
    fn session_path(&self) -> Option<PathBuf> {
        None
    }
    fn offline(&self) -> bool {
        true
    }
    fn headers(&self) -> Result<Vec<(String, String)>, Fail> {
        Err(Fail::new(notes_preview::Kind::Other, "offline"))
    }
    /// `CENSUS_SELF_EXEC=1`: prepare in the isolated child like the engine.
    fn ink_child_executable(&self) -> Option<PathBuf> {
        std::env::var_os("CENSUS_SELF_EXEC").and_then(|_| std::env::current_exe().ok())
    }
}

fn bump(map: &mut BTreeMap<String, usize>, key: impl Into<String>) {
    *map.entry(key.into()).or_default() += 1;
}

/// Structural reason for a page the local renderer did not admit.
fn page_reason(page: &rs_ops::xml::Element) -> String {
    let objects: Vec<&rs_ops::xml::Element> = page.iter().filter(|n| n.tag == "object").collect();
    let mut kinds: Vec<String> = Vec::new();
    for o in &objects {
        let t = o.get("type").unwrap_or("?");
        let label = match t {
            "stroke" => continue,
            "shape" | "line" if rs_ops::page_shapes::renderer_skippable_shape(o) => continue,
            "shape" | "line" => "shape/line (foreign)".to_string(),
            other => other.to_string(),
        };
        if !kinds.contains(&label) {
            kinds.push(label);
        }
    }
    kinds.sort();
    let tags: Vec<String> = page.iter().map(|n| n.tag.clone()).collect();
    let mut flags = Vec::new();
    if tags.iter().any(|t| t == "pdfData" || t == "pdfDataList") {
        flags.push("pdf");
    }
    if page.get_or("bgImageId", "-1") != "-1" {
        flags.push("bgImage");
    }
    if objects.is_empty() {
        flags.push("no objects (blank)");
    } else if kinds.is_empty() {
        flags.push("strokes/our shapes only (refused by stroke admission or page attrs)");
    }
    let mut parts: Vec<String> = kinds.into_iter().map(|k| format!("has {k}")).collect();
    parts.extend(flags.iter().map(|f| f.to_string()));
    parts.join(" + ")
}

fn note_error_class(message: &str) -> String {
    // Fixed code strings only; keep the first clause.
    message.split(['(', ':']).next().unwrap_or("").trim().chars().take(70).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = notes_preview::run_child(&args) {
        std::process::exit(code);
    }
    let target = PathBuf::from(&args[1]);
    let live = args.get(2).map(PathBuf::from);
    let store = Arc::new(rs_store::storage::StoreCore::open(&target).unwrap());
    let previews = PreviewManager::new(store.clone(), Arc::new(Offline)).unwrap();
    let rows = store.with_db(|db| {
        use rs_store::db::Sql;
        db.query("SELECT id, trashed, title FROM notes WHERE loaded=1 AND deleted_remote=0 AND base_document IS NOT NULL", &[])
    }).unwrap();
    let mut sources: BTreeMap<String, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut note_errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut renders: BTreeMap<String, usize> = BTreeMap::new();
    let (mut total, mut unavailable) = (0usize, 0usize);
    for row in rows.iter() {
        let id = row.text("id").unwrap_or_default().to_string();
        let trashed = row.at(1).truthy();
        // Optional private focus on one note named by the user (never printed).
        if let Ok(wanted) = std::env::var("CENSUS_TITLE_CONTAINS") {
            if !row.text("title").unwrap_or_default().contains(wanted.as_str()) {
                continue;
            }
        }
        let scope = if trashed { "trash" } else { "library" };
        let entry = match previews.entry(&id) {
            Ok(e) => e,
            Err(_) => {
                bump(&mut sources, format!("{scope}: note inventory failed"));
                continue;
            }
        };
        let raw = store.raw(&id).unwrap();
        let snapshot = notes_host::snapshot::loads(raw.get("base_document").and_then(Val::as_str).unwrap()).unwrap();
        let resource = notes_host::snapshot::from_dict(&snapshot).ok().map(|s| s.resource_xml);
        let root = resource.as_deref().and_then(|r| rs_ops::xml::parse_bytes(r).ok());
        let pages_xml: Vec<rs_ops::xml::Element> = root.as_ref()
            .and_then(|r| r.children.iter().find(|c| c.tag == "pageList"))
            .map(|l| l.children.iter().filter(|p| p.tag == "page").cloned().collect())
            .unwrap_or_default();
        let note_error = match notes_preview::ink::prepare::prepare_pages(&snapshot, None, false) {
            Ok(_) => None,
            Err(e) => Some(note_error_class(&e.0)),
        };
        for page in &entry.pages {
            total += 1;
            let index = page.get("index").and_then(Val::as_i64).unwrap_or(-1);
            // Optional: really render every page locally (offline; canvas
            // pages exercise the local stand-in), counting outcomes only.
            if std::env::var_os("CENSUS_RENDER").is_some() && entry.candidates.contains_key(&index) {
                let kind = match entry.candidates.get(&index) {
                    Some(Candidate::Canvas(_)) => "canvas→local stand-in",
                    Some(Candidate::Pdf(_)) => "pdf",
                    Some(Candidate::Ink(c)) if c.composite => "composite",
                    _ => "ink",
                };
                let only = std::env::var("CENSUS_KIND").ok();
                if kind != "pdf" && only.as_deref().is_none_or(|k| k == kind) {
                    let started = std::time::Instant::now();
                    if std::env::var_os("CENSUS_TRACE").is_some() {
                        eprintln!("trace: note#{} page {index} kind {kind} at {:?}", rows.iter().position(|r| r.text("id").unwrap_or_default() == id).unwrap_or(0), std::time::SystemTime::now());
                    }
                    let mut outcome = previews.request(&id, index, 0, 120.0);
                    if matches!(outcome, Ok(None)) {
                        outcome = previews.request(&id, index, 0, 120.0);
                    }
                    let label = match outcome {
                        Ok(Some(_)) => "rendered".to_string(),
                        Ok(None) => "still pending".into(),
                        Err(e) => format!("error {} {}", e.code, e.status),
                    };
                    let secs = started.elapsed().as_secs();
                    let bucket = if secs < 5 { "<5s" } else if secs < 30 { "5-30s" } else if secs < 120 { "30-120s" } else { ">120s" };
                    bump(&mut renders, format!("{scope}: {kind}: {label} [{bucket}]"));
                }
            }
            // Optional: direct timing of renderer-only pages (structure counts only).
            if std::env::var_os("CENSUS_DIAG").is_some() && matches!(entry.candidates.get(&index), Some(Candidate::Ink(c)) if !c.composite) {
                use notes_preview::InkBackend;
                let t0 = std::time::Instant::now();
                let body = notes_preview::ink::prepare::page_body(&snapshot, index);
                let prep = t0.elapsed().as_millis();
                let strokes = String::from_utf8_lossy(&body).matches("\"rendering\"").count();
                let flow = String::from_utf8_lossy(&body).contains("\"flow\"");
                let native = notes_preview::NativeInk::new(notes_preview::env::default_project_root(), None);
                let t1 = std::time::Instant::now();
                let outcome = native.render_page(&snapshot, index, &|_| None);
                let forced = notes_preview::ink::prepare::forced_composite(&snapshot, index);
                eprintln!("diag: body {} KiB, strokes {strokes}, flow {flow}, prepare {prep} ms, render {} ms, outcome {}, forced composite {} KiB",
                    body.len() / 1024, t1.elapsed().as_millis(),
                    match &outcome { Ok((_, true)) => "ok".to_string(), Ok((_, false)) => "stand-in".to_string(), Err(e) => format!("err {}", e.message) },
                    forced.len() / 1024);
            }
            match entry.candidates.get(&index) {
                Some(Candidate::Canvas(c)) => {
                    let cached = live.as_ref().zip(previews.path(&Candidate::Canvas(c.clone())).ok())
                        .is_some_and(|(dir, p): (&PathBuf, PathBuf)| Path::new(dir).join(p.file_name().unwrap()).is_file());
                    bump(&mut sources, format!("{scope}: canvas cache ({})", if cached { "already downloaded" } else { "needs download" }));
                }
                Some(Candidate::Pdf(c)) => bump(&mut sources, format!("{scope}: pdf ({})", if c.overlay.is_some() { "with overlay" } else { "background" })),
                Some(Candidate::Ink(c)) if c.composite => bump(&mut sources, format!("{scope}: local page composite ({})",
                    c.notice.as_deref().unwrap_or("complete"))),
                Some(Candidate::Ink(_)) => bump(&mut sources, format!("{scope}: local renderer")),
                None => {
                    unavailable += 1;
                    bump(&mut sources, format!("{scope}: UNAVAILABLE"));
                    let mut structural = pages_xml.get(index as usize).map(page_reason).unwrap_or_else(|| "page missing".into());
                    let body_text = root.as_ref().and_then(|r| r.children.iter().find(|c| c.tag == "bodyText"))
                        .is_some_and(|b| b.iter().any(|n| n.tag == "text" && n.text.as_deref().is_some_and(|t| !t.trim().is_empty())));
                    let other_content = pages_xml.iter().any(|p| p.iter().any(|n| n.tag == "object" || n.tag == "pdfDataList"));
                    structural.push_str(&format!(" [{} of {} pages; body text: {body_text}; other pages with content: {other_content}]",
                        if index as usize + 1 == pages_xml.len() { "last" } else { "not last" }, pages_xml.len()));
                    let key = match &note_error {
                        Some(n) => format!("{scope}: [note refused: {n}] {structural}"),
                        None => format!("{scope}: {structural}"),
                    };
                    bump(&mut reasons, key);
                    if let Some(n) = &note_error {
                        bump(&mut note_errors, n.clone());
                    }
                }
            }
        }
    }
    println!("pages total: {total}, unavailable: {unavailable}");
    println!("\nby source:");
    for (k, v) in &sources {
        println!("  {v:5}  {k}");
    }
    println!("\nunavailable by structural reason:");
    for (k, v) in &reasons {
        println!("  {v:5}  {k}");
    }
    if !renders.is_empty() {
        println!("\nlocal render outcomes (offline):");
        for (k, v) in &renders {
            println!("  {v:5}  {k}");
        }
    }
    println!("\nnote-level refusal classes (pages):");
    for (k, v) in &note_errors {
        println!("  {v:5}  {k}");
    }
}
