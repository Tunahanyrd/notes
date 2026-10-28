//! Pure-Rust port of `samsung_notes.document`: bounded XML snapshots and
//! conservative plain-text edits.
use std::borrow::Cow;

use crate::error::{Error, Result};
use crate::mime::{self, Hooks, Limits, Part};
use crate::payload::{self, PayloadResult};
use crate::util;
use crate::writer;
use crate::xml::{self, Element};

pub const MAX_TEXT: usize = 256 * 1024;
pub const MAX_INT: i128 = i64::MAX as i128;

/// Limits of `samsung_notes.document` plus those of `tools.notes_payload`.
#[derive(Clone, Copy, Debug)]
pub struct DocLimits {
    pub max_body_bytes: usize,
    pub max_parts: i64,
    pub max_readonly_parts: i64,
    pub max_text: usize,
    pub payload: Limits,
}

impl Default for DocLimits {
    fn default() -> Self {
        let payload = Limits::default();
        DocLimits {
            max_body_bytes: payload.max_body_bytes,
            max_parts: payload.max_parts,
            max_readonly_parts: payload.max_readonly_parts,
            max_text: MAX_TEXT,
            payload,
        }
    }
}

const STRUCTURE: &str = "Belgenin yapısı güvenli düzenleme için uygun değil.";
const GENERIC: &str = "Bu notun biçimi henüz düzenlenemiyor.";

/// `one(parent, name)`: index of the single child whose lower local name matches.
pub fn one(parent: &Element, name: &str) -> Result<usize> {
    let mut found = None;
    for (i, child) in parent.children.iter().enumerate() {
        if child.local() == name {
            if found.is_some() {
                return Err(Error::document(STRUCTURE));
            }
            found = Some(i);
        }
    }
    found.ok_or_else(|| Error::document(STRUCTURE))
}

/// `text_value(value)` for a Rust string.
pub fn text_value(value: &str, max_text: usize) -> Result<()> {
    if value.len() > max_text || value.chars().any(writer::bad_text_char) {
        return Err(Error::document("Metin geçersiz veya desteklenen boyutu aşıyor."));
    }
    Ok(())
}

/// `encode_text(value)`.
pub fn encode_text(value: &str, max_text: usize) -> Result<String> {
    text_value(value, max_text)?;
    Ok(writer::double_b64(value))
}

/// `integer(value)` (default handled by callers).
pub fn integer(value: Option<&str>) -> Result<i128> {
    let err = || Error::document("Belge zaman veya sürüm bilgisi geçersiz.");
    let v = value.ok_or_else(err)?;
    if v.is_empty() || v.len() > 19 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    let n: i128 = v.parse().map_err(|_| err())?;
    if n > MAX_INT {
        return Err(err());
    }
    Ok(n)
}

/// `normalize_commit(value)`.
pub fn normalize_commit(value: Option<&str>) -> Result<&str> {
    let err = || Error::document("Senkronizasyon sürümü eksik veya geçersiz.");
    let v = value.ok_or_else(err)?;
    if v.is_empty() || v.chars().count() > 1024 || v.chars().any(|c| (c as u32) < 33) {
        return Err(err());
    }
    match v.find("_lc") {
        Some(i) if i > 0 => Ok(&v[..i]),
        _ => Ok(v),
    }
}

/// Paths (child indices from the resource root) of the editable components.
#[derive(Clone, Debug)]
pub struct PlainStructure {
    pub attrs: Vec<usize>,
    pub tree: Vec<usize>,
    pub title: Vec<usize>,
    pub body: Vec<usize>,
    pub tree_body: Vec<usize>,
}

fn py_strip_empty(s: &str) -> bool {
    s.chars().all(mime::py_is_space)
}

/// `_plain_structure(resource)`. Errors: `Document` (specific reason) or
/// `Value` (any other ValueError/UnicodeError the Python version raised).
pub fn plain_structure(resource: &Element, payload_limits: &Limits, max_text: usize) -> Result<PlainStructure> {
    structure(resource, payload_limits, max_text, false)
}

