//! Pure-Rust process boundary: the same resource limits, parent binding and
//! bwrap sandboxes the Python `*_worker` modules install, applied directly in
//! the forked child (no interpreter start per page).
//!
//! Platform split (see `notes_platform`): Linux keeps bubblewrap,
//! PDEATHSIG and CPU pinning; macOS uses [`pdf_args`]/[`ink_args`] with the
//! `notes_platform::isolation` prefix (reduced isolation, documented in
//! docs/MACOS_PORT.md), no PDEATHSIG and no pinning.
use std::ffi::CString;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Clone, Copy)]
pub struct Limits {
    pub address_space: u64,
    pub cpu_seconds: u64,
    /// None keeps the inherited RLIMIT_FSIZE.
    pub file_size: Option<u64>,
}

pub const PDF_LIMITS: Limits = Limits { address_space: 768 * 1024 * 1024, cpu_seconds: 30, file_size: Some(16 * 1024 * 1024) };
pub const INK_LIMITS: Limits = Limits { address_space: 1024 * 1024 * 1024, cpu_seconds: 25, file_size: Some(32 * 1024 * 1024) };

/// preview_worker._bind_parent: PDEATHSIG=SIGTERM tied to `expected_pid`.
pub fn bind_parent(expected_pid: i64) -> bool {
    // SAFETY: getppid is async-signal-safe, takes no arguments and cannot
    // fail; this runs in a pre_exec child, so nothing here allocates.
    unsafe {
        if expected_pid <= 1 || libc::getppid() as i64 != expected_pid {
            return false;
        }
        // Linux: PR_SET_PDEATHSIG. macOS has no equivalent (Ok(false)): the
        // owner's timeout/kill bounds the child instead.
        if notes_platform::process::set_parent_death_signal().is_err() {
            return false;
        }
        libc::getppid() as i64 == expected_pid
    }
}

/// resource.setrlimit calls of the workers (AS, CPU, CORE=0, optional FSIZE).
/// On macOS the address-space limit is best effort (not enforced by XNU).
pub fn apply_limits(limits: &Limits, core_zero: bool) -> io::Result<()> {
    use notes_platform::process::{set_limit, Limit};
    set_limit(Limit::AddressSpace, limits.address_space)?;
    set_limit(Limit::Cpu, limits.cpu_seconds)?;
    if core_zero {
        set_limit(Limit::Core, 0)?;
    }
    if let Some(size) = limits.file_size {
        set_limit(Limit::FileSize, size)?;
    }
    Ok(())
}

/// os.sched_setaffinity(0, {min(os.sched_getaffinity(0))}); no-op on macOS.
pub fn pin_lowest_cpu() -> io::Result<()> {
    notes_platform::process::pin_lowest_cpu()
}

/// os.execv; only returns on failure.
pub fn execv(args: &[String]) -> io::Error {
    let owned: Vec<CString> = match args.iter().map(|a| CString::new(a.as_bytes())).collect() {
        Ok(v) => v,
        Err(_) => return io::Error::from(io::ErrorKind::InvalidInput),
    };
    if owned.is_empty() {
        return io::Error::from(io::ErrorKind::InvalidInput);
    }
    let mut pointers: Vec<*const libc::c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    pointers.push(std::ptr::null());
    // SAFETY: `pointers` is a NULL-terminated array of pointers into `owned`
    // (NUL-terminated CStrings), all alive across the call; the first entry
    // is non-null because `owned` is not empty. execv returns only on error.
    unsafe { libc::execv(pointers[0], pointers.as_ptr()) };
    io::Error::last_os_error()
}

/// Poppler argv for one page in the current [`mode`]: the bwrap sandbox
/// (default, unchanged) or the rlimited direct exec of [`Mode::Direct`].
pub fn pdf_bwrap_args(source: &str, output: &str, page: u32, width: u32, height: u32) -> Vec<String> {
    pdf_args_for(mode(), source, output, page, width, height)
}

