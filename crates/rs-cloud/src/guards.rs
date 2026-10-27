//! compareForUpsync guards (pure Rust). Two failure kinds mirror Python:
//! `Conflict` = the guard raised ConflictError() itself (default message);
//! `Invalid` = ValueError/TypeError/ParseError, which callers turn into
//! ConflictError(<caller message>).
use roxmltree::{Document, Node};
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
pub enum GuardError {
    Conflict,
    Invalid,
}
use GuardError::*;

pub const MAX_COMPARE: usize = 2 * 1024 * 1024;

/// document.normalize_commit (Err = DocumentError)
pub fn normalize_commit(value: Option<&str>) -> Result<String, ()> {
    let v = value.ok_or(())?;
    let n = v.chars().count();
    if n == 0 || n > 1024 || v.chars().any(|c| (c as u32) < 33) {
        return Err(());
    }
    Ok(match v.find("_lc") {
        Some(i) if i > 0 => v[..i].to_string(),
        _ => v.to_string(),
    })
}

fn py_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `re.search(r'<!\s*(?:DOCTYPE|ENTITY)\b', text, re.I)`
pub fn has_dtd_marker(text: &str) -> bool {
    let mut rest = text;
    while let Some(i) = rest.find("<!") {
        let after = rest[i + 2..].trim_start_matches(crate::util::py_isspace);
        for word in ["doctype", "entity"] {
            let n = word.len();
            if after.len() >= n && after.is_char_boundary(n) && after[..n].eq_ignore_ascii_case(word) {
                let next = after[n..].chars().next();
                if !next.map(py_word).unwrap_or(false) {
                    return true;
                }
            }
        }
        rest = &rest[i + 2..];
    }
    false
}

pub fn local(n: Node) -> String {
    n.tag_name().name().to_lowercase()
}

pub fn get_id<'a>(n: Node<'a, '_>) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == "id" && a.namespace().is_none()).map(|a| a.value())
}

fn has_id(n: Node) -> bool {
    get_id(n).is_some()
}

pub fn element_children(n: Node) -> usize {
    n.children().filter(|c| c.is_element()).count()
}

/// ElementTree `.text` of an element.
pub fn et_text(n: Node) -> Option<String> {
    let mut out: Option<String> = None;
    for c in n.children() {
        if c.is_element() {
            break;
        }
        if c.is_text() {
            out.get_or_insert_with(String::new).push_str(c.text().unwrap_or(""));
        }
    }
    out
}

fn elements<'a, 'i>(root: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    root.descendants().filter(|n| n.is_element())
}

fn parent_el<'a, 'i>(n: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    n.parent().filter(|p| p.is_element())
}

const TREE_KINDS: [&str; 5] = ["page", "layer", "object", "bodytext", "contentfile"];

/// Shared prelude: size, decoding, DTD, root and commit checks.
pub fn with_compare_root<T>(
    raw: Option<&[u8]>,
    baseline: Option<&str>,
    body: impl FnOnce(Node) -> Result<T, GuardError>,
) -> Result<T, GuardError> {
    let raw = match raw {
        Some(r) if r.len() <= MAX_COMPARE => r,
        _ => return Err(Conflict),
    };
    let raw = raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(raw);
    let text = std::str::from_utf8(raw).map_err(|_| Invalid)?;
    if text.contains('\0') || has_dtd_marker(text) {
        return Err(Conflict);
    }
    let doc = Document::parse(text).map_err(|_| Invalid)?;
    let root = doc.root_element();
    if local(root) != "compareforupsyncview" {
        return Err(Conflict);
    }
    let commits: Vec<Node> = elements(root).filter(|n| local(*n) == "commitid" && element_children(*n) == 0).collect();
    if commits.len() != 1 {
        return Err(Conflict);
    }
    let found = normalize_commit(et_text(commits[0]).as_deref()).map_err(|_| Invalid)?;
    let wanted = normalize_commit(baseline).map_err(|_| Invalid)?;
    if found != wanted {
        return Err(Conflict);
    }
    body(root)
}