/// Structure gate of the rich text editor (title/body text objects only).
///
/// Unlike [`plain_structure`] it accepts pages whose layers hold objects
/// (handwriting, images, shapes) and their content files: an editor upload
/// sends the note tree and the changed text objects only (the page list and
/// content file list go out empty, exactly like a title edit), so every page
/// object and binary part stays byte-for-byte on the server. A text object
/// without a `<text>` child is a native empty text box. Still rejected:
/// objects outside the page list, free-floating objects, voice recordings /
/// attachments, and the inventory, identity and text-object checks of the
/// plain gate.
pub fn editor_structure(resource: &Element, payload_limits: &Limits, max_text: usize) -> Result<PlainStructure> {
    structure(resource, payload_limits, max_text, true)
}

/// Index of the at most one child with this lower local name.
fn optional_one(parent: &Element, name: &str) -> Result<Option<usize>> {
    let mut found = None;
    for (i, child) in parent.children.iter().enumerate() {
        if child.local() == name {
            if found.is_some() {
                return Err(Error::document(STRUCTURE));
            }
            found = Some(i);
        }
    }
    Ok(found)
}

/// Addresses of every node under the given subtrees (the subtree roots included).
fn subtree_set<'a>(roots: impl IntoIterator<Item = &'a Element>) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    for root in roots {
        for n in root.iter() {
            out.insert(n as *const Element as usize);
        }
    }
    out
}

pub const EDITOR_FLOATING: &str = "Bu nottaki serbest yerleşimli nesneler yüzünden metin henüz güvenle düzenlenemiyor.";
pub const EDITOR_MEDIA: &str = "Ses kaydı veya ek dosya içeren notlarda metin henüz güvenle düzenlenemiyor.";
pub const EDITOR_INCONSISTENT: &str = "Notun sayfa içeriği kendi içinde tutarsız görünüyor; metin güvenle düzenlenemiyor.";
pub const EDITOR_FOREIGN_OBJECT: &str = "Bu notta metnin içine yerleşmiş nesneler var; metin henüz güvenle düzenlenemiyor.";