pub fn pdf_args_for(mode: Mode, source: &str, output: &str, page: u32, width: u32, height: u32) -> Vec<String> {
    if mode == Mode::Direct {
        let page = (page + 1).to_string();
        let output = format!("{}/page", output.trim_end_matches('/'));
        let mut args = direct_prefix();
        args.extend([
            tool_path_for(mode, "pdftoppm").to_string_lossy().into_owned(), "-q".into(), "-f".into(), page.clone(),
            "-l".into(), page, "-singlefile".into(), "-scale-to-x".into(), width.to_string(),
            "-scale-to-y".into(), height.to_string(), "-cropbox".into(), source.into(), output,
        ]);
        return args;
    }
    let page = (page + 1).to_string();
    [
        "/usr/bin/bwrap", "--unshare-all", "--die-with-parent", "--new-session",
        "--ro-bind", "/usr", "/usr", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib", "/lib64",
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--dir", "/etc",
        "--ro-bind", "/etc/fonts", "/etc/fonts", "--ro-bind", source, "/input.pdf",
        "--bind", output, "/output", "--chdir", "/tmp", "--clearenv",
        "--setenv", "HOME", "/tmp", "--setenv", "PATH", "/usr/bin", "--setenv", "LC_ALL", "C",
        "/usr/bin/pdftoppm", "-q", "-f", &page, "-l", &page, "-singlefile",
        "-scale-to-x", &width.to_string(), "-scale-to-y", &height.to_string(), "-cropbox", "/input.pdf", "/output/page",
    ].iter().map(|s| s.to_string()).collect()
}

/// Renderer/rasterizer argv in the current [`mode`] (see [`pdf_bwrap_args`]).
pub fn ink_bwrap_args(binary: &str, png: bool) -> Vec<String> {
    ink_args_for(mode(), binary, png)
}

pub fn ink_args_for(mode: Mode, binary: &str, png: bool) -> Vec<String> {
    if mode == Mode::Direct {
        let mut args = direct_prefix();
        args.push(binary.into());
        if png {
            args.push("--format=png".into());
        }
        return args;
    }
    let mut args: Vec<String> = [
        "/usr/bin/bwrap", "--unshare-all", "--die-with-parent", "--new-session",
        "--ro-bind", "/usr", "/usr", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib", "/lib64",
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--ro-bind", binary, "/renderer",
        "--chdir", "/tmp", "--clearenv", "--setenv", "PATH", "/usr/bin", "--setenv", "HOME", "/tmp",
        "--setenv", "LC_ALL", "C", "/renderer",
    ].iter().map(|s| s.to_string()).collect();
    if png {
        args.push("--format=png".into());
    }
    args
}

/// Whether the PDF page renderer can run: Linux needs `/usr/bin/bwrap` and
/// `/usr/bin/pdftoppm` (unchanged); macOS needs Poppler's `pdftoppm`
/// (Homebrew/MacPorts/PATH, see `notes_platform::tools`).
pub fn pdf_available() -> bool {
    #[cfg(not(target_os = "macos"))]
    {
        match mode() {
            Mode::Bwrap => Path::new("/usr/bin/bwrap").is_file() && Path::new("/usr/bin/pdftoppm").is_file(),
            Mode::Direct => tool_path_for(Mode::Direct, "pdftoppm").is_file(),
        }
    }
    #[cfg(target_os = "macos")]
    {
        notes_platform::tools::available(notes_platform::tools::Tool::Pdftoppm)
    }
}

