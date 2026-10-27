//! Pure-Rust port of `tools.notes_writer` (offline plain-text research note builder).
use crate::error::{Error, Result};
use crate::util;
use crate::xml::{self, Element};

pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const FORMAT_VERSION: i64 = 4000;
pub const CREATE_PATH: &str = "/samsungnotes/v3/notes";
pub const MAX_TIMESTAMP: i128 = i64::MAX as i128;

/// Character rule shared with `samsung_notes.document.text_value`.
pub fn bad_text_char(c: char) -> bool {
    let o = c as u32;
    (o < 32 && !matches!(c, '\t' | '\r' | '\n')) || (0xD800..=0xDFFF).contains(&o) || o == 0xFFFE || o == 0xFFFF
}

/// `_text(value)` for a valid Rust string.
pub fn check_text(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::value("nonempty plain text required"));
    }
    if value.chars().any(bad_text_char) {
        return Err(Error::value("unsupported text character"));
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(Error::value("plain text exceeds research-note limit"));
    }
    Ok(())
}

/// `_timestamp(value)` for an integer value.
pub fn check_timestamp(value: i128) -> Result<i64> {
    if !(1..=MAX_TIMESTAMP).contains(&value) {
        return Err(Error::value("positive signed-64-bit millisecond timestamp required"));
    }
    Ok(value as i64)
}

pub fn check_uuid(value: &str) -> Result<()> {
    if !util::is_canonical_uuid(value) {
        return Err(Error::value("canonical lower-case UUID required"));
    }
    Ok(())
}

/// `object_hash(object_id, modified_ms)`: SHA256(UTF8(id + decimal time)).
pub fn object_hash(object_id: &str, modified_ms: i128) -> Result<[u8; 32]> {
    check_uuid(object_id)?;
    check_timestamp(modified_ms)?;
    Ok(util::sha256(format!("{object_id}{modified_ms}").as_bytes()))
}

/// `parent_hash(parent_id, modified_ms, child_hashes)`.
pub fn parent_hash(parent_id: &str, modified_ms: i128, children: &[[u8; 32]]) -> Result<[u8; 32]> {
    let own = object_hash(parent_id, modified_ms)?;
    let mut data = Vec::with_capacity(children.len() * 32 + 32);
    for c in children {
        data.extend_from_slice(c);
    }
    data.extend_from_slice(&own);
    Ok(util::sha256(&data))
}

/// Base64(Base64(UTF8(text))) without validation.
pub fn double_b64(value: &str) -> String {
    util::b64encode(util::b64encode(value.as_bytes()).as_bytes())
}