/// CloudAPI._compare_guard: only bodyText entries with exactly `expected` ids.
pub fn compare_guard(raw: Option<&[u8]>, baseline: Option<&str>, expected: &[Option<String>]) -> Result<(), GuardError> {
    with_compare_root(raw, baseline, |root| {
        let mut ids: Vec<Option<String>> = Vec::new();
        for node in elements(root) {
            if has_id(node) {
                if local(node) != "bodytext" {
                    return Err(Conflict);
                }
                ids.push(get_id(node).map(str::to_string));
            } else if TREE_KINDS.contains(&local(node).as_str()) {
                return Err(Conflict);
            }
        }
        let set: HashSet<&Option<String>> = ids.iter().collect();
        let want: HashSet<&Option<String>> = expected.iter().collect();
        if ids.len() != set.len() || set != want {
            return Err(Conflict);
        }
        Ok(())
    })
}

/// CloudAPI._compare_ink_guard
pub fn compare_ink_guard(
    raw: Option<&[u8]>,
    baseline: Option<&str>,
    page_id: Option<&str>,
    layer_id: Option<&str>,
    stroke_id: Option<&str>,
    content_file_ids: &[Option<String>],
) -> Result<(), GuardError> {
    with_compare_root(raw, baseline, |root| {
        let expected = [("page", page_id), ("layer", layer_id), ("object", stroke_id)];
        let mut found: Vec<(&str, Node)> = Vec::new();
        let mut found_files: Vec<Option<String>> = Vec::new();
        let unique: HashSet<&Option<String>> = content_file_ids.iter().collect();
        if unique.len() != content_file_ids.len() {
            return Err(Conflict);
        }
        for node in elements(root) {
            let kind = local(node);
            let id = get_id(node).map(str::to_string);
            if kind == "contentfile" && content_file_ids.contains(&id) {
                if found_files.contains(&id) || element_children(node) > 0 {
                    return Err(Conflict);
                }
                let parent = parent_el(node);
                let ok = match parent {
                    Some(p) => local(p) == "contentfilelist" && parent_el(p) == Some(root),
                    None => false,
                };
                if !ok {
                    return Err(Conflict);
                }
                found_files.push(id);
                continue;
            }
            if has_id(node) || TREE_KINDS.contains(&kind.as_str()) {
                let slot = expected.iter().find(|(k, _)| *k == kind);
                match slot {
                    Some((k, want)) if !found.iter().any(|(f, _)| f == k) && id.as_deref() == *want => found.push((k, node)),
                    _ => return Err(Conflict),
                }
            }
        }
        let files: HashSet<&Option<String>> = found_files.iter().collect();
        if found.len() != 3 || files != unique {
            return Err(Conflict);
        }
        let get = |k: &str| found.iter().find(|(f, _)| *f == k).map(|(_, n)| *n).unwrap();
        for (kind, container, ancestor) in [("object", "objectlist", "layer"), ("layer", "layerlist", "page")] {
            match parent_el(get(kind)) {
                Some(p) if local(p) == container && parent_el(p) == Some(get(ancestor)) => {}
                _ => return Err(Conflict),
            }
        }
        match parent_el(get("page")) {
            Some(p) if local(p) == "pagelist" && parent_el(p) == Some(root) => Ok(()),
            _ => Err(Conflict),
        }
    })
}

fn under_root_list(node: Node, root: Node, list: &str) -> bool {
    match parent_el(node) {
        Some(p) => local(p) == list && parent_el(p) == Some(root),
        None => false,
    }
}