fn structure(resource: &Element, payload_limits: &Limits, max_text: usize, editor: bool) -> Result<PlainStructure> {
    let d = Error::document;
    let attrs_i = one(resource, "attributes")?;
    let tree_i = one(resource, "notetree")?;
    let attrs = &resource.children[attrs_i];
    let tree = &resource.children[tree_i];
    if integer(attrs.get("formatVersion"))? != 4000 {
        return Err(d("Bu notun biçim sürümü henüz düzenlenemiyor."));
    }
    let at_title = one(attrs, "title")?;
    let title_i = one(&attrs.children[at_title], "object")?;
    let title_path = vec![attrs_i, at_title, title_i];
    let body_text_i = one(resource, "bodytext")?;
    let body_i = one(&resource.children[body_text_i], "object")?;
    let body_path = vec![body_text_i, body_i];
    let tree_body_i = one(tree, "bodytext")?;
    let title = resource.at(&title_path);
    let body = resource.at(&body_path);
    let tree_body = &tree.children[tree_body_i];
    const ALLOWED: [&str; 6] = ["rect", "originalrect", "drawnrect", "text", "linecoloreffect", "linestyleeffect"];
    for obj in [title, body] {
        if obj.get("type") != Some("textBox") {
            return Err(d("Bu not düz metinden farklı nesneler içeriyor."));
        }
        let text_i = if editor { optional_one(obj, "text")? } else { Some(one(obj, "text")?) };
        let encoded = text_i.and_then(|i| obj.children[i].text.clone()).unwrap_or_default();
        if obj.children.iter().any(|n| !ALLOWED.contains(&n.local().as_str())) {
            return Err(d("Ek metin biçimlendirmesi bu sürümde salt okunur."));
        }
        let compact: String = encoded.chars().filter(|c| !mime::py_is_space(*c)).collect();
        if !compact.is_ascii() {
            return Err(Error::value("string argument should contain only ASCII characters"));
        }
        let raw = util::b64decode_strict(compact.as_bytes()).ok_or_else(|| Error::value("Invalid base64-encoded string"))?;
        let rich = String::from_utf8(raw).map_err(|_| Error::value("'utf-8' codec can't decode bytes"))?;
        if rich.contains("<?a")
            || obj.iter().any(|n| {
                matches!(n.local().as_str(), "spanorderdatalist" | "zerolengthspanlist" | "path") && !n.children.is_empty()
            })
        {
            return Err(d("Biçimlendirilmiş metin bu sürümde salt okunur."));
        }
        let decoded = payload::decode_rich_text(&encoded, payload_limits).map_err(|e| Error::Value(Cow::Owned(e.to_string())))?;
        text_value(&decoded, max_text)?;
        match obj.get("id") {
            None | Some("") => return Err(d("Notun nesne kimlikleri desteklenmiyor.")),
            Some(id) => {
                if !util::uuid_parses(id) {
                    return Err(Error::value("badly formed hexadecimal UUID string"));
                }
                if !util::is_canonical_uuid(id) {
                    return Err(d("Notun nesne kimlikleri desteklenmiyor."));
                }
            }
        }
    }
    // Page content is carried unchanged by an editor write: the page and
    // content-file lists (and their note-tree mirrors) and the attributes'
    // string table that page objects refer to (pen/brush names).
    let page_content = if editor {
        let mut roots: Vec<&Element> = Vec::new();
        roots.extend(resource.children.iter().filter(|n| matches!(n.local().as_str(), "pagelist" | "contentfilelist")));
        roots.extend(tree.children.iter().filter(|n| matches!(n.local().as_str(), "pagelist" | "contentfilelist")));
        roots.extend(attrs.children.iter().filter(|n| n.local() == "stringidlist"));
        subtree_set(roots)
    } else {
        Default::default()
    };
    let in_pages = |n: &Element| page_content.contains(&(n as *const Element as usize));
    if editor {
        for node in resource.iter() {
            let name = node.local();
            if matches!(name.as_str(), "floatingobjectlist") && !node.children.is_empty() {
                return Err(d(EDITOR_FLOATING));
            }
            if matches!(name.as_str(), "attachment" | "voice" | "extrabundle") {
                return Err(d(EDITOR_MEDIA));
            }
        }
        if resource.iter().filter(|n| n.local() == "object" && !in_pages(n)).count() != 2 {
            return Err(d(EDITOR_FOREIGN_OBJECT));
        }
    } else if resource.iter().filter(|n| n.local() == "object").count() != 2 {
        return Err(d("Çizim veya ek nesne içeren notlar salt okunur."));
    }
    let body_text = &resource.children[body_text_i];
    if tree_body.get("id") != body.get("id") || body_text.get("id") != body.get("id") {
        return Err(d("Belge gövde kimlikleri uyuşmuyor."));
    }
    let pages = &resource.children[one(resource, "pagelist")?].children;
    let tree_pages = &tree.children[one(tree, "pagelist")?].children;
    if pages.is_empty() || pages.len() > 1000 || pages.len() != tree_pages.len() {
        return Err(d("Sayfa yapısı bu sürümde düzenlenemiyor."));
    }
    let ptr = |e: &Element| e as *const Element as usize;
    let mut known: Vec<usize> = vec![ptr(resource), ptr(title), ptr(body), ptr(body_text), ptr(tree_body)];
    let mut unique: Vec<Option<&str>> = Vec::new();
    for id in [resource.get("id"), title.get("id"), body.get("id")] {
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    if unique.len() != 3 {
        return Err(d("Tekrarlanan belge kimliği var."));
    }
    for (page, tp) in pages.iter().zip(tree_pages.iter()) {
        if page.local() != "page" || tp.local() != "page" || page.get("id") != tp.get("id") {
            return Err(d("Sayfa envanteri uyuşmuyor."));
        }
        let layers = &page.children[one(page, "layerlist")?].children;
        let tlayers = &tp.children[one(tp, "layerlist")?].children;
        if layers.is_empty() || layers.len() != tlayers.len() {
            return Err(d("Katman envanteri uyuşmuyor."));
        }
        let pairs = std::iter::once((page, tp)).chain(layers.iter().zip(tlayers.iter()));
        for (a, b) in pairs {
            let identity = a.get("id");
            if identity.map_or(true, str::is_empty) || unique.contains(&identity) || b.get("id") != identity {
                return Err(d("Tekrarlanan veya farklı kaynak kimliği var."));
            }
            unique.push(identity);
            known.push(ptr(a));
            known.push(ptr(b));
        }
        for layer in layers.iter().chain(tlayers.iter()) {
            if layer.local() != "layer"
                || (!editor
                    && layer.children.iter().any(|n| {
                        n.local() != "objectlist" || !n.children.is_empty() || !py_strip_empty(n.text.as_deref().unwrap_or(""))
                    }))
            {
                return Err(d("Çizim içeren notlar salt okunur."));
            }
        }
        if editor {
            // The note tree is uploaded as the server's own inventory of the
            // pages: refuse a document whose page objects and tree disagree.
            let ids = |layer: &Element| -> Vec<Option<String>> {
                layer
                    .children
                    .iter()
                    .filter(|n| n.local() == "objectlist")
                    .flat_map(|list| list.children.iter().map(|o| o.get("id").map(str::to_string)))
                    .collect()
            };
            if layers.iter().zip(tlayers.iter()).any(|(a, b)| ids(a) != ids(b)) {
                return Err(d(EDITOR_INCONSISTENT));
            }
        }
    }
    if resource.iter().any(|n| n.has_attr("id") && !known.contains(&ptr(n)) && !in_pages(n)) {
        return Err(d("Ek kaynak içeren notlar salt okunur."));
    }
    for node in resource.iter() {
        if in_pages(node) {
            continue;
        }
        let name = node.local();
        if matches!(name.as_str(), "contentfile" | "downloadurl" | "stroke" | "attachment" | "voice" | "extrabundle") {
            return Err(d("Ek veya gömülü veri içeren notlar salt okunur."));
        }
        if matches!(name.as_str(), "contentfilelist" | "floatingobjectlist") && !node.children.is_empty() {
            return Err(d("Ek veya çizim içeren notlar salt okunur."));
        }
    }
    Ok(PlainStructure {
        attrs: vec![attrs_i],
        tree: vec![tree_i],
        title: title_path,
        body: body_path,
        tree_body: vec![tree_i, tree_body_i],
    })
}

/// Parsed snapshot fields (raw body, content type and server id are the caller's inputs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedDocument {
    pub title: String,
    pub body: String,
    pub editable: bool,
    pub read_only_reason: Option<String>,
    pub local_id: Option<String>,
    pub commit_id: Option<String>,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    pub resource_xml: Vec<u8>,
}

