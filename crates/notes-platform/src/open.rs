//! Opening a local file or URL with the user's default application.
//!
//! Linux: `xdg-open` (resolved through `PATH` at spawn time, exactly as
//! before). macOS: `open` (also resolved through `PATH`, so tests can put a
//! fake first; `/usr/bin/open` on a stock system). Callers keep their own
//! stdio/session handling; see [`crate::process::detach_session`].
use std::process::Command;

/// The opener program name.
#[cfg(not(target_os = "macos"))]
pub const OPENER: &str = "xdg-open";
/// The opener program name.
#[cfg(target_os = "macos")]
pub const OPENER: &str = "open";

/// `Command::new(OPENER)`.
pub fn command() -> Command {
    Command::new(OPENER)
}
