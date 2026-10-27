//! Offline protocol research and PKCE primitives. No login, HTTP, or note mutations.
//!
//! The field names come from Notes 4.4.45.37, `b4.a.c`. Required fields and
//! limits below are conservative client validation rules, not a complete
//! specification of what every Samsung server version accepts.

use roxmltree::{Document, Node, ParsingOptions};
use std::{error::Error, fmt};

pub mod callback;
pub mod entrypoint;
pub mod pkce;
pub mod signin;
pub mod tree;

pub const MAX_XML_BYTES: usize = 2 * 1024 * 1024;
const MAX_XML_NODES: u32 = 50_000;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct NoteChangesPage {
    pub has_next: bool,
    /// Opaque value: never parse as a timestamp or normalize it.
    pub last_change_point: String,
    pub changes: Vec<NoteChange>,
    /// Raw XML for unrecognized elements, retained in input order.
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct NoteChange {
    pub server_id: String,
    pub resource_id: Option<String>,
    pub sync_modified_time: Option<i64>,
    /// Absence is distinct from false; this parser never applies deletions.
    pub is_deleted: Option<bool>,
    pub is_locked: Option<bool>,
    /// Note-level flag; not the EDP `noteTree@encrypted` flag.
    pub is_encrypted: Option<bool>,
    /// Preserved as received. Android decodes this separately.
    pub model_name_encoded: Option<String>,
    /// Preserved as received; no guessed commit-ID normalization.
    pub commit_id: Option<String>,
    pub is_updated_by_device: Option<bool>,
    pub sdk_version: Option<i32>,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLarge,
    InvalidXml,
    WrongRoot,
    MissingField(&'static str),
    DuplicateField(&'static str),
    InvalidField(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "XML exceeds the input size limit"),
            Self::InvalidXml => write!(f, "XML is malformed or uses unsupported XML features"),
            Self::WrongRoot => write!(f, "expected NoteChangesView"),
            Self::MissingField(field) => write!(f, "missing {field}"),
            Self::DuplicateField(field) => write!(f, "duplicate {field}"),
            Self::InvalidField(field) => write!(f, "invalid {field}"),
        }
    }
}
impl Error for ParseError {}

pub fn parse_note_changes(xml: &str) -> Result<NoteChangesPage, ParseError> {
    if xml.len() > MAX_XML_BYTES {
        return Err(ParseError::TooLarge);
    }
    let doc = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            ..Default::default()
        },
    )
    .map_err(|_| ParseError::InvalidXml)?;
    let root = doc.root_element();
    if !root
        .tag_name()
        .name()
        .eq_ignore_ascii_case("NoteChangesView")
    {
        return Err(ParseError::WrongRoot);
    }
    let mut has_next = None;
    let mut cursor = None;
    let mut changes = Vec::new();
    let mut extensions = Vec::new();
    for node in root.children().filter(Node::is_element) {
        if named(node, root, "hasNext") {
            set(&mut has_next, boolean(node, "hasNext")?, "hasNext")?;
        } else if named(node, root, "lastChangePoint") {
            set(
                &mut cursor,
                scalar(node, "lastChangePoint")?,
                "lastChangePoint",
            )?;
        } else if named(node, root, "changes") {
            changes.push(parse_change(node, xml)?);
        } else {
            extensions.push(xml[node.range()].to_owned());
        }
    }
    let has_next = has_next.ok_or(ParseError::MissingField("hasNext"))?;
    let last_change_point = cursor.ok_or(ParseError::MissingField("lastChangePoint"))?;
    if has_next && last_change_point.is_empty() {
        return Err(ParseError::InvalidField("lastChangePoint"));
    }
    Ok(NoteChangesPage {
        has_next,
        last_change_point,
        changes,
        extensions,
    })
}

fn named(node: Node<'_, '_>, parent: Node<'_, '_>, name: &str) -> bool {
    node.tag_name().namespace() == parent.tag_name().namespace()
        && node.tag_name().name().eq_ignore_ascii_case(name)
}

fn scalar(node: Node<'_, '_>, field: &'static str) -> Result<String, ParseError> {
    if node.children().any(|child| child.is_element()) {
        return Err(ParseError::InvalidField(field));
    }
    Ok(node
        .children()
        .filter(Node::is_text)
        .filter_map(|child| child.text())
        .collect())
}

