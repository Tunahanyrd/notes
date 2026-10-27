//! Minimal xml.etree.ElementTree-compatible element tree: parsing (via
//! roxmltree, expat-equivalent for well-formed input), `ET.tostring`
//! byte-exact serialization (ns0.. prefixes, escaping, `<x />`) and
//! `ET.canonicalize(ET.tostring(e, encoding='unicode'))` (C14N 2.0 defaults).
use std::collections::HashMap;

pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Element {
    /// Clark notation: `{uri}local` or `local`.
    pub tag: String,
    pub attrib: Vec<(String, String)>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn new(tag: impl Into<String>) -> Self {
        Element { tag: tag.into(), ..Default::default() }
    }
    /// `local()` from samsung_notes.document: lower-cased local name.
    pub fn local(&self) -> String {
        self.tag.rsplit('}').next().unwrap_or("").to_lowercase()
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrib.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        match self.attrib.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.attrib.push((key.to_string(), value)),
        }
    }
    pub fn pop_attr(&mut self, key: &str) {
        self.attrib.retain(|(k, _)| k != key);
    }
    /// `Element.clear()`.
    pub fn clear(&mut self) {
        self.children.clear();
        self.attrib.clear();
        self.text = None;
        self.tail = None;
    }
    /// Pre-order iteration including self (`Element.iter()`).
    pub fn iter(&self) -> Vec<&Element> {
        let mut out = Vec::new();
        fn walk<'a>(e: &'a Element, out: &mut Vec<&'a Element>) {
            out.push(e);
            for c in &e.children { walk(c, out); }
        }
        walk(self, &mut out);
        out
    }
    pub fn iter_mut_apply(&mut self, f: &mut dyn FnMut(&mut Element)) {
        f(self);
        for c in &mut self.children { c.iter_mut_apply(f); }
    }
}

#[derive(Debug, Clone)]
pub struct ParseError(pub String);

/// `ET.fromstring(bytes)`.
pub fn parse(data: &[u8]) -> Result<Element, ParseError> {
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    let owned;
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => {
            let head = String::from_utf8_lossy(&data[..data.len().min(200)]).to_ascii_lowercase();
            if head.starts_with("<?xml") && (head.contains("iso-8859-1") || head.contains("latin-1") || head.contains("latin1")) {
                owned = data.iter().map(|&b| b as char).collect::<String>();
                &owned
            } else {
                return Err(ParseError("not well-formed (invalid token): line 1, column 0".into()));
            }
        }
    };
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, options)
        .map_err(|e| ParseError(format!("{e}")))?;
    if depth_exceeds(&doc, MAX_DEPTH) {
        return Err(ParseError("XML element nesting exceeds limit".into()));
    }
    Ok(convert(doc.root_element()))
}

/// Element nesting accepted by [`parse`]: `convert`, `iter`, the serializers
/// and the tree's `Drop` recurse once per level, and a stack overflow aborts
/// the engine. Note XML reaching this parser is already limited to 64 levels
/// by rs-doc; the bound keeps any other caller safe.
pub const MAX_DEPTH: usize = 256;

/// Iterative depth check over roxmltree's flat node storage (no recursion).
fn depth_exceeds(doc: &roxmltree::Document, limit: usize) -> bool {
    let mut node = doc.root();
    let mut depth = 0usize;
    'descend: loop {
        if let Some(child) = node.first_child() {
            depth += 1;
            if child.is_element() && depth > limit {
                return true;
            }
            node = child;
            continue;
        }
        loop {
            if let Some(next) = node.next_sibling() {
                node = next;
                continue 'descend;
            }
            match node.parent() {
                Some(parent) => {
                    node = parent;
                    depth -= 1;
                }
                None => return false,
            }
        }
    }
}

fn clark(ns: Option<&str>, name: &str) -> String {
    match ns {
        Some(uri) if !uri.is_empty() => format!("{{{uri}}}{name}"),
        _ => name.to_string(),
    }
}

