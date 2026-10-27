//! ink_cache.release_page_cache over the DOM (mutations may be logged).
use super::xml::{NodeId, Tree};
use super::{b64decode_strict, doc_integer, ink, sha256_hex, InkErr, R};
use std::collections::HashMap;

struct Contract {
    identity: String,
    count: i64,
    tick: i64,
    prefix: String,
    suffix: String,
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// ink_cache.pdf_background: the page's single PDF background file id.
pub fn pdf_background(t: &Tree, page: NodeId) -> R<Option<String>> {
    let lists: Vec<NodeId> = t.children(page).iter().copied().filter(|n| t.tag(*n) == "pdfDataList").collect();
    if lists.is_empty() {
        return Ok(None);
    }
    let fail = || InkErr::Ink("unsupported PDF background metadata".into());
    let keys = |n: NodeId| -> std::collections::BTreeSet<&str> { t.nodes[n].attrs.iter().map(|(k, _)| k.as_str()).collect() };
    if lists.len() != 1 || t.len(lists[0]) != 1 || !t.nodes[lists[0]].attrs.is_empty() {
        return Err(fail());
    }
    let data = t.children(lists[0])[0];
    if t.tag(data) != "pdfData" || keys(data) != ["hash", "pageIndex"].into_iter().collect() || t.len(data) != 1 {
        return Err(fail());
    }
    let hash = t.get(data, "hash").unwrap_or("");
    if !is_sha256(hash) || doc_integer(t.get(data, "pageIndex")).is_err() {
        return Err(fail());
    }
    let rect = t.children(data)[0];
    if t.tag(rect) != "pdfRect" || t.len(rect) != 0 || keys(rect) != ["l", "t", "r", "b"].into_iter().collect() {
        return Err(fail());
    }
    let mut values = Vec::new();
    for key in ["l", "t", "r", "b"] {
        values.push(doc_integer(t.get(rect, key)).map_err(|_| fail())?);
    }
    let width = doc_integer(t.get(page, "width")).map_err(|_| fail())?;
    let height = doc_integer(t.get(page, "height")).map_err(|_| fail())?;
    if values != [0, 0, width, height] {
        return Err(fail());
    }
    Ok(Some(hash.to_string()))
}

fn file_contract(t: &Tree, node: NodeId, max_count: i64) -> R<Contract> {
    let fail = || InkErr::Ink("cache media metadata contract differs".into());
    let identity = t.get(node, "id");
    let bind = doc_integer(t.get(node, "bindNumber")).map_err(|_| fail())?;
    let count = doc_integer(t.get(node, "refCount")).map_err(|_| fail())?;
    let tick = doc_integer(t.get(node, "modifiedTime")).map_err(|_| fail())?;
    let ident = identity.unwrap_or("");
    if !(ident.len() == 64 && ident.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        || bind < 0 || !(0..=max_count).contains(&count)
    {
        return Err(fail());
    }
    let name_raw = t.get(node, "name").ok_or_else(fail)?;
    let decoded = b64decode_strict(name_raw).ok_or_else(fail)?;
    let name = String::from_utf8(decoded).map_err(|_| fail())?;
    if name.is_empty() || name.len() > 1024 || name.contains('\0') {
        return Err(fail());
    }
    let kids = t.children(node);
    if kids.len() != 1 || t.tag(kids[0]) != "fileHash" || t.nodes[kids[0]].text.as_deref() != Some(ident)
        || t.len(kids[0]) != 0
    {
        return Err(fail());
    }
    let attached = t.get_or(node, "isFileAttached", "").to_lowercase();
    if attached != "true" && attached != "false" {
        return Err(fail());
    }
    let prefix = format!("{ident}{bind}{name}");
    let suffix = format!("{count}{tick}");
    let hash = t.get(node, "hash");
    let matches: Vec<bool> = [false, true]
        .into_iter()
        .filter(|modern| {
            let s = format!("{prefix}{suffix}{}", if *modern { attached.as_str() } else { "" });
            Some(sha256_hex(s.as_bytes()).as_str()) == hash
        })
        .collect();
    if matches.len() != 1 {
        return Err(fail());
    }
    Ok(Contract { identity: ident.to_string(), count, tick, prefix,
                  suffix: if matches[0] { attached } else { String::new() } })
}

/// ink_cache.release_page_cache. `page` may be absent from the tree.
pub fn release_page_cache(t: &mut Tree, page: Option<NodeId>, tick: i128) -> R<Vec<String>> {
    let root = t.root;
    let pages = t.one(root, "pagelist")?;
    let mut pdf_refs: Vec<(String, i64)> = Vec::new();
    for candidate in t.children(pages).to_vec() {
        let bad_child = t.children(candidate).iter()
            .any(|n| !matches!(t.tag(*n), "layerList" | "drawnRect" | "canvasCacheDataMap" | "pdfDataList"));
        if t.tag(candidate) != "page" || bad_child || t.get_or(candidate, "bgImageId", "-1") != "-1"
            || !t.get_or(candidate, "templateUri", "").is_empty()
        {
            return ink("page has unsupported background or media metadata");
        }
        if let Some(reference) = pdf_background(t, candidate)? {
            match pdf_refs.iter_mut().find(|(h, _)| *h == reference) {
                Some((_, n)) => *n += 1,
                None => pdf_refs.push((reference, 1)),
            }
        }
        let layers = t.one(candidate, "layerlist")?;
        for layer in t.children(layers) {
            if t.tag(*layer) != "layer" || t.children(*layer).iter().any(|n| t.tag(*n) != "objectList") {
                return ink("unknown layer metadata");
            }
        }
    }
    let files = t.one(root, "contentfilelist")?;
    let tree = t.one(root, "notetree")?;
    let tree_files: Vec<NodeId> = t.children(tree).iter().copied().filter(|n| t.tag(*n) == "contentFileList").collect();
    if tree_files.len() > 1 {
        return ink("duplicate content-file tree");
    }
    if t.len(files) > 0 && tree_files.is_empty() {
        return ink("content-file tree missing");
    }
    let tnodes: Vec<NodeId> = tree_files.first().map(|n| t.children(*n).to_vec()).unwrap_or_default();
    if t.children(files).iter().chain(&tnodes).any(|n| t.tag(*n) != "contentFile") {
        return ink("unknown content-file entry");
    }
    let mut contracts: Vec<(i64, NodeId, Contract)> = Vec::new();
    let mut by_id: HashMap<String, NodeId> = HashMap::new();
    let page_count = t.len(pages) as i64;
    for node in t.children(files).to_vec() {
        let is_pdf = pdf_refs.iter().any(|(h, _)| Some(h.as_str()) == t.get(node, "id"));
        let c = file_contract(t, node, if is_pdf { page_count } else { 1 })?;
        let bind = doc_integer(t.get(node, "bindNumber"))?;
        if contracts.iter().any(|(b, _, _)| *b == bind) || by_id.contains_key(&c.identity) {
            return ink("duplicate cache file binding");
        }
        by_id.insert(c.identity.clone(), node);
        contracts.push((bind, node, c));
    }
    let tids: std::collections::HashSet<Option<&str>> = tnodes.iter().map(|n| t.get(*n, "id")).collect();
    let ids: std::collections::HashSet<Option<&str>> = by_id.keys().map(|k| Some(k.as_str())).collect();
    if tnodes.len() != by_id.len() || tids != ids {
        return ink("cache file tree inventory differs");
    }
    for n in &tnodes {
        let f = by_id[t.get(*n, "id").unwrap()];
        if t.get(*n, "t") != t.get(f, "modifiedTime") || t.get(*n, "h") != t.get(f, "hash") || t.len(*n) != 0 {
            return ink("cache file tree differs");
        }
    }
    let all = t.iter(root);
    let maps: Vec<NodeId> = all.iter().copied().filter(|n| t.tag(*n) == "canvasCacheDataMap").collect();
    let mut references: Vec<(i64, NodeId)> = Vec::new();
    for cache in &maps {
        let parent = t.nodes[*cache].parent;
        if parent.is_none_or(|p| t.tag(p) != "page") || t.len(*cache) != 1 {
            return ink("unsupported cache map structure");
        }
        let pair = t.children(*cache)[0];
        if t.tag(pair) != "pair" || t.len(pair) != 0 {
            return ink("unsupported cache entry");
        }
        let bind = doc_integer(t.get(pair, "fileId"))?;
        if !contracts.iter().any(|(b, _, _)| *b == bind) || references.iter().any(|(b, _)| *b == bind) {
            return ink("shared or unknown cache binding");
        }
        references.push((bind, *cache));
    }
    for n in &all {
        if t.has(*n, "fileId") && (t.tag(*n) != "pair" || !t.nodes[*n].parent.is_some_and(|p| maps.contains(&p))) {
            return ink("non-cache media reference");
        }
    }
    if pdf_refs.iter().any(|(h, _)| !by_id.contains_key(h)) {
        return ink("PDF background file missing");
    }
    for (bind, _, c) in &contracts {
        let referenced = references.iter().any(|(b, _)| b == bind);
        if let Some((_, uses)) = pdf_refs.iter().find(|(h, _)| *h == c.identity) {
            // A PDF entry is never a cache and keeps its exact metadata.
            if referenced || c.count != *uses {
                return ink("PDF reference count differs");
            }
        } else if c.count != referenced as i64 {
            return ink("cache reference count differs");
        }
    }
    let mut changed: Vec<String> = Vec::new();
    for (bind, cache) in references {
        if t.nodes[cache].parent != page || page.is_none() {
            continue;
        }
        let page = page.unwrap();
        let (_, node, c) = contracts.iter().find(|(b, _, _)| *b == bind).unwrap();
        let node = *node;
        if !(c.tick as i128) .lt(&tick) || tick >= (1i128 << 63) {
            return ink("cache metadata timestamp must advance");
        }
        let digest = sha256_hex(format!("{}0{}{}", c.prefix, tick, c.suffix).as_bytes());
        let identity = c.identity.clone();
        t.set(node, "refCount", "0");
        t.set(node, "modifiedTime", &tick.to_string());
        t.set(node, "hash", &digest);
        let target = *tnodes.iter().find(|n| t.get(**n, "id") == Some(identity.as_str())).unwrap();
        t.set(target, "t", &tick.to_string());
        t.set(target, "h", &digest);
        t.remove(page, cache);
        changed.push(identity);
    }
    changed.sort();
    Ok(changed)
}
