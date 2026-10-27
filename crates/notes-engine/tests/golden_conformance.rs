//! HTTP conformance of the pure engine against the frozen golden fixtures
//! (`conformance/golden/`, recorded from the historical Python reference over
//! the synthetic library). Needs only `python3` (stdlib; the harness never
//! imports the `samsung_notes` package) and, for full coverage, the optional
//! tools listed in `conformance/run.py` (pdfinfo, pdftoppm, bwrap, magick,
//! xdg-user-dir, ink-render) — the recording had all of them, so the test is
//! `#[ignore]` by default:
//!
//!     cargo test -p notes-engine --test golden_conformance -- --ignored
//!
//! CI runs the same replay directly (`python3 crates/notes-engine/conformance/run.py --replay ...`).

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "needs python3 and the optional preview/export tools; run with --ignored"]
fn replay_matches_golden_fixtures() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    let status = Command::new("python3")
        .arg(manifest.join("conformance/run.py"))
        .arg("--replay")
        .arg("--candidate")
        .arg(env!("CARGO_BIN_EXE_notes-engine"))
        .current_dir(&root)
        .status()
        .expect("python3 must be on PATH to run the golden replay");
    assert!(status.success(), "golden conformance replay failed (see the FAIL lines above)");
}
