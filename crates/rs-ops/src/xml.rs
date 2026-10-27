//! Minimal mutable element tree with xml.etree.ElementTree semantics:
//! `fromstring` (expat-equivalent text/tail model, comments and PIs dropped)
//! and `tostring(encoding='utf-8', xml_declaration=True)` byte-exact output.

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Element {
    pub tag: String,
    pub attrib: Vec<(String, String)>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn new(tag: &str, attrs: &[(&str, &str)]) -> Self {
        let mut e = Element { tag: tag.to_string(), ..Default::default() };
        for (k, v) in attrs {
            e.set(k, v);
        }
        e
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrib.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).unwrap_or(default)
    }

    pub fn has(&self, key: &str) -> bool {
        self.attrib.iter().any(|(k, _)| k == key)
    }

    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(slot) = self.attrib.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value.to_string();
        } else {
            self.attrib.push((key.to_string(), value.to_string()));
        }
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// `tag.rsplit('}', 1)[-1].lower()`
    pub fn local(&self) -> String {
        local_name(&self.tag)
    }

    pub fn find(&self, tag: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.tag == tag)
    }

    pub fn find_mut(&mut self, tag: &str) -> Option<&mut Element> {
        self.children.iter_mut().find(|c| c.tag == tag)
    }

    pub fn find_index(&self, tag: &str) -> Option<usize> {
        self.children.iter().position(|c| c.tag == tag)
    }

    pub fn findall(&self, tag: &str) -> Vec<&Element> {
        self.children.iter().filter(|c| c.tag == tag).collect()
    }

    /// ElementTree `find('a/b')` for plain child paths.
    pub fn find_path(&self, path: &str) -> Option<&Element> {
        let mut current = self;
        for step in path.split('/') {
            current = current.find(step)?;
        }
        Some(current)
    }

    pub fn sub_element(&mut self, tag: &str, attrs: &[(&str, &str)]) -> &mut Element {
        self.children.push(Element::new(tag, attrs));
        self.children.last_mut().unwrap()
    }

    /// Pre-order iteration including self (ElementTree `iter()`).
    pub fn iter(&self) -> Iter<'_> {
        Iter { stack: vec![self] }
    }

    pub fn iter_tag<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.iter().filter(move |e| e.tag == tag)
    }

    pub fn attr_key_set(&self) -> std::collections::BTreeSet<&str> {
        self.attrib.iter().map(|(k, _)| k.as_str()).collect()
    }
}

pub struct Iter<'a> {
    stack: Vec<&'a Element>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a Element;
    fn next(&mut self) -> Option<&'a Element> {
        let e = self.stack.pop()?;
        for c in e.children.iter().rev() {
            self.stack.push(c);
        }
        Some(e)
    }
}

pub fn local_name(tag: &str) -> String {
    tag.rsplit('}').next().unwrap_or("").to_lowercase()
}

/// Parse like `ET.fromstring` on bytes (expat honours a UTF-8 BOM).
pub fn parse_bytes(raw: &[u8]) -> Result<Element, String> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    let text = std::str::from_utf8(raw).map_err(|e| format!("not well-formed (invalid token): {e}"))?;
    parse_str(text)
}

pub fn parse_str(text: &str) -> Result<Element, String> {
    let options = roxmltree::ParsingOptions { allow_dtd: false, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, options).map_err(|e| e.to_string())?;
    if depth_exceeds(&doc, MAX_DEPTH) {
        return Err("XML element nesting exceeds limit".into());
    }
    Ok(convert(doc.root_element()))
}

/// Element nesting accepted by [`parse_str`]: `convert`, `tostring` and the
/// tree's `Drop` recurse once per level, and a stack overflow aborts the
/// engine (not a catchable panic). Note XML reaching this parser is already
/// limited to 64 levels by rs-doc; the bound keeps every other caller safe
/// (found by fuzz/ `xml`: 100 000 nested `<a>` overflowed the stack).
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

fn qualified(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(ns) if !ns.is_empty() => format!("{{{ns}}}{name}"),
        _ => name.to_string(),
    }
}

fn convert(node: roxmltree::Node) -> Element {
    let tag = node.tag_name();
    let mut e = Element { tag: qualified(tag.namespace(), tag.name()), ..Default::default() };
    for a in node.attributes() {
        e.attrib.push((qualified(a.namespace(), a.name()), a.value().to_string()));
    }
    let mut pending: Option<String> = None;
    for child in node.children() {
        if child.is_element() {
            let mut c = convert(child);
            let text = pending.take();
            if e.children.is_empty() {
                e.text = text;
            } else {
                e.children.last_mut().unwrap().tail = text;
            }
            c.tail = None;
            e.children.push(c);
        } else if child.is_text() {
            let t = child.text().unwrap_or("");
            match pending.as_mut() {
                Some(p) => p.push_str(t),
                None => pending = Some(t.to_string()),
            }
        }
    }
    let text = pending.take().filter(|t| !t.is_empty());
    if e.children.is_empty() {
        e.text = text;
    } else {
        e.children.last_mut().unwrap().tail = text;
    }
    if e.text.as_deref() == Some("") {
        e.text = None;
    }
    e
}

