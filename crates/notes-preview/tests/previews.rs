//! Ports of tests/test_previews.py, test_previews_review.py and
//! test_preview_scheduling.py (synthetic data only).
mod common;
use common::*;
use notes_preview::manager::{Entry, JobState, PreviewOptions, Source};
use notes_preview::{fetch, Candidate, Fail, Kind, PreviewManager, Val};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::{Duration, Instant};

struct Event(Mutex<bool>, Condvar);
impl Event {
    fn new() -> Arc<Event> {
        Arc::new(Event(Mutex::new(false), Condvar::new()))
    }
    fn set(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }
    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.0.lock().unwrap();
        let (guard, _) = self.1.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
        *guard
    }
}

fn src(items: &[&str]) -> Source {
    Source(items.iter().map(|s| Val::s(*s)).collect())
}

fn entry_of(source: Source, candidate: Candidate) -> Entry {
    let mut candidates = BTreeMap::new();
    candidates.insert(0, candidate);
    Entry { source, pages: vec![], candidates }
}

#[test]
fn redirect_strips_all_cloud_credentials_and_requires_file_hash() {
    let raw = b"synthetic-spi".to_vec();
    let calls: Mutex<Vec<(String, Vec<(String, String)>)>> = Mutex::new(vec![]);
    let headers = || Ok(vec![("Authorization".to_string(), "Bearer synthetic".to_string()), ("X-SN-Model".into(), "synthetic".into())]);
    let transport = |url: &str, h: &[(String, String)]| {
        let mut c = calls.lock().unwrap();
        c.push((url.to_string(), h.to_vec()));
        if c.len() == 1 {
            Ok((302, vec![], Some("https://eu1rgn-down.file.samsungcloud.com/file?signature=synthetic".to_string())))
        } else {
            Ok((200, raw.clone(), None))
        }
    };
    let ok = || Ok(());
    assert_eq!(fetch::fetch_preview_with(&candidate(&raw), &headers, &transport, &ok).unwrap(), raw);
    let c = calls.lock().unwrap();
    assert!(c[0].1.iter().any(|(k, _)| k == "Authorization"));
    assert_eq!(c[1].1, vec![("Accept".to_string(), "application/octet-stream".to_string())]);
    let changed = |_: &str, _: &[(String, String)]| Ok((200, b"changed".to_vec(), None));
    assert_eq!(fetch::fetch_preview_with(&candidate(&raw), &headers, &changed, &ok).unwrap_err().kind, Kind::Preview);
}

#[test]
fn redirect_to_another_origin_stops_before_credentials_or_file_fetch() {
    let calls = AtomicUsize::new(0);
    let headers = || Ok(vec![("Authorization".to_string(), "Bearer synthetic".to_string())]);
    let transport = |_: &str, _: &[(String, String)]| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok((302, vec![], Some("https://evil.example/file".to_string())))
    };
    let ok = || Ok(());
    assert_eq!(fetch::fetch_preview_with(&candidate(b"synthetic-spi"), &headers, &transport, &ok).unwrap_err().kind, Kind::Preview);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut bad = candidate(b"x");
    bad.sha256 = "ab".repeat(32);
    bad.download_url = Some("https://ew1a-api.samsungcloud.com@evil.example/file".into());
    assert_eq!(fetch::fetch_preview_with(&bad, &headers, &transport, &ok).unwrap_err().kind, Kind::Preview);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn manager(dir: &std::path::Path, env: Arc<TestEnv>, options: PreviewOptions) -> PreviewManager {
    PreviewManager::with_options(store(dir), env, options).unwrap()
}