fn convert(node: roxmltree::Node) -> Element {
    let tag = node.tag_name();
    let mut e = Element::new(clark(tag.namespace(), tag.name()));
    for a in node.attributes() {
        e.attrib.push((clark(a.namespace(), a.name()), a.value().to_string()));
    }
    for child in node.children() {
        if child.is_element() {
            e.children.push(convert(child));
        } else if child.is_text() {
            let t = child.text().unwrap_or("");
            if t.is_empty() { continue; }
            let slot = match e.children.last_mut() { Some(last) => &mut last.tail, None => &mut e.text };
            slot.get_or_insert_with(String::new).push_str(t);
        }
    }
    e
}

const WELL_KNOWN: &[(&str, &str)] = &[
    (XML_NS, "xml"),
    ("http://www.w3.org/1999/xhtml", "html"),
    ("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "rdf"),
    ("http://schemas.xmlsoap.org/wsdl/", "wsdl"),
    ("http://www.w3.org/2001/XMLSchema", "xs"),
    ("http://www.w3.org/2001/XMLSchema-instance", "xsi"),
    ("http://purl.org/dc/elements/1.1/", "dc"),
];

struct Names {
    qnames: HashMap<String, String>,
    /// uri -> prefix in insertion order (ET `_namespaces`).
    namespaces: Vec<(String, String)>,
}

fn names(root: &Element) -> Names {
    let mut n = Names { qnames: HashMap::new(), namespaces: Vec::new() };
    fn add(n: &mut Names, qname: &str) {
        if n.qnames.contains_key(qname) { return; }
        if let Some(rest) = qname.strip_prefix('{') {
            let (uri, local) = rest.rsplit_once('}').unwrap_or((rest, ""));
            let prefix = match n.namespaces.iter().find(|(u, _)| u == uri) {
                Some((_, p)) => p.clone(),
                None => {
                    let p = WELL_KNOWN.iter().find(|(u, _)| *u == uri).map(|(_, p)| p.to_string())
                        .unwrap_or_else(|| format!("ns{}", n.namespaces.len()));
                    if p != "xml" { n.namespaces.push((uri.to_string(), p.clone())); }
                    p
                }
            };
            let q = if prefix.is_empty() { local.to_string() } else { format!("{prefix}:{local}") };
            n.qnames.insert(qname.to_string(), q);
        } else {
            n.qnames.insert(qname.to_string(), qname.to_string());
        }
    }
    for e in root.iter() {
        add(&mut n, &e.tag);
        for (k, _) in &e.attrib { add(&mut n, k); }
    }
    n
}

fn escape_cdata(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_attrib(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#09;"),
            _ => out.push(c),
        }
    }
}

fn serialize(e: &Element, names: &Names, root: bool, out: &mut String) {
    let tag = &names.qnames[&e.tag];
    out.push('<');
    out.push_str(tag);
    if root {
        let mut decls: Vec<&(String, String)> = names.namespaces.iter().collect();
        decls.sort_by(|a, b| a.1.cmp(&b.1));
        for (uri, prefix) in decls {
            out.push_str(" xmlns");
            if !prefix.is_empty() { out.push(':'); out.push_str(prefix); }
            out.push_str("=\"");
            escape_attrib(uri, out);
            out.push('"');
        }
    }
    for (k, v) in &e.attrib {
        out.push(' ');
        out.push_str(&names.qnames[k]);
        out.push_str("=\"");
        escape_attrib(v, out);
        out.push('"');
    }
    let text = e.text.as_deref().filter(|t| !t.is_empty());
    if text.is_some() || !e.children.is_empty() {
        out.push('>');
        if let Some(t) = text { escape_cdata(t, out); }
        for c in &e.children { serialize(c, names, false, out); }
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    } else {
        out.push_str(" />");
    }
    if let Some(t) = e.tail.as_deref().filter(|t| !t.is_empty()) { escape_cdata(t, out); }
}

/// `ET.tostring(e, encoding='unicode')`.
pub fn tostring_unicode(e: &Element) -> String {
    let n = names(e);
    let mut out = String::new();
    serialize(e, &n, true, &mut out);
    out
}