/// CloudAPI._compare_page_guard
pub fn compare_page_guard(raw: Option<&[u8]>, baseline: Option<&str>, page_ids: &[Option<String>]) -> Result<(), GuardError> {
    with_compare_root(raw, baseline, |root| {
        let mut found: HashSet<Option<String>> = HashSet::new();
        for node in elements(root) {
            let kind = local(node);
            if has_id(node) || TREE_KINDS.contains(&kind.as_str()) {
                let id = get_id(node).map(str::to_string);
                if kind != "page" || !page_ids.contains(&id) || found.contains(&id) {
                    return Err(Conflict);
                }
                if !under_root_list(node, root, "pagelist") {
                    return Err(Conflict);
                }
                found.insert(id);
            }
        }
        let want: HashSet<Option<String>> = page_ids.iter().cloned().collect();
        if found != want {
            return Err(Conflict);
        }
        Ok(())
    })
}

/// CloudAPI._compare_new_pages_guard
pub fn compare_new_pages_guard(
    raw: Option<&[u8]>,
    baseline: Option<&str>,
    page_ids: &[Option<String>],
    layer_ids: &[Option<String>],
    file_id: Option<&str>,
) -> Result<(), GuardError> {
    with_compare_root(raw, baseline, |root| {
        let mut found: HashSet<Option<String>> = HashSet::new();
        let file_id = file_id.map(str::to_string);
        for node in elements(root) {
            let kind = local(node);
            if !has_id(node) && !TREE_KINDS.contains(&kind.as_str()) {
                continue;
            }
            let id = get_id(node).map(str::to_string);
            if found.contains(&id) {
                return Err(Conflict);
            }
            let container = parent_el(node);
            let ok = if kind == "page" && page_ids.contains(&id) {
                under_root_list(node, root, "pagelist")
            } else if kind == "layer" && layer_ids.contains(&id) {
                let page = container.and_then(parent_el);
                match (container, page) {
                    (Some(c), Some(p)) => {
                        let pid = get_id(p).map(str::to_string);
                        local(c) == "layerlist"
                            && local(p) == "page"
                            && page_ids.contains(&pid)
                            && layer_ids.iter().position(|x| *x == id) == page_ids.iter().position(|x| *x == pid)
                    }
                    _ => false,
                }
            } else if kind == "contentfile" && id == file_id {
                under_root_list(node, root, "contentfilelist")
            } else {
                false
            };
            if !ok {
                return Err(Conflict);
            }
            found.insert(id);
        }
        let mut want: HashSet<Option<String>> = page_ids.iter().cloned().collect();
        want.extend(layer_ids.iter().cloned());
        want.insert(file_id);
        if found != want {
            return Err(Conflict);
        }
        Ok(())
    })
}

/// cloud_page_ops.compare_page_ops_guard
pub fn compare_page_ops_guard(
    raw: Option<&[u8]>,
    baseline: Option<&str>,
    pages: &[Option<String>],
    layers: &[(Option<String>, Option<String>)],
    files: &[Option<String>],
) -> Result<(), GuardError> {
    with_compare_root(raw, baseline, |root| {
        let mut found: HashSet<Option<String>> = HashSet::new();
        for node in elements(root) {
            let kind = local(node);
            if !has_id(node) && !TREE_KINDS.contains(&kind.as_str()) {
                continue;
            }
            let id = get_id(node).map(str::to_string);
            let container = parent_el(node);
            if found.contains(&id) {
                return Err(Conflict);
            }
            let layer_page = layers.iter().rev().find(|(l, _)| *l == id).map(|(_, p)| p.clone());
            let ok = if kind == "page" && pages.contains(&id) {
                under_root_list(node, root, "pagelist")
            } else if kind == "layer" && layer_page.is_some() {
                let page = container.and_then(parent_el);
                match (container, page) {
                    (Some(c), Some(p)) => {
                        local(c) == "layerlist" && local(p) == "page" && get_id(p).map(str::to_string) == layer_page.unwrap()
                    }
                    _ => false,
                }
            } else if kind == "contentfile" && files.contains(&id) {
                under_root_list(node, root, "contentfilelist") && element_children(node) == 0
            } else {
                false
            };
            if !ok {
                return Err(Conflict);
            }
            found.insert(id);
        }
        let mut want: HashSet<Option<String>> = pages.iter().cloned().collect();
        want.extend(layers.iter().map(|(l, _)| l.clone()));
        want.extend(files.iter().cloned());
        if found != want {
            return Err(Conflict);
        }
        Ok(())
    })
}

