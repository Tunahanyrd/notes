//! Process isolation for untrusted-input helpers (Poppler on downloaded
//! PDFs, the GPL ink renderer on note payloads).
//!
//! * Linux keeps bubblewrap (`rs_render::sandbox::{pdf,ink}_bwrap_args`):
//!   new user/pid/net/ipc namespaces, read-only `/usr`, tmpfs `/tmp`, only
//!   the input/output bound, cleared environment.
//! * macOS has no bubblewrap. The fallback built here is an argv prefix
//!   `/usr/bin/env -i HOME=... PATH=/usr/bin:/bin LC_ALL=C` (cleared
//!   environment) optionally followed by
//!   `/usr/bin/sandbox-exec -p <profile>` whose Seatbelt profile denies all
//!   network access and every file write outside the one private output
//!   directory. The rlimits (CPU, file size, no core; address space is not
//!   enforced by XNU), the new session and the timeout kill still apply.
//!   **Reduced isolation compared with Linux:** the helper can read any file
//!   the user can read, there are no namespaces, and without
//!   `sandbox-exec` (deprecated by Apple but present through macOS 15) only
//!   the environment is cleared. `NOTES_MACOS_SANDBOX_EXEC=0` disables the
//!   Seatbelt layer for troubleshooting.
//!
//! The profile/prefix builders are plain string code compiled on every
//! platform so they are unit-tested on Linux too.
use std::path::Path;

/// `sandbox-exec` location on macOS.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// Environment switch: `0` disables the Seatbelt layer.
pub const SANDBOX_EXEC_SWITCH: &str = "NOTES_MACOS_SANDBOX_EXEC";

/// SBPL string literal.
pub fn sbpl_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Seatbelt profile: everything allowed except network access and file
/// writes; writes are re-allowed only below `write_dir` (canonical path) and
/// to `/dev/null`. Later rules take precedence in SBPL.
pub fn seatbelt_profile(write_dir: Option<&Path>) -> String {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n");
    profile.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/dtracehelper\")");
    if let Some(dir) = write_dir {
        profile.push_str(" (subpath ");
        profile.push_str(&sbpl_string(&dir.to_string_lossy()));
        profile.push(')');
    }
    profile.push_str(")\n");
    profile
}

/// The macOS argv prefix (see the module docs). `use_seatbelt` decides
/// whether the `sandbox-exec` layer is added.
pub fn macos_prefix(write_dir: Option<&Path>, home: &Path, use_seatbelt: bool) -> Vec<String> {
    let mut args = vec![
        "/usr/bin/env".to_string(),
        "-i".to_string(),
        format!("HOME={}", home.to_string_lossy()),
        "PATH=/usr/bin:/bin".to_string(),
        "LC_ALL=C".to_string(),
    ];
    if use_seatbelt {
        args.push(SANDBOX_EXEC.to_string());
        args.push("-p".to_string());
        args.push(seatbelt_profile(write_dir));
    }
    args
}

/// Whether the Seatbelt layer can be used here: macOS, not disabled by
/// `NOTES_MACOS_SANDBOX_EXEC=0`, and a trivial profile runs `/usr/bin/true`
/// successfully (checked once per process).
#[cfg(target_os = "macos")]
pub fn seatbelt_usable() -> bool {
    use std::sync::OnceLock;
    static USABLE: OnceLock<bool> = OnceLock::new();
    if std::env::var_os(SANDBOX_EXEC_SWITCH).is_some_and(|v| v == "0") {
        return false;
    }
    *USABLE.get_or_init(|| {
        Path::new(SANDBOX_EXEC).is_file()
            && std::process::Command::new(SANDBOX_EXEC)
                .args(["-p", "(version 1)\n(allow default)\n(deny network*)\n", "/usr/bin/true"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    })
}

/// Not macOS: never.
#[cfg(not(target_os = "macos"))]
pub fn seatbelt_usable() -> bool {
    false
}

/// `macos_prefix` with the Seatbelt layer when usable. `write_dir` is
/// canonicalized (macOS temp paths live behind the `/var` → `/private/var`
/// symlink and Seatbelt matches real paths).
pub fn prefix(write_dir: Option<&Path>, home: &Path) -> Vec<String> {
    let canonical = write_dir.map(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf()));
    macos_prefix(canonical.as_deref(), home, seatbelt_usable())
}
