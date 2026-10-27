//! Samsung Cloud response bodies as the transaction layer reads them:
//! compare guards (body text, ink, page, new pages, page ops), the create
//! response view, the createUploadURLs scan, the change-feed export and the
//! JSON loader. Input: one selector byte, then the response body.
#![no_main]
use libfuzzer_sys::fuzz_target;
use notes_cloud::guards;

fn ids(v: &[&str]) -> Vec<Option<String>> {
    v.iter().map(|s| Some(s.to_string())).collect()
}

fuzz_target!(|data: &[u8]| {
    let Some((sel, body)) = notes_fuzz::selector(data) else { return };
    let baseline = Some("synthetic-commit");
    let raw = Some(body);
    match sel % 8 {
        0 => {
            let _ = guards::compare_guard(raw, baseline, &ids(&["a", "b"]));
            let _ = guards::compare_guard(raw, None, &[]);
        }
        1 => {
            let _ = guards::compare_ink_guard(raw, baseline, Some("p"), Some("l"), Some("o"), &ids(&["f"]));
        }
        2 => {
            let _ = guards::compare_page_guard(raw, baseline, &ids(&["p1", "p2"]));
            let _ = guards::compare_new_pages_guard(raw, baseline, &ids(&["p1"]), &ids(&["l1"]), Some("f"));
        }
        3 => {
            let layers = vec![(Some("p1".to_string()), Some("l1".to_string()))];
            let _ = guards::compare_page_ops_guard(raw, baseline, &ids(&["p1"]), &layers, &ids(&["f"]));
        }
        4 => {
            let _ = guards::default_view(body);
        }
        5 => {
            let _ = guards::find_upload_entry(body, "c95f605839212fa15cff4ab1b30b4fd94cc0183e0e76ab6f5b5ec5e6d7b6e1d5");
        }
        6 => {
            let _ = notes_cloud::json::loads(body);
            let _ = notes_cloud::util::gunzip_limited(body, 1 << 20);
        }
        _ => {
            if let Ok(text) = std::str::from_utf8(body) {
                if let Ok(page) = notes_protocol::parse_note_changes(text) {
                    let _ = format!("{page:?}");
                }
            }
        }
    }
});
