//! Full note document: `parse_document` of a downsync response, then every
//! projection the UI and the preview pipeline compute from it (editor, ink,
//! page geometry, page style, page ops, shapes, templates, organization,
//! stroke inventory, renderer page preparation).
//! Input: `<content-type>\n<body>`.
#![no_main]
use libfuzzer_sys::fuzz_target;
use notes_fuzz::{page_ids, split_content_type};

fuzz_target!(|data: &[u8]| {
    let (content_type, body) = split_content_type(data);
    let Ok(snapshot) = notes_host::snapshot::parse_document(body, &content_type, Some("fuzz-server")) else { return };
    let _ = notes_host::areas::editor_projection(&snapshot);
    let _ = notes_host::areas::project_ink_snapshot(&snapshot, true);
    let _ = notes_cloud::editor::projection(&snapshot);
    let _ = notes_cloud::ops::stroke_inventory(&snapshot);
    let _ = notes_cloud::ops::other_opaque(&snapshot);
    let _ = notes_cloud::metadata::document_organization(&snapshot);
    let _ = rs_editor::edit::editor_content_fingerprint(&snapshot.resource_xml);
    let _ = rs_editor::edit::title_content_fingerprint(&snapshot.resource_xml);
    let _ = rs_doc::doc::page_inventory(&snapshot.resource_xml);
    let _ = rs_render::ink_render::shape_flags(&snapshot.resource_xml);

    let dict = notes_host::snapshot::to_dict(&snapshot);
    let _ = notes_host::areas::editor_projection_val(&dict);
    let _ = notes_host::areas::project_ink(&dict, false);
    let _ = notes_host::areas::page_background_state(Some(&dict));
    let _ = notes_host::areas::project_shapes(&dict);
    if let Ok(state) = notes_host::areas::page_ops_state(Some(&dict)) {
        for id in page_ids(&state).iter().take(4) {
            let _ = notes_host::areas::project_page_geometry(&dict, id);
        }
    }
    if let Ok(text) = rs_store::val::dumps(&dict, true, true) {
        let _ = notes_host::areas::templates(&[("fuzz-note".to_string(), text)]);
    }
    let _ = notes_preview::payload::inventory_from_snapshot(&dict);
    let _ = notes_preview::ink::prepare::inventory_json(&dict);
    let _ = notes_preview::ink::prepare::pen_templates(&dict);
    let _ = notes_preview::ink::prepare::page_body(&dict, 0);
});
