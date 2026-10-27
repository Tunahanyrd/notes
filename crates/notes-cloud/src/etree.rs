//! A small mutable element tree with xml.etree.ElementTree semantics:
//! `fromstring` (text/tail model, Clark-notation names, no comments/PIs) and
//! byte-identical `tostring` for the encodings cloud code uses.
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    /// Clark notation: `{uri}local` or `local`.
    pub tag: String,
    pub attrib: Vec<(String, String)>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<Element>,
}

fn clark(ns: Option<&str>, local: &str) -> String {
    match ns {
        Some(uri) if !uri.is_empty() => format!("{{{}}}{}", uri, local),
        _ => local.to_string(),
    }
}

/// `document.local` / ElementTree `tag.rsplit('}', 1)[-1].lower()`
pub fn local_lower(tag: &str) -> String {
    tag.rsplit('}').next().unwrap_or(tag).to_lowercase()
}

impl Element {
    pub fn new(tag: &str) -> Element {
        Element { tag: tag.to_string(), attrib: Vec::new(), text: None, tail: None, children: Vec::new() }
    }
    pub fn local(&self) -> String {
        local_lower(&self.tag)
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrib.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    pub fn set(&mut self, key: &str, value: &str) {
        match self.attrib.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value.to_string(),
            None => self.attrib.push((key.to_string(), value.to_string())),
        }
    }
    pub fn remove_attr(&mut self, key: &str) {
        self.attrib.retain(|(k, _)| k != key);
    }
    /// `Element.clear()`
    pub fn clear(&mut self) {
        self.attrib.clear();
        self.text = None;
        self.tail = None;
        self.children.clear();
    }
    /// Pre-order like `Element.iter()` (includes self).
    pub fn iter(&self) -> Vec<&Element> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.iter());
        }
        out
    }
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(|c| c.count()).sum::<usize>()
    }
}

/// `ET.fromstring(text)`; `allow_dtd` mirrors expat accepting an internal DTD.
pub fn parse(text: &str, allow_dtd: bool) -> Result<Element, String> {
    let opt = roxmltree::ParsingOptions { allow_dtd, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, opt).map_err(|e| e.to_string())?;
    if depth_exceeds(&doc, MAX_DEPTH) {
        return Err("XML element nesting exceeds limit".into());
    }
    Ok(convert(doc.root_element()))
}

/// Element nesting accepted by [`parse`]. `convert`, `iter`, `count` and the
/// tree's `Drop` recurse once per level; Cloud responses are a few levels
/// deep and note XML is already limited to 64 levels by rs-doc. Without this
/// bound a hostile multi-megabyte response of nested tags overflowed the
/// thread stack, which aborts the whole engine process (not a catchable
/// panic).
pub const MAX_DEPTH: usize = 256;

/// Iterative depth check: first-child / next-sibling / parent walk over
/// roxmltree's flat node storage, no recursion.
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

pub fn parse_bytes(raw: &[u8], allow_dtd: bool) -> Result<Element, String> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(raw);
    let text = std::str::from_utf8(raw).map_err(|e| e.to_string())?;
    parse(text, allow_dtd)
}

fn convert(node: roxmltree::Node) -> Element {
    let tag = node.tag_name();
    let mut e = Element::new(&clark(tag.namespace(), tag.name()));
    for a in node.attributes() {
        e.attrib.push((clark(a.namespace(), a.name()), a.value().to_string()));
    }
    for child in node.children() {
        if child.is_element() {
            e.children.push(convert(child));
        } else if child.is_text() {
            let t = child.text().unwrap_or("");
            let slot = match e.children.last_mut() {
                Some(last) => &mut last.tail,
                None => &mut e.text,
            };
            slot.get_or_insert_with(String::new).push_str(t);
        }
    }
    e
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

fn well_known(uri: &str) -> Option<&'static str> {
    Some(match uri {
        "http://www.w3.org/XML/1998/namespace" => "xml",
        "http://www.w3.org/1999/xhtml" => "html",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#" => "rdf",
        "http://schemas.xmlsoap.org/wsdl/" => "wsdl",
        "http://www.w3.org/2001/XMLSchema" => "xs",
        "http://www.w3.org/2001/XMLSchema-instance" => "xsi",
        "http://purl.org/dc/elements/1.1/" => "dc",
        _ => return None,
    })
}

