//! Minimal ElementTree-compatible XML tree (pure Rust).
//!
//! Parsing follows `xml.etree.ElementTree` (expat + TreeBuilder) semantics:
//! namespaced names become `{uri}local`, comments and processing
//! instructions are dropped (surrounding text is merged), text before the
//! first child is `text` and text after an element is its `tail`.
//! Serialization is byte-identical to `ET.tostring(..., encoding='utf-8')`
//! and `canonicalize_subtree` reproduces `ET.canonicalize(ET.tostring(e))`.
use std::borrow::Cow;
use std::collections::HashMap;

use crate::error::{Error, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    /// `{namespace}local` or `local`.
    pub tag: String,
    /// Attributes in document/insertion order (dict semantics: unique keys).
    pub attrs: Vec<(String, String)>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn new(tag: impl Into<String>) -> Self {
        Element { tag: tag.into(), ..Default::default() }
    }

    pub fn with_attrs(tag: impl Into<String>, attrs: &[(&str, &str)]) -> Self {
        let mut e = Element::new(tag);
        for (k, v) in attrs {
            e.set(*k, *v);
        }
        e
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// `element.set(key, value)`: keeps the position of an existing key.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if let Some(slot) = self.attrs.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.attrs.push((key, value));
        }
    }

    pub fn has_attr(&self, key: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == key)
    }

    pub fn push(&mut self, child: Element) -> &mut Element {
        self.children.push(child);
        self.children.last_mut().unwrap()
    }

    /// `node.tag.rsplit('}', 1)[-1].lower()` (samsung_notes.document.local).
    pub fn local(&self) -> String {
        local_lower(&self.tag)
    }

    /// Pre-order iteration (`element.iter()`), including `self`.
    pub fn iter(&self) -> PreOrder<'_> {
        PreOrder { stack: vec![self] }
    }

    /// Child by path of indices.
    pub fn at(&self, path: &[usize]) -> &Element {
        let mut node = self;
        for &i in path {
            node = &node.children[i];
        }
        node
    }

    pub fn at_mut(&mut self, path: &[usize]) -> &mut Element {
        let mut node = self;
        for &i in path {
            node = &mut node.children[i];
        }
        node
    }

    /// Number of elements in the subtree (including self) and maximum depth (self = 1).
    pub fn shape(&self) -> (usize, usize) {
        let mut count = 0;
        let mut max_depth = 0;
        let mut stack = vec![(self, 1usize)];
        while let Some((node, depth)) = stack.pop() {
            count += 1;
            max_depth = max_depth.max(depth);
            for child in &node.children {
                stack.push((child, depth + 1));
            }
        }
        (count, max_depth)
    }
}

pub struct PreOrder<'a> {
    stack: Vec<&'a Element>,
}

impl<'a> Iterator for PreOrder<'a> {
    type Item = &'a Element;
    fn next(&mut self) -> Option<&'a Element> {
        let node = self.stack.pop()?;
        for child in node.children.iter().rev() {
            self.stack.push(child);
        }
        Some(node)
    }
}

/// Local part of an ElementTree name.
pub fn local_name(tag: &str) -> &str {
    match tag.rfind('}') {
        Some(i) => &tag[i + 1..],
        None => tag,
    }
}

/// Python `str.lower()` of the local name.
pub fn local_lower(tag: &str) -> String {
    let name = local_name(tag);
    if name.is_ascii() {
        name.to_ascii_lowercase()
    } else {
        name.to_lowercase()
    }
}

/// Python `str.casefold()` of the local name (tools.notes_payload._local).
pub fn local_casefold(tag: &str) -> String {
    casefold(local_name(tag)).into_owned()
}

/// Approximation of Python `str.casefold()`: lowercase plus the common full
/// case foldings that differ from `lower()`.
pub fn casefold(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        if s.bytes().any(|b| b.is_ascii_uppercase()) {
            return Cow::Owned(s.to_ascii_lowercase());
        }
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ς' => out.push('σ'),
            'ſ' => out.push('s'),
            'µ' => out.push('μ'),
            'ϐ' => out.push('β'),
            'ϑ' => out.push('θ'),
            'ϕ' => out.push('φ'),
            'ϖ' => out.push('π'),
            'ϰ' => out.push('κ'),
            'ϱ' => out.push('ρ'),
            'ϵ' => out.push('ε'),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            'İ' => out.push_str("i\u{307}"),
            _ => out.extend(c.to_lowercase()),
        }
    }
    Cow::Owned(out)
}