/// Isolated `pdftoppm` for one page, writing `<output>/page.ppm`. Linux:
/// [`pdf_bwrap_args`]. macOS: `notes_platform::isolation` prefix (cleared
/// environment + Seatbelt: no network, writes only below `output`).
pub fn pdf_args(source: &str, output: &str, page: u32, width: u32, height: u32) -> Vec<String> {
    #[cfg(not(target_os = "macos"))]
    {
        pdf_bwrap_args(source, output, page, width, height)
    }
    #[cfg(target_os = "macos")]
    {
        let out = Path::new(output);
        let mut args = notes_platform::isolation::prefix(Some(out), out);
        let program = notes_platform::tools::program(notes_platform::tools::Tool::Pdftoppm);
        let page = (page + 1).to_string();
        let target = out.join("page");
        args.push(program.to_string_lossy().into_owned());
        for a in ["-q", "-f", &page, "-l", &page, "-singlefile", "-scale-to-x", &width.to_string(), "-scale-to-y", &height.to_string(), "-cropbox", source] {
            args.push(a.to_string());
        }
        args.push(target.to_string_lossy().into_owned());
        args
    }
}

/// The rsvg-convert executable: `/usr/bin/rsvg-convert` on Linux
/// (unchanged), the located Homebrew/MacPorts/PATH copy on macOS.
pub fn rsvg_path() -> std::path::PathBuf {
    #[cfg(not(target_os = "macos"))]
    if mode() == Mode::Direct {
        return tool_path_for(Mode::Direct, "rsvg-convert");
    }
    notes_platform::tools::program(notes_platform::tools::Tool::RsvgConvert)
}

/// Whether the ink renderer's sandbox prerequisites exist: Linux needs
/// `/usr/bin/bwrap` and `/usr/bin/rsvg-convert` (unchanged); macOS needs
/// nothing extra (rsvg-convert is only used with
/// `SAMSUNG_NOTES_INK_RASTER=rsvg`, which then fails cleanly if missing).
pub fn ink_sandbox_available() -> bool {
    #[cfg(not(target_os = "macos"))]
    {
        // Direct mode (Flatpak, no/unusable bwrap): like macOS, rsvg-convert
        // is only needed for SAMSUNG_NOTES_INK_RASTER=rsvg.
        match mode() {
            Mode::Bwrap => Path::new("/usr/bin/bwrap").is_file() && rsvg_path().is_file(),
            Mode::Direct => true,
        }
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
}

/// Isolated renderer/rasterizer invocation reading stdin, writing stdout.
/// Linux: [`ink_bwrap_args`]. macOS: cleared environment + Seatbelt (no
/// network, no file writes).
pub fn ink_args(binary: &str, png: bool) -> Vec<String> {
    #[cfg(not(target_os = "macos"))]
    {
        ink_bwrap_args(binary, png)
    }
    #[cfg(target_os = "macos")]
    {
        let mut args = notes_platform::isolation::prefix(None, Path::new("/var/empty"));
        args.push(binary.to_string());
        if png {
            args.push("--format=png".into());
        }
        args
    }
}

/// How the external renderers (Poppler `pdftoppm`, the GPL `ink-render`
/// CLI, optional `rsvg-convert`) are isolated.
///
/// * [`Mode::Bwrap`] (default on a Linux host where `/usr/bin/bwrap` works):
///   the bubblewrap sandboxes above — no network, no host filesystem except
///   read-only `/usr` and the one input/output path. Unchanged behaviour.
/// * [`Mode::Direct`]: bwrap is missing or unusable (inside Flatpak nested
///   user namespaces are blocked; Ubuntu's AppArmor userns restriction; no
///   bubblewrap on macOS). The same program runs directly through
///   `/usr/bin/env -i` (cleared environment, `PATH=/usr/bin:/app/bin`,
///   `HOME=/tmp`, `LC_ALL=C`) with the same rlimits (AS/CPU/CORE=0/FSIZE),
///   own session where the caller asked for one, PDEATHSIG and timeout — but
///   **without** namespace/filesystem/network isolation. Inside Flatpak the
///   app sandbox itself still applies.
///
/// `NOTES_SANDBOX=bwrap|direct` (alias `none`) overrides detection. The
/// choice is made once per process and reported on stderr once when it is
/// [`Mode::Direct`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Bwrap,
    Direct,
}

