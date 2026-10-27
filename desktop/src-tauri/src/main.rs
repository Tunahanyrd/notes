use std::{fs, io, os::unix::fs::PermissionsExt, path::PathBuf, process::{Child, Command, Stdio}, sync::Mutex, time::{Duration, Instant}};
use serde::Deserialize;
use std::os::unix::process::CommandExt;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Deserialize)]
struct Runtime { port: u16, url: String, token: String, pid: u32 }
struct Child_ { child: Child, _directory: tempfile::TempDir }
impl Drop for Child_ {
    fn drop(&mut self) { if matches!(self.child.try_wait(), Ok(None)) { let _ = self.child.kill(); } let _ = self.child.wait(); }
}
/// The engine serving the UI: a `notes-engine` child process or the pure
/// engine running in this process.
enum Engine {
    #[allow(dead_code)] // kept alive for its Drop (kills the child)
    Process(Child_),
    #[cfg(embedded_engine)]
    InProcess(Option<notes_engine::launcher::Running>),
}
impl Drop for Engine {
    fn drop(&mut self) {
        #[cfg(embedded_engine)]
        if let Engine::InProcess(running) = self { if let Some(r) = running.take() { r.stop(); } }
    }
}
#[cfg(embedded_engine)]
fn launch_embedded(root: &std::path::Path) -> Result<(Engine, tauri::Url), Box<dyn std::error::Error>> {
    // Linux: $XDG_STATE_HOME or ~/.local/state; macOS: ~/Library/Application
    // Support/org.linuxnotes.desktop (an explicit XDG_STATE_HOME still wins).
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let data_dir = std::env::var_os("LINUX_NOTES_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| notes_platform::dirs::library_dir(&home));
    let frontend = std::env::var_os("LINUX_NOTES_FRONTEND_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| root.join("desktop/dist"));
    let running = notes_engine::launcher::start(notes_engine::launcher::StartOptions {
        data_dir, port: 0, offline: std::env::var_os("LINUX_NOTES_OFFLINE").is_some(), auto_sync: true,
        session: None, frontend_dir: frontend, font_dir: None,
    }).map_err(io::Error::other)?;
    let url = tauri::Url::parse(&running.url())?;
    Ok((Engine::InProcess(Some(running)), url))
}
#[cfg(not(embedded_engine))]
fn launch_embedded(_root: &std::path::Path) -> Result<(Engine, tauri::Url), Box<dyn std::error::Error>> {
    Err(io::Error::other("Bu derleme gömülü not hizmeti içermiyor (embedded-engine).").into())
}
/// Which engine serves the UI; both are the pure-Rust engine (there is no
/// Python engine any more). Linux default (LINUX_NOTES_ENGINE unset): the
/// `notes-engine` binary as a child process (`native`/`binary`);
/// `LINUX_NOTES_ENGINE=embedded` runs it in-process (needs the
/// `embedded-engine` feature). macOS always links the pure engine and
/// defaults to `embedded` (see build.rs `embedded_engine`).
#[derive(Clone, Copy, PartialEq)]
enum Mode { Binary, Embedded }
fn mode() -> Mode {
    // Release/Flatpak builds may bake their default with
    // LINUX_NOTES_DEFAULT_ENGINE=embedded|native at compile time. The runtime
    // variable wins; unknown values fall back to the platform default.
    let runtime = std::env::var("LINUX_NOTES_ENGINE").ok();
    match runtime.as_deref().or(option_env!("LINUX_NOTES_DEFAULT_ENGINE")) {
        Some("native") | Some("binary") => Mode::Binary,
        Some("embedded") => Mode::Embedded,
        _ if cfg!(target_os = "macos") => Mode::Embedded,
        _ => Mode::Binary,
    }
}
fn project_root(resources: Option<&std::path::Path>) -> PathBuf {
    // Root order: runtime override, else the root baked in by build.rs
    // (LINUX_NOTES_INSTALL_ROOT for packages, the source checkout otherwise),
    // else Tauri's resource directory when it holds the frontend (deb/rpm:
    // /usr/lib/<product>, AppImage: $APPDIR/usr/lib/<product>; see
    // packaging/linux/tauri.release.conf.json.in).
    if let Some(root) = std::env::var_os("LINUX_NOTES_PROJECT_ROOT") { return PathBuf::from(root); }
    // macOS .app: Contents/Resources (bundled desktop/dist, ink-render) wins
    // over the build checkout so a local bundle is tested as shipped.
    #[cfg(target_os = "macos")]
    if let Some(dir) = resources.filter(|d| d.join("desktop/dist/index.html").is_file()) { return dir.to_path_buf(); }
    let baked = PathBuf::from(env!("LINUX_NOTES_DEFAULT_ROOT"));
    if baked.join("desktop/dist/index.html").is_file() { return baked; }
    match resources { Some(dir) if dir.join("desktop/dist/index.html").is_file() => dir.to_path_buf(), _ => baked }
}
/// `notes-engine` binary: LINUX_NOTES_ENGINE_BIN, next to this executable,
/// `<root>/bin`, `<root>/target/{release,debug}`, then PATH.
fn engine_binary(root: &std::path::Path) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LINUX_NOTES_ENGINE_BIN") { return Some(PathBuf::from(path)); }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() { if let Some(dir) = exe.parent() { candidates.push(dir.join("notes-engine")); } }
    for dir in ["bin", "target/release", "target/debug"] { candidates.push(root.join(dir).join("notes-engine")); }
    if let Some(path) = std::env::var_os("PATH") { candidates.extend(std::env::split_paths(&path).map(|d| d.join("notes-engine"))); }
    candidates.into_iter().find(|p| fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
}
fn launch(resources: Option<&std::path::Path>) -> Result<(Engine, tauri::Url), Box<dyn std::error::Error>> {
    let root = project_root(resources);
    let mode = mode();
    if !root.join("desktop/dist/index.html").is_file() {
        return Err(io::Error::other("Uygulama kaynakları bulunamadı. Proje dizinini ve ön yüz derlemesini kontrol edin.").into());
    }
    if mode == Mode::Embedded {
        // The in-process engine resolves its resources (ink renderer) from
        // the same root the child engines receive in their environment.
        if std::env::var_os("LINUX_NOTES_PROJECT_ROOT").is_none() { std::env::set_var("LINUX_NOTES_PROJECT_ROOT", &root); }
        return launch_embedded(&root);
    }
    // Runtime file directory: $XDG_RUNTIME_DIR/samsung-notes-linux (0700) per
    // the XDG spec, else the temp dir (Linux /tmp, macOS per-user $TMPDIR).
    // tools/_xdg.py runtime_files() looks in both places.
    let builder = { let mut b = tempfile::Builder::new(); b.prefix("linux-notes-"); b };
    let directory = match notes_platform::dirs::runtime_dir() {
        Some(base) => builder.tempdir_in(base)?,
        None => builder.tempdir()?,
    };
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    let runtime_file = directory.path().join("runtime.json");
    let binary = engine_binary(&root).ok_or_else(|| io::Error::other("Yerel not hizmeti (notes-engine) bulunamadı."))?;
    let mut command = Command::new(binary);
    // Default frontend: <root>/desktop/dist.
    command.env("LINUX_NOTES_PROJECT_ROOT", &root);
    command.current_dir(&root)
        .args(["--port", "0", "--runtime-file"])
        .arg(&runtime_file).arg("--no-browser")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(path) = std::env::var_os("LINUX_NOTES_DATA_DIR") { command.arg("--data-dir").arg(path); }
    if std::env::var_os("LINUX_NOTES_OFFLINE").is_some() { command.arg("--offline"); }
    // Keep only our engine tied to this launcher, even if the GUI is terminated.
    let owner_pid = std::process::id();
    // SAFETY: the closure runs in the forked child of a multi-threaded process,
    // so it may only make async-signal-safe calls and must not allocate:
    // prctl/getppid are plain syscalls and `from_raw_os_error` builds the
    // error without a heap allocation (`io::Error::other` would box a String).
    unsafe {
        command.pre_exec(move || {
            // Linux: PR_SET_PDEATHSIG; macOS has none (the Drop of Child_ kills it).
            notes_platform::process::set_parent_death_signal()?;
            if libc::getppid() as u32 != owner_pid {
                // The launcher already exited.
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    let mut engine = Child_ { child, _directory: directory };
    let started = Instant::now();
    loop {
        if engine.child.try_wait()?.is_some() { return Err(io::Error::other("Yerel not hizmeti başlatılamadı.").into()); }
        if let Ok(metadata) = fs::symlink_metadata(&runtime_file) {
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.len() > 8192 {
                return Err(io::Error::other("Yerel hizmet oturum dosyası güvenli değil.").into());
            }
            if let Ok(value) = serde_json::from_slice::<Runtime>(&fs::read(&runtime_file)?) {
                let url = tauri::Url::parse(&value.url)?;
                let valid_token = value.token.len() >= 20 && value.token.len() <= 256 && value.token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                if !valid_token || value.pid != engine.child.id() || value.port == 0 || url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port() != Some(value.port) || !url.username().is_empty() || url.password().is_some() || url.path() != "/" || url.query().is_some() || url.fragment() != Some(format!("token={}", value.token).as_str()) {
                    return Err(io::Error::other("Yerel hizmet oturumu doğrulanamadı.").into());
                }
                return Ok((Engine::Process(engine), url));
            }
        }
        if started.elapsed() > Duration::from_secs(30) { return Err(io::Error::other("Yerel not hizmetinin başlatılması zaman aşımına uğradı.").into()); }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn main() {
    // Embedded engine: this executable is also the ms-app sign-in handler
    // (`__notes-login-callback`) when the pure engine signed in.
    #[cfg(embedded_engine)]
    if let Some(code) = notes_engine::run_hidden_subcommand() { std::process::exit(code); }
    // Packaged Linux builds with the sibling engine (AppImage: the ms-app
    // handler runs `$APPIMAGE __notes-login-callback <uri>`, i.e. this
    // shell): hand the callback to that notes-engine. The URI is never logged.
    #[cfg(not(embedded_engine))]
    if std::env::args_os().nth(1).is_some_and(|a| a == "__notes-login-callback") {
        let Some(engine) = engine_binary(&project_root(None)) else {
            eprintln!("Giriş dönüşü iletilemedi: notes-engine bulunamadı.");
            std::process::exit(1);
        };
        let error = Command::new(engine).args(std::env::args_os().skip(1)).exec();
        eprintln!("Giriş dönüşü iletilemedi: {}", error.kind());
        std::process::exit(1);
    }
    let context = tauri::generate_context!();
    let app = tauri::Builder::default().setup(|app| {
        let resources = app.path().resource_dir().ok();
        let (engine, url) = launch(resources.as_deref())?;
        let origin = url.origin();
        app.manage(Mutex::new(Some(engine)));
        WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
            .title("Notlar").inner_size(1320.0, 860.0).min_inner_size(760.0, 540.0)
            .on_navigation(move |next| next.origin() == origin)
            .build()?;
        Ok(())
    }).build(context).expect("Notlar başlatılamadı");
    app.run(|handle, event| {
        // macOS: LaunchServices delivers ms-app://... sign-in callbacks to the
        // running .app (Info.plist CFBundleURLTypes) as an Apple Event; hand
        // them to the engine's callback dispatch (Linux uses a desktop-entry
        // handler process instead). The URI is never logged.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = &event {
            for url in urls.iter().filter(|u| u.scheme() == notes_platform::url_scheme::SCHEME) {
                let uri = url.as_str().to_owned();
                std::thread::spawn(move || {
                    if let Err(message) = notes_engine::deliver_login_callback(&uri) { eprintln!("{message}"); }
                });
                if let Some(window) = handle.get_webview_window("main") { let _ = window.set_focus(); }
            }
        }
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(engine) = handle.try_state::<Mutex<Option<Engine>>>() { if let Ok(mut value) = engine.lock() { value.take(); } }
        }
    });
}
