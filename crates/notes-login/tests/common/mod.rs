//! Shared offline test fixtures: fixed clock, deterministic randomness,
//! scripted network, temp HOME/XDG directories and fake desktop helpers.
#![allow(dead_code)]
use notes_login::http::{HttpRequest, HttpResponse, NetError, Network};
use notes_login::json::Value;
use notes_login::runtime::{Layout, Runtime};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub struct FakeRuntime {
    pub now: Mutex<f64>,
    pub calls: AtomicU64,
}

impl FakeRuntime {
    pub fn new(now: f64) -> FakeRuntime {
        FakeRuntime { now: Mutex::new(now), calls: AtomicU64::new(0) }
    }
    pub fn set(&self, now: f64) {
        *self.now.lock().unwrap() = now;
    }
}

impl Runtime for FakeRuntime {
    fn now(&self) -> f64 {
        *self.now.lock().unwrap()
    }
    /// Same sequence as tests/py/harness.py `token_bytes`.
    fn fill_random(&self, buf: &mut [u8]) {
        let k = self.calls.fetch_add(1, Ordering::SeqCst);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((k as usize * 37 + i * 11 + 5) & 0xff) as u8;
        }
    }
}

#[derive(Clone, Debug)]
pub enum Canned {
    Response { status: u16, headers: Vec<(String, String)>, body: Vec<u8> },
    TransportError,
}

pub fn json_response(status: u16, body: &str) -> Canned {
    Canned::Response { status, headers: vec![("Content-Type".into(), "application/json".into())], body: body.as_bytes().to_vec() }
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub request: HttpRequest,
    pub timeout: Duration,
    pub read_limit: usize,
}

#[derive(Default)]
pub struct FakeNet {
    pub queue: Mutex<VecDeque<Canned>>,
    pub log: Mutex<Vec<Recorded>>,
}

impl FakeNet {
    pub fn new(items: Vec<Canned>) -> FakeNet {
        FakeNet { queue: Mutex::new(items.into()), log: Mutex::new(Vec::new()) }
    }
    pub fn push(&self, item: Canned) {
        self.queue.lock().unwrap().push_back(item);
    }
    pub fn count(&self) -> usize {
        self.log.lock().unwrap().len()
    }
    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }
}

impl Network for FakeNet {
    fn request(&self, request: &HttpRequest, timeout: Duration, read_limit: usize) -> Result<HttpResponse, NetError> {
        assert!(request.url.starts_with("https://"), "only HTTPS is ever requested");
        self.log.lock().unwrap().push(Recorded { request: request.clone(), timeout, read_limit });
        match self.queue.lock().unwrap().pop_front().expect("unexpected extra network request") {
            Canned::TransportError => Err(NetError::new("synthetic transport failure")),
            Canned::Response { status, headers, mut body } => {
                body.truncate(read_limit);
                Ok(HttpResponse { status, headers, body })
            }
        }
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh directory under cargo's per-test temp dir (never the user's home).
pub fn temp_dir(tag: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("notes-login-tests");
    std::fs::create_dir_all(&base).unwrap();
    let dir = base.join(format!("{tag}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const XDG_MIME: &str = r#"#!/bin/sh
echo "$*" >> "$HOME/.fake-xdg-mime.log"
store="$HOME/.fake-xdg-mime-default"
if [ "$1" = query ]; then
  if [ -f "$store" ]; then IFS= read -r line < "$store" || [ -n "$line" ]; printf '%s\n' "$line"; fi
  exit 0
fi
if [ "$1" = default ]; then printf '%s\n' "$2" > "$store"; exit 0; fi
exit 1
"#;

fn logger(name: &str) -> String {
    format!("#!/bin/sh\necho \"$*\" >> \"$HOME/.fake-{name}.log\"\nexit 0\n")
}

/// Temp HOME with XDG dirs and a PATH holding only fake helpers.
pub struct Sandbox {
    pub root: PathBuf,
    pub home: PathBuf,
    pub bin: PathBuf,
    pub layout: Layout,
}

impl Sandbox {
    pub fn new(tag: &str) -> Sandbox {
        Sandbox::at(&temp_dir(tag))
    }

    pub fn at(root: &Path) -> Sandbox {
        let home = root.join("home");
        let bin = root.join("bin");
        for d in [&home, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        for (name, body) in [
            ("xdg-mime", XDG_MIME.to_string()),
            ("xdg-open", logger("xdg-open")),
            // macOS opener (notes_platform::open::OPENER) logs to the same file.
            ("open", logger("xdg-open")),
            ("desktop-file-validate", logger("desktop-file-validate")),
            ("update-desktop-database", logger("update-desktop-database")),
        ] {
            let p = bin.join(name);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let state_home = home.join(".local/state");
        let data_home = home.join(".local/share");
        let config_home = home.join(".config");
        let layout = Layout {
            state: state_home.join("samsung-notes-research"),
            data_home: data_home.clone(),
            config_home: config_home.clone(),
            command_env: vec![
                ("PATH".into(), bin.clone().into_os_string()),
                ("HOME".into(), home.clone().into_os_string()),
                ("XDG_STATE_HOME".into(), state_home.into_os_string()),
                ("XDG_DATA_HOME".into(), data_home.into_os_string()),
                ("XDG_CONFIG_HOME".into(), config_home.into_os_string()),
            ],
        };
        Sandbox { root: root.to_path_buf(), home, bin, layout }
    }

    pub fn env(&self) -> Vec<(OsString, OsString)> {
        self.layout.command_env.clone()
    }

    pub fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.home.join(format!(".fake-{name}.log"))).unwrap_or_default()
    }

    pub fn add_notify_send(&self) {
        let p = self.bin.join("notify-send");
        std::fs::write(&p, logger("notify-send")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

pub fn obj(items: Vec<(&str, Value)>) -> Value {
    Value::obj(items)
}

pub fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every file below `root`: (relative path, mode, bytes), sorted.
pub fn tree(root: &Path) -> Vec<(String, u32, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, u32, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                out.push((rel + "/", meta.permissions().mode() & 0o777, Vec::new()));
                walk(base, &p, out);
            } else {
                out.push((rel, meta.permissions().mode() & 0o777, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let p = entry.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_tree(&p, &dest);
            std::fs::set_permissions(&dest, std::fs::metadata(&p).unwrap().permissions()).unwrap();
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// Repo root (crates/notes-login/../..).
pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

pub const ENTRYPOINT_FIXTURE: &[u8] = include_bytes!("../../../notes-protocol/tests/fixtures/signin/entrypoint.json");