pub const BWRAP: &str = "/usr/bin/bwrap";
/// Directories searched for helper programs in [`Mode::Direct`] (`/app/bin`
/// is where Flatpak modules install).
pub const DIRECT_TOOL_DIRS: [&str; 2] = ["/usr/bin", "/app/bin"];

/// Pure decision used by [`mode`]: `override_value` is `NOTES_SANDBOX`,
/// `in_flatpak` whether `/.flatpak-info` exists, `bwrap_present` whether
/// `/usr/bin/bwrap` is a file, `probe` runs a trivial bwrap sandbox.
pub fn decide(override_value: Option<&str>, in_flatpak: bool, bwrap_present: bool, probe: impl FnOnce() -> bool)
              -> (Mode, Option<&'static str>) {
    match override_value.map(str::trim) {
        Some("bwrap") => return (Mode::Bwrap, None),
        Some("direct") | Some("none") => return (Mode::Direct, Some("NOTES_SANDBOX requests it")),
        _ => {}
    }
    if in_flatpak {
        return (Mode::Direct, Some("running inside Flatpak (nested bubblewrap is not available)"));
    }
    if !bwrap_present {
        return (Mode::Direct, Some("/usr/bin/bwrap is not installed"));
    }
    if probe() {
        (Mode::Bwrap, None)
    } else {
        (Mode::Direct, Some("/usr/bin/bwrap cannot create a sandbox here (user namespaces restricted?)"))
    }
}

/// `bwrap --unshare-all --die-with-parent --ro-bind / / true`, 5 s bound.
pub fn probe_bwrap() -> bool {
    let child = Command::new(BWRAP)
        .args(["--unshare-all", "--die-with-parent", "--ro-bind", "/", "/", "true"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .env_clear().spawn();
    let Ok(mut child) = child else { return false };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The process-wide [`Mode`] (detected once).
pub fn mode() -> Mode {
    static MODE: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        let value = std::env::var("NOTES_SANDBOX").ok();
        let (mode, reason) = decide(value.as_deref(), Path::new("/.flatpak-info").exists(),
                                    Path::new(BWRAP).is_file(), probe_bwrap);
        if let Some(reason) = reason {
            eprintln!("notes: renderer sandbox: bubblewrap disabled ({reason}); helpers run with rlimits only, without namespace isolation");
        }
        mode
    })
}

/// Helper program path: `/usr/bin/<name>` in [`Mode::Bwrap`] (the path bound
/// into the sandbox); first existing of [`DIRECT_TOOL_DIRS`] otherwise.
pub fn tool_path(name: &str) -> std::path::PathBuf {
    tool_path_for(mode(), name)
}

pub fn tool_path_for(mode: Mode, name: &str) -> std::path::PathBuf {
    let fixed = Path::new("/usr/bin").join(name);
    if mode == Mode::Bwrap {
        return fixed;
    }
    DIRECT_TOOL_DIRS.iter().map(|d| Path::new(d).join(name)).find(|p| p.is_file()).unwrap_or(fixed)
}

/// Whether the sandbox itself is usable: bwrap present (Bwrap mode) or
/// always (Direct mode).
pub fn sandbox_available() -> bool {
    match mode() {
        Mode::Bwrap => Path::new(BWRAP).is_file(),
        Mode::Direct => true,
    }
}

/// Runs a `/usr/bin/env -i NAME=VALUE... PROGRAM ...` prefix (direct mode,
/// macOS) without the `env` binary: `env` would read a program path that
/// contains `=` as one more assignment. Only the variables the prefix
/// builders set are recognised; anything else is executed unchanged.
fn split_env_prefix(args: &[String]) -> (Option<Vec<(String, String)>>, &[String]) {
    if args.len() < 3 || args[0] != "/usr/bin/env" || args[1] != "-i" {
        return (None, args);
    }
    let mut vars = Vec::new();
    let mut index = 2;
    while index < args.len() && vars.len() < 3 {
        match args[index].split_once('=') {
            Some((name, value)) if matches!(name, "PATH" | "HOME" | "LC_ALL") => vars.push((name.to_string(), value.to_string())),
            _ => break,
        }
        index += 1;
    }
    if index >= args.len() {
        return (None, args);
    }
    (Some(vars), &args[index..])
}

fn direct_prefix() -> Vec<String> {
    ["/usr/bin/env", "-i", "PATH=/usr/bin:/app/bin", "HOME=/tmp", "LC_ALL=C"].iter().map(|s| s.to_string()).collect()
}

pub enum RunError {
    /// Could not start (OSError in Python).
    Spawn,
    Timeout,
}

pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}

