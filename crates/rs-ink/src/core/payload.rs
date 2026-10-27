//! ink.inspect_ink_payload over an already parsed payload.
use super::binary::{wrapper_ok, MAX_CANVAS_DIMENSION, MAX_CANVAS_PIXELS};
use super::xml::{NodeId, Tree};
use super::{ink, InkErr, R};
use std::collections::HashMap;

/// The parts of tools.notes_payload.PayloadResult this module reads.
pub struct ParsedPayload {
    pub locked: bool,
    pub xml_documents: Vec<Vec<u8>>,
    /// (bind_number, file_hash)
    pub content_files: Vec<(Option<String>, String)>,
    /// (id, url)
    pub download_urls: Vec<(String, String)>,
    /// (name, body length, first 12 body bytes)
    pub parts: Vec<(Option<String>, usize, Vec<u8>)>,
}

impl ParsedPayload {
    /// From an rs-doc parse result over `body`.
    pub fn from_rs_doc(r: &rs_doc::payload::PayloadResult, body: &[u8]) -> Self {
        ParsedPayload {
            locked: r.locked,
            xml_documents: r.xml_parts.iter().map(|i| body[r.parts[*i].body.clone()].to_vec()).collect(),
            content_files: r.content_files.iter().map(|c| (c.bind_number.clone(), c.file_hash.clone())).collect(),
            download_urls: r.download_urls.iter().map(|d| (d.id.clone(), d.url.clone())).collect(),
            parts: r.parts.iter().map(|p| {
                let b = &body[p.body.clone()];
                (p.name.clone(), b.len(), b[..b.len().min(12)].to_vec())
            }).collect(),
        }
    }
}

/// ink.inspect_ink_payload for raw bytes (PayloadError becomes InkError).
pub fn inspect_raw(raw: &[u8], content_type: &str, limits: &rs_doc::mime::Limits) -> R<InkInventory> {
    let parsed = rs_doc::payload::parse_payload(raw, content_type, Some(limits.max_readonly_parts), limits,
                                                &mut rs_doc::mime::NoHooks)
        .map_err(|e| match InkErr::from(e) {
            InkErr::Payload(m) => InkErr::Ink(m),
            other => other,
        })?;
    inspect_ink_payload(&ParsedPayload::from_rs_doc(&parsed, raw))
}

#[derive(Clone, Debug, PartialEq)]
pub struct CanvasPreviewCandidate {
    pub page_index: usize,
    pub key: i64,
    pub width: i64,
    pub height: i64,
    pub is_dark_mode: bool,
    pub bind_number: String,
    pub sha256: String,
    pub download_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InkInventory {
    pub stroke_parts: usize,
    pub wrapped_v4000_parts: usize,
    pub page_count: usize,
    pub canvas_candidates: Vec<CanvasPreviewCandidate>,
    pub unresolved_canvas_references: usize,
}

/// ink._integer
fn canvas_integer(value: Option<&str>, minimum: i64, maximum: i64) -> R<i64> {
    let v = match value {
        Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => v,
        _ => return ink("invalid canvas integer"),
    };
    if v.len() > 10 {
        return ink("canvas integer exceeds limit");
    }
    let n: i64 = v.parse().unwrap();
    if !(minimum <= n && n <= maximum) {
        return ink("canvas integer exceeds limit");
    }
    Ok(n)
}

pub fn inspect_ink_payload(parsed: &ParsedPayload) -> R<InkInventory> {
    if parsed.locked {
        return ink("locked notes are unsupported");
    }
    let mut roots: Vec<Tree> = Vec::new();
    for xml in &parsed.xml_documents {
        roots.push(Tree::parse(xml).map_err(|_| InkErr::Payload("malformed XML".into()))?);
    }
    let mut resources: Vec<(usize, NodeId)> = Vec::new();
    for (i, t) in roots.iter().enumerate() {
        for n in t.iter(t.root) {
            if t.local(n) == "noteresource" {
                resources.push((i, n));
            }
        }
    }
    if resources.len() != 1 {
        return ink("exactly one note resource required");
    }
    let mut bindings: HashMap<&str, &str> = HashMap::new();
    for (bind, hash) in &parsed.content_files {
        let Some(bind) = bind else { continue };
        let previous = *bindings.entry(bind.as_str()).or_insert(hash.as_str());
        if previous != hash {
            return ink("ambiguous content file binding");
        }
    }
    let mut urls: HashMap<String, &str> = HashMap::new();
    for (id, url) in &parsed.download_urls {
        urls.insert(id.to_lowercase(), url.as_str());
    }
    let (ti, resource) = resources[0];
    let t = &roots[ti];
    let children = |n: NodeId, name: &str| -> Vec<NodeId> {
        t.children(n).iter().copied().filter(|c| t.local(*c) == name).collect()
    };
    let lists = children(resource, "pagelist");
    if lists.len() > 1 {
        return ink("ambiguous page list");
    }
    let pages = if lists.is_empty() { vec![] } else { children(lists[0], "page") };
    let mut candidates = Vec::new();
    let mut unresolved = 0usize;
    for (index, page) in pages.iter().enumerate() {
        let maps = children(*page, "canvascachedatamap");
        if maps.len() > 1 {
            return ink("ambiguous canvas cache map");
        }
        let mut keys = std::collections::HashSet::new();
        for cache in maps {
            for pair in t.children(cache).to_vec() {
                if t.local(pair) != "pair" {
                    return ink("unsupported canvas cache entry");
                }
                let key = canvas_integer(t.get(pair, "key"), 0, i32::MAX as i64)?;
                if !keys.insert(key) {
                    return ink("duplicate canvas cache key");
                }
                let bind = canvas_integer(t.get(pair, "fileId"), 0, i32::MAX as i64)?.to_string();
                let width = canvas_integer(t.get(pair, "width"), 1, MAX_CANVAS_DIMENSION as i64)?;
                let height = canvas_integer(t.get(pair, "height"), 1, MAX_CANVAS_DIMENSION as i64)?;
                if (width * height) as u64 > MAX_CANVAS_PIXELS {
                    return ink("canvas decoded size exceeds limit");
                }
                let dark = t.get_or(pair, "isDarkMode", "false").to_lowercase();
                if dark != "true" && dark != "false" {
                    return ink("invalid canvas theme flag");
                }
                let Some(digest) = bindings.get(bind.as_str()) else {
                    unresolved += 1;
                    continue;
                };
                candidates.push(CanvasPreviewCandidate {
                    page_index: index, key, width, height, is_dark_mode: dark == "true", bind_number: bind,
                    sha256: digest.to_string(), download_url: urls.get(*digest).map(|u| u.to_string()),
                });
            }
        }
    }
    let strokes: Vec<_> = parsed.parts.iter().filter(|p| p.0.as_deref() == Some("strokeResource")).collect();
    let wrapped = strokes.iter().filter(|p| wrapper_ok(&p.2, p.1)).count();
    Ok(InkInventory { stroke_parts: strokes.len(), wrapped_v4000_parts: wrapped, page_count: pages.len(),
                      canvas_candidates: candidates, unresolved_canvas_references: unresolved })
}