#[test]
fn pending_requests_share_one_decode_and_restart_uses_verified_cache() {
    let dir = TempDir::new();
    let env = TestEnv::new(Some(dir.path()), false);
    let (released, entered) = (Event::new(), Event::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let image = png(1, 1, b"\x12\x34\x56\xff");
    let entry = entry_of(src(&["synthetic-id", "commit"]), Candidate::Canvas(candidate(b"synthetic-spi")));
    let (e1, e2, r, c, p) = (entry.clone(), entry.clone(), released.clone(), calls.clone(), image.clone());
    let en = entered.clone();
    let options = PreviewOptions {
        decoder: Some(Arc::new(move |_raw: &[u8], size| {
            assert_eq!(size, (1, 1));
            c.fetch_add(1, Ordering::SeqCst);
            en.set();
            assert!(r.wait(Duration::from_secs(2)));
            Ok(p.clone())
        })),
        fetcher: Some(Arc::new(|_, _, _| Ok(b"synthetic-spi".to_vec()))),
        entry_hook: Some(Arc::new(move |_| Ok(e1.clone()))),
        source_hook: Some(Arc::new(move |_| Ok(e2.source.clone()))),
        ..Default::default()
    };
    let m = manager(dir.path(), env.clone(), options);
    assert_eq!(m.request("synthetic-id", 0, 0, 0.0).unwrap(), None);
    assert!(entered.wait(Duration::from_secs(2)));
    assert_eq!(m.request("synthetic-id", 0, 0, 0.0).unwrap(), None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    released.set();
    let until = Instant::now() + Duration::from_secs(2);
    let mut result = None;
    while result.is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
        result = m.request("synthetic-id", 0, 0, 0.0).unwrap();
    }
    assert_eq!(result.as_deref(), Some(image.as_slice()));
    use std::os::unix::fs::PermissionsExt;
    let path = m.path(&entry.candidates[&0]).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    m.close();
    m.join(Duration::from_secs(2));
    let e3 = entry.clone();
    let restarted = manager(dir.path(), env, PreviewOptions {
        decoder: Some(Arc::new(|_: &[u8], _| panic!("cached page must not decode again"))),
        fetcher: Some(Arc::new(|_, _, _| panic!("cached page must not download again"))),
        entry_hook: Some(Arc::new(move |_| Ok(e3.clone()))),
        ..Default::default()
    });
    assert_eq!(restarted.request("synthetic-id", 0, 0, 0.0).unwrap(), Some(image));
    restarted.close();
}

#[test]
fn corrupted_idat_and_bad_chunk_orders_are_not_valid_previews() {
    let mut image = png(1, 1, b"\x12\x34\x56\xff");
    assert!(notes_preview::png_valid(&image, 1, 1));
    let mut pos = 8;
    while &image[pos + 4..pos + 8] != b"IDAT" {
        pos += 12 + u32::from_be_bytes(image[pos..pos + 4].try_into().unwrap()) as usize;
    }
    image[pos + 8] ^= 1;
    assert!(!notes_preview::png_valid(&image, 1, 1));
    fn chunk(kind: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = (payload.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out.extend_from_slice(&rs_render::pixels::crc32(&[kind, payload]).to_be_bytes());
        out
    }
    let signature = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let header = chunk(b"IHDR", &ihdr);
    let data = chunk(b"IDAT", &rs_render::pixels::deflate6(b"\0\x12\x34\x56\xff"));
    let end = chunk(b"IEND", b"");
    let cat = |parts: &[&Vec<u8>]| parts.iter().flat_map(|p| p.iter().copied()).collect::<Vec<u8>>();
    assert!(notes_preview::png_valid(&cat(&[&signature, &header, &data, &end]), 1, 1));
    for invalid in [cat(&[&signature, &header, &end]), cat(&[&signature, &header, &header, &data, &end]),
                    cat(&[&signature, &data, &header, &end]), cat(&[&signature, &header, &data, &end, &data])] {
        assert!(!notes_preview::png_valid(&invalid, 1, 1));
    }
}

#[test]
fn source_changed_during_decode_is_not_published() {
    let dir = TempDir::new();
    let env = TestEnv::new(Some(dir.path()), false);
    let (entered, released) = (Event::new(), Event::new());
    let image = png(1, 1, b"\x12\x34\x56\xff");
    let entry = entry_of(src(&["synthetic-id", "before-commit"]), Candidate::Canvas(candidate(b"synthetic-spi")));
    let current = Arc::new(Mutex::new(entry.source.clone()));
    let (en, re, p, e1, cur) = (entered.clone(), released.clone(), image.clone(), entry.clone(), current.clone());
    let m = manager(dir.path(), env, PreviewOptions {
        decoder: Some(Arc::new(move |_: &[u8], _| { en.set(); assert!(re.wait(Duration::from_secs(2))); Ok(p.clone()) })),
        fetcher: Some(Arc::new(|_, _, _| Ok(b"synthetic-spi".to_vec()))),
        entry_hook: Some(Arc::new(move |_| Ok(e1.clone()))),
        source_hook: Some(Arc::new(move |_| Ok(cur.lock().unwrap().clone()))),
        ..Default::default()
    });
    let path = m.path(&entry.candidates[&0]).unwrap();
    assert_eq!(m.request("synthetic-id", 0, 0, 0.0).unwrap(), None);
    assert!(entered.wait(Duration::from_secs(2)));
    *current.lock().unwrap() = src(&["synthetic-id", "after-commit"]);
    released.set();
    assert!(m.wait_idle(Some(Duration::from_secs(5))));
    assert!(!path.exists(), "old source must not be published after decode");
    let partial = std::fs::read_dir(m.directory()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".partial-")).count();
    assert_eq!(partial, 0);
    let job = m.job(&path.file_name().unwrap().to_string_lossy()).unwrap();
    assert_eq!(job.state, JobState::Error);
    assert_eq!(job.error.as_deref(), Some("Not veya hesap değişti. Sayfayı yeniden açabilirsin."));
    // Errors are reported (422) until retry_after.
    let error = m.request("synthetic-id", 0, 0, 0.0).unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("preview_unavailable", 422));
    m.close();
    m.join(Duration::from_secs(2));
}