/// Fork+exec `args` with the worker's limits applied in the child, feeding
/// `input` to stdin, collecting stdout, killing the child (or its session)
/// on timeout. stderr is discarded like the Python callers do.
pub fn run(args: &[String], input: Option<Vec<u8>>, limits: Limits, new_session: bool, timeout: Duration)
           -> Result<Outcome, RunError> {
    run_with_cwd(args, input, limits, new_session, timeout, None, true)
}

fn command(args: &[String], stdin: bool, limits: Limits, new_session: bool, cwd: Option<&Path>, pin: bool) -> Command {
    // SAFETY: getpid has no preconditions and cannot fail.
    let parent = unsafe { libc::getpid() } as i64;
    let (env, argv) = split_env_prefix(args);
    let mut command = Command::new(&argv[0]);
    if let Some(vars) = env {
        command.env_clear().envs(vars);
    }
    command.args(&argv[1..]).stdout(Stdio::piped()).stderr(Stdio::null())
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() });
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    // SAFETY: the closure runs in the forked child of a multi-threaded
    // process and only makes async-signal-safe calls without allocating:
    // setsid, getppid/prctl (bind_parent), setrlimit (apply_limits),
    // sched_{get,set}affinity (pin_lowest_cpu); errors are built with
    // `from_raw_os_error`/`last_os_error`, which do not allocate.
    unsafe {
        command.pre_exec(move || {
            if new_session && libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if !bind_parent(parent) {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            apply_limits(&limits, true)?;
            if pin {
                pin_lowest_cpu()?;
            }
            Ok(())
        });
    }
    command
}

/// Spawn with stdin fed from a thread; the caller reads stdout as a stream.
pub fn spawn_streaming(args: &[String], input: Vec<u8>, limits: Limits, new_session: bool, cwd: Option<&Path>)
                       -> io::Result<(std::process::Child, std::process::ChildStdout)> {
    let mut child = command(args, true, limits, new_session, cwd, false).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let stdout = child.stdout.take().expect("piped stdout");
    Ok((child, stdout))
}

