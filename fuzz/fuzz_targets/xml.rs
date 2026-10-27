//! Every XML reader: the ElementTree-compatible trees of rs-doc, rs-editor,
//! rs-ops, notes-cloud and rs-cloud, the structure pre-check, the Cloud
//! metadata readers (folder changes, note list), the change feed and note
//! tree of notes-protocol, the compare/upload/create response guards, the
//! PDF layout reader and the shape object reader. Input: raw XML bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;

fn walk_ops(e: &rs_ops::xml::Element) {
    for node in e.iter() {
        let _ = rs_ops::page_shapes::read_shape_object(node);
        let _ = rs_ops::page_shapes::renderer_skippable_shape(node);
    }
}

fuzz_target!(|data: &[u8]| {
    let limits = rs_doc::mime::Limits::default();
    if let Ok(root) = rs_doc::xml::fromstring(data) {
        let _ = rs_doc::xml::tostring(&root);
        let _ = rs_doc::xml::canonicalize_subtree(&root);
    }
    let _ = rs_doc::payload::parse_xml(data, &limits);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = rs_doc::xml::structure_exceeded_before(text, data.len(), 64, 4096);
        let _ = notes_protocol::parse_note_changes(text);
        let _ = notes_protocol::tree::parse_note_tree(text);
    }
    let _ = rs_doc::doc::page_inventory(data);
    if let Ok(root) = rs_editor::etree::parse(data) {
        let _ = rs_editor::etree::tostring_unicode(&root);
        let _ = rs_editor::etree::canonicalize(&root);
    }
    let _ = rs_editor::edit::editor_content_fingerprint(data);
    let _ = rs_editor::edit::title_content_fingerprint(data);
    if let Ok(root) = rs_ops::xml::parse_bytes(data) {
        let _ = rs_ops::xml::tostring(&root);
        walk_ops(&root);
        let _ = rs_ops::ink_projection::note_ink_reason(&root);
        for page in root.iter_tag("page") {
            let _ = rs_ops::ink_projection::page_ink_reason(page);
        }
    }
    for allow_dtd in [false, true] {
        if let Ok(root) = notes_cloud::etree::parse_bytes(data, allow_dtd) {
            let _ = notes_cloud::etree::tostring_unicode(&root);
        }
    }
    let _ = notes_cloud::metadata::parse_folder_changes(data);
    let _ = notes_cloud::metadata::parse_note_list(data);
    let _ = rs_render::ink_render::shape_flags(data);
    if let Ok(texts) = rs_render::pdf_layout::parse_all(&[data.to_vec()]) {
        let _ = rs_render::pdf_layout::pdf_pages(&texts);
        let files = std::collections::HashSet::new();
        let urls = std::collections::HashMap::new();
        let _ = rs_render::pdf_layout::placements(&texts, &files, &urls, 4 * 1024 * 1024);
    }
});
