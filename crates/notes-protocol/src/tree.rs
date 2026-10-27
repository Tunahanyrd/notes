//! Bounded offline note-tree inspection from Notes 4.4.45.37 `n1.o`.
//!
//! This is the comparison tree, not `NoteResource`, a document renderer, or a
//! synchronization engine. `k1.a.m` uses returned entity IDs to filter an upload;
//! no such filtering or mutation is implemented here. Required IDs, duplicate
//! rejection and resource limits are conservative client rules, not claims
//! about every server version. Hashes remain opaque; `t` has no inferred unit.

use roxmltree::{Document, Node, ParsingOptions};
use std::{collections::HashSet, error::Error, fmt};

pub const MAX_TREE_XML_BYTES: usize = crate::MAX_XML_BYTES;
pub const MAX_TREE_NODES: u32 = 50_000;
pub const MAX_TREE_DEPTH: usize = 64;
pub const MAX_TREE_ENTITIES: usize = 4_096;
pub const MAX_TREE_FIELD_BYTES: usize = 16 * 1024;
pub const MAX_TREE_EXTENSION_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteTree {
    /// Missing stays missing; Android initializes its field to zero.
    pub version: Option<i32>,
    /// EDP state on the comparison tree, NOT NoteResource's note-lock flags.
    /// Parsing this flag does not establish key availability or decrypt data.
    pub encrypted: Option<bool>,
    pub is_stroke_grouped: Option<bool>,
    pub pages: Vec<Page>,
    pub content_files: Vec<TreeEntity>,
    pub floating_objects: Vec<TreeEntity>,
    pub body_text: Option<TreeEntity>,
    pub model_name_encoded: Option<String>,
    /// Exact received text; Android applies an additional helper not replicated.
    pub commit_id: Option<String>,
    pub is_updated_by_device: Option<bool>,
    /// Unknown root children, in input order. Fragments may require inherited
    /// namespace bindings; they are NOT standalone replay documents.
    pub extensions: Vec<String>,
    /// Exact input retained, including unknown attributes and namespace context.
    /// No serializer or promise of editable round-trip behavior is provided.
    pub source_xml: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntity {
    pub id: String,
    /// Source `n1.o.e` uses decimal Java Long.parseLong; absence is preserved.
    pub t: Option<i64>,
    pub h: Option<String>,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub entity: TreeEntity,
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub entity: TreeEntity,
    pub objects: Vec<TreeEntity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeParseError {
    LimitExceeded(&'static str),
    InvalidXml,
    WrongRoot,
    MissingField(&'static str),
    InvalidField(&'static str),
    DuplicateField(&'static str),
    DuplicateIdentity,
}

impl fmt::Display for TreeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded(limit) => write!(f, "note tree exceeds {limit} limit"),
            Self::InvalidXml => write!(f, "note tree XML is malformed or unsupported"),
            Self::WrongRoot => write!(f, "expected noteTree or CompareForUpsyncView"),
            Self::MissingField(field) => write!(f, "missing tree {field}"),
            Self::InvalidField(field) => write!(f, "invalid tree {field}"),
            Self::DuplicateField(field) => write!(f, "duplicate tree {field}"),
            Self::DuplicateIdentity => write!(f, "duplicate tree entity identity"),
        }
    }
}
impl Error for TreeParseError {}

type Result<T> = std::result::Result<T, TreeParseError>;

fn bounded(text: &str, field: &'static str) -> Result<String> {
    if text.len() > MAX_TREE_FIELD_BYTES {
        return Err(TreeParseError::LimitExceeded(field));
    }
    Ok(text.to_owned())
}

fn boolean(text: &str, field: &'static str) -> Result<bool> {
    // Android Boolean.parseBoolean has no trim. Reject invalid spellings rather
    // than silently reinterpreting them as false or normalizing them to true.
    if text.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if text.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        Err(TreeParseError::InvalidField(field))
    }
}

fn assign<T>(slot: &mut Option<T>, value: T, field: &'static str) -> Result<()> {
    if slot.is_some() {
        return Err(TreeParseError::DuplicateField(field));
    }
    *slot = Some(value);
    Ok(())
}

fn named(node: Node<'_, '_>, parent: Node<'_, '_>, name: &str) -> bool {
    node.tag_name().namespace() == parent.tag_name().namespace()
        && node.tag_name().name().eq_ignore_ascii_case(name)
}

fn scalar(node: Node<'_, '_>, field: &'static str) -> Result<String> {
    if node.children().any(|n| n.is_element()) {
        return Err(TreeParseError::InvalidField(field));
    }
    let value: String = node
        .children()
        .filter_map(|n| n.text().filter(|_| n.is_text()))
        .collect();
    bounded(&value, field)
}

struct Reader<'a> {
    xml: &'a str,
    identities: HashSet<String>,
    extension_bytes: usize,
}

impl Reader<'_> {
    fn extension(&mut self, node: Node<'_, '_>, into: &mut Vec<String>) -> Result<()> {
        let raw = &self.xml[node.range()];
        self.extension_bytes = self
            .extension_bytes
            .checked_add(raw.len())
            .ok_or(TreeParseError::LimitExceeded("extension bytes"))?;
        if self.extension_bytes > MAX_TREE_EXTENSION_BYTES {
            return Err(TreeParseError::LimitExceeded("extension bytes"));
        }
        into.push(raw.to_owned());
        Ok(())
    }

    fn entity(&mut self, node: Node<'_, '_>) -> Result<TreeEntity> {
        let mut id = None;
        let mut t = None;
        let mut h = None;
        for attr in node.attributes().filter(|a| a.namespace().is_none()) {
            // n1.o.e matches these attribute names case-sensitively.
            match attr.name() {
                "id" => assign(&mut id, bounded(attr.value(), "id")?, "id")?,
                "t" => assign(
                    &mut t,
                    attr.value()
                        .parse::<i64>()
                        .map_err(|_| TreeParseError::InvalidField("t"))?,
                    "t",
                )?,
                "h" => assign(&mut h, bounded(attr.value(), "h")?, "h")?,
                _ => (),
            }
        }
        let id = id
            .filter(|s| !s.is_empty())
            .ok_or(TreeParseError::MissingField("id"))?;
        if self.identities.len() >= MAX_TREE_ENTITIES {
            return Err(TreeParseError::LimitExceeded("entity count"));
        }
        if !self.identities.insert(id.clone()) {
            return Err(TreeParseError::DuplicateIdentity);
        }
        Ok(TreeEntity {
            id,
            t,
            h,
            extensions: Vec::new(),
        })
    }

    fn leaf(&mut self, node: Node<'_, '_>) -> Result<TreeEntity> {
        let mut entity = self.entity(node)?;
        for child in node.children().filter(Node::is_element) {
            self.extension(child, &mut entity.extensions)?;
        }
        Ok(entity)
    }

    fn leaves(
        &mut self,
        list: Node<'_, '_>,
        name: &str,
        extensions: &mut Vec<String>,
    ) -> Result<Vec<TreeEntity>> {
        let mut items = Vec::new();
        for child in list.children().filter(Node::is_element) {
            if named(child, list, name) {
                items.push(self.leaf(child)?);
            } else {
                self.extension(child, extensions)?;
            }
        }
        Ok(items)
    }

    fn layer(&mut self, node: Node<'_, '_>) -> Result<Layer> {
        let mut entity = self.entity(node)?;
        let mut objects = None;
        for child in node.children().filter(Node::is_element) {
            if named(child, node, "objectList") {
                let parsed = self.leaves(child, "object", &mut entity.extensions)?;
                assign(&mut objects, parsed, "objectList")?;
            } else {
                self.extension(child, &mut entity.extensions)?;
            }
        }
        Ok(Layer {
            entity,
            objects: objects.unwrap_or_default(),
        })
    }

    fn page(&mut self, node: Node<'_, '_>) -> Result<Page> {
        let mut entity = self.entity(node)?;
        let mut layers = None;
        for child in node.children().filter(Node::is_element) {
            if named(child, node, "layerList") {
                let mut parsed = Vec::new();
                for layer in child.children().filter(Node::is_element) {
                    if named(layer, child, "layer") {
                        parsed.push(self.layer(layer)?);
                    } else {
                        self.extension(layer, &mut entity.extensions)?;
                    }
                }
                assign(&mut layers, parsed, "layerList")?;
            } else {
                self.extension(child, &mut entity.extensions)?;
            }
        }
        Ok(Page {
            entity,
            layers: layers.unwrap_or_default(),
        })
    }
}

