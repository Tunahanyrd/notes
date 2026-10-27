//! Differential replay against the Python oracle.
//!
//! `record.py` (reference/python/generators/notes-cloud/, see the crate PORTING.md) runs the Python cloud unit tests
//! with recording wrappers and writes one JSON file per test: for every
//! top-level cloud call the full temp-root tree before/after, arguments,
//! transport exchanges, clock values, drawn uuid4/secrets values and the
//! result or exception. This test replays each step against the Rust port
//! (same tree, same transport answers, same clock/random values) and compares
//! the result, every outgoing request and the resulting tree byte for byte.
//!
//! Set `NOTES_CLOUD_DIFF_DIR=<dir>` to run it; without it the test is a no-op.
use notes_cloud::api::{CloudApi, Options, SnapIn};
use notes_cloud::transport::{Request, Response, Transport};
use notes_cloud::util::Random;
use notes_cloud::{json, ops, Fail, Kind, Snapshot, Val};
use rs_ink::core::binary::RawSamples;
use rs_store::val::py_eq;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn b64d(s: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

fn get<'a>(v: &'a Val, k: &str) -> &'a Val {
    v.get(k).unwrap_or(&Val::Null)
}

fn list(v: &Val) -> &[Val] {
    match v {
        Val::List(items) => items,
        _ => &[],
    }
}

// ---------------------------------------------------------------- replay collaborators

struct Replay {
    exchanges: Vec<Val>,
    pos: AtomicUsize,
    seen: Mutex<Vec<(String, Request)>>,
}

fn fail_of(e: &Val) -> Fail {
    let class = get(e, "class").as_str().unwrap_or("");
    let message = get(e, "message").as_str().map(str::to_string).unwrap_or_else(|| get(e, "str").as_str().unwrap_or("").to_string());
    let mro: Vec<&str> = list(get(e, "mro")).iter().filter_map(|v| v.as_str()).collect();
    if mro.contains(&"APIError") {
        let code = get(e, "code").as_str().unwrap_or("");
        let mut f = Fail::api(code, &message);
        f.ambiguous = get(e, "ambiguous").truthy();
        f.kind = match class {
            "ConflictError" => Kind::Conflict,
            "UnsupportedDocumentError" => Kind::Unsupported,
            _ => Kind::Api,
        };
        return f;
    }
    if mro.contains(&"OSError") {
        return Fail::os(message);
    }
    if mro.contains(&"ValueError") {
        return Fail::value(message);
    }
    Fail::new(Kind::Other, message)
}

impl Transport for Replay {
    fn send(&self, _directory: &Path, name: &str, request: &Request, _limit: usize) -> Result<Response, Fail> {
        self.seen.lock().unwrap().push((name.to_string(), request.clone()));
        let i = self.pos.fetch_add(1, Ordering::SeqCst);
        let Some(e) = self.exchanges.get(i) else { return Err(Fail::os("replay: no recorded exchange left")) };
        if let Some(raise) = e.get("raise") {
            return Err(fail_of(raise));
        }
        let r = get(e, "response");
        Ok(Response {
            status: get(r, "status").as_i64(),
            body: get(r, "raw").as_str().map(b64d),
            content_type: get(r, "ctype").as_str().map(str::to_string),
        })
    }
}

struct SeqRandom {
    uuid4: Mutex<Vec<String>>,
    hex: Mutex<Vec<String>>,
    choice: Mutex<Vec<String>>,
    safe: Mutex<Vec<String>>,
    short: AtomicUsize,
}

fn pop(v: &Mutex<Vec<String>>) -> Option<String> {
    let mut v = v.lock().unwrap();
    if v.is_empty() {
        None
    } else {
        Some(v.remove(0))
    }
}

