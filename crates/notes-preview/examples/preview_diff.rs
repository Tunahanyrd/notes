//! Differential driver: the Rust PreviewManager/ThumbnailManager over a
//! synthetic store copy, printing the same JSON shape as the Python runner
//! (scratch `diff_run.py`). Offline, no network. Usage: preview_diff <store-dir>
use notes_preview::{Fail, PreviewEnv, PreviewManager, ThumbnailManager, Val};
use std::path::PathBuf;
use std::sync::Arc;

struct Offline;
impl PreviewEnv for Offline {
    fn session_path(&self) -> Option<PathBuf> { None }
    fn offline(&self) -> bool { true }
    fn headers(&self) -> Result<Vec<(String, String)>, Fail> { Err(Fail::new(notes_preview::Kind::Other, "offline")) }
    fn ink_child_executable(&self) -> Option<PathBuf> {
        std::env::var_os("PREVIEW_DIFF_SELF_EXEC").and_then(|_| std::env::current_exe().ok())
    }
}

fn sha(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn call<T>(r: Result<T, Fail>, ok: impl FnOnce(T) -> Val) -> Val {
    match r {
        Ok(v) => Val::dict(vec![("ok", ok(v))]),
        Err(e) if e.is_store() => Val::dict(vec![("error", Val::List(vec![Val::s(e.code), Val::s(e.message), Val::Int(e.status as i64)]))]),
        Err(e) => Val::dict(vec![("exception", Val::s(format!("{:?}: {}", e.kind, e.message)))]),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = notes_preview::run_child(&args) {
        std::process::exit(code);
    }
    let target = PathBuf::from(&args[1]);
    let ids = rs_store::val::loads(&std::fs::read_to_string(target.join("ids.json")).unwrap()).unwrap();
    let store = Arc::new(rs_store::storage::StoreCore::open(&target).unwrap());
    let previews = Arc::new(PreviewManager::new(store.clone(), Arc::new(Offline)).unwrap());
    let thumbs = ThumbnailManager::new(store.clone(), previews.clone()).unwrap();
    let Val::Dict(mut ids) = ids else { panic!() };
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    let mut results = Vec::new();
    for (name, identity) in ids {
        let identity = identity.as_str().unwrap().to_string();
        let mut item: Vec<(String, Val)> = Vec::new();
        let manifest = previews.manifest(&identity);
        if let Ok(m) = &manifest {
            item.push(("manifest_json".into(), Val::s(rs_store::val::dumps(m, true, true).unwrap())));
        }
        item.push(("manifest".into(), call(manifest, |v| v)));
        for page in [0, 1] {
            item.push((format!("page{page}"), call(previews.request(&identity, page, 0, 60.0), |v| v.map_or(Val::Null, |b| Val::s(sha(&b))))));
        }
        let revision = store.get(&identity).unwrap().get("revision").unwrap().as_i64().unwrap();
        let mut thumb = thumbs.request(&identity, revision);
        if matches!(thumb, Ok((None, _))) {
            std::thread::sleep(std::time::Duration::from_millis(500));
            thumb = thumbs.request(&identity, revision);
        }
        item.push(("thumbnail".into(), call(thumb, |(png, complete)| Val::List(vec![png.map_or(Val::Null, |b| Val::s(sha(&b))), Val::Bool(complete)]))));
        results.push((name, Val::Dict(item)));
    }
    previews.close();
    previews.join(std::time::Duration::from_secs(2));
    let mut files = Vec::new();
    for sub in ["page-previews", "thumbnails"] {
        let mut stack = vec![target.join(sub)];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() { stack.push(p) } else {
                    files.push((p.strip_prefix(&target).unwrap().to_string_lossy().into_owned(), Val::s(sha(&std::fs::read(&p).unwrap()))));
                }
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    println!("{}", rs_store::val::dumps(&Val::dict(vec![("results", Val::Dict(results)), ("files", Val::Dict(files))]), true, true).unwrap());
}