pub fn parse_note_tree(xml: &str) -> Result<NoteTree> {
    if xml.len() > MAX_TREE_XML_BYTES {
        return Err(TreeParseError::LimitExceeded("input bytes"));
    }
    let doc = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_TREE_NODES,
            ..Default::default()
        },
    )
    .map_err(|_| TreeParseError::InvalidXml)?;
    let root = doc.root_element();
    if !["noteTree", "CompareForUpsyncView"]
        .iter()
        .any(|s| root.tag_name().name().eq_ignore_ascii_case(s))
    {
        return Err(TreeParseError::WrongRoot);
    }
    for node in root.descendants().filter(Node::is_element) {
        if node
            .ancestors()
            .filter(Node::is_element)
            .take(MAX_TREE_DEPTH + 1)
            .count()
            > MAX_TREE_DEPTH
        {
            return Err(TreeParseError::LimitExceeded("element depth"));
        }
    }
    let mut tree = NoteTree {
        version: None,
        encrypted: None,
        is_stroke_grouped: None,
        pages: Vec::new(),
        content_files: Vec::new(),
        floating_objects: Vec::new(),
        body_text: None,
        model_name_encoded: None,
        commit_id: None,
        is_updated_by_device: None,
        extensions: Vec::new(),
        source_xml: xml.to_owned(),
    };
    for attr in root.attributes().filter(|a| a.namespace().is_none()) {
        if attr.name().eq_ignore_ascii_case("ver") {
            assign(
                &mut tree.version,
                attr.value()
                    .parse::<i32>()
                    .map_err(|_| TreeParseError::InvalidField("ver"))?,
                "ver",
            )?;
        } else if attr.name().eq_ignore_ascii_case("encrypted") {
            assign(
                &mut tree.encrypted,
                boolean(attr.value(), "encrypted")?,
                "encrypted",
            )?;
        } else if attr.name().eq_ignore_ascii_case("isStrokeGrouped") {
            assign(
                &mut tree.is_stroke_grouped,
                boolean(attr.value(), "isStrokeGrouped")?,
                "isStrokeGrouped",
            )?;
        }
    }
    let mut reader = Reader {
        xml,
        identities: HashSet::new(),
        extension_bytes: 0,
    };
    let mut seen_lists = HashSet::new();
    for node in root.children().filter(Node::is_element) {
        if named(node, root, "pageList") {
            if !seen_lists.insert("pageList") {
                return Err(TreeParseError::DuplicateField("pageList"));
            }
            for child in node.children().filter(Node::is_element) {
                if named(child, node, "page") {
                    tree.pages.push(reader.page(child)?);
                } else {
                    reader.extension(child, &mut tree.extensions)?;
                }
            }
        } else if named(node, root, "contentFileList") {
            if !seen_lists.insert("contentFileList") {
                return Err(TreeParseError::DuplicateField("contentFileList"));
            }
            tree.content_files = reader.leaves(node, "contentFile", &mut tree.extensions)?;
        } else if named(node, root, "floatingObjectList") {
            if !seen_lists.insert("floatingObjectList") {
                return Err(TreeParseError::DuplicateField("floatingObjectList"));
            }
            tree.floating_objects = reader.leaves(node, "object", &mut tree.extensions)?;
        } else if named(node, root, "bodyText") {
            assign(&mut tree.body_text, reader.leaf(node)?, "bodyText")?;
        } else if named(node, root, "modelName") {
            assign(
                &mut tree.model_name_encoded,
                scalar(node, "modelName")?,
                "modelName",
            )?;
        } else if named(node, root, "commitId") {
            assign(&mut tree.commit_id, scalar(node, "commitId")?, "commitId")?;
        } else if named(node, root, "isUpdatedByDevice") {
            assign(
                &mut tree.is_updated_by_device,
                boolean(&scalar(node, "isUpdatedByDevice")?, "isUpdatedByDevice")?,
                "isUpdatedByDevice",
            )?;
        } else {
            reader.extension(node, &mut tree.extensions)?;
        }
    }
    Ok(tree)
}