pub fn run_with_cwd(args: &[String], input: Option<Vec<u8>>, limits: Limits, new_session: bool, timeout: Duration,
                    cwd: Option<&Path>, pin: bool) -> Result<Outcome, RunError> {
    let mut child = command(args, input.is_some(), limits, new_session, cwd, pin).spawn().map_err(|_| RunError::Spawn)?;
    let pid = child.id() as i32;
    let writer = match (input, child.stdin.take()) {
        (Some(data), Some(mut stdin)) => Some(std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        })),
        _ => None,
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let (sender, receiver) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let status = child.wait();
        let _ = sender.send(status);
    });
    let status = match receiver.recv_timeout(timeout) {
        Ok(status) => status,
        Err(_) => {
            // SAFETY: kill() takes plain integers. The waiter thread has not returned from wait(), so the child is not reaped and its pid/pgid cannot have been reused.
            unsafe { libc::kill(if new_session { -pid } else { pid }, libc::SIGKILL) };
            let _ = receiver.recv();
            let _ = waiter.join();
            let _ = reader.join();
            if let Some(w) = writer {
                let _ = w.join();
            }
            return Err(RunError::Timeout);
        }
    };
    let _ = waiter.join();
    let stdout = reader.join().unwrap_or_default();
    if let Some(w) = writer {
        let _ = w.join();
    }
    let code = status.ok().and_then(|s| s.code());
    Ok(Outcome { code, stdout })
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    #[test]
    fn override_wins() {
        assert_eq!(decide(Some("bwrap"), true, false, || false).0, Mode::Bwrap);
        assert_eq!(decide(Some("none"), false, true, || true).0, Mode::Direct);
        assert_eq!(decide(Some("direct"), false, true, || true).0, Mode::Direct);
    }

    #[test]
    fn detection_order() {
        assert_eq!(decide(None, true, true, || panic!("no probe in flatpak")).0, Mode::Direct);
        assert_eq!(decide(None, false, false, || panic!("no probe without bwrap")).0, Mode::Direct);
        assert_eq!(decide(None, false, true, || true), (Mode::Bwrap, None));
        assert_eq!(decide(Some("bogus"), false, true, || false).0, Mode::Direct);
    }

    #[test]
    fn bwrap_argv_is_unchanged() {
        let a = pdf_args_for(Mode::Bwrap, "/s.pdf", "/o", 0, 10, 20);
        assert_eq!(a[0], "/usr/bin/bwrap");
        assert!(a.ends_with(&["-cropbox".into(), "/input.pdf".into(), "/output/page".into()]));
        let i = ink_args_for(Mode::Bwrap, "/r", true);
        assert_eq!(i[0], "/usr/bin/bwrap");
        assert_eq!(&i[i.len() - 2..], &["/renderer".to_string(), "--format=png".to_string()]);
        assert_eq!(tool_path_for(Mode::Bwrap, "pdftoppm"), Path::new("/usr/bin/pdftoppm"));
    }

    #[test]
    fn direct_argv_clears_env_and_keeps_paths() {
        let a = pdf_args_for(Mode::Direct, "/s.pdf", "/o/", 2, 10, 20);
        assert_eq!(&a[..5], &["/usr/bin/env", "-i", "PATH=/usr/bin:/app/bin", "HOME=/tmp", "LC_ALL=C"]);
        assert!(a[5].ends_with("/pdftoppm"));
        assert_eq!(&a[6..], &["-q", "-f", "3", "-l", "3", "-singlefile", "-scale-to-x", "10", "-scale-to-y", "20",
                              "-cropbox", "/s.pdf", "/o/page"]);
        assert!(!a.iter().any(|x| x.contains("bwrap")));
        let i = ink_args_for(Mode::Direct, "/r", false);
        assert_eq!(&i[5..], &["/r"]);
    }

    #[test]
    fn env_prefix_runs_program_whose_path_contains_equals() {
        // `env` would take "/…/a=b/show" for an assignment; the prefix is
        // applied with env_clear instead, so the program runs and sees only
        // the prefix's variables.
        let dir = std::env::temp_dir().join(format!("notes-env-prefix-{}", std::process::id()));
        let sub = dir.join("a=b");
        std::fs::create_dir_all(&sub).unwrap();
        let program = sub.join("show");
        std::fs::write(&program, "#!/bin/sh\nprintf '%s|%s' \"$PATH\" \"$HOME\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut args = direct_prefix();
        args.push(program.to_string_lossy().into_owned());
        let out = run(&args, None, PDF_LIMITS, true, Duration::from_secs(10)).ok().expect("spawn");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out.code, Some(0));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "/usr/bin:/app/bin|/tmp");
    }

    #[test]
    fn direct_mode_runs_with_limits() {
        // The rlimited exec path works without bubblewrap.
        let args: Vec<String> = ["/usr/bin/env", "-i", "PATH=/usr/bin", "/bin/sh", "-c", "printf $PATH"].iter().map(|s| s.to_string()).collect();
        let out = run(&args, None, PDF_LIMITS, true, Duration::from_secs(10)).ok().expect("spawn");
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout, b"/usr/bin");
    }
}