fn escape_cdata(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn escape_attrib(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' => out.push_str("&#10;"),
            '\t' => out.push_str("&#09;"),
            c => out.push(c),
        }
    }
}

const WELL_KNOWN: &[(&str, &str)] = &[
    ("http://www.w3.org/XML/1998/namespace", "xml"),
    ("http://www.w3.org/1999/xhtml", "html"),
    ("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "rdf"),
    ("http://schemas.xmlsoap.org/wsdl/", "wsdl"),
    ("http://www.w3.org/2001/XMLSchema", "xs"),
    ("http://www.w3.org/2001/XMLSchema-instance", "xsi"),
    ("http://purl.org/dc/elements/1.1/", "dc"),
];

struct Names {
    namespaces: Vec<(String, String)>, // uri -> prefix, insertion order
}

impl Names {
    fn qname(&mut self, name: &str) -> String {
        if let Some(rest) = name.strip_prefix('{') {
            if let Some(pos) = rest.rfind('}') {
                let (uri, local) = (&rest[..pos], &rest[pos + 1..]);
                let prefix = match self.namespaces.iter().find(|(u, _)| u == uri) {
                    Some((_, p)) => p.clone(),
                    None => {
                        let p = WELL_KNOWN
                            .iter()
                            .find(|(u, _)| *u == uri)
                            .map(|(_, p)| p.to_string())
                            .unwrap_or_else(|| format!("ns{}", self.namespaces.len()));
                        if p != "xml" {
                            self.namespaces.push((uri.to_string(), p.clone()));
                        }
                        p
                    }
                };
                return format!("{prefix}:{local}");
            }
        }
        name.to_string()
    }
}

fn write(e: &Element, out: &mut String, names: &mut Names, decls: Option<&str>) {
    let tag = names.qname(&e.tag);
    out.push('<');
    out.push_str(&tag);
    if let Some(d) = decls {
        out.push_str(d);
    }
    for (k, v) in &e.attrib {
        out.push(' ');
        let key = names.qname(k);
        out.push_str(&key);
        out.push_str("=\"");
        escape_attrib(v, out);
        out.push('"');
    }
    let has_text = e.text.as_deref().is_some_and(|t| !t.is_empty());
    if has_text || !e.children.is_empty() {
        out.push('>');
        if let Some(t) = &e.text {
            escape_cdata(t, out);
        }
        for c in &e.children {
            write(c, out, names, None);
        }
        out.push_str("</");
        out.push_str(&tag);
        out.push('>');
    } else {
        out.push_str(" />");
    }
    if let Some(t) = &e.tail {
        escape_cdata(t, out);
    }
}

/// `ET.tostring(root, encoding='unicode')` body (no declaration).
pub fn tostring(root: &Element) -> String {
    let mut names = Names { namespaces: Vec::new() };
    let namespaced = root.iter().any(|e| e.tag.starts_with('{') || e.attrib.iter().any(|(k, _)| k.starts_with('{')));
    let mut out = String::new();
    if namespaced {
        // Collect prefixes in ElementTree's order first (tags, then attribute keys).
        for e in root.iter() {
            names.qname(&e.tag);
            for (k, _) in &e.attrib {
                names.qname(k);
            }
        }
        let mut sorted = names.namespaces.clone();
        sorted.sort_by(|a, b| a.1.cmp(&b.1));
        let mut decl = String::new();
        for (uri, prefix) in sorted {
            decl.push_str(" xmlns");
            if !prefix.is_empty() {
                decl.push(':');
                decl.push_str(&prefix);
            }
            decl.push_str("=\"");
            escape_attrib(&uri, &mut decl);
            decl.push('"');
        }
        write(root, &mut out, &mut names, Some(&decl));
    } else {
        write(root, &mut out, &mut names, None);
    }
    out
}

/// `document.xml_bytes`: `ET.tostring(root, encoding='utf-8', xml_declaration=True)`.
pub fn xml_bytes(root: &Element) -> Vec<u8> {
    let mut out = String::from("<?xml version='1.0' encoding='utf-8'?>\n");
    out.push_str(&tostring(root));
    out.into_bytes()
}

#[cfg(test)]
mod depth_tests {
    use super::*;

    #[test]
    fn deep_nesting_is_an_error_not_a_stack_overflow() {
        let n = 100_000;
        let text = format!("{}{}", "<a>".repeat(n), "</a>".repeat(n));
        assert_eq!(parse_str(&text).unwrap_err(), "XML element nesting exceeds limit");
        let ok = format!("{}{}", "<a>".repeat(MAX_DEPTH), "</a>".repeat(MAX_DEPTH));
        assert!(parse_str(&ok).is_ok());
        let over = format!("{}{}", "<a>".repeat(MAX_DEPTH + 1), "</a>".repeat(MAX_DEPTH + 1));
        assert!(parse_str(&over).is_err());
    }
}
