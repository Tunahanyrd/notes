//! Clock, randomness, directory layout and helper-command execution. Every
//! side channel is injectable so tests never touch the user's environment.
use crate::error::{Error, Result};
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// `time.time()` and `secrets.token_bytes()`.
pub trait Runtime: Send + Sync {
    /// `time.time()`
    fn now(&self) -> f64;
    /// `secrets.token_bytes(len(buf))`
    fn fill_random(&self, buf: &mut [u8]);
}

/// Wall clock and the OS CSPRNG.
pub struct SystemRuntime;

impl Runtime for SystemRuntime {
    fn now(&self) -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
    }
    fn fill_random(&self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("OS randomness unavailable");
    }
}

/// `int(time.time())`
pub fn unix_seconds(rt: &dyn Runtime) -> i64 {
    rt.now().trunc() as i64
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `secrets.token_hex(n)`
pub fn token_hex(rt: &dyn Runtime, n: usize) -> String {
    let mut buf = vec![0u8; n];
    rt.fill_random(&mut buf);
    hex(&buf)
}

/// `secrets.token_urlsafe(n)`
pub fn token_urlsafe(rt: &dyn Runtime, n: usize) -> String {
    use base64::Engine;
    let mut buf = vec![0u8; n];
    rt.fill_random(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

/// `datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ-')`
pub fn utc_stamp(rt: &dyn Runtime) -> String {
    let secs = rt.now().floor() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z-", y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Directories and subprocess environment of the sign-in flow.
///
/// `state` is `notes_login.STATE` == `session.RESEARCH`
/// (`$XDG_STATE_HOME/samsung-notes-research`). `command_env` is added to the
/// environment of every helper process (`xdg-mime`, `xdg-open`,
/// `desktop-file-validate`, `update-desktop-database`, `notify-send`) and its
/// `PATH`, when present, is also used for `shutil.which` lookups. Production
/// leaves it empty (inherit the engine's environment, like Python).
#[derive(Clone, Debug)]
pub struct Layout {
    pub state: PathBuf,
    pub data_home: PathBuf,
    pub config_home: PathBuf,
    pub command_env: Vec<(OsString, OsString)>,
}

fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h);
    }
    #[allow(deprecated)]
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

impl Layout {
    /// The paths the Python tools compute from `XDG_STATE_HOME`,
    /// `XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `HOME` at import time (Linux;
    /// on macOS the defaults are `~/Library/Application Support/
    /// org.linuxnotes.desktop`, see `notes_platform::dirs`).
    pub fn from_env() -> Layout {
        let home = home();
        Layout {
            state: notes_platform::dirs::research_dir(&home),
            data_home: notes_platform::dirs::data_home(&home),
            config_home: notes_platform::dirs::config_home(&home),
            command_env: Vec::new(),
        }
    }

    /// `STATE / 'sessions'`
    pub fn sessions(&self) -> PathBuf {
        self.state.join("sessions")
    }

    fn path_var(&self) -> Option<OsString> {
        self.command_env
            .iter()
            .rev()
            .find(|(k, _)| k == OsStr::new("PATH"))
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var_os("PATH"))
    }

    /// A `Command` with the overridden environment applied.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        for (k, v) in &self.command_env {
            cmd.env(k, v);
        }
        cmd
    }

    /// `shutil.which(name)`
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let path = self.path_var()?;
        for dir in std::env::split_paths(&path) {
            let candidate = if dir.as_os_str().is_empty() { PathBuf::from(name) } else { dir.join(name) };
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// `notes_login.command(*args)`: `subprocess.run(args, check=True,
    /// capture_output=True, text=True).stdout.strip()`.
    pub fn run_command(&self, args: &[&OsStr]) -> Result<String> {
        let output = self
            .command(args[0])
            .args(&args[1..])
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            return Err(Error::Subprocess("helper command failed".into()));
        }
        let text = String::from_utf8(output.stdout).map_err(|_| Error::value("helper output is not UTF-8"))?;
        Ok(py_strip(&text).to_string())
    }
}

fn is_executable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0 && access_x(path),
        Err(_) => false,
    }
}

fn access_x(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: c is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// Python `str.isspace()` for one character.
pub fn py_isspace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | '\u{1c}'..='\u{1f}' | ' ' | '\u{85}' | '\u{a0}')
        || (c as u32 > 0xff && c.is_whitespace())
}

/// Python `str.strip()`
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}
