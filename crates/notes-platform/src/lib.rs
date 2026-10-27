//! Operating-system seams of the Notes engine.
//!
//! One module per concern, so a further platform only has to fill in the
//! same functions:
//!
//! | module | concern |
//! |---|---|
//! | [`dirs`] | state/library/research/data/config/downloads directories |
//! | [`open`] | opening a file or URL with the user's default application |
//! | [`process`] | detached sessions, parent-death binding, rlimits, CPU pinning, bounded capture |
//! | [`tools`] | external helper discovery (Poppler, librsvg, ImageMagick, bwrap) and install hints |
//! | [`isolation`] | the macOS replacement for the bwrap preview sandbox (env -i + Seatbelt) |
//! | [`net`] | the loopback listener socket |
//! | [`url_scheme`] | how the `ms-app` sign-in callback reaches the app |
//!
//! **Linux contract:** every function returns exactly what the engine
//! computed before this crate existed (XDG variables, `xdg-open`,
//! `xdg-user-dir`, fixed `/usr/bin` tool paths for the bwrap sandboxes,
//! `PR_SET_PDEATHSIG`, `sched_setaffinity`, `SOCK_CLOEXEC`). Other Unix
//! targets except macOS take the Linux (XDG) branch where it compiles.
//!
//! **macOS:** `~/Library/Application Support/org.linuxnotes.desktop`,
//! `/usr/bin/open`, Homebrew/MacPorts tool lookup, no bwrap, no
//! PDEATHSIG (see `docs/MACOS_PORT.md`).
//!
//! Unix-only assumptions that remain in the callers (0600/0700 modes,
//! `O_NOFOLLOW`, `flock`, `setsid`, `umask`) are listed in
//! `docs/MACOS_PORT.md` for a future non-Unix port; there is deliberately no
//! Windows code here.
pub mod dirs;
pub mod isolation;
pub mod net;
pub mod open;
pub mod process;
pub mod tools;
pub mod url_scheme;

/// The platform this build targets, for diagnostics.
pub const PLATFORM: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "linux") {
    "linux"
} else {
    "unix"
};