/// Parse failure with the byte offset reported by the parser.
#[derive(Debug, Clone)]
pub struct ParseFailure {
    pub offset: usize,
    pub message: String,
}

fn qualified(ns: Option<&str>, name: &str) -> String {
    match ns {
        Some(ns) => {
            let mut s = String::with_capacity(ns.len() + name.len() + 2);
            s.push('{');
            s.push_str(ns);
            s.push('}');
            s.push_str(name);
            s
        }
        None => name.to_string(),
    }
}

fn make(node: roxmltree::Node<'_, '_>) -> Element {
    let tag = node.tag_name();
    Element {
        tag: qualified(tag.namespace(), tag.name()),
        attrs: node
            .attributes()
            .map(|a| (qualified(a.namespace(), a.name()), a.value().to_string()))
            .collect(),
        text: None,
        tail: None,
        children: Vec::new(),
    }
}

fn append(slot: &mut Option<String>, text: &str) {
    match slot {
        Some(s) => s.push_str(text),
        None => *slot = Some(text.to_string()),
    }
}

fn offset_of(text: &str, row: u32, col: u32) -> usize {
    let mut line = 1u32;
    let mut start = 0usize;
    if row > 1 {
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line += 1;
                if line == row {
                    start = i + 1;
                    break;
                }
            }
        }
        if line < row {
            return text.len();
        }
    }
    let rest = &text[start..];
    let mut chars = 1u32;
    for (i, _) in rest.char_indices() {
        if chars >= col {
            return start + i;
        }
        chars += 1;
    }
    text.len()
}

/// Element nesting accepted by [`parse`]. The tree's `Drop`, `iter` and
/// serializers recurse once per level, so a hostile document of nested tags
/// would overflow the thread stack (an abort of the whole engine, not a
/// catchable panic). Note payloads are limited to 64 levels
/// (`Limits::max_xml_depth`), far below this bound.
pub const MAX_DEPTH: usize = 256;

/// Parse a document into an ElementTree-like tree.
pub fn parse(text: &str) -> std::result::Result<Element, ParseFailure> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // XML end-of-line handling happens before parsing (expat does it; roxmltree
    // keeps lone CR). Character references such as &#13; are unaffected.
    let normalized;
    let text = if text.contains('\r') {
        normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        normalized.as_str()
    } else {
        text
    };
    let options = roxmltree::ParsingOptions { allow_dtd: false, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, options).map_err(|e| {
        let pos = e.pos();
        ParseFailure { offset: offset_of(text, pos.row, pos.col), message: e.to_string() }
    })?;
    let root = doc.root_element();
    let mut stack: Vec<Element> = vec![make(root)];
    let mut iters = vec![root.children()];
    loop {
        let next = iters.last_mut().unwrap().next();
        match next {
            None => {
                iters.pop();
                let done = stack.pop().unwrap();
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Ok(done),
                }
            }
            Some(node) if node.is_element() => {
                if iters.len() >= MAX_DEPTH {
                    // Reported like an expat failure at this element so the
                    // payload layer still answers "XML structural limit exceeded".
                    return Err(ParseFailure { offset: node.range().start, message: "XML element nesting exceeds limit".into() });
                }
                stack.push(make(node));
                iters.push(node.children());
            }
            Some(node) if node.is_text() => {
                let value = node.text().unwrap_or("");
                if value.is_empty() {
                    continue;
                }
                let parent = stack.last_mut().unwrap();
                match parent.children.last_mut() {
                    Some(prev) => append(&mut prev.tail, value),
                    None => append(&mut parent.text, value),
                }
            }
            Some(_) => {}
        }
    }
}

/// `ET.fromstring(raw)` for UTF-8 input; failures map to `ParseError`.
pub fn fromstring(raw: &[u8]) -> Result<Element> {
    let text = std::str::from_utf8(raw).map_err(|_| Error::XmlParse("not well-formed (invalid token)".into()))?;
    parse(text).map_err(|e| Error::XmlParse(e.message))
}