/// Full snapshot (pure-Rust form of `DocumentSnapshot`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSnapshot {
    pub title: String,
    pub body: String,
    pub editable: bool,
    pub read_only_reason: Option<String>,
    pub local_id: Option<String>,
    pub commit_id: Option<String>,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    pub resource_xml: Vec<u8>,
    pub raw_body: Vec<u8>,
    pub content_type: String,
    pub server_id: Option<String>,
}

/// Source of the MIME parts used by the `parse_payload` step: `None` reuses
/// the parts already split by `parse_document`.
pub type Repart<'a> = &'a mut dyn FnMut() -> Result<Option<(Vec<Part>, Vec<u8>)>>;

/// `parse_document(raw, content_type)` without the raw/content_type/server_id copies.
pub fn parse_document_fields<H: Hooks>(
    raw: &[u8],
    content_type: &str,
    limits: &DocLimits,
    hooks: &mut H,
    repart: Repart<'_>,
) -> Result<ParsedDocument> {
    if raw.is_empty() || raw.len() > limits.max_body_bytes {
        return Err(Error::document("Belge boş veya boyut sınırını aşıyor."));
    }
    let pl = &limits.payload;
    let parts = mime::parts(raw, content_type, limits.max_readonly_parts, pl, hooks)?;
    let mut roots: Vec<Element> = Vec::new();
    for p in parts.iter().filter(|p| payload::is_xml_type(&p.content_type)) {
        roots.push(payload::parse_xml(&raw[p.body.clone()], pl)?);
    }
    let mut resources: Vec<(usize, Vec<usize>)> = Vec::new();
    for (ri, root) in roots.iter().enumerate() {
        // pre-order with paths
        let mut stack: Vec<(&Element, Vec<usize>)> = vec![(root, Vec::new())];
        while let Some((node, path)) = stack.pop() {
            if node.local() == "noteresource" {
                resources.push((ri, path.clone()));
            }
            for (i, child) in node.children.iter().enumerate().rev() {
                let mut p = path.clone();
                p.push(i);
                stack.push((child, p));
            }
        }
    }
    if resources.len() != 1 {
        return Err(Error::document("Yanıtta tek bir not bulunamadı."));
    }
    let (ri, rpath) = &resources[0];
    let resource = roots[*ri].at(rpath);
    let encrypted = resource.iter().any(|n| {
        n.get("encrypted").unwrap_or("").to_lowercase() == "true" || n.get("isEncrypted").unwrap_or("").to_lowercase() == "true"
    });
    let mut title = String::new();
    let mut body = String::new();
    let mut reason: Option<String> = None;
    if encrypted {
        reason = Some("Şifreli notlar bu sürümde açılamıyor.".into());
    } else {
        payload::check_body(raw, pl)?;
        if !(1..=pl.max_readonly_parts).contains(&limits.max_readonly_parts) {
            return Err(Error::Payload("invalid MIME part bound"));
        }
        let parsed: PayloadResult = match repart()? {
            Some((new_parts, buf)) => payload::payload_from_parts(&buf, new_parts, pl)?,
            None => {
                // same parts and roots: only the walk remains
                let mut out = PayloadResult::default();
                let mut preview = 0usize;
                let xml_idx: Vec<usize> =
                    parts.iter().enumerate().filter(|(_, p)| payload::is_xml_type(&p.content_type)).map(|(i, _)| i).collect();
                for (k, root) in roots.iter().enumerate() {
                    out.xml_parts.push(xml_idx[k]);
                    out.xml_roots.push(xml::local_casefold(&root.tag));
                    payload::walk_root(root, &mut out, &mut preview, pl)?;
                }
                if out.xml_parts.is_empty() {
                    return Err(Error::Payload("payload contains no supported XML document"));
                }
                out
            }
        };
        let titles: Vec<&str> = parsed.text_previews.iter().filter(|p| p.context == "title").map(|p| p.text.as_str()).collect();
        let bodies: Vec<&str> = parsed.text_previews.iter().filter(|p| p.context == "body").map(|p| p.text.as_str()).collect();
        if titles.len() == 1 {
            title = titles[0].to_string();
        }
        let mut all: Vec<&str> = bodies.clone();
        all.extend(parsed.text_previews.iter().filter(|p| p.context == "object").map(|p| p.text.as_str()));
        body = all.join("\n");
        if parsed.locked {
            reason = Some("Kilitli notlar bu sürümde salt okunur.".into());
        } else if parts.len() as i64 > limits.max_parts
            || !parsed.content_files.is_empty()
            || !parsed.download_urls.is_empty()
            || parts.len() != roots.len()
        {
            reason = Some("Ek içeren notlar bu sürümde salt okunur.".into());
        } else if titles.len() != 1 || bodies.len() != 1 {
            reason = Some("Bu notun metin yapısı henüz düzenlenemiyor.".into());
        } else {
            match plain_structure(resource, pl, limits.max_text) {
                Ok(_) => {}
                Err(Error::Document(m)) => reason = Some(m.into_owned()),
                Err(Error::Value(_)) | Err(Error::Payload(_)) | Err(Error::Type(_)) => reason = Some(GENERIC.into()),
                Err(other) => return Err(other),
            }
        }
    }
    // _commit(roots)
    let mut commits: Vec<&Element> = Vec::new();
    for root in &roots {
        commits.extend(root.iter().filter(|n| n.local() == "commitid"));
    }
    let commit = if commits.len() != 1 || !commits[0].children.is_empty() {
        None
    } else {
        let value = commits[0].text.as_deref();
        normalize_commit(value)?;
        value.map(str::to_string)
    };
    if commit.is_none() && reason.is_none() {
        reason = Some("Notun senkronizasyon sürümü alınamadı.".into());
    }
    let attrs = resource.children.iter().find(|n| n.local() == "attributes");
    let date = |key: &str| -> Option<i64> { attrs.and_then(|a| integer(a.get(key)).ok()).map(|v| v as i64) };
    Ok(ParsedDocument {
        title,
        body,
        editable: reason.is_none(),
        read_only_reason: reason,
        local_id: resource.get("id").map(str::to_string),
        commit_id: commit,
        created_ms: date("displayCreatedTime"),
        modified_ms: date("displayModifiedTime"),
        resource_xml: xml::tostring(resource)?,
    })
}