impl Random for SeqRandom {
    fn token_urlsafe(&self, n: usize) -> String {
        pop(&self.safe).unwrap_or_else(|| {
            self.short.fetch_add(1, Ordering::SeqCst);
            notes_cloud::OsRandom.token_urlsafe(n)
        })
    }
    fn uuid4(&self) -> String {
        pop(&self.uuid4).unwrap_or_else(|| {
            self.short.fetch_add(1, Ordering::SeqCst);
            notes_cloud::OsRandom.uuid4()
        })
    }
    fn token_hex(&self, n: usize) -> String {
        pop(&self.hex).unwrap_or_else(|| {
            self.short.fetch_add(1, Ordering::SeqCst);
            notes_cloud::OsRandom.token_hex(n)
        })
    }
    fn choice(&self, alphabet: &str, n: usize) -> String {
        (0..n)
            .map(|_| {
                pop(&self.choice).unwrap_or_else(|| {
                    self.short.fetch_add(1, Ordering::SeqCst);
                    notes_cloud::OsRandom.choice(alphabet, 1)
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------- trees

fn set_mode(path: &Path, mode: u32) {
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

fn write_tree(root: &Path, t: &Val) {
    std::fs::create_dir_all(root).unwrap();
    set_mode(root, 0o700);
    let mut dirs: Vec<(&String, &Val)> = match get(t, "dirs") {
        Val::Dict(items) => items.iter().map(|(k, v)| (k, v)).collect(),
        _ => vec![],
    };
    dirs.sort_by_key(|(k, _)| k.matches('/').count());
    for (rel, _) in &dirs {
        std::fs::create_dir_all(root.join(rel)).unwrap();
    }
    if let Val::Dict(files) = get(t, "files") {
        for (rel, f) in files {
            let p = root.join(rel);
            if let Some(link) = f.get("link").and_then(|l| l.as_str()) {
                std::os::unix::fs::symlink(link, &p).unwrap();
                continue;
            }
            std::fs::write(&p, b64d(get(f, "data").as_str().unwrap_or(""))).unwrap();
            set_mode(&p, get(f, "mode").as_i64().unwrap_or(0o600) as u32);
        }
    }
    // Deepest first so parent modes do not block child writes above.
    dirs.reverse();
    for (rel, mode) in dirs {
        set_mode(&root.join(rel), mode.as_i64().unwrap_or(0o700) as u32);
    }
}

#[derive(PartialEq, Debug)]
enum Entry {
    Dir(u32),
    File(u32, Vec<u8>),
    Link(String),
}

fn read_tree(root: &Path) -> BTreeMap<String, Entry> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Entry>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            let mode = meta.permissions().mode() & 0o7777;
            if meta.file_type().is_symlink() {
                out.insert(rel, Entry::Link(std::fs::read_link(&p).unwrap().to_string_lossy().into_owned()));
            } else if meta.is_dir() {
                out.insert(rel, Entry::Dir(mode));
                walk(root, &p, out);
            } else {
                out.insert(rel, Entry::File(mode, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn expected_tree(t: &Val) -> BTreeMap<String, Entry> {
    let mut out = BTreeMap::new();
    if let Val::Dict(dirs) = get(t, "dirs") {
        for (k, m) in dirs {
            out.insert(k.clone(), Entry::Dir(m.as_i64().unwrap_or(0) as u32));
        }
    }
    if let Val::Dict(files) = get(t, "files") {
        for (k, f) in files {
            let e = match f.get("link").and_then(|l| l.as_str()) {
                Some(l) => Entry::Link(l.to_string()),
                None => Entry::File(get(f, "mode").as_i64().unwrap_or(0) as u32, b64d(get(f, "data").as_str().unwrap_or(""))),
            };
            out.insert(k.clone(), e);
        }
    }
    out
}

fn tree_diff(want: &BTreeMap<String, Entry>, got: &BTreeMap<String, Entry>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in want {
        match got.get(k) {
            None => out.push(format!("missing {k}")),
            Some(g) if g != v => {
                let detail = match (v, g) {
                    (Entry::File(m1, d1), Entry::File(m2, d2)) if m1 == m2 => {
                        let a = String::from_utf8_lossy(d1);
                        let b = String::from_utf8_lossy(d2);
                        let at = a.chars().zip(b.chars()).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
                        let lo = at.saturating_sub(60);
                        format!(
                            "bytes differ at {at}: py=…{}… rs=…{}…",
                            a.chars().skip(lo).take(140).collect::<String>(),
                            b.chars().skip(lo).take(140).collect::<String>()
                        )
                    }
                    _ => format!("{v:?} vs {g:?}").chars().take(200).collect(),
                };
                out.push(format!("differs {k}: {detail}"));
            }
            _ => {}
        }
    }
    for k in got.keys() {
        if !want.contains_key(k) {
            out.push(format!("extra {k}"));
        }
    }
    out
}

// ---------------------------------------------------------------- arguments

fn unwrap_markers(v: &Val) -> Val {
    match v {
        Val::Dict(items) if items.len() == 1 && items[0].0 == "$snapshot" => items[0].1.clone(),
        other => other.clone(),
    }
}

fn s<'a>(args: &'a Val, k: &str) -> Result<&'a str, String> {
    get(args, k).as_str().ok_or_else(|| format!("type: {k} is {}", get(args, k).type_name()))
}

fn opt_s<'a>(args: &'a Val, k: &str) -> Result<Option<&'a str>, String> {
    match get(args, k) {
        Val::Null => Ok(None),
        Val::Str(x) => Ok(Some(x)),
        other => Err(format!("type: {k} is {}", other.type_name())),
    }
}

fn int(args: &Val, k: &str) -> Result<i64, String> {
    match get(args, k) {
        Val::Int(i) => Ok(*i),
        other => Err(format!("type: {k} is {}", other.type_name())),
    }
}

fn opt_int(args: &Val, k: &str) -> Result<Option<i64>, String> {
    match get(args, k) {
        Val::Null => Ok(None),
        Val::Int(i) => Ok(Some(*i)),
        other => Err(format!("type: {k} is {}", other.type_name())),
    }
}

fn float(args: &Val, k: &str) -> Result<f64, String> {
    match get(args, k) {
        Val::Int(i) => Ok(*i as f64),
        Val::Float(f) => Ok(*f),
        other => Err(format!("type: {k} is {}", other.type_name())),
    }
}

fn bytes(args: &Val, k: &str) -> Result<Vec<u8>, String> {
    match get(args, k).get("$bytes").and_then(|b| b.as_str()) {
        Some(b) => Ok(b64d(b)),
        None => Err(format!("type: {k} is not bytes")),
    }
}

fn strs(args: &Val, k: &str) -> Result<Vec<String>, String> {
    match get(args, k) {
        Val::List(items) => items.iter().map(|v| v.as_str().map(str::to_string).ok_or_else(|| format!("type: {k} item"))).collect(),
        Val::Str(x) => Ok(vec![x.clone()]),
        other => Err(format!("type: {k} is {}", other.type_name())),
    }
}

fn samples(args: &Val, k: &str) -> Result<RawSamples, String> {
    let v = get(get(args, k), "$samples");
    if v.is_null() {
        return Err(format!("type: {k} is not StrokeSamples"));
    }
    let iv = notes_host::deps::ink_val;
    Ok(RawSamples {
        points: list(get(v, "points"))
            .iter()
            .map(|p| match p {
                Val::List(xy) if xy.len() == 2 => Some([iv(&xy[0]), iv(&xy[1])]),
                _ => None,
            })
            .collect(),
        pressures: list(get(v, "pressures")).iter().map(iv).collect(),
        timestamps: list(get(v, "timestamps")).iter().map(iv).collect(),
        tilts: list(get(v, "tilts")).iter().map(iv).collect(),
        orientations: list(get(v, "orientations")).iter().map(iv).collect(),
    })
}

fn snap_dict(s: &Snapshot) -> Val {
    Val::dict(vec![("$snapshot", notes_host::snapshot::to_dict(s))])
}

/// Run one step's call. Outer Err: the Python arguments are not expressible
/// with the Rust signature (a type-level divergence, skipped).
fn dispatch(api: &Arc<CloudApi>, target: &str, method: &str, args: &Val) -> Result<Result<Val, Fail>, String> {
    let snapshot = unwrap_markers(get(args, "snapshot"));
    let snap = SnapIn::Dict(&snapshot);
    Ok(match (target, method) {
        ("api", "auth_status") => api.auth_status(),
        ("api", "changes") => api.changes(opt_s(args, "cursor")?),
        ("api", "get_note") => api.get_note(s(args, "server_id")?).map(|s| snap_dict(&s)),
        ("api", "upload_content_file") => api.upload_content_file(&bytes(args, "data")?),
        ("api", "create") => api.create(s(args, "title")?, s(args, "body")?, s(args, "operation_id")?, opt_s(args, "local_id")?),
        ("api", "update") => api.update(s(args, "server_id")?, snap, s(args, "title")?, s(args, "body")?, opt_s(args, "baseline_commit")?, s(args, "operation_id")?),
        ("api", "update_title") => api.update_title(s(args, "server_id")?, snap, s(args, "title")?, s(args, "operation_id")?),
        ("api", "create_editor") => api.create_editor(s(args, "title")?, get(args, "model"), s(args, "operation_id")?, opt_s(args, "local_id")?),
        ("api", "update_editor") => api.update_editor(
            s(args, "server_id")?,
            snap,
            s(args, "title")?,
            get(args, "model"),
            opt_s(args, "baseline_commit")?,
            s(args, "operation_id")?,
        ),
        ("api", "update_page_background") => {
            api.update_page_background(s(args, "server_id")?, snap, int(args, "color_argb")?, s(args, "operation_id")?, opt_int(args, "now_ms")?)
        }
        ("api", "add_pdf_pages") => {
            let sizes: Vec<(f64, f64)> = list(get(args, "page_sizes"))
                .iter()
                .map(|p| match p {
                    Val::List(wh) if wh.len() == 2 => Ok((wh[0].as_f64().ok_or("type: size")?, wh[1].as_f64().ok_or("type: size")?)),
                    _ => Err("type: page size".to_string()),
                })
                .collect::<Result<_, String>>()?;
            let indices: Option<Vec<i64>> = match get(args, "page_indices") {
                Val::Null => None,
                Val::List(items) => Some(items.iter().map(|v| v.as_i64().ok_or("type: index".to_string())).collect::<Result<_, _>>()?),
                _ => return Err("type: page_indices".into()),
            };
            api.add_pdf_pages(
                s(args, "server_id")?,
                snap,
                &bytes(args, "pdf")?,
                s(args, "file_name")?,
                &sizes,
                indices.as_deref(),
                opt_int(args, "template_type")?,
                s(args, "operation_id")?,
                opt_int(args, "now_ms")?,
            )
        }
        ("api", "update_ink_clone") => api.update_ink_clone(
            s(args, "server_id")?,
            snap,
            s(args, "source_stroke_id")?,
            s(args, "new_stroke_id")?,
            float(args, "dx")?,
            float(args, "dy")?,
            s(args, "operation_id")?,
            opt_int(args, "now_ms")?,
        ),
        ("api", m @ ("append_ink_stroke" | "append_ink_stroke_v2")) => {
            let f = if m == "append_ink_stroke" { CloudApi::append_ink_stroke } else { CloudApi::append_ink_stroke_v2 };
            f(
                api,
                s(args, "server_id")?,
                snap,
                s(args, "source_stroke_id")?,
                &samples(args, "samples")?,
                s(args, "new_stroke_id")?,
                s(args, "operation_id")?,
                opt_int(args, "now_ms")?,
            )
        }
        ("api", "append_ink_stroke_v3") => api.append_ink_stroke_v3(
            s(args, "server_id")?,
            snap,
            s(args, "source_stroke_id")?,
            &samples(args, "samples")?,
            s(args, "new_stroke_id")?,
            s(args, "operation_id")?,
            get(args, "color_argb"),
            get(args, "pen_size"),
            opt_int(args, "now_ms")?,
        ),
        ("api", "reconcile") => api.reconcile(s(args, "operation_id")?),
        ("api", "renew_cloud_token") => api.renew_cloud_token(),
        ("api", "renew_notes_token") => api.renew_notes_token(opt_s(args, "operation_id")?),
        ("api", "_headers") => api.headers(get(args, "mutation").truthy()).map(|h| Val::Dict(h.into_iter().map(|(k, v)| (k, Val::s(v))).collect())),
        ("page_ops", "add_page") => ops::add_page(api, s(args, "server_id")?, snap, int(args, "after_index")?, s(args, "operation_id")?, opt_int(args, "now_ms")?),
        ("page_ops", "delete_pages") => {
            ops::delete_pages(api, s(args, "server_id")?, snap, &strs(args, "page_ids")?, s(args, "operation_id")?, opt_int(args, "now_ms")?)
        }
        ("page_ops", "reorder_pages") => {
            ops::reorder_pages(api, s(args, "server_id")?, snap, &strs(args, "page_ids_in_order")?, s(args, "operation_id")?, opt_int(args, "now_ms")?)
        }
        ("page_ops", "append_first_stroke") => ops::append_first_stroke(
            api,
            s(args, "server_id")?,
            snap,
            s(args, "page_id")?,
            &samples(args, "samples")?,
            get(args, "color_argb"),
            get(args, "size"),
            s(args, "new_stroke_id")?,
            s(args, "operation_id")?,
            opt_int(args, "now_ms")?,
        ),
        ("page_ops", "reconcile") => ops::reconcile_page(api, s(args, "operation_id")?),
        ("ink_erase", "erase_ink_strokes") => ops::erase_ink_strokes(
            api,
            s(args, "server_id")?,
            snap,
            s(args, "page_id")?,
            s(args, "layer_id")?,
            &strs(args, "stroke_ids")?,
            s(args, "operation_id")?,
            opt_int(args, "now_ms")?,
        ),
        ("ink_erase", "move_ink_strokes") => ops::move_ink_strokes(
            api,
            s(args, "server_id")?,
            snap,
            s(args, "page_id")?,
            s(args, "layer_id")?,
            &strs(args, "stroke_ids")?,
            get(args, "dx"),
            get(args, "dy"),
            s(args, "operation_id")?,
            opt_int(args, "now_ms")?,
        ),
        ("ink_erase", "reconcile") => ops::reconcile_erase(api, s(args, "operation_id")?),
        ("shapes", "add_shape") => {
            ops::add_shape(api, s(args, "server_id")?, snap, s(args, "page_id")?, get(args, "shape"), s(args, "operation_id")?, opt_int(args, "now_ms")?)
        }
        ("shapes", "reconcile") => ops::reconcile_shape(api, s(args, "operation_id")?),
        ("metadata", m) => {
            let meta = notes_cloud::CloudMetadata::new(api.clone());
            match m {
                "folder_changes" => meta.folder_changes(opt_s(args, "cursor")?),
                "inventory" => meta.inventory(),
                "note_list" => meta.note_list(int(args, "modified_after")?, opt_s(args, "page_token")?),
                "note_inventory" => meta.note_inventory(int(args, "max_pages")?),
                "create_folder" => meta.create_folder(s(args, "name")?, s(args, "parent_id")?, s(args, "operation_id")?, opt_s(args, "folder_id")?),
                "update_folder" => meta.update_folder(s(args, "folder_id")?, get(args, "changes"), get(args, "baseline_node"), s(args, "operation_id")?),
                "update_organization" => meta.update_organization(s(args, "server_id")?, snap, get(args, "changes"), s(args, "operation_id")?, get(args, "baseline_node")),
                _ => return Err(format!("no dispatcher for metadata.{m}")),
            }
        }
        _ => return Err(format!("no dispatcher for {target}.{method}")),
    })
}

fn kind_matches(kind: Kind, mro: &[&str]) -> bool {
    let want = match kind {
        Kind::Api => "APIError",
        Kind::Conflict => "ConflictError",
        Kind::Unsupported => "UnsupportedDocumentError",
        Kind::Document => "DocumentError",
        Kind::PageOps => "PageOpsError",
        Kind::Payload => "PayloadError",
        Kind::Ink => "InkError",
        Kind::XmlParse => "ParseError",
        Kind::Value => "ValueError",
        Kind::Unicode => "UnicodeError",
        Kind::Type => "TypeError",
        Kind::Key => "KeyError",
        Kind::Attribute => "AttributeError",
        Kind::Overflow => "OverflowError",
        Kind::Os | Kind::Disconnect => "OSError",
        _ => "Exception",
    };
    mro.first() == Some(&want)
}

fn compare_outcome(step: &Val, got: &Result<Val, Fail>) -> Option<String> {
    match (step.get("error"), got) {
        (None, Ok(v)) => {
            let want = get(step, "result");
            if py_eq(want, v) {
                None
            } else {
                let a = json::dumps(want, &json::DEFAULT);
                let b = json::dumps(v, &json::DEFAULT);
                let at = a.chars().zip(b.chars()).position(|(x, y)| x != y).unwrap_or(0);
                Some(format!("result differs at {at}: py={} rs={}", a.chars().skip(at.saturating_sub(40)).take(160).collect::<String>(), b.chars().skip(at.saturating_sub(40)).take(160).collect::<String>()))
            }
        }
        (None, Err(f)) => Some(format!("python ok, rust error {:?} {} {}", f.kind, f.code, f.message)),
        (Some(e), Ok(_)) => Some(format!("python error {} {:?}, rust ok", get(e, "class").as_str().unwrap_or(""), get(e, "message"))),
        (Some(e), Err(f)) => {
            let mro: Vec<&str> = list(get(e, "mro")).iter().filter_map(|v| v.as_str()).collect();
            let message = get(e, "message").as_str().map(str::to_string).unwrap_or_else(|| get(e, "str").as_str().unwrap_or("").to_string());
            let api = mro.contains(&"APIError");
            let mut problems = Vec::new();
            if !kind_matches(f.kind, &mro) {
                problems.push(format!("class py={} rs={:?}", mro.first().unwrap_or(&""), f.kind));
            }
            if api {
                if get(e, "code").as_str() != Some(f.code.as_str()) {
                    problems.push(format!("code py={:?} rs={}", get(e, "code"), f.code));
                }
                if get(e, "ambiguous").truthy() != f.ambiguous {
                    problems.push(format!("ambiguous py={:?} rs={}", get(e, "ambiguous"), f.ambiguous));
                }
            }
            if message != f.message && (api || mro.contains(&"DocumentError") || mro.contains(&"InkError")) {
                problems.push(format!("message py={message:?} rs={:?}", f.message));
            }
            if problems.is_empty() {
                None
            } else {
                Some(problems.join("; "))
            }
        }
    }
}

fn compare_requests(step: &Val, seen: &[(String, Request)]) -> Option<String> {
    let want = list(get(step, "exchanges"));
    if want.len() != seen.len() {
        return Some(format!("request count py={} rs={} (py: {:?}; rs: {:?})", want.len(), seen.len(),
            want.iter().map(|e| get(e, "name").as_str().unwrap_or("").to_string()).collect::<Vec<_>>(),
            seen.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()));
    }
    for (i, (e, (name, r))) in want.iter().zip(seen).enumerate() {
        let w = get(e, "request");
        if get(e, "name").as_str() != Some(name) {
            return Some(format!("request {i} name py={:?} rs={name}", get(e, "name")));
        }
        if get(w, "method").as_str() != Some(r.method.as_str()) || get(w, "url").as_str() != Some(r.url.as_str()) {
            return Some(format!("request {i} {name}: line py={:?} {:?} rs={} {}", get(w, "method"), get(w, "url"), r.method, r.url));
        }
        let headers: Vec<(String, String)> = list(get(w, "headers"))
            .iter()
            .map(|h| (list(h)[0].as_str().unwrap_or("").to_string(), list(h)[1].as_str().unwrap_or("").to_string()))
            .collect();
        if headers != r.headers {
            return Some(format!("request {i} {name}: headers py={headers:?} rs={:?}", r.headers));
        }
        let data = get(w, "data").as_str().map(b64d);
        if data != r.data {
            return Some(format!("request {i} {name}: body differs (py {} bytes, rs {} bytes)", data.map(|d| d.len()).unwrap_or(0), r.data.as_ref().map(|d| d.len()).unwrap_or(0)));
        }
    }
    None
}

fn auth_owned(step: &Val) -> bool {
    let m = get(step, "method").as_str().unwrap_or("");
    m.starts_with("renew_")
        || list(get(step, "exchanges")).iter().any(|e| {
            let url = get(get(e, "request"), "url").as_str().unwrap_or("");
            url.contains("/token/renew") || url.contains("samsungosp.com")
        })
}

#[derive(Default)]
struct Tally {
    matched: usize,
    skipped: Vec<String>,
    auth: Vec<String>,
    mismatched: Vec<String>,
}

fn run_step(scratch: &Path, test: &str, index: usize, step: &Val, tally: &mut Tally) {
    let label = format!("{test}#{index} {}.{}", get(step, "target").as_str().unwrap_or(""), get(step, "method").as_str().unwrap_or(""));
    let root = scratch.join(format!("{}-{index}", test.replace(['/', ' '], "_")));
    let _ = std::fs::remove_dir_all(&root);
    write_tree(&root, get(step, "before"));
    let session = root.join(get(step, "session").as_str().unwrap_or("session"));
    let work = root.join(get(step, "work").as_str().unwrap_or("work"));
    let replay = Arc::new(Replay { exchanges: list(get(step, "exchanges")).to_vec(), pos: AtomicUsize::new(0), seen: Mutex::new(Vec::new()) });
    let clock_values: Vec<f64> = list(get(step, "clock")).iter().filter_map(|v| v.as_f64()).collect();
    let clock_pos = Arc::new(AtomicUsize::new(0));
    let strs_of = |k: &str| list(get(step, k)).iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>();
    let random = SeqRandom { uuid4: Mutex::new(strs_of("uuid4")), hex: Mutex::new(strs_of("token_hex")), choice: Mutex::new(strs_of("choice")), safe: Mutex::new(strs_of("token_urlsafe")), short: AtomicUsize::new(0) };
    let parser_values: Vec<Val> = list(get(step, "parser")).to_vec();
    let parser_pos = Arc::new(AtomicUsize::new(0));
    let mut options = Options::default().with_random(random);
    let cp = clock_pos.clone();
    options.clock = Arc::new(move || {
        let i = cp.fetch_add(1, Ordering::SeqCst);
        clock_values.get(i).or(clock_values.last()).copied().unwrap_or(1_700_000_000.0)
    });
    if get(step, "has_transport").truthy() {
        options.transport = replay.clone();
    }
    if get(step, "has_parser").truthy() {
        let pp = parser_pos.clone();
        options = options.with_changes_parser(move |_raw| {
            let i = pp.fetch_add(1, Ordering::SeqCst);
            Ok(parser_values.get(i).cloned().unwrap_or(Val::Null))
        });
    }
    let method = get(step, "method").as_str().unwrap_or("");
    let target = get(step, "target").as_str().unwrap_or("");
    let outcome: Result<Val, Fail> = if method == "__init__" {
        CloudApi::new(&session, &work, options).map(|_| Val::Null)
    } else {
        match CloudApi::new(&session, &work, options) {
            Err(f) => Err(Fail { message: format!("construction failed: {}", f.message), ..f }),
            Ok(api) => match dispatch(&Arc::new(api), target, method, get(step, "args")) {
                Ok(r) => r,
                Err(why) => {
                    tally.skipped.push(format!("{label}: {why}"));
                    return;
                }
            },
        }
    };
    let mut problems = Vec::new();
    if let Some(p) = compare_outcome(step, &outcome) {
        problems.push(p);
    }
    if let Some(p) = compare_requests(step, &replay.seen.lock().unwrap()) {
        problems.push(p);
    }
    let diff = tree_diff(&expected_tree(get(step, "after")), &read_tree(&root));
    if !diff.is_empty() {
        problems.push(format!("tree: {}", diff.iter().take(4).cloned().collect::<Vec<_>>().join(" | ")));
    }
    let _ = std::fs::remove_dir_all(&root);
    if problems.is_empty() {
        tally.matched += 1;
    } else if auth_owned(step) {
        tally.auth.push(format!("{label}: {}", problems.join(" || ")));
    } else {
        tally.mismatched.push(format!("{label}: {}", problems.join(" || ")));
    }
}

fn replay_files(files: Vec<PathBuf>, tag: &str) -> Tally {
    let only = std::env::var("NOTES_CLOUD_DIFF_ONLY").ok();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("notes-cloud-diff-{tag}"));
    let mut tally = Tally::default();
    let mut files = files;
    files.sort();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().trim_end_matches(".gz").trim_end_matches(".json").to_string();
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        let raw = std::fs::read(&file).unwrap();
        let raw = if file.extension().is_some_and(|e| e == "gz") { notes_cloud::util::gunzip_limited(&raw, 512 << 20).unwrap() } else { raw };
        let doc = json::loads(&raw).unwrap();
        for (i, step) in list(get(&doc, "steps")).iter().enumerate() {
            run_step(&scratch, &name, i, step, &mut tally);
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    tally
}

fn report(tally: &Tally) -> String {
    format!(
        "matched {}\nauth-owned {}\n{}\nskipped (type-level) {}\n{}\nMISMATCHED {}\n{}\n",
        tally.matched,
        tally.auth.len(),
        tally.auth.join("\n"),
        tally.skipped.len(),
        tally.skipped.join("\n"),
        tally.mismatched.len(),
        tally.mismatched.join("\n")
    )
}

/// Committed synthetic recordings (tests/fixtures/oracle/*.json.gz).
#[test]
fn replay_committed_oracle_fixtures() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle");
    let files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".json.gz")).collect();
    assert!(!files.is_empty());
    let tally = replay_files(files, "fixtures");
    let text = report(&tally);
    eprintln!("{text}");
    assert!(tally.matched > 100, "{text}");
    assert!(tally.mismatched.is_empty() && tally.auth.is_empty(), "{text}");
}

/// A full recording directory written by record.py (`NOTES_CLOUD_DIFF_DIR`).
#[test]
fn replay_python_oracle_dir() {
    let Ok(dir) = std::env::var("NOTES_CLOUD_DIFF_DIR") else {
        eprintln!("NOTES_CLOUD_DIFF_DIR not set; full differential replay skipped");
        return;
    };
    let files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n != "summary.json" && (n.ends_with(".json") || n.ends_with(".json.gz"))))
        .collect();
    let tally = replay_files(files, "dir");
    let text = report(&tally);
    if let Ok(path) = std::env::var("NOTES_CLOUD_DIFF_REPORT") {
        std::fs::write(path, &text).unwrap();
    }
    eprintln!("{text}");
    assert!(tally.mismatched.is_empty() && tally.auth.is_empty(), "{} steps diverge from the Python oracle", tally.mismatched.len() + tally.auth.len());
}