/// Crude streaming estimate used only on parse failures: did the element
/// start events before `limit` exceed the structural bounds? Mirrors the
/// streaming check in `tools.notes_payload._xml` (events precede the error).
pub fn structure_exceeded_before(text: &str, limit: usize, max_depth: i64, max_nodes: i64) -> bool {
    let bytes = text.as_bytes();
    let end = limit.min(bytes.len());
    let mut i = 0usize;
    let mut depth: i64 = 0;
    let mut count: i64 = 0;
    while i < end {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            i = find(bytes, i + 4, b"-->").map_or(bytes.len(), |p| p + 3);
        } else if rest.starts_with(b"<![CDATA[") {
            i = find(bytes, i + 9, b"]]>").map_or(bytes.len(), |p| p + 3);
        } else if rest.starts_with(b"<?") {
            i = find(bytes, i + 2, b"?>").map_or(bytes.len(), |p| p + 2);
        } else if rest.starts_with(b"</") {
            depth -= 1;
            i = find(bytes, i + 2, b">").map_or(bytes.len(), |p| p + 1);
        } else if rest.starts_with(b"<!") {
            i = find(bytes, i + 2, b">").map_or(bytes.len(), |p| p + 1);
        } else {
            // start tag: skip quoted attribute values
            let mut j = i + 1;
            let mut quote = 0u8;
            while j < bytes.len() {
                let b = bytes[j];
                if quote != 0 {
                    if b == quote {
                        quote = 0;
                    }
                } else if b == b'"' || b == b'\'' {
                    quote = b;
                } else if b == b'>' {
                    break;
                }
                j += 1;
            }
            if j >= end {
                break; // the start event was not complete before the error
            }
            let empty = j > i && bytes[j - 1] == b'/';
            depth += 1;
            count += 1;
            if depth > max_depth || count > max_nodes {
                return true;
            }
            if empty {
                depth -= 1;
            }
            i = j + 1;
        }
    }
    false
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

// ---------------------------------------------------------------- serialize