/// `parse_document(raw, content_type, server_id=...)` with default limits.
pub fn parse_document(raw: &[u8], content_type: &str, server_id: Option<&str>) -> Result<DocumentSnapshot> {
    let f = parse_document_fields(raw, content_type, &DocLimits::default(), &mut mime::NoHooks, &mut || Ok(None))?;
    Ok(DocumentSnapshot {
        title: f.title,
        body: f.body,
        editable: f.editable,
        read_only_reason: f.read_only_reason,
        local_id: f.local_id,
        commit_id: f.commit_id,
        created_ms: f.created_ms,
        modified_ms: f.modified_ms,
        resource_xml: f.resource_xml,
        raw_body: raw.to_vec(),
        content_type: content_type.to_string(),
        server_id: server_id.map(str::to_string),
    })
}

/// `now_ms` argument as seen by Python code.
#[derive(Clone, Copy, Debug)]
pub enum NowArg {
    /// `None`: use the clock.
    Missing,
    /// An exact `int` (may be outside i64).
    Int(i128),
    /// Any other object.
    Invalid,
}

impl NowArg {
    fn resolve(self) -> Option<i128> {
        match self {
            NowArg::Missing => Some(writer::now_ms() as i128),
            NowArg::Int(v) => Some(v),
            NowArg::Invalid => None,
        }
    }
}

