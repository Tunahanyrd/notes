//! Directories. Every function takes the caller's notion of `home` so the
//! existing `HOME`/`getpwuid` fallbacks of each caller stay exactly as they
//! were.
//!
//! | | Linux (and other non-macOS Unix) | macOS |
//! |---|---|---|
//! | state home | `$XDG_STATE_HOME` or `~/.local/state` | `$XDG_STATE_HOME` or `~/Library/Application Support/org.linuxnotes.desktop` |
//! | data home | `$XDG_DATA_HOME` or `~/.local/share` | `$XDG_DATA_HOME` or the same Application Support folder |
//! | config home | `$XDG_CONFIG_HOME` or `~/.config` | `$XDG_CONFIG_HOME` or the same Application Support folder |
//! | cache home | `$XDG_CACHE_HOME` or `~/.cache` | `$XDG_CACHE_HOME` or `~/Library/Caches/org.linuxnotes.desktop` |
//! | runtime | `$XDG_RUNTIME_DIR/samsung-notes-linux` (0700), else temp dir | per-user `$TMPDIR` |
//! | library | `<state home>/samsung-notes-linux` | same rule |
//! | research (sessions) | `<state home>/samsung-notes-research` | same rule |
//! | downloads | `xdg-user-dir DOWNLOAD` (5 s), else `home` | `~/Downloads`, else `home` |
//!
//! Unset, empty or relative `XDG_*` values are ignored (XDG Base Directory
//! specification). On macOS an explicitly set (absolute) `XDG_*` variable still wins: that is the
//! documented way to run a fully isolated test profile. There is no
//! migration: a first run on macOS starts with an empty library.
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::time::Duration;

/// Leaf name of the private library (SQLite + caches).
pub const LIBRARY_DIR: &str = "samsung-notes-linux";
/// Leaf name of the private sign-in/session state.
pub const RESEARCH_DIR: &str = "samsung-notes-research";
/// macOS: folder under `~/Library/Application Support` (the bundle identifier).
pub const MACOS_APP_SUPPORT: &str = "org.linuxnotes.desktop";

/// XDG Base Directory rule: a variable that is unset, empty or not an
/// absolute path is ignored and the default is used (same rule as
/// tools/_xdg.py).
fn env_or(var: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(value) if value.is_absolute() => value,
        _ => fallback(),
    }
}

/// `~/Library/Application Support/org.linuxnotes.desktop` (macOS base).
pub fn macos_app_support(home: &Path) -> PathBuf {
    home.join("Library/Application Support").join(MACOS_APP_SUPPORT)
}

#[cfg(not(target_os = "macos"))]
fn default_base(home: &Path, xdg_relative: &str) -> PathBuf {
    home.join(xdg_relative)
}

#[cfg(target_os = "macos")]
fn default_base(home: &Path, _xdg_relative: &str) -> PathBuf {
    macos_app_support(home)
}

/// `$XDG_STATE_HOME` or the platform default.
pub fn state_home(home: &Path) -> PathBuf {
    env_or("XDG_STATE_HOME", || default_base(home, ".local/state"))
}

/// `$XDG_DATA_HOME` or the platform default.
pub fn data_home(home: &Path) -> PathBuf {
    env_or("XDG_DATA_HOME", || default_base(home, ".local/share"))
}

/// `$XDG_CONFIG_HOME` or the platform default.
pub fn config_home(home: &Path) -> PathBuf {
    env_or("XDG_CONFIG_HOME", || default_base(home, ".config"))
}

/// The default private library directory (`--data-dir` default).
pub fn library_dir(home: &Path) -> PathBuf {
    state_home(home).join(LIBRARY_DIR)
}

/// The private sign-in state (`session.RESEARCH`).
pub fn research_dir(home: &Path) -> PathBuf {
    state_home(home).join(RESEARCH_DIR)
}

/// `$XDG_CACHE_HOME` or the platform default (`~/.cache`; macOS
/// `~/Library/Caches/org.linuxnotes.desktop`).
pub fn cache_home(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    let default = home.join("Library/Caches").join(MACOS_APP_SUPPORT);
    #[cfg(not(target_os = "macos"))]
    let default = home.join(".cache");
    env_or("XDG_CACHE_HOME", || default)
}

/// Leaf name of the per-session runtime directory under `$XDG_RUNTIME_DIR`.
pub const RUNTIME_DIR: &str = "samsung-notes-linux";

/// Where short-lived per-session files (the desktop shell's runtime.json
/// directory) go: `$XDG_RUNTIME_DIR/samsung-notes-linux` (created 0700) when
/// `XDG_RUNTIME_DIR` is an absolute existing directory, else `None` and the
/// caller falls back to the temp dir (Linux: `/tmp`; macOS: the per-user
/// `$TMPDIR`, which is already private, so macOS always returns `None`).
pub fn runtime_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_dir())?;
    let dir = base.join(RUNTIME_DIR);
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    // Only a real, private directory owned by us.
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid {
        return None;
    }
    if meta.permissions().mode() & 0o077 != 0 && std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_err() {
        return None;
    }
    Some(dir)
}

/// Where "Save as file" exports go.
#[cfg(not(target_os = "macos"))]
pub fn downloads_dir(home: &Path) -> PathBuf {
    let folder = crate::process::capture(&["xdg-user-dir", "DOWNLOAD"], Duration::from_secs(5))
        .map(|(_, out)| String::from_utf8_lossy(&out).trim().to_string())
        .unwrap_or_default();
    if !folder.is_empty() && Path::new(&folder).is_dir() {
        PathBuf::from(folder)
    } else {
        home.to_path_buf()
    }
}

/// Where "Save as file" exports go.
#[cfg(target_os = "macos")]
pub fn downloads_dir(home: &Path) -> PathBuf {
    let folder = home.join("Downloads");
    if folder.is_dir() {
        folder
    } else {
        home.to_path_buf()
    }
}
