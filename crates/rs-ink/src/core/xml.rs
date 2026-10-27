//! Minimal ElementTree-compatible DOM: `ET.fromstring` text/tail model and
//! byte-exact `ET.tostring(elem, encoding='utf-8', xml_declaration=True)`.
use super::{InkErr, R};

pub type NodeId = usize;

/// Tag used for nodes whose Python tag is not a str (comments, PIs, ...).
pub const NON_STR_TAG: &str = "\u{0}<non-str-tag>";

#[derive(Clone, Debug)]
pub struct Node {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<NodeId>,
    pub parent: Option<NodeId>,
}

#[derive(Clone, Debug)]
pub enum Op {
    Set(NodeId, String, String),
    Remove(NodeId, NodeId),
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: NodeId,
    /// When set, mutations are recorded (to replay them on Python objects).
    pub log: Option<Vec<Op>>,
}

impl Tree {
    pub fn add(&mut self, tag: &str, attrs: Vec<(String, String)>, parent: Option<NodeId>) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node { tag: tag.to_string(), attrs, text: None, tail: None, children: vec![], parent });
        if let Some(p) = parent {
            self.nodes[p].children.push(id);
        }
        id
    }
    pub fn tag(&self, id: NodeId) -> &str {
        &self.nodes[id].tag
    }
    pub fn get(&self, id: NodeId, key: &str) -> Option<&str> {
        self.nodes[id].attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    pub fn get_or<'a>(&'a self, id: NodeId, key: &str, default: &'a str) -> &'a str {
        self.get(id, key).unwrap_or(default)
    }
    pub fn has(&self, id: NodeId, key: &str) -> bool {
        self.nodes[id].attrs.iter().any(|(k, _)| k == key)
    }
    pub fn set(&mut self, id: NodeId, key: &str, value: &str) {
        let node = &mut self.nodes[id];
        if let Some(e) = node.attrs.iter_mut().find(|(k, _)| k == key) {
            e.1 = value.to_string();
        } else {
            node.attrs.push((key.to_string(), value.to_string()));
        }
        if let Some(log) = self.log.as_mut() {
            log.push(Op::Set(id, key.to_string(), value.to_string()));
        }
    }
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }
    pub fn len(&self, id: NodeId) -> usize {
        self.nodes[id].children.len()
    }
    /// Element.remove(child)
    pub fn remove(&mut self, parent: NodeId, child: NodeId) {
        if let Some(pos) = self.nodes[parent].children.iter().position(|c| *c == child) {
            self.nodes[parent].children.remove(pos);
            self.nodes[child].parent = None;
            if let Some(log) = self.log.as_mut() {
                log.push(Op::Remove(parent, child));
            }
        }
    }
    /// Element.append(child) for a detached new node.
    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.nodes[parent].children.push(child);
        self.nodes[child].parent = Some(parent);
    }
    /// Pre-order `elem.iter()`.
    pub fn iter(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            for c in self.nodes[n].children.iter().rev() {
                stack.push(*c);
            }
        }
        out
    }
    /// document.local(): lower-cased local name.
    pub fn local(&self, id: NodeId) -> String {
        let tag = &self.nodes[id].tag;
        tag.rsplit('}').next().unwrap_or(tag).to_lowercase()
    }
    /// Element.find(tag) for a plain child tag.
    pub fn find(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        self.nodes[id].children.iter().copied().find(|c| self.nodes[*c].tag == tag)
    }
    /// document.one(parent, name): exactly one child with that local name.
    pub fn one(&self, id: NodeId, name: &str) -> R<NodeId> {
        let lname = name.to_lowercase();
        let found: Vec<NodeId> = self.nodes[id].children.iter().copied().filter(|c| self.local(*c) == lname).collect();
        if found.len() != 1 {
            return Err(InkErr::Document(super::DOC_STRUCTURE.into()));
        }
        Ok(found[0])
    }

    /// xml.etree.ElementTree.fromstring for the UTF-8 documents used here.
    pub fn parse(raw: &[u8]) -> R<Tree> {
        let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
        let text = std::str::from_utf8(raw).map_err(|_| InkErr::Parse("not well-formed (invalid token)".into()))?;
        let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
        let doc = roxmltree::Document::parse_with_options(text, opts)
            .map_err(|e| InkErr::Parse(format!("not well-formed: {e}")))?;
        let mut tree = Tree::default();
        fn qname(ns: Option<&str>, local: &str) -> String {
            match ns {
                Some(u) if !u.is_empty() => format!("{{{u}}}{local}"),
                _ => local.to_string(),
            }
        }
        fn build(tree: &mut Tree, node: roxmltree::Node, parent: Option<NodeId>) -> NodeId {
            let tag = qname(node.tag_name().namespace(), node.tag_name().name());
            let attrs = node.attributes().map(|a| (qname(a.namespace(), a.name()), a.value().to_string())).collect();
            let id = tree.add(&tag, attrs, parent);
            let mut last: Option<NodeId> = None;
            for child in node.children() {
                if child.is_element() {
                    let c = build(tree, child, Some(id));
                    last = Some(c);
                } else if child.is_text() {
                    let t = child.text().unwrap_or("");
                    let slot = match last {
                        None => &mut tree.nodes[id].text,
                        Some(c) => &mut tree.nodes[c].tail,
                    };
                    slot.get_or_insert_with(String::new).push_str(t);
                }
            }
            id
        }
        let root = build(&mut tree, doc.root_element(), None);
        tree.root = root;
        Ok(tree)
    }

    /// ET.tostring(elem, encoding='utf-8', xml_declaration=True)
    pub fn to_bytes(&self, id: NodeId) -> Vec<u8> {
        let (qnames, namespaces) = self.namespaces(id);
        let mut out = String::from("<?xml version='1.0' encoding='utf-8'?>\n");
        self.write(&mut out, id, &qnames, Some(&namespaces));
        out.into_bytes()
    }

    fn namespaces(&self, id: NodeId) -> (std::collections::HashMap<String, String>, Vec<(String, String)>) {
        use std::collections::HashMap;
        let mut qnames: HashMap<String, String> = HashMap::new();
        let mut namespaces: Vec<(String, String)> = Vec::new(); // (uri, prefix)
        let well_known = |uri: &str| -> Option<&'static str> {
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
        };
        let mut add = |q: &str, qnames: &mut HashMap<String, String>| {
            if qnames.contains_key(q) {
                return;
            }
            if let Some(rest) = q.strip_prefix('{') {
                if let Some((uri, local)) = rest.rsplit_once('}') {
                    let prefix = match namespaces.iter().find(|(u, _)| u == uri) {
                        Some((_, p)) => p.clone(),
                        None => {
                            let p = well_known(uri).map(str::to_string).unwrap_or_else(|| format!("ns{}", namespaces.len()));
                            if p != "xml" {
                                namespaces.push((uri.to_string(), p.clone()));
                            }
                            p
                        }
                    };
                    let v = if prefix.is_empty() { local.to_string() } else { format!("{prefix}:{local}") };
                    qnames.insert(q.to_string(), v);
                    return;
                }
            }
            qnames.insert(q.to_string(), q.to_string());
        };
        for n in self.iter(id) {
            let node = &self.nodes[n];
            if node.tag != NON_STR_TAG {
                add(&node.tag, &mut qnames);
            }
            for (k, _) in &node.attrs {
                add(k, &mut qnames);
            }
        }
        (qnames, namespaces)
    }

    fn write(&self, out: &mut String, id: NodeId, qnames: &std::collections::HashMap<String, String>,
             namespaces: Option<&Vec<(String, String)>>) {
        let node = &self.nodes[id];
        let tag = qnames.get(&node.tag).cloned().unwrap_or_else(|| node.tag.clone());
        out.push('<');
        out.push_str(&tag);
        if let Some(ns) = namespaces {
            let mut sorted: Vec<&(String, String)> = ns.iter().collect();
            sorted.sort_by(|a, b| a.1.cmp(&b.1));
            for (uri, prefix) in sorted {
                let k = if prefix.is_empty() { String::new() } else { format!(":{prefix}") };
                out.push_str(&format!(" xmlns{k}=\"{}\"", escape_attrib(uri)));
            }
        }
        for (k, v) in &node.attrs {
            let qk = qnames.get(k).cloned().unwrap_or_else(|| k.clone());
            out.push(' ');
            out.push_str(&qk);
            out.push_str("=\"");
            out.push_str(&escape_attrib(v));
            out.push('"');
        }
        let has_text = node.text.as_deref().is_some_and(|t| !t.is_empty());
        if has_text || !node.children.is_empty() {
            out.push('>');
            if let Some(t) = &node.text {
                out.push_str(&escape_cdata(t));
            }
            for c in &node.children {
                self.write(out, *c, qnames, None);
            }
            out.push_str("</");
            out.push_str(&tag);
            out.push('>');
        } else {
            out.push_str(" />");
        }
        if let Some(t) = &node.tail {
            out.push_str(&escape_cdata(t));
        }
    }
}

pub fn escape_cdata(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn escape_attrib(t: &str) -> String {
    escape_cdata(t)
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#09;")
}