/// `new_document(title, body, local_id=..., now_ms=...)` -> (identity, XML).
pub fn new_document(title: &str, body: &str, local_id: Option<&str>, now: NowArg, max_text: usize) -> Result<(String, Vec<u8>)> {
    text_value(title, max_text)?;
    text_value(body, max_text)?;
    let identity = match local_id {
        None => util::uuid4(),
        Some(id) => {
            if !util::uuid_parses(id) {
                return Err(Error::value("badly formed hexadecimal UUID string"));
            }
            if !util::is_canonical_uuid(id) {
                return Err(Error::document("Yeni not kimliği geçersiz."));
            }
            id.to_string()
        }
    };
    let children: Vec<String> =
        ["title", "body", "page", "layer"].iter().map(|l| util::uuid5(&identity, l).expect("canonical")).collect();
    let now = now.resolve().ok_or_else(|| Error::value("positive signed-64-bit millisecond timestamp required"))?;
    let note = writer::ResearchNote::new(
        [&identity, &children[0], &children[1], &children[2], &children[3]],
        now,
        now,
        if title.is_empty() { " " } else { title },
        if body.is_empty() { " " } else { body },
        1,
    )?;
    let root = note.element_with_texts(&encode_text(title, max_text)?, &encode_text(body, max_text)?);
    Ok((identity, xml::tostring(&root)?))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentEdit {
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub requested_ids: Vec<String>,
    pub title: String,
    pub body: String,
}

/// The snapshot fields `prepare_edit` reads.
pub struct EditSource<'a> {
    pub editable: bool,
    pub read_only_reason: Option<&'a str>,
    /// `None` when the snapshot attribute is not a str (always "changed").
    pub title: Option<&'a str>,
    pub body: Option<&'a str>,
    pub resource_xml: &'a [u8],
}

