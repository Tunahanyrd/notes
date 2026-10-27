//! Shared helpers for the fuzz targets in `fuzz_targets/`.
//!
//! Every target calls the real public entry points of the product crates and
//! only checks that they return (`Ok` or a proper error) without panicking,
//! hanging or allocating without bound. Inputs are synthetic; see
//! docs/FUZZING.md.
use notes_host::Val;

/// Split `<content-type>\n<body>` (the payload/document input format). Input
/// without a newline is a body with the Samsung multipart content type.
pub fn split_content_type(data: &[u8]) -> (String, &[u8]) {
    match data.iter().position(|&b| b == b'\n') {
        Some(i) if i <= 512 => (String::from_utf8_lossy(&data[..i]).into_owned(), &data[i + 1..]),
        _ => ("multipart/form-data; boundary=SyntheticInkBoundary".to_string(), data),
    }
}

/// `dict[key]` of a JSON-like value.
pub fn get<'a>(v: &'a Val, key: &str) -> Option<&'a Val> {
    match v {
        Val::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// Page ids listed by a `page_ops_state` / projection result (`pages[*].id`).
pub fn page_ids(state: &Val) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(Val::List(pages)) = get(state, "pages") {
        for page in pages {
            if let Some(Val::Str(id)) = get(page, "id") {
                out.push(id.clone());
            }
        }
    }
    out
}

/// First byte as a selector, the rest as the payload.
pub fn selector(data: &[u8]) -> Option<(u8, &[u8])> {
    data.split_first().map(|(s, rest)| (*s, rest))
}