/// `encode_plain_text(value)`.
pub fn encode_plain_text(value: &str) -> Result<String> {
    check_text(value)?;
    Ok(double_b64(value))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResearchNote {
    pub note_id: String,
    pub title_id: String,
    pub body_id: String,
    pub page_id: String,
    pub layer_id: String,
    pub created_ms: i64,
    pub modified_ms: i64,
    pub title: String,
    pub body: String,
    pub revision: i64,
}

pub struct Hashes {
    pub title: String,
    pub body: String,
    pub layer: String,
    pub page: String,
}

impl ResearchNote {
    /// Dataclass `__post_init__` validation order. Integers are given as i128
    /// so out-of-range Python ints are rejected with the same messages.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ids: [&str; 5],
        created_ms: i128,
        modified_ms: i128,
        title: &str,
        body: &str,
        revision: i128,
    ) -> Result<Self> {
        for id in ids {
            check_uuid(id)?;
        }
        for (i, a) in ids.iter().enumerate() {
            if ids[i + 1..].contains(a) {
                return Err(Error::value("research-note identifiers must be distinct"));
            }
        }
        check_timestamp(created_ms)?;
        check_timestamp(modified_ms)?;
        if modified_ms < created_ms || !(1..=(i32::MAX as i128)).contains(&revision) {
            return Err(Error::value("invalid research-note revision"));
        }
        check_text(title)?;
        check_text(body)?;
        Ok(ResearchNote {
            note_id: ids[0].into(),
            title_id: ids[1].into(),
            body_id: ids[2].into(),
            page_id: ids[3].into(),
            layer_id: ids[4].into(),
            created_ms: created_ms as i64,
            modified_ms: modified_ms as i64,
            title: title.into(),
            body: body.into(),
            revision: revision as i64,
        })
    }

    pub fn hashes(&self) -> Hashes {
        let m = self.modified_ms as i128;
        let layer = parent_hash(&self.layer_id, m, &[]).expect("validated");
        Hashes {
            title: util::hex(&object_hash(&self.title_id, m).expect("validated")),
            body: util::hex(&object_hash(&self.body_id, m).expect("validated")),
            layer: util::hex(&layer),
            page: util::hex(&parent_hash(&self.page_id, m, &[layer]).expect("validated")),
        }
    }

    pub fn tree_element(&self) -> Element {
        let h = self.hashes();
        let m = self.modified_ms.to_string();
        let mut tree = Element::with_attrs("noteTree", &[("ver", "4000"), ("isStrokeGrouped", "false")]);
        let pages = tree.push(Element::new("pageList"));
        let page = pages.push(Element::with_attrs("page", &[("id", &self.page_id), ("t", &m), ("h", &h.page)]));
        let layers = page.push(Element::new("layerList"));
        layers.push(Element::with_attrs("layer", &[("id", &self.layer_id), ("t", &m), ("h", &h.layer)]));
        tree.push(Element::with_attrs("bodyText", &[("id", &self.body_id), ("t", &m), ("h", &h.body)]));
        tree
    }

    pub fn tree_xml(&self) -> Vec<u8> {
        xml::tostring(&self.tree_element()).expect("plain names")
    }

    /// Resource tree with explicit encoded title/body texts.
    pub fn element_with_texts(&self, title_text: &str, body_text: &str) -> Element {
        let h = self.hashes();
        let created = self.created_ms.to_string();
        let modified = self.modified_ms.to_string();
        let fv = FORMAT_VERSION.to_string();
        let rev = self.revision.to_string();
        let mut root = Element::with_attrs(
            "NoteResource",
            &[
                ("id", &self.note_id),
                ("syncModifiedTime", &modified),
                ("createdTime", &created),
                ("isLocked", "false"),
                ("isEncrypted", "false"),
                ("ownerId", ""),
                ("xmlVer", "2"),
            ],
        );
        root.push(self.tree_element());
        let attrs = root.push(Element::with_attrs(
            "attributes",
            &[
                ("isBgColorInverted", "true"),
                ("tapeVisibility", "true"),
                ("formatVersion", &fv),
                ("fileRevision", &rev),
                ("modifiedTime", &modified),
                ("editTime", &modified),
                ("width", "1080"),
                ("height", "1527"),
                ("pageHorizontalPadding", "0"),
                ("pageVerticalPadding", "24"),
                ("minFormatVersion", &fv),
                ("lastEditedPageIndex", "0"),
                ("lastEditedPageImageId", "-1"),
                ("lastEditedPageTime", &modified),
                ("orientation", "0"),
                ("pageMode", "0"),
                ("documentType", "0"),
                ("ownerId", ""),
                ("lastRecognizedDataModifiedTime", "0"),
                ("fixedFont", ""),
                ("fixedTextDirection", "2"),
                ("fixedBackgroundTheme", "2"),
                ("isFavorite", "false"),
                ("state", "normal"),
                ("bookmark", "[]"),
                ("mappedAt", "0"),
                ("isConverted", "false"),
                ("displayCreatedTime", &created),
                ("displayModifiedTime", &modified),
                ("msLastSyncTime", "0"),
            ],
        ));
        let title = attrs.push(Element::new("title"));
        title.push(self.text_object(&self.title_id, &h.title, title_text, true));
        attrs.push(Element::with_attrs("mergedByServer", &[("mathMode", "0")]));
        let pages = root.push(Element::new("pageList"));
        let page = pages.push(Element::with_attrs(
            "page",
            &[
                ("id", &self.page_id),
                ("hash", &h.page),
                ("isTextOnly", "false"),
                ("orientation", "0"),
                ("width", "1080"),
                ("height", "1527"),
                ("offsetX", "0"),
                ("offsetY", "0"),
                ("formatVersion", &fv),
                ("bgImageMode", "2"),
                ("bgColor", "-197380"),
                ("minFormatVersion", &fv),
                ("currentLayerIndex", "0"),
                ("createdTime", &created),
                ("modifiedTime", &modified),
            ],
        ));
        let layers = page.push(Element::new("layerList"));
        layers.push(Element::with_attrs(
            "layer",
            &[
                ("id", &self.layer_id),
                ("hash", &h.layer),
                ("flagEventForwardable", "true"),
                ("visible", "true"),
                ("lockState", "false"),
                ("layerNumber", "0"),
                ("createdTime", &created),
                ("modifiedTime", &modified),
                ("thumbnail", "-1"),
            ],
        ));
        let body = root.push(Element::with_attrs("bodyText", &[("id", &self.body_id)]));
        body.push(self.text_object(&self.body_id, &h.body, body_text, false));
        root
    }

    pub fn element(&self) -> Element {
        self.element_with_texts(&double_b64(&self.title), &double_b64(&self.body))
    }

    /// `ResearchNote.xml()`.
    pub fn xml(&self) -> Vec<u8> {
        xml::tostring(&self.element()).expect("plain names")
    }

    fn text_object(&self, id: &str, digest: &str, encoded: &str, is_title: bool) -> Element {
        let created = self.created_ms.to_string();
        let modified = self.modified_ms.to_string();
        let (lr, tb) = if is_title { ("11.25", "0.0") } else { ("16.0", "10.0") };
        let mut obj = Element::with_attrs(
            "object",
            &[
                ("type", "textBox"),
                ("id", id),
                ("hash", digest),
                ("formatVersion", "4000"),
                ("rotatable", "true"),
                ("selectable", "true"),
                ("movable", "true"),
                ("flippable", "true"),
                ("visible", "true"),
                ("isReplayable", "true"),
                ("clippable", "true"),
                ("isTemplateObject", "false"),
                ("timeStamp", "0"),
                ("resizable", "0"),
                ("haveBeenToAtt", "true"),
                ("lockState", "false"),
                ("createdTime", &created),
                ("modifiedTime", &modified),
                ("removable", "true"),
                ("isHorizontalFlipped", "false"),
                ("isVerticalFlipped", "false"),
                ("isHintTextVisible", "false"),
                ("isTextReadOnly", "false"),
                ("isTextEditable", "true"),
                ("isTransparency", "false"),
                ("shapeType", "4"),
                ("originalDegree", "0.0"),
                ("textGravity", "0"),
                ("ellipsisType", "2"),
                ("leftMargin", lr),
                ("rightMargin", lr),
                ("topMargin", tb),
                ("bottomMargin", tb),
            ],
        );
        if !is_title {
            obj.set("textAutoFit", "2");
        }
        for tag in ["rect", "originalRect"] {
            obj.push(Element::with_attrs(tag, &[("l", "0.0"), ("t", "0.0"), ("r", "0.0"), ("b", "0.0")]));
        }
        let mut text = Element::new("text");
        text.text = Some(encoded.to_string());
        obj.push(text);
        obj
    }
}