/// `ET.tostring(e, encoding='utf-8', xml_declaration=True)` (document.xml_bytes).
pub fn xml_bytes(e: &Element) -> Vec<u8> {
    let mut out = String::from("<?xml version='1.0' encoding='utf-8'?>\n");
    out.push_str(&tostring_unicode(e));
    out.into_bytes()
}

// ---- C14N 2.0 -------------------------------------------------------------

fn escape_cdata_c14n(s: &str, out: &mut String) {
    // The text is first re-parsed from ET.tostring output, so XML end-of-line
    // normalization applies (CR LF / CR -> LF) before escaping.
    let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
    for c in normalized.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_attrib_c14n(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            _ => out.push(c),
        }
    }
}

fn split_clark(name: &str) -> (&str, &str) {
    match name.strip_prefix('{').and_then(|r| r.rsplit_once('}')) {
        Some((uri, local)) => (uri, local),
        None => ("", name),
    }
}

/// `ET.canonicalize(ET.tostring(e, encoding='unicode'))`.
pub fn canonicalize(e: &Element) -> String {
    let n = names(e);
    let prefix_of: HashMap<&str, &str> = n.namespaces.iter().map(|(u, p)| (u.as_str(), p.as_str())).collect();
    let mut declared: Vec<Vec<String>> = Vec::new();
    let mut out = String::new();
    c14n(e, &prefix_of, &mut declared, &mut out, true);
    out
}

fn c14n(e: &Element, prefix_of: &HashMap<&str, &str>, declared: &mut Vec<Vec<String>>, out: &mut String, root: bool) {
    declared.push(Vec::new());
    let mut names: Vec<&str> = vec![e.tag.as_str()];
    names.extend(e.attrib.iter().map(|(k, _)| k.as_str()));
    names.sort_by(|a, b| {
        let key = |s: &str| -> Vec<String> { s.splitn(2, '}').map(str::to_string).collect() };
        key(a).cmp(&key(b))
    });
    let qname = |name: &str, declared: &mut Vec<Vec<String>>| -> String {
        let (uri, local) = split_clark(name);
        if uri.is_empty() { return local.to_string(); }
        let prefix = if uri == XML_NS { "xml" } else { prefix_of.get(uri).copied().unwrap_or("") };
        let known = uri == XML_NS || declared.iter().any(|level| level.iter().any(|u| u == uri));
        if !known { declared.last_mut().unwrap().push(uri.to_string()); }
        format!("{prefix}:{local}")
    };
    let mut resolved: HashMap<&str, String> = HashMap::new();
    for name in &names {
        if !resolved.contains_key(name) {
            let q = qname(name, declared);
            resolved.insert(name, q);
        }
    }
    let mut attr_list: Vec<(String, String)> = declared.last().unwrap().iter()
        .map(|uri| (format!("xmlns:{}", prefix_of.get(uri.as_str()).copied().unwrap_or("")), uri.clone()))
        .collect();
    attr_list.sort();
    let mut attrs: Vec<&(String, String)> = e.attrib.iter().collect();
    attrs.sort();
    for (k, v) in attrs {
        let (uri, local) = split_clark(k);
        attr_list.push((if uri.is_empty() { local.to_string() } else { resolved[k.as_str()].clone() }, v.clone()));
    }
    let tag = resolved[e.tag.as_str()].clone();
    out.push('<');
    out.push_str(&tag);
    for (k, v) in &attr_list {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_attrib_c14n(v, out);
        out.push('"');
    }
    out.push('>');
    if let Some(t) = e.text.as_deref() { escape_cdata_c14n(t, out); }
    for c in &e.children { c14n(c, prefix_of, declared, out, false); }
    out.push_str("</");
    out.push_str(&tag);
    out.push('>');
    declared.pop();
    if !root {
        if let Some(t) = e.tail.as_deref() { escape_cdata_c14n(t, out); }
    }
}