#[test]
fn disconnect_during_broker_stops_followup_file_get() {
    let dir = TempDir::new();
    let env = TestEnv::new(Some(dir.path()), false);
    let (entered, released) = (Event::new(), Event::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let raw = b"synthetic-spi".to_vec();
    let entry = entry_of(src(&["synthetic-id", "commit"]), Candidate::Canvas(candidate(&raw)));
    let (en, re, c) = (entered.clone(), released.clone(), calls.clone());
    *env.http.lock().unwrap() = Some(Box::new(move |_, _| {
        if c.fetch_add(1, Ordering::SeqCst) == 0 {
            en.set();
            assert!(re.wait(Duration::from_secs(2)));
            return Ok((302, vec![], Some("https://eu1rgn-down.file.samsungcloud.com/file?signature=synthetic".to_string())));
        }
        Ok((200, raw.clone(), None))
    }));
    let (e1, e2) = (entry.clone(), entry.clone());
    let image = png(1, 1, b"\x12\x34\x56\xff");
    let m = manager(dir.path(), env.clone(), PreviewOptions {
        decoder: Some(Arc::new(move |_: &[u8], _| Ok(image.clone()))),
        entry_hook: Some(Arc::new(move |_| Ok(e1.clone()))),
        source_hook: Some(Arc::new(move |_| Ok(e2.source.clone()))),
        ..Default::default()
    });
    assert_eq!(m.request("synthetic-id", 0, 0, 0.0).unwrap(), None);
    assert!(entered.wait(Duration::from_secs(2)));
    env.set_session(None); // user disconnects while the broker GET completes
    released.set();
    assert!(m.wait_idle(Some(Duration::from_secs(5))));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "disconnect must stop the next network request");
    m.close();
    m.join(Duration::from_secs(2));
}

fn scheduling(dir: &std::path::Path, names: &[&str], workers: usize,
              decode: Arc<dyn Fn(&[u8], (i64, i64)) -> Result<Vec<u8>, Fail> + Send + Sync>) -> PreviewManager {
    let env = TestEnv::new(Some(dir), false);
    let entries: Arc<BTreeMap<String, Entry>> = Arc::new(names.iter().map(|n| {
        (n.to_string(), entry_of(src(&[n, "commit"]), Candidate::Canvas(candidate(n.as_bytes()))))
    }).collect());
    let (a, b, c) = (entries.clone(), entries.clone(), entries.clone());
    manager(dir, env, PreviewOptions {
        decoder: Some(decode),
        fetcher: Some(Arc::new(move |item, _, _| {
            Ok(c.iter().find(|(_, e)| matches!(&e.candidates[&0], Candidate::Canvas(x) if x == item)).unwrap().0.as_bytes().to_vec())
        })),
        entry_hook: Some(Arc::new(move |id| Ok(a[id].clone()))),
        source_hook: Some(Arc::new(move |id| Ok(b[id].source.clone()))),
        workers: Some(workers),
        ..Default::default()
    })
}

