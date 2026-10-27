//! Shared synthetic fixtures (no network, temporary directories only).
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use notes_engine::cloud::{Cloud, CloudFactory, Metadata};
use notes_engine::engine::{Engine, Options};
use notes_engine::sessions::NoLogin;
use notes_engine::stub_previews::NoPreviewFactory;
use notes_engine::util::R;
use notes_host::snapshot::{self, Snapshot};
use notes_host::{Fail, Val};

pub const NOW: i128 = 1700000000123;
pub const LOCAL_ID: &str = "00000000-0000-4000-8000-000000000001";

pub struct TempDir(pub PathBuf);
impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!("notes-engine-test-{}-{}-{}", tag, std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // tempfile.TemporaryDirectory() is private (0700).
        std::os::unix::fs::PermissionsExt::set_mode(&mut std::fs::metadata(&base).unwrap().permissions(), 0o700);
        std::fs::set_permissions(&base, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        TempDir(base)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// tests/test_engine_review.py `document()`
pub fn document(body: &str, commit: &str, server: &str) -> Snapshot {
    let (_, raw) = rs_doc::doc::new_document("Title", body, Some(LOCAL_ID), rs_doc::doc::NowArg::Int(NOW), rs_doc::doc::MAX_TEXT).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let inner = match text.find("?>") {
        Some(at) if text.starts_with("<?xml") => text[at + 2..].trim_start().to_string(),
        _ => text,
    };
    let wrapped = format!("<CompareNoteView>{}<commitId>{}</commitId></CompareNoteView>", inner, commit);
    snapshot::parse_document(wrapped.as_bytes(), "application/xml", Some(server)).unwrap()
}

/// `ReviewCloud` from tests/test_engine_review.py (plus write outcomes).
pub struct ReviewCloud {
    pub current: Mutex<(String, String)>,
    pub reads: Mutex<Vec<String>>,
    pub fail_once: Mutex<bool>,
    pub disconnect_on_read: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    pub change_count: Mutex<usize>,
    pub writes: AtomicUsize,
    /// Outcome of the next `update`: None = verified write.
    pub update_error: Mutex<Option<Fail>>,
}

impl ReviewCloud {
    pub fn new() -> Arc<ReviewCloud> {
        Arc::new(ReviewCloud {
            current: Mutex::new(("A".into(), "commit-1".into())),
            reads: Mutex::new(vec![]),
            fail_once: Mutex::new(false),
            disconnect_on_read: Mutex::new(None),
            change_count: Mutex::new(1),
            writes: AtomicUsize::new(0),
            update_error: Mutex::new(None),
        })
    }
}

fn unsupported() -> Fail {
    Fail::api("unsupported", "not used by these tests")
}

impl Cloud for ReviewCloud {
    fn auth_status(&self) -> R<Val> {
        Ok(Val::dict(vec![("valid", Val::Bool(true)), ("renewable", Val::Bool(true))]))
    }
    fn initial_cursor(&self) -> Val {
        Val::s("initial")
    }
    fn changes(&self, cursor: &Val) -> R<Val> {
        let commit = self.current.lock().unwrap().1.clone();
        let changes = if cursor.as_str() == Some("after-page") {
            vec![]
        } else {
            (0..*self.change_count.lock().unwrap())
                .map(|i| {
                    Val::dict(vec![
                        ("server_id", Val::s(format!("server-{}", i + 1))),
                        ("resource_id", Val::s(LOCAL_ID)),
                        ("commit_id", Val::s(commit.clone())),
                        ("is_deleted", Val::Bool(false)),
                        ("is_locked", Val::Bool(false)),
                        ("is_encrypted", Val::Bool(false)),
                        ("sync_modified_time", Val::Int(NOW as i64)),
                    ])
                })
                .collect()
        };
        Ok(Val::dict(vec![("changes", Val::List(changes)), ("has_next", Val::Bool(false)), ("last_change_point", Val::s("after-page"))]))
    }
    fn get_note(&self, server_id: &Val) -> R<Snapshot> {
        let server = server_id.as_str().unwrap().to_string();
        self.reads.lock().unwrap().push(server.clone());
        if let Some(f) = self.disconnect_on_read.lock().unwrap().as_ref() {
            f();
        }
        {
            let mut fail = self.fail_once.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(Fail::api("unsupported_payload", "Synthetic temporary incomplete payload"));
            }
        }
        let (body, commit) = self.current.lock().unwrap().clone();
        Ok(document(&body, &commit, &server))
    }
    fn headers(&self) -> R<Vec<(String, String)>> {
        Ok(vec![])
    }
    fn create(&self, _: &Val, _: &Val, _: &Val, _: &Val) -> R<Val> {
        Err(unsupported())
    }
    fn create_editor(&self, _: &Val, _: &Val, _: &Val, _: &Val) -> R<Val> {
        Err(unsupported())
    }
    fn update(&self, server_id: &Val, _snapshot: &Snapshot, _title: &Val, body: &Val, _operation_id: &Val) -> R<Val> {
        if let Some(e) = self.update_error.lock().unwrap().take() {
            return Err(e);
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        let fresh = document(body.as_str().unwrap(), "commit-2", server_id.as_str().unwrap());
        Ok(Val::dict(vec![
            ("verified", Val::Bool(true)),
            ("server_id", server_id.clone()),
            ("local_id", Val::s(LOCAL_ID)),
            ("commit_id", Val::s("commit-2")),
            ("snapshot", snapshot::to_dict(&fresh)),
        ]))
    }
    fn update_title(&self, _: &Val, _: &Snapshot, _: &Val, _: &Val) -> R<Val> {
        Err(unsupported())
    }
    fn update_editor(&self, _: &Val, _: &Snapshot, _: &Val, _: &Val, _: &Val) -> R<Val> {
        Err(unsupported())
    }
    fn update_page_background(&self, _: &Val, _: &Snapshot, _: &Val, _: &Val) -> R<Val> {
        Err(unsupported())
    }
    fn add_pdf_pages(&self, _: &Val, _: &Snapshot, _: &[u8], _: &Val, _: &Val, _: &Val, _: &Val, _: &Val) -> R<Val> {
        Err(unsupported())
    }
    fn append_ink_stroke(&self, _: i64, _: &Val, _: &Snapshot, _: &Val, _: &Val, _: &Val, _: &str, _: Option<(&Val, &Val)>) -> R<Val> {
        Err(unsupported())
    }
    fn append_first_stroke(&self, _: &Val, _: &Snapshot, _: &Val, _: &Val, _: &Val, _: &Val, _: &Val, _: &str) -> R<Val> {
        Err(unsupported())
    }
    fn erase_ink_strokes(&self, _: &Val, _: &Snapshot, _: &Val, _: &Val, _: &Val, _: &str) -> R<Val> {
        Err(unsupported())
    }
    fn move_ink_strokes(&self, _: &Val, _: &Snapshot, _: &Val, _: &Val, _: &Val, _: &Val, _: &Val, _: &str) -> R<Val> {
        Err(unsupported())
    }
    fn page_op(&self, _: &str, _: &Val, _: &Snapshot, _: &Val, _: &Val) -> R<Val> {
        Err(unsupported())
    }
}

pub struct FixedFactory(pub Arc<ReviewCloud>);
impl CloudFactory for FixedFactory {
    fn open(&self, _session: &Path, _work_dir: &Path) -> R<Arc<dyn Cloud>> {
        Ok(self.0.clone() as Arc<dyn Cloud>)
    }
    fn metadata(&self, _api: &Arc<dyn Cloud>) -> Option<Arc<dyn Metadata>> {
        None
    }
}

/// `Engine(root/'app', offline=True, auto_sync=False, interval=3600,
/// api_factory=lambda *a: api, metadata_factory=None)` then switched online
/// with a synthetic session path, like EngineReviewTests.setUp.
pub fn review_engine(root: &Path, api: &Arc<ReviewCloud>) -> Arc<Engine> {
    let engine = Engine::new(
        &root.join("app"),
        Options {
            offline: true,
            session: None,
            auto_sync: false,
            interval: 3600.0,
            cloud: Arc::new(FixedFactory(api.clone())),
            previews: Arc::new(NoPreviewFactory),
            login: Arc::new(NoLogin),
        },
    )
    .unwrap();
    engine.sessions.set_offline(false);
    engine.sessions.set_path(Some(root.join("synthetic-session")));
    engine.apis.api().unwrap();
    engine
}

pub fn offline_engine(root: &Path) -> Arc<Engine> {
    Engine::new(root, notes_engine::launcher::engine_options(true, None, true)).unwrap()
}