/// `prepare_edit(snapshot, title, body, now_ms=...)`.
pub fn prepare_edit(src: &EditSource<'_>, title: &str, body: &str, now: NowArg, limits: &DocLimits) -> Result<DocumentEdit> {
    if !src.editable {
        let reason = src.read_only_reason.filter(|r| !r.is_empty()).unwrap_or("Bu not salt okunur.");
        return Err(Error::Document(Cow::Owned(reason.to_string())));
    }
    text_value(title, limits.max_text)?;
    text_value(body, limits.max_text)?;
    let mut root = xml::fromstring(src.resource_xml)?;
    let s = plain_structure(&root, &limits.payload, limits.max_text)?;
    let title_changed = src.title != Some(title);
    let body_changed = src.body != Some(body);
    if !title_changed && !body_changed {
        return Err(Error::document("Kaydedilecek bir değişiklik yok."));
    }
    let clock = || Error::document("Saat bilgisi sunucudaki nottan yeni değil.");
    let now = now.resolve().ok_or_else(clock)?;
    let sync = integer(root.get("syncModifiedTime"))?;
    if !(sync < now && now <= MAX_INT) {
        return Err(clock());
    }
    let tick = {
        let a = integer(root.at(&s.attrs).get("modifiedTime"))?;
        let b = integer(root.at(&s.body).get("modifiedTime"))?;
        let c = integer(root.at(&s.tree_body).get("t"))?;
        let d = integer(root.at(&s.title).get("modifiedTime"))?;
        a.max(b).max(c).max(d) + 1
    };
    let revision = integer(root.at(&s.attrs).get("fileRevision"))? + 1;
    if tick > MAX_INT || revision > i32::MAX as i128 {
        return Err(Error::document("Belge sürüm sınırına ulaşıldı."));
    }
    let mut requested = Vec::new();
    for (path, changed, value) in [(&s.title, title_changed, title), (&s.body, body_changed, body)] {
        if changed {
            let encoded = encode_text(value, limits.max_text)?;
            let obj = root.at_mut(path);
            let ti = one(obj, "text")?;
            obj.children[ti].text = Some(encoded);
            obj.set("modifiedTime", tick.to_string());
            let id = obj.get("id").unwrap_or("").to_string();
            obj.set("hash", util::hex(&writer::object_hash(&id, tick)?));
        }
    }
    if body_changed {
        let hash = root.at(&s.body).get("hash").unwrap_or("").to_string();
        let id = root.at(&s.body).get("id").unwrap_or("").to_string();
        let tb = root.at_mut(&s.tree_body);
        tb.set("t", tick.to_string());
        tb.set("h", hash);
        requested.push(id);
    }
    {
        let attrs = root.at_mut(&s.attrs);
        attrs.set("modifiedTime", tick.to_string());
        attrs.set("fileRevision", revision.to_string());
        attrs.set("displayModifiedTime", now.to_string());
    }
    root.set("syncModifiedTime", now.to_string());
    let full = xml::tostring(&root)?;
    let tree_raw = xml::tostring(root.at(&s.tree))?;
    let mut upload = root;
    upload.tag = "NoteResource".into();
    let pi = one(&upload, "pagelist")?;
    upload.children.remove(pi);
    if !body_changed {
        let bi = one(&upload, "bodytext")?;
        upload.children.remove(bi);
    }
    Ok(DocumentEdit {
        full_xml: full,
        tree_xml: tree_raw,
        upload_xml: xml::tostring(&upload)?,
        requested_ids: requested,
        title: title.to_string(),
        body: body.to_string(),
    })
}

/// `multipart(xml)` -> (gzip bytes, Content-Type).
pub fn multipart(xml_bytes: &[u8]) -> (Vec<u8>, String) {
    let boundary = format!("notes-app-{}", util::token_hex(16));
    let mut raw = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"noteResource\"\r\nContent-Type: application/xml; charset=UTF-8\r\n\r\n"
    )
    .into_bytes();
    raw.extend_from_slice(xml_bytes);
    raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (util::gzip_compress(&raw), format!("multipart/form-data;boundary={boundary}"))
}

/// `page_inventory(snapshot)` from the snapshot's `resource_xml`.
pub fn page_inventory(resource_xml: &[u8]) -> Result<String> {
    let root = xml::fromstring(resource_xml)?;
    let pi = one(&root, "pagelist")?;
    xml::canonicalize_subtree(&root.children[pi])
}

/// `DocumentSnapshot.to_dict()` JSON-ready pieces for pure-Rust users.
pub fn raw_sha256_hex(raw: &[u8]) -> String {
    util::hex(&util::sha256(raw))
}
