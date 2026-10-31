//! Local-only diagnostic for the fixed synthetic 520-page book.
use std::path::Path;
fn main() {
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from).expect("synthetic fixture path").canonicalize().expect("fixture path");
    assert!(path.starts_with(Path::new("/tmp/samsung-codex")), "synthetic scratch only");
    let text = std::fs::read_to_string(path).expect("synthetic fixture");
    let snapshot = rs_store::val::loads(&text).expect("synthetic snapshot");
    drop(text);
    let start = std::time::Instant::now();
    let page = notes_preview::ink::prepare::prepare_pages(&snapshot, Some(0), true).expect("legacy page");
    assert!(page.iter().any(|p| p.index == 0));
    drop(page);
    drop(snapshot);
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe { libc::malloc_trim(0); }
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    for line in status.lines().filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:")) { println!("{line}"); }
    println!("elapsed_ms={}", start.elapsed().as_millis());
}