#[test]
fn opened_page_is_decoded_before_queued_thumbnails() {
    let dir = TempDir::new();
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let (release, first) = (Event::new(), Event::new());
    let image = png(1, 1, b"\x12\x34\x56\xff");
    let (o, r, f) = (order.clone(), release.clone(), first.clone());
    let m = scheduling(dir.path(), &["thumb-a", "thumb-b", "thumb-c", "opened", "warm"], 1, Arc::new(move |raw: &[u8], _| {
        o.lock().unwrap().push(String::from_utf8(raw.to_vec()).unwrap());
        if raw == b"thumb-a" {
            f.set();
            assert!(r.wait(Duration::from_secs(2)));
        }
        Ok(image.clone())
    }));
    m.request("thumb-a", 0, 1, 0.0).unwrap();
    assert!(first.wait(Duration::from_secs(2)));
    m.request("warm", 0, 2, 0.0).unwrap();
    m.request("thumb-b", 0, 1, 0.0).unwrap();
    m.request("thumb-c", 0, 1, 0.0).unwrap();
    m.request("opened", 0, 0, 0.0).unwrap();
    m.request("warm", 0, 0, 0.0).unwrap(); // the user opened the warm-up note too
    release.set();
    assert!(m.wait_idle(Some(Duration::from_secs(5))));
    assert_eq!(*order.lock().unwrap(), ["thumb-a", "opened", "warm", "thumb-b", "thumb-c"]);
    m.close();
    m.join(Duration::from_secs(2));
}

#[test]
fn waiting_request_returns_the_page_when_it_finishes() {
    let dir = TempDir::new();
    let image = png(1, 1, b"\x12\x34\x56\xff");
    let p = image.clone();
    let m = scheduling(dir.path(), &["note"], 2, Arc::new(move |_: &[u8], _| {
        std::thread::sleep(Duration::from_millis(200));
        Ok(p.clone())
    }));
    let started = Instant::now();
    assert_eq!(m.request("note", 0, 0, 3.0).unwrap(), Some(image));
    assert!(started.elapsed() < Duration::from_secs(2));
    m.close();
    m.join(Duration::from_secs(2));
}

#[test]
fn several_pages_decode_at_the_same_time() {
    let dir = TempDir::new();
    let barrier = Arc::new(Barrier::new(3));
    let image = png(1, 1, b"\x12\x34\x56\xff");
    let (b, p) = (barrier.clone(), image.clone());
    let m = scheduling(dir.path(), &["a", "b", "c"], 3, Arc::new(move |_: &[u8], _| {
        b.wait();
        Ok(p.clone())
    }));
    for name in ["a", "b", "c"] {
        m.request(name, 0, 0, 0.0).unwrap();
    }
    assert!(m.wait_idle(Some(Duration::from_secs(5))));
    for name in ["a", "b", "c"] {
        assert_eq!(m.request(name, 0, 0, 0.0).unwrap().as_deref(), Some(image.as_slice()));
    }
    m.close();
    m.join(Duration::from_secs(2));
}

#[test]
fn offline_canvas_without_cache_is_503_and_missing_page_is_404() {
    let dir = TempDir::new();
    let env = TestEnv::new(None, true);
    let entry = entry_of(src(&["n", "c"]), Candidate::Canvas(candidate(b"x")));
    let e1 = entry.clone();
    let m = manager(dir.path(), env, PreviewOptions { entry_hook: Some(Arc::new(move |_| Ok(e1.clone()))), ..Default::default() });
    let error = m.request("n", 0, 0, 0.0).unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("preview_offline", 503));
    let error = m.request("n", 1, 0, 0.0).unwrap_err();
    assert_eq!((error.code.as_str(), error.status, error.message.as_str()),
               ("preview_unavailable", 404, "Bu sayfa için görüntü önizlemesi bulunmuyor."));
}

#[test]
fn unknown_note_source_is_not_found() {
    let dir = TempDir::new();
    let m = PreviewManager::new(store(dir.path()), TestEnv::new(None, true)).unwrap();
    let error = m.manifest("missing").unwrap_err();
    assert_eq!((error.code.as_str(), error.status, error.message.as_str()), ("not_found", 404, "Not bulunamadı."));
    // A plain text note has no pages and a provider-dependent flag.
    let s = store(dir.path());
    let note = s.create(&Val::s("Synthetic"), &Val::s("Text")).unwrap();
    let manifest = m.manifest(note.get("id").unwrap().as_str().unwrap()).unwrap();
    assert_eq!(manifest.get("pages"), Some(&Val::List(vec![])));
}