fn boolean(node: Node<'_, '_>, field: &'static str) -> Result<bool, ParseError> {
    let text = scalar(node, field)?;
    // Android Boolean.parseBoolean does not trim. Reject unknown spellings
    // rather than accidentally turning whitespace-padded input into deletion.
    match text.as_str() {
        value if value.eq_ignore_ascii_case("true") => Ok(true),
        value if value.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(ParseError::InvalidField(field)),
    }
}

fn number(node: Node<'_, '_>, field: &'static str) -> Result<i64, ParseError> {
    let text = scalar(node, field)?;
    // a1.t.S/R use parseLong/parseInt with a lowercase 0x prefix switch.
    // In particular, whitespace is not normalized by those helpers.
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => i64::from_str_radix(hex, 16),
        None => text.parse::<i64>(),
    };
    parsed.map_err(|_| ParseError::InvalidField(field))
}

fn set<T>(target: &mut Option<T>, value: T, field: &'static str) -> Result<(), ParseError> {
    if target.is_some() {
        return Err(ParseError::DuplicateField(field));
    }
    *target = Some(value);
    Ok(())
}

fn parse_change(root: Node<'_, '_>, xml: &str) -> Result<NoteChange, ParseError> {
    let mut server_id = None;
    let mut sdk_version_seen = false;
    let mut item = NoteChange {
        server_id: String::new(),
        resource_id: None,
        sync_modified_time: None,
        is_deleted: None,
        is_locked: None,
        is_encrypted: None,
        model_name_encoded: None,
        commit_id: None,
        is_updated_by_device: None,
        sdk_version: None,
        extensions: Vec::new(),
    };
    for node in root.children().filter(Node::is_element) {
        if named(node, root, "serverId") {
            set(&mut server_id, scalar(node, "serverId")?, "serverId")?;
        } else if named(node, root, "resourceId") {
            set(
                &mut item.resource_id,
                scalar(node, "resourceId")?,
                "resourceId",
            )?;
        } else if named(node, root, "syncModifiedTime") {
            let value = number(node, "syncModifiedTime")?;
            set(&mut item.sync_modified_time, value, "syncModifiedTime")?;
        } else if named(node, root, "isDeleted") {
            set(
                &mut item.is_deleted,
                boolean(node, "isDeleted")?,
                "isDeleted",
            )?;
        } else if named(node, root, "isLocked") {
            set(&mut item.is_locked, boolean(node, "isLocked")?, "isLocked")?;
        } else if named(node, root, "isEncrypted") {
            set(
                &mut item.is_encrypted,
                boolean(node, "isEncrypted")?,
                "isEncrypted",
            )?;
        } else if named(node, root, "modelName") {
            set(
                &mut item.model_name_encoded,
                scalar(node, "modelName")?,
                "modelName",
            )?;
        } else if named(node, root, "commitId") {
            set(&mut item.commit_id, scalar(node, "commitId")?, "commitId")?;
        } else if named(node, root, "isUpdatedByDevice") {
            set(
                &mut item.is_updated_by_device,
                boolean(node, "isUpdatedByDevice")?,
                "isUpdatedByDevice",
            )?;
        } else if named(node, root, "sdkVersion") {
            if sdk_version_seen {
                return Err(ParseError::DuplicateField("sdkVersion"));
            }
            sdk_version_seen = true;
            // Live Cloud tombstones can contain an empty sdkVersion element.
            // Preserve unknown as None; do not invent version zero or trim a
            // nonempty invalid value into an accepted deletion record.
            if !scalar(node, "sdkVersion")?.is_empty() {
                let value = i32::try_from(number(node, "sdkVersion")?)
                    .map_err(|_| ParseError::InvalidField("sdkVersion"))?;
                item.sdk_version = Some(value);
            }
        } else {
            item.extensions.push(xml[node.range()].to_owned());
        }
    }
    item.server_id = server_id
        .filter(|value| !value.is_empty())
        .ok_or(ParseError::MissingField("serverId"))?;
    Ok(item)
}
