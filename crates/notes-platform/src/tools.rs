//! External helper programs.
//!
//! | tool | Linux (unchanged) | macOS |
//! |---|---|---|
//! | `pdftoppm` (Poppler) | `/usr/bin/pdftoppm` only (bound into the bwrap `/usr`) | first of `/opt/homebrew/bin`, `/usr/local/bin`, `/opt/local/bin`, then `PATH` |
//! | `rsvg-convert` (librsvg) | `/usr/bin/rsvg-convert` only | same search |
//! | `magick` (ImageMagick 7) | `PATH` lookup (`shutil.which`) | same search |
//! | `bwrap` | `/usr/bin/bwrap` | never (no bubblewrap on macOS) |
//!
//! The macOS search does not rely on `PATH` alone because an app started
//! from Finder/Dock gets only `/usr/bin:/bin:/usr/sbin:/sbin`.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pdftoppm,
    RsvgConvert,
    Magick,
    Bwrap,
}

impl Tool {
    pub const ALL: [Tool; 4] = [Tool::Pdftoppm, Tool::RsvgConvert, Tool::Magick, Tool::Bwrap];

    /// Executable name.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Pdftoppm => "pdftoppm",
            Tool::RsvgConvert => "rsvg-convert",
            Tool::Magick => "magick",
            Tool::Bwrap => "bwrap",
        }
    }

    /// What the tool is used for (diagnostics).
    pub fn purpose(self) -> &'static str {
        match self {
            Tool::Pdftoppm => "PDF page previews",
            Tool::RsvgConvert => "optional SVG rasterizer (SAMSUNG_NOTES_INK_RASTER=rsvg)",
            Tool::Magick => "Save as PDF",
            Tool::Bwrap => "preview sandbox",
        }
    }

    /// Homebrew formula providing it (macOS install hint).
    pub fn brew_formula(self) -> Option<&'static str> {
        match self {
            Tool::Pdftoppm => Some("poppler"),
            Tool::RsvgConvert => Some("librsvg"),
            Tool::Magick => Some("imagemagick"),
            Tool::Bwrap => None,
        }
    }

    /// Whether this platform uses the tool at all.
    pub fn used_here(self) -> bool {
        !(cfg!(target_os = "macos") && self == Tool::Bwrap)
    }
}

/// Regular file with an execute bit (`shutil.which` check).
pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable_file(p))
}

/// Package-manager prefixes searched on macOS before `PATH`.
pub const MACOS_TOOL_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"];

/// The executable for `tool`, if installed.
#[cfg(not(target_os = "macos"))]
pub fn locate(tool: Tool) -> Option<PathBuf> {
    match tool {
        Tool::Magick => search_path(tool.name()),
        _ => {
            let fixed = PathBuf::from("/usr/bin").join(tool.name());
            fixed.is_file().then_some(fixed)
        }
    }
}

/// The executable for `tool`, if installed.
#[cfg(target_os = "macos")]
pub fn locate(tool: Tool) -> Option<PathBuf> {
    if tool == Tool::Bwrap {
        return None;
    }
    MACOS_TOOL_DIRS
        .iter()
        .map(|dir| Path::new(dir).join(tool.name()))
        .find(|p| is_executable_file(p))
        .or_else(|| search_path(tool.name()))
}

/// What to execute for `tool`. Linux: the fixed sandbox path
/// (`/usr/bin/<name>`) or, for ImageMagick, the bare name resolved through
/// `PATH` at spawn time — exactly the previous behaviour. macOS: the located
/// executable, else the bare name (spawn then fails cleanly).
#[cfg(not(target_os = "macos"))]
pub fn program(tool: Tool) -> PathBuf {
    match tool {
        Tool::Magick => PathBuf::from(tool.name()),
        _ => PathBuf::from("/usr/bin").join(tool.name()),
    }
}

/// What to execute for `tool` (see the Linux variant).
#[cfg(target_os = "macos")]
pub fn program(tool: Tool) -> PathBuf {
    locate(tool).unwrap_or_else(|| PathBuf::from(tool.name()))
}

/// `locate(tool).is_some()`.
pub fn available(tool: Tool) -> bool {
    locate(tool).is_some()
}

/// Install hint for a missing tool. `None` on Linux (messages there are
/// part of the HTTP contract and stay byte-identical).
pub fn install_hint(tool: Tool) -> Option<String> {
    if cfg!(target_os = "macos") {
        tool.brew_formula().map(|formula| format!("brew install {formula}"))
    } else {
        None
    }
}

/// `message` plus the platform install hint, if any.
pub fn with_install_hint(message: &str, tool: Tool) -> String {
    match install_hint(tool) {
        Some(hint) => format!("{message} ({hint})"),
        None => message.to_string(),
    }
}

/// Tools this platform uses that are not installed (startup diagnostics).
pub fn missing() -> Vec<Tool> {
    Tool::ALL.iter().copied().filter(|t| t.used_here() && !available(*t)).collect()
}

/// One line per missing tool, for stderr at engine start.
pub fn missing_report() -> Vec<String> {
    missing()
        .into_iter()
        .map(|t| match install_hint(t) {
            Some(hint) => format!("{} not found ({}); install with: {}", t.name(), t.purpose(), hint),
            None => format!("{} not found ({})", t.name(), t.purpose()),
        })
        .collect()
}

/// Extra CA bundle loaded after OpenSSL's default verify paths. Linux:
/// none (the distribution OpenSSL already points at the system store).
/// The system CA bundle, loaded in addition to OpenSSL's default paths.
/// macOS: `/etc/ssl/cert.pem` (Apple's bundled roots), because a statically
/// linked Homebrew OpenSSL otherwise only knows its own `OPENSSLDIR`, which
/// does not exist on Macs without Homebrew. Linux: an OpenSSL bundled in an
/// AppImage keeps its build host's `OPENSSLDIR` (Ubuntu `/usr/lib/ssl`), which
/// does not exist on Arch, Fedora or openSUSE, so the distribution's bundle is
/// found by its well-known path. `SSL_CERT_FILE` always wins.
pub fn extra_ca_file() -> Option<PathBuf> {
    if std::env::var_os("SSL_CERT_FILE").is_some() {
        return None;
    }
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["/etc/ssl/cert.pem"]
    } else {
        &[
            "/etc/ssl/certs/ca-certificates.crt",                // Debian, Ubuntu, Arch, Gentoo
            "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem", // Fedora, RHEL
            "/etc/pki/tls/certs/ca-bundle.crt",                  // older Fedora/RHEL
            "/etc/ssl/ca-bundle.pem",                            // openSUSE
            "/etc/ssl/cert.pem",                                 // Alpine, Void, Arch
        ]
    };
    candidates.iter().map(PathBuf::from).find(|path| path.is_file())
}
