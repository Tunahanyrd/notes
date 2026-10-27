//! Hostile Cloud responses must fail with an error, never take the engine
//! down. Deeply nested XML used to overflow the thread stack in the
//! recursive tree conversion (an abort of the whole process, not a panic).
use notes_cloud::{etree, metadata};

fn nested(depth: usize) -> Vec<u8> {
    let mut raw = Vec::with_capacity(depth * 7 + 16);
    raw.extend_from_slice(b"<r>");
    for _ in 0..depth {
        raw.extend_from_slice(b"<a>");
    }
    for _ in 0..depth {
        raw.extend_from_slice(b"</a>");
    }
    raw.extend_from_slice(b"</r>");
    raw
}

/// Run on a thread with the default 2 MiB stack, like the sync worker.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(2 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn deeply_nested_folder_metadata_is_rejected() {
    let raw = nested(1_000_000);
    assert!(raw.len() < metadata::MAX_XML);
    assert!(on_small_stack(move || metadata::xml(&raw).is_err()));
}

#[test]
fn deeply_nested_xml_is_rejected_by_the_tree_parser() {
    let raw = nested(100_000);
    assert!(on_small_stack(move || etree::parse_bytes(&raw, false).is_err()));
    // Ordinary documents still parse.
    let ok = nested(etree::MAX_DEPTH - 1);
    assert!(etree::parse_bytes(&ok, false).is_ok());
}