pub fn now_ms() -> i64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock");
    (d.as_nanos() / 1_000_000) as i64
}

/// `new_plain_text_note(title, body, now_ms=...)` (now already validated).
pub fn new_plain_text_note(title: &str, body: &str, now: i64) -> Result<ResearchNote> {
    let ids: Vec<String> = (0..5).map(|_| util::uuid4()).collect();
    check_text(title)?;
    check_text(body)?;
    ResearchNote::new([&ids[0], &ids[1], &ids[2], &ids[3], &ids[4]], now as i128, now as i128, title, body, 1)
}

/// `revise_research_note(note, title, body, now_ms=...)`.
pub fn revise_research_note(note: &ResearchNote, title: &str, body: &str, now: i64) -> Result<ResearchNote> {
    if now <= note.modified_ms {
        return Err(Error::value("revision timestamp must increase"));
    }
    check_text(title)?;
    check_text(body)?;
    ResearchNote::new(
        [&note.note_id, &note.title_id, &note.body_id, &note.page_id, &note.layer_id],
        note.created_ms as i128,
        now as i128,
        title,
        body,
        note.revision as i128 + 1,
    )
}

pub fn boundary_ok(b: &str) -> bool {
    (16..=70).contains(&b.len()) && b.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// `create_multipart(note, boundary=...)` -> (gzip bytes, Content-Type).
pub fn create_multipart(note: &ResearchNote, boundary: Option<&str>) -> Result<(Vec<u8>, String)> {
    let generated;
    let boundary = match boundary {
        Some(b) => b,
        None => {
            generated = format!("notes-research-{}", util::token_hex(16));
            &generated
        }
    };
    if !boundary_ok(boundary) {
        return Err(Error::value("invalid multipart boundary"));
    }
    let mut raw = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"noteResource\"\r\nContent-Type: application/xml; charset=UTF-8\r\n\r\n"
    )
    .into_bytes();
    raw.extend_from_slice(&note.xml());
    raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((util::gzip_compress(&raw), format!("multipart/form-data;boundary={boundary}")))
}