const NAMESPACE_MAP: &[(&str, &str)] = &[
    ("http://www.w3.org/XML/1998/namespace", "xml"),
    ("http://www.w3.org/1999/xhtml", "html"),
    ("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "rdf"),
    ("http://schemas.xmlsoap.org/wsdl/", "wsdl"),
    ("http://www.w3.org/2001/XMLSchema", "xs"),
    ("http://www.w3.org/2001/XMLSchema-instance", "xsi"),
    ("http://purl.org/dc/elements/1.1/", "dc"),
];

struct Names {
    qnames: HashMap<String, String>,
    /// uri -> prefix, in insertion order
    namespaces: Vec<(String, String)>,
}

impl Names {
    fn add(&mut self, qname: &str) -> Result<()> {
        if self.qnames.contains_key(qname) {
            return Ok(());
        }
        if let Some(rest) = qname.strip_prefix('{') {
            let Some(close) = rest.rfind('}') else {
                return Err(Error::Type(format!("cannot serialize {qname:?} (type str)").into()));
            };
            let (uri, tag) = (&rest[..close], &rest[close + 1..]);
            let prefix = match self.namespaces.iter().find(|(u, _)| u == uri) {
                Some((_, p)) => p.clone(),
                None => {
                    let p = NAMESPACE_MAP
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
            let q = if prefix.is_empty() { tag.to_string() } else { format!("{prefix}:{tag}") };
            self.qnames.insert(qname.to_string(), q);
        } else {
            self.qnames.insert(qname.to_string(), qname.to_string());
        }
        Ok(())
    }
}

fn collect_names(root: &Element) -> Result<Names> {
    let mut names = Names { qnames: HashMap::new(), namespaces: Vec::new() };
    for node in root.iter() {
        names.add(&node.tag)?;
        for (k, _) in &node.attrs {
            names.add(k)?;
        }
    }
    Ok(names)
}

fn escape_cdata(out: &mut String, text: &str) {
    if !text.bytes().any(|b| matches!(b, b'&' | b'<' | b'>')) {
        out.push_str(text);
        return;
    }
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_attrib(out: &mut String, text: &str) {
    if !text.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\r' | b'\n' | b'\t')) {
        out.push_str(text);
        return;
    }
    for c in text.chars() {
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

/// `ET.tostring(root, encoding='unicode')` (no declaration), tail included.
pub fn tostring_unicode(root: &Element) -> Result<String> {
    let names = collect_names(root)?;
    let mut out = String::with_capacity(1024);
    // explicit stack: (element, next child index, is_root)
    let mut stack: Vec<(&Element, usize)> = Vec::new();
    open(&mut out, root, &names, true);
    if is_empty(root) {
        out.push_str(" />");
        if let Some(t) = &root.tail {
            escape_cdata(&mut out, t);
        }
        return Ok(out);
    }
    stack.push((root, 0));
    while let Some((node, idx)) = stack.pop() {
        if idx < node.children.len() {
            stack.push((node, idx + 1));
            let child = &node.children[idx];
            open(&mut out, child, &names, false);
            if is_empty(child) {
                out.push_str(" />");
                if let Some(t) = &child.tail {
                    escape_cdata(&mut out, t);
                }
            } else {
                stack.push((child, 0));
            }
        } else {
            out.push_str("</");
            out.push_str(&names.qnames[&node.tag]);
            out.push('>');
            if let Some(t) = &node.tail {
                escape_cdata(&mut out, t);
            }
        }
    }
    Ok(out)
}

fn is_empty(e: &Element) -> bool {
    e.children.is_empty() && e.text.as_deref().map_or(true, str::is_empty)
}

fn open(out: &mut String, e: &Element, names: &Names, root: bool) {
    out.push('<');
    out.push_str(&names.qnames[&e.tag]);
    if root && !names.namespaces.is_empty() {
        let mut decls: Vec<&(String, String)> = names.namespaces.iter().collect();
        decls.sort_by(|a, b| a.1.cmp(&b.1));
        for (uri, prefix) in decls {
            out.push_str(" xmlns");
            if !prefix.is_empty() {
                out.push(':');
                out.push_str(prefix);
            }
            out.push_str("=\"");
            escape_attrib(out, uri);
            out.push('"');
        }
    }
    for (k, v) in &e.attrs {
        out.push(' ');
        out.push_str(&names.qnames[k]);
        out.push_str("=\"");
        escape_attrib(out, v);
        out.push('"');
    }
    if !is_empty(e) {
        out.push('>');
        if let Some(t) = &e.text {
            escape_cdata(out, t);
        }
    }
}

pub const XML_DECLARATION: &str = "<?xml version='1.0' encoding='utf-8'?>\n";

/// `ET.tostring(root, encoding='utf-8', xml_declaration=True)`.
pub fn tostring(root: &Element) -> Result<Vec<u8>> {
    let body = tostring_unicode(root)?;
    let mut out = Vec::with_capacity(XML_DECLARATION.len() + body.len());
    out.extend_from_slice(XML_DECLARATION.as_bytes());
    out.extend_from_slice(body.as_bytes());
    Ok(out)
}

// ---------------------------------------------------------------- C14N 2.0

fn c14n_text(out: &mut String, text: &str) {
    // The serialized text is re-parsed by ET.canonicalize: line ends normalize.
    let normalized: Cow<'_, str> = if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    };
    for c in normalized.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn c14n_attr(out: &mut String, text: &str) {
    for c in text.chars() {
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

/// `ET.canonicalize(ET.tostring(element, encoding='unicode'))`.
pub fn canonicalize_subtree(root: &Element) -> Result<String> {
    if let Some(tail) = &root.tail {
        if tail.chars().any(|c| !matches!(c, ' ' | '\t' | '\n' | '\r')) {
            return Err(Error::XmlParse("junk after document element".into()));
        }
    }
    let names = collect_names(root)?;
    let prefix_of = |uri: &str| -> String {
        names
            .namespaces
            .iter()
            .find(|(u, _)| u == uri)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| "xml".to_string())
    };
    let split = |q: &str| -> Option<(String, String)> {
        let rest = q.strip_prefix('{')?;
        let close = rest.rfind('}')?;
        Some((rest[..close].to_string(), rest[close + 1..].to_string()))
    };
    let mut out = String::with_capacity(1024);
    // declared (uri, prefix) per open element
    let mut declared: Vec<Vec<(String, String)>> = Vec::new();
    let mut stack: Vec<(&Element, usize)> = Vec::new();
    let emit_open = |out: &mut String, e: &Element, declared: &mut Vec<Vec<(String, String)>>| {
        let mut new: Vec<(String, String)> = Vec::new();
        let mut used = vec![e.tag.as_str()];
        used.extend(e.attrs.iter().map(|(k, _)| k.as_str()));
        for q in used {
            if let Some((uri, _)) = split(q) {
                let prefix = prefix_of(&uri);
                if prefix == "xml" {
                    continue;
                }
                let known = declared.iter().flatten().chain(new.iter()).any(|(u, p)| *u == uri && *p == prefix);
                if !known {
                    new.push((uri, prefix));
                }
            }
        }
        let tag = match split(&e.tag) {
            Some((uri, local)) => format!("{}:{}", prefix_of(&uri), local),
            None => e.tag.clone(),
        };
        out.push('<');
        out.push_str(&tag);
        let mut decls: Vec<(String, String)> = new.iter().map(|(u, p)| (format!("xmlns:{p}"), u.clone())).collect();
        decls.sort();
        for (k, v) in &decls {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            c14n_attr(out, v);
            out.push('"');
        }
        let mut attrs: Vec<&(String, String)> = e.attrs.iter().collect();
        attrs.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in attrs {
            out.push(' ');
            match split(k) {
                Some((uri, local)) => {
                    out.push_str(&prefix_of(&uri));
                    out.push(':');
                    out.push_str(&local);
                }
                None => out.push_str(k),
            }
            out.push_str("=\"");
            c14n_attr(out, v);
            out.push('"');
        }
        out.push('>');
        if let Some(t) = &e.text {
            c14n_text(out, t);
        }
        declared.push(new);
        tag
    };
    let mut tags: Vec<String> = Vec::new();
    tags.push(emit_open(&mut out, root, &mut declared));
    stack.push((root, 0));
    while let Some((node, idx)) = stack.pop() {
        if idx < node.children.len() {
            stack.push((node, idx + 1));
            let child = &node.children[idx];
            tags.push(emit_open(&mut out, child, &mut declared));
            stack.push((child, 0));
        } else {
            let tag = tags.pop().unwrap();
            declared.pop();
            out.push_str("</");
            out.push_str(&tag);
            out.push('>');
            if !stack.is_empty() {
                if let Some(t) = &node.tail {
                    c14n_text(&mut out, t);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_matches_elementtree() {
        let e = parse("<a x=\"1&#10;2&#9;3&#13;4 &quot;&lt;&gt;&amp;'\" y=\"a\nb\">x<!--c-->y<?pi z?>w<b/>t1<!--d-->t2</a>").unwrap();
        assert_eq!(e.text.as_deref(), Some("xyw"));
        assert_eq!(e.children[0].tail.as_deref(), Some("t1t2"));
        let out = String::from_utf8(tostring(&e).unwrap()).unwrap();
        assert_eq!(
            out,
            "<?xml version='1.0' encoding='utf-8'?>\n<a x=\"1&#10;2&#09;3&#13;4 &quot;&lt;&gt;&amp;'\" y=\"a b\">xyw<b />t1t2</a>"
        );
        let n = parse("<n:a xmlns:n=\"urn:x\" xmlns=\"urn:d\" n:k=\"v\"><b/><n:c xmlns:m=\"urn:m\" m:z=\"1\"/></n:a>").unwrap();
        assert_eq!(
            tostring_unicode(&n).unwrap(),
            "<ns0:a xmlns:ns0=\"urn:x\" xmlns:ns1=\"urn:d\" xmlns:ns2=\"urn:m\" ns0:k=\"v\"><ns1:b /><ns0:c ns2:z=\"1\" /></ns0:a>"
        );
        assert_eq!(
            canonicalize_subtree(&n).unwrap(),
            "<ns0:a xmlns:ns0=\"urn:x\" ns0:k=\"v\"><ns1:b xmlns:ns1=\"urn:d\"></ns1:b><ns0:c xmlns:ns2=\"urn:m\" ns2:z=\"1\"></ns0:c></ns0:a>"
        );
    }
}