// ------------------------------------------------------------ responses

#[derive(Debug)]
pub enum ViewError {
    Value(&'static str),
    Parse(String),
}

/// tools/notes_sync_probe.default_view -> ordered (name, text) pairs.
pub fn default_view(raw: &[u8]) -> Result<Vec<(String, String)>, ViewError> {
    let upper = raw.to_ascii_uppercase();
    let contains = |needle: &[u8]| upper.windows(needle.len()).any(|w| w == needle);
    if raw.len() > MAX_COMPARE || contains(b"<!DOCTYPE") || contains(b"<!ENTITY") {
        return Err(ViewError::Value("unsafe default view XML"));
    }
    let text = std::str::from_utf8(raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(raw))
        .map_err(|e| ViewError::Parse(e.to_string()))?;
    let doc = Document::parse(text).map_err(|e| ViewError::Parse(e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "DefaultView" {
        return Err(ViewError::Value("unexpected create response root"));
    }
    let mut result: Vec<(String, String)> = Vec::new();
    for node in root.children().filter(|n| n.is_element()) {
        let name = node.tag_name().name();
        if name == "resourceId" || name == "commitId" {
            let text = et_text(node).unwrap_or_default();
            if result.iter().any(|(k, _)| k == name) || element_children(node) > 0 || text.is_empty() || text.chars().count() > 1024 {
                return Err(ViewError::Value("invalid create response identity"));
            }
            result.push((name.to_string(), text));
        }
    }
    if result.len() != 2 {
        return Err(ViewError::Value("create response is missing identity or commit"));
    }
    Ok(result)
}

#[derive(Debug, PartialEq)]
pub enum UploadAnswer {
    Unparsable,
    Ambiguous,
    Missing,
    Found(String, Vec<(String, String)>),
}

/// The createUploadURLs answer scan of CloudAPI.upload_content_file.
pub fn find_upload_entry(raw: &[u8], file_hash: &str) -> UploadAnswer {
    let text = match std::str::from_utf8(raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(raw)) {
        Ok(t) => t,
        Err(_) => return UploadAnswer::Unparsable,
    };
    let opt = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = match Document::parse_with_options(text, opt) {
        Ok(d) => d,
        Err(_) => return UploadAnswer::Unparsable,
    };
    let mut found: Option<(String, Vec<(String, String)>)> = None;
    for group in elements(doc.root_element()) {
        let kind = local(group);
        if !matches!(kind.as_str(), "created" | "duplicated" | "failed") {
            continue;
        }
        for view in elements(group) {
            if view == group {
                continue;
            }
            let kids: Vec<Node> = view.children().filter(|n| n.is_element()).collect();
            if !kids.iter().any(|n| local(*n) == "hash") {
                continue;
            }
            let mut fields: Vec<(String, String)> = Vec::new();
            for n in kids {
                let key = local(n);
                let value = crate::util::py_strip(&et_text(n).unwrap_or_default()).to_string();
                match fields.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 = value,
                    None => fields.push((key, value)),
                }
            }
            let hash = fields.iter().find(|(k, _)| k == "hash").map(|(_, v)| v.to_lowercase()).unwrap_or_default();
            if hash == file_hash {
                if found.is_some() {
                    return UploadAnswer::Ambiguous;
                }
                found = Some((kind.clone(), fields));
            }
        }
    }
    match found {
        Some((k, f)) => UploadAnswer::Found(k, f),
        None => UploadAnswer::Missing,
    }
}
