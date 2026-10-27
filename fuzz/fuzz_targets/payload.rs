//! Note payload / MIME layer: `rs_doc` multipart + header parsing, the read-only
//! payload walk, the ink inventory, PDF background inspection (parser only, no
//! Poppler) and the Cloud `decode_parts` (gzip + multipart).
//! Input: `<content-type>\n<body>`.
#![no_main]
use libfuzzer_sys::fuzz_target;
use notes_fuzz::split_content_type;

fuzz_target!(|data: &[u8]| {
    let (content_type, body) = split_content_type(data);
    let limits = rs_doc::mime::Limits::default();
    let _ = rs_doc::mime::parse_headers(body, &limits);
    let _ = rs_doc::mime::content_type_headers(&content_type, &limits, &mut rs_doc::mime::NoHooks);
    let _ = rs_doc::mime::parts(body, &content_type, limits.max_parts, &limits, &mut rs_doc::mime::NoHooks);
    let _ = rs_doc::mime::samsung_fast_match(body);
    if let Ok(parsed) = rs_doc::payload::parse(body, &content_type, None) {
        let _ = parsed.text();
    }
    let _ = rs_ink::core::payload::inspect_raw(body, &content_type, &limits);
    let _ = rs_ink::core::prepare::payload_parts(body, &content_type, &limits);
    if let Ok(parsed) = notes_preview::payload::parse_payload(body.to_vec(), &content_type) {
        let _ = notes_preview::payload::inspect_ink_payload(&parsed);
        let _ = notes_preview::pdf::pdf_page_indices(&parsed);
        let _ = notes_preview::pdf::inspect_pdf_backgrounds(&parsed);
    }
    let _ = notes_cloud::api::decode_parts(body, &content_type);
});
