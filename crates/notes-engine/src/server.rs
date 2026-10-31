//! Loopback HTTP server (`AppServer`): bind without SO_REUSEADDR, accept
//! loop, one thread per connection (`daemon_threads`), request handling in
//! `rs_engine::server_core` (identical wire behaviour to the Python server).
use std::net::{TcpListener, ToSocketAddrs};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rs_engine::server_core::{self, Backend, Call, Config, Failure, Target};

use crate::backend::RustBackend;
use crate::engine::Engine;

/// Bind like `TCPServer` with `allow_reuse_address = False`.
pub fn bind_ipv4(host: &str, port: u16) -> std::io::Result<TcpListener> {
    let address = (host, port)
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "Address family for hostname not supported"))?;
    let std::net::SocketAddr::V4(v4) = address else { unreachable!() };
    notes_platform::net::bind_tcp_v4(v4, 5)
}

struct Shared {
    cfg: Config,
    backend: Arc<dyn Backend>,
    listener: Mutex<Option<TcpListener>>,
    stop: AtomicBool,
}

enum StartupState {
    Migrating { completed: u64, total: u64 },
    Ready(Arc<Engine>),
    Failed,
}

/// Allows the loopback server to answer status while storage is being prepared.
pub struct StartupBackend { state: Mutex<StartupState> }
impl StartupBackend {
    pub fn new() -> Self { Self { state: Mutex::new(StartupState::Migrating { completed: 0, total: 0 }) } }
    pub fn progress(&self, completed: u64, total: u64) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, StartupState::Migrating { .. }) { *state = StartupState::Migrating { completed: completed.min(total), total }; }
    }
    pub fn ready(&self, engine: Arc<Engine>) { *self.state.lock().unwrap() = StartupState::Ready(engine); }
    pub fn fail(&self) { *self.state.lock().unwrap() = StartupState::Failed; }
    pub fn engine(&self) -> Option<Arc<Engine>> {
        match &*self.state.lock().unwrap() { StartupState::Ready(engine) => Some(engine.clone()), _ => None }
    }
    fn backend(&self) -> Result<RustBackend, Failure> {
        self.engine().map(|engine| RustBackend { engine }).ok_or_else(|| Failure::store("migration_in_progress", "Notlar hazırlanıyor.", 503))
    }
}
impl Default for StartupBackend { fn default() -> Self { Self::new() } }
impl Backend for StartupBackend {
    fn json(&self, call: Call) -> Result<Vec<u8>, Failure> {
        if call.target == Target::Engine && call.method == "status" {
            match &*self.state.lock().unwrap() {
                StartupState::Migrating { completed, total } => return Ok(format!("{{\"migration\":{{\"state\":\"running\",\"completed\":{completed},\"total\":{total}}}}}").into_bytes()),
                StartupState::Failed => return Ok(b"{\"migration\":{\"state\":\"failed\",\"completed\":0,\"total\":0}}".to_vec()),
                StartupState::Ready(engine) => return RustBackend { engine: engine.clone() }.json(call),
            }
        }
        self.backend()?.json(call)
    }
    fn bytes_or_none(&self, call: Call) -> Result<Option<Vec<u8>>, Failure> { self.backend()?.bytes_or_none(call) }
    fn ink_layer(&self, call: Call) -> Result<Option<(Vec<u8>, String)>, Failure> { self.backend()?.ink_layer(call) }
    fn text_ink_layer(&self, call: Call) -> Result<Option<(Vec<u8>, String)>, Failure> { self.backend()?.text_ink_layer(call) }
    fn bytes_and_flag(&self, call: Call) -> Result<(Option<Vec<u8>>, bool), Failure> { self.backend()?.bytes_and_flag(call) }
    fn note_text(&self, call: Call) -> Result<(String, String), Failure> { self.backend()?.note_text(call) }
    fn max_pdf_bytes(&self) -> Result<u64, Failure> { Ok(crate::engine::MAX_PDF_BYTES) }
    fn is_word_char(&self, c: char) -> bool { crate::engine::is_word_char(c) }
}

/// A running loopback server.
#[derive(Clone)]
pub struct AppServer {
    shared: Arc<Shared>,
    pub port: u16,
    pub token: String,
}

impl AppServer {
    /// `AppServer(('127.0.0.1', port), engine, token, static_dir, font_dir=)`
    pub fn bind(port: u16, engine: Arc<Engine>, token: &str, static_dir: &Path, font_dir: Option<PathBuf>) -> std::io::Result<AppServer> {
        Self::bind_backend(port, Arc::new(RustBackend { engine }), token, static_dir, font_dir)
    }

    pub fn bind_starting(port: u16, backend: Arc<StartupBackend>, token: &str, static_dir: &Path, font_dir: Option<PathBuf>) -> std::io::Result<AppServer> {
        Self::bind_backend(port, backend, token, static_dir, font_dir)
    }

    fn bind_backend(port: u16, backend: Arc<dyn Backend>, token: &str, static_dir: &Path, font_dir: Option<PathBuf>) -> std::io::Result<AppServer> {
        let listener = bind_ipv4("127.0.0.1", port)?;
        let port = listener.local_addr()?.port();
        let cfg = Config {
            token: token.to_string(),
            port,
            static_dir: server_core::realpath(static_dir),
            font_dir: font_dir.unwrap_or_else(|| crate::sessions::research_dir().join("ui-reference/fonts")),
        };
        Ok(AppServer {
            shared: Arc::new(Shared { cfg, backend, listener: Mutex::new(Some(listener)), stop: AtomicBool::new(false) }),
            port,
            token: token.to_string(),
        })
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// `origin + '/#token=' + token`
    pub fn url(&self) -> String {
        format!("{}/#token={}", self.origin(), self.token)
    }

    /// `serve_forever(poll_interval)`: returns after `shutdown()`.
    pub fn serve_forever(&self, poll_interval: Duration) {
        loop {
            if self.shared.stop.load(Ordering::SeqCst) {
                return;
            }
            let fd = match self.shared.listener.lock().unwrap().as_ref() {
                Some(l) => l.as_raw_fd(),
                None => return,
            };
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: `pfd` is a valid, exclusively borrowed pollfd array of length 1. If the listener was closed concurrently, poll reports POLLNVAL and the loop exits on the next lock.
            let ready = unsafe { libc::poll(&mut pfd, 1, poll_interval.as_millis() as i32) };
            if self.shared.stop.load(Ordering::SeqCst) {
                return;
            }
            if ready > 0 {
                let accepted = match self.shared.listener.lock().unwrap().as_ref() {
                    Some(l) => l.accept(),
                    None => return,
                };
                if let Ok((stream, _)) = accepted {
                    let _ = stream.set_nonblocking(false);
                    let shared = Arc::clone(&self.shared);
                    let _ = std::thread::Builder::new()
                        .name("notes-http".into())
                        .stack_size(64 << 20)
                        .spawn(move || server_core::serve_connection(&shared.cfg, shared.backend.as_ref(), stream));
                }
            }
        }
    }

    /// Start `serve_forever` on a background thread.
    pub fn spawn(&self) -> std::thread::JoinHandle<()> {
        let me = self.clone();
        std::thread::Builder::new().name("notes-accept".into()).spawn(move || me.serve_forever(Duration::from_millis(200))).expect("spawn accept loop")
    }

    pub fn shutdown(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// `server_close()`
    pub fn close(&self) {
        self.shutdown();
        self.shared.listener.lock().unwrap().take();
    }

}
