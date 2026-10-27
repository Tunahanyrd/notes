//! Loopback HTTP server (`AppServer`): bind without SO_REUSEADDR, accept
//! loop, one thread per connection (`daemon_threads`), request handling in
//! `rs_engine::server_core` (identical wire behaviour to the Python server).
use std::net::{TcpListener, ToSocketAddrs};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rs_engine::server_core::{self, Config};

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
    backend: RustBackend,
    listener: Mutex<Option<TcpListener>>,
    stop: AtomicBool,
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
        let listener = bind_ipv4("127.0.0.1", port)?;
        let port = listener.local_addr()?.port();
        let cfg = Config {
            token: token.to_string(),
            port,
            static_dir: server_core::realpath(static_dir),
            font_dir: font_dir.unwrap_or_else(|| crate::sessions::research_dir().join("ui-reference/fonts")),
        };
        Ok(AppServer {
            shared: Arc::new(Shared { cfg, backend: RustBackend { engine }, listener: Mutex::new(Some(listener)), stop: AtomicBool::new(false) }),
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
                        .spawn(move || server_core::serve_connection(&shared.cfg, &shared.backend, stream));
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

    pub fn engine(&self) -> &Arc<Engine> {
        &self.shared.backend.engine
    }
}
