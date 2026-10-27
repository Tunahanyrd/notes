//! JSON values a local client sends in API bodies, decoded like the server
//! (`pyjson::loads` -> `from_json`) and handed to the pure validators the
//! engine runs before touching storage or the Cloud: preferences, shortcuts,
//! the rich editor model, ink points, shape intents, text fields.
//! Input: one selector byte, then a JSON body.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((sel, body)) = notes_fuzz::selector(data) else { return };
    let Ok(json) = rs_engine::pyjson::loads(body) else { return };
    let v = notes_engine::util::from_json(&json);
    match sel % 8 {
        0 => {
            let _ = rs_store::preferences::validate_preferences(&v);
        }
        1 => {
            let _ = rs_store::shortcuts::validate_shortcuts(&v);
            let _ = rs_store::shortcuts::prepare(&v, || Ok(rs_store::shortcuts::default_shortcuts()));
        }
        2 => {
            if let Ok(model) = notes_host::areas::normalize_model(&v) {
                let _ = notes_cloud::editor::editor_model_fingerprint(&model);
                let _ = notes_cloud::editor::new_editor_document("Başlık", &model, None, 1_700_000_000_000);
            }
        }
        3 => {
            let _ = notes_host::areas::samples_from_points(&v);
            let _ = notes_cloud::writes::samples_from_points(&v);
        }
        4 => {
            let _ = notes_cloud::ops::shape_intent(&v);
        }
        5 => {
            let title = notes_fuzz::get(&v, "title").cloned().unwrap_or(notes_host::Val::Null);
            let text = notes_fuzz::get(&v, "body").cloned().unwrap_or(notes_host::Val::Null);
            let _ = rs_store::storage::validate_text(&title, &text);
            let _ = rs_store::storage::normalize_commit_val(&v);
        }
        6 => {
            let _ = rs_store::organization_store::favorite_from_document(&v);
            let _ = notes_cloud::metadata::document_organization(&v);
        }
        _ => {
            let _ = notes_host::areas::editor_projection_val(&v);
            let _ = notes_host::areas::project_ink(&v, true);
            let _ = notes_host::areas::page_ops_state(Some(&v));
            let _ = notes_host::areas::page_background_state(Some(&v));
            let _ = notes_host::areas::project_shapes(&v);
        }
    }
});
