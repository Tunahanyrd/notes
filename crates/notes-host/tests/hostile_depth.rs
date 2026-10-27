//! Deeply nested hostile input must be rejected, not overflow the stack
//! (a stack overflow aborts the whole engine process, it is not a panic).
//! Each case runs on a 2 MiB thread: the std default and the sync worker.
use notes_host::snapshot;

fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(2 << 20).spawn(f).unwrap().join().unwrap();
}

fn nested_json(depth: usize) -> String {
    format!("{}{}", "[".repeat(depth), "]".repeat(depth))
}

fn nested_xml(depth: usize) -> String {
    format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth))
}

#[test]
fn deep_json_is_rejected() {
    on_small_stack(|| {
        for depth in [rs_store::val::MAX_DEPTH + 1, 100_000] {
            assert!(rs_store::val::loads(&nested_json(depth)).is_err());
            let object = format!("{}1{}", "{\"k\":".repeat(depth), "}".repeat(depth));
            assert!(rs_store::val::loads(&object).is_err());
        }
        // Nesting up to the bound still parses, serializes and drops.
        let value = rs_store::val::loads(&nested_json(rs_store::val::MAX_DEPTH)).unwrap();
        assert!(rs_store::val::dumps_utf8(&value).is_ok());
    });
}

#[test]
fn deep_xml_documents_are_rejected() {
    on_small_stack(|| {
        for depth in [rs_doc::xml::MAX_DEPTH + 1, 5_000, 200_000] {
            let xml = nested_xml(depth);
            assert!(rs_doc::xml::fromstring(xml.as_bytes()).is_err());
            assert!(rs_editor::etree::parse(xml.as_bytes()).is_err());
            assert!(rs_doc::doc::page_inventory(xml.as_bytes()).is_err());
            assert!(rs_editor::edit::editor_content_fingerprint(xml.as_bytes()).is_err());
            assert!(snapshot::parse_document(xml.as_bytes(), "application/xml", Some("srv-deep")).is_err());
        }
        // The bound itself still parses (payload limits reject it later).
        let xml = nested_xml(rs_doc::xml::MAX_DEPTH);
        assert!(rs_doc::xml::fromstring(xml.as_bytes()).is_ok());
        assert!(rs_editor::etree::parse(xml.as_bytes()).is_ok());
    });
}