/// ElementTree._namespaces
fn namespaces(root: &Element) -> (HashMap<String, String>, Vec<(String, String)>) {
    let mut qnames: HashMap<String, String> = HashMap::new();
    let mut ns: Vec<(String, String)> = Vec::new(); // uri -> prefix, insertion order
    let add = |q: &str, qnames: &mut HashMap<String, String>, ns: &mut Vec<(String, String)>| {
        if qnames.contains_key(q) {
            return;
        }
        if let Some(rest) = q.strip_prefix('{') {
            let (uri, tag) = rest.rsplit_once('}').unwrap_or((rest, ""));
            let prefix = match ns.iter().find(|(u, _)| u == uri) {
                Some((_, p)) => p.clone(),
                None => {
                    let p = well_known(uri).map(str::to_string).unwrap_or_else(|| format!("ns{}", ns.len()));
                    if p != "xml" {
                        ns.push((uri.to_string(), p.clone()));
                    }
                    p
                }
            };
            let value = if prefix.is_empty() { tag.to_string() } else { format!("{}:{}", prefix, tag) };
            qnames.insert(q.to_string(), value);
        } else {
            qnames.insert(q.to_string(), q.to_string());
        }
    };
    for e in root.iter() {
        add(&e.tag, &mut qnames, &mut ns);
        for (k, _) in &e.attrib {
            add(k, &mut qnames, &mut ns);
        }
    }
    (qnames, ns)
}

fn serialize(e: &Element, qnames: &HashMap<String, String>, ns: Option<&[(String, String)]>, out: &mut String) {
    let tag = &qnames[&e.tag];
    out.push('<');
    out.push_str(tag);
    if let Some(ns) = ns {
        let mut sorted: Vec<&(String, String)> = ns.iter().collect();
        sorted.sort_by(|a, b| a.1.cmp(&b.1));
        for (uri, prefix) in sorted {
            out.push_str(" xmlns");
            if !prefix.is_empty() {
                out.push(':');
                out.push_str(prefix);
            }
            out.push_str("=\"");
            escape_attrib(uri, out);
            out.push('"');
        }
    }
    for (k, v) in &e.attrib {
        out.push(' ');
        out.push_str(&qnames[k]);
        out.push_str("=\"");
        escape_attrib(v, out);
        out.push('"');
    }
    let text = e.text.as_deref().unwrap_or("");
    if !text.is_empty() || !e.children.is_empty() {
        out.push('>');
        escape_cdata(text, out);
        for c in &e.children {
            serialize(c, qnames, None, out);
        }
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    } else {
        out.push_str(" />");
    }
    if let Some(tail) = &e.tail {
        escape_cdata(tail, out);
    }
}

/// `ET.tostring(e, encoding='unicode')`
pub fn tostring_unicode(e: &Element) -> String {
    let (qnames, ns) = namespaces(e);
    let mut out = String::new();
    serialize(e, &qnames, if ns.is_empty() { None } else { Some(&ns) }, &mut out);
    out
}

/// `ET.tostring(e, encoding='utf-8', xml_declaration=True)`
pub fn tostring_utf8_decl(e: &Element) -> Vec<u8> {
    let mut s = String::from("<?xml version='1.0' encoding='utf-8'?>\n");
    s.push_str(&tostring_unicode(e));
    s.into_bytes()
}

/// `ET.tostring(e)` (us-ascii with xmlcharrefreplace, no declaration)
pub fn tostring_ascii(e: &Element) -> Vec<u8> {
    let mut out = String::new();
    for c in tostring_unicode(e).chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            out.push_str(&format!("&#{};", c as u32));
        }
    }
    out.into_bytes()
}
