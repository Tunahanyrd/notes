//! Small pure helpers mirroring document/notes_writer/ink_editor primitives.
use crate::err::*;
use crate::xml::Element;
use sha2::{Digest, Sha256};

pub const MAX_READONLY_PARTS: usize = 16384;
pub const MAX_STROKE_BYTES: usize = 2 * 1024 * 1024;
pub const I63: i128 = 1 << 63;

/// `document.integer(value)` without default.
pub fn integer(value: Option<&str>) -> R<i64> {
    const MSG: &str = "Belge zaman veya sürüm bilgisi geçersiz.";
    let v = value.ok_or_else(|| e_doc(MSG))?;
    if v.is_empty() || v.len() > 19 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return doc(MSG);
    }
    let n: u64 = v.parse().map_err(|_| e_doc(MSG))?;
    if n > i64::MAX as u64 {
        return doc(MSG);
    }
    Ok(n as i64)
}

/// `document.normalize_commit`.
pub fn normalize_commit(value: Option<&str>) -> R<String> {
    const MSG: &str = "Senkronizasyon sürümü eksik veya geçersiz.";
    let v = value.ok_or_else(|| e_doc(MSG))?;
    if v.is_empty() || v.chars().count() > 1024 || v.chars().any(|c| (c as u32) < 33) {
        return doc(MSG);
    }
    match v.find("_lc") {
        Some(i) if i > 0 => Ok(v[..i].to_string()),
        _ => Ok(v.to_string()),
    }
}

/// `str(uuid.UUID(v)) == v`.
pub fn canonical_uuid(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

fn uuid_parses(v: &str) -> bool {
    let s = v.replace("urn:", "").replace("uuid:", "");
    let s = s.trim_matches(|c| c == '{' || c == '}').replace('-', "");
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn hex(data: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

/// `notes_writer.object_hash` (ValueError on non-canonical id/time).
pub fn object_hash(id: &str, modified: i64) -> R<[u8; 32]> {
    if !canonical_uuid(id) {
        return value(if uuid_parses(id) { "canonical lower-case UUID required" } else { "badly formed hexadecimal UUID string" });
    }
    if modified < 1 {
        return value("positive signed-64-bit millisecond timestamp required");
    }
    Ok(sha256(format!("{id}{modified}").as_bytes()))
}

pub fn object_hash_opt(id: Option<&str>, modified: i64) -> R<[u8; 32]> {
    match id {
        Some(i) => object_hash(i, modified),
        None => value("canonical lower-case UUID required"),
    }
}

/// `notes_writer.parent_hash`.
pub fn parent_hash(id: Option<&str>, modified: i64, children: &[[u8; 32]]) -> R<[u8; 32]> {
    let own = object_hash_opt(id, modified)?;
    let mut h = Sha256::new();
    for c in children {
        h.update(c);
    }
    h.update(own);
    Ok(h.finalize().into())
}

/// `bytes.fromhex(value)` (ASCII whitespace between bytes allowed).
pub fn fromhex(input: Option<&str>) -> R<Vec<u8>> {
    let v = match input {
        Some(v) => v,
        None => return type_err("fromhex() argument must be str, not None"),
    };
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    let digit = |c: u8| (c as char).to_digit(16);
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let hi = digit(b[i]);
        let lo = if i + 1 < b.len() { digit(b[i + 1]) } else { None };
        match (hi, lo) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            (None, _) => return value(&format!("non-hexadecimal number found in fromhex() arg at position {i}")),
            (_, None) => return value(&format!("non-hexadecimal number found in fromhex() arg at position {}", i + 1)),
        }
        i += 2;
    }
    Ok(out)
}

/// A child hash for parent_hash: fromhex + 32-byte check.
pub fn hash32(value: Option<&str>) -> R<[u8; 32]> {
    let raw = fromhex(value)?;
    raw.try_into().map_err(|_| Error::new(Kind::Value, "child hashes must be raw SHA256 bytes"))
}

pub fn hashes<'a, I: IntoIterator<Item = &'a Element>>(nodes: I) -> R<Vec<[u8; 32]>> {
    nodes.into_iter().map(|n| hash32(n.get("hash"))).collect()
}

/// `ink_editor._f32`.
pub fn f32r(v: f64) -> R<f64> {
    let r = v as f32;
    if !r.is_finite() {
        return ink("non-finite or unrepresentable stroke value");
    }
    Ok(r as f64)
}

/// Python `repr(float)`.
pub fn py_float_repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let s = format!("{:e}", v.abs());
    let (mant, exp) = s.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let n = digits.len() as i32;
    let decpt = exp + 1;
    let sign = if v < 0.0 { "-" } else { "" };
    let body = if decpt <= -4 || decpt > 16 {
        let mut m = digits[..1].to_string();
        if n > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if decpt >= n {
        format!("{}{}.0", digits, "0".repeat((decpt - n) as usize))
    } else {
        format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
    };
    format!("{sign}{body}")
}

/// Fresh random UUID4 string.
pub fn uuid4() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system random source");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// `document.one(parent, name)` returning the child index.
pub fn one_idx(parent: &Element, name_lower: &str) -> R<usize> {
    let found: Vec<usize> = parent
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| c.local() == name_lower)
        .map(|(i, _)| i)
        .collect();
    if found.len() != 1 {
        return doc("Belgenin yapısı güvenli düzenleme için uygun değil.");
    }
    Ok(found[0])
}

pub fn one<'a>(parent: &'a Element, name_lower: &str) -> R<&'a Element> {
    Ok(&parent.children[one_idx(parent, name_lower)?])
}

pub fn one_mut<'a>(parent: &'a mut Element, name_lower: &str) -> R<&'a mut Element> {
    let i = one_idx(parent, name_lower)?;
    Ok(&mut parent.children[i])
}

/// `ids == [...]` style helper.
pub fn ids(nodes: &[Element]) -> Vec<Option<&str>> {
    nodes.iter().map(|n| n.get("id")).collect()
}

pub fn lower_ne_false(e: &Element, key: &str) -> bool {
    e.get_or(key, "false").to_lowercase() != "false"
}

/// `any(n.get('isLocked'...) ...)` locked/encrypted flags.
pub fn locked_flags(e: &Element) -> bool {
    lower_ne_false(e, "isLocked") || lower_ne_false(e, "isEncrypted") || lower_ne_false(e, "encrypted")
}

/// Strip root sections and filter for an upload copy: tag NoteResource,
/// bodyText/floatingObjectList removed.
pub fn upload_copy(root: &Element) -> Element {
    let mut upload = root.clone();
    upload.tag = "NoteResource".into();
    upload.children.retain(|n| n.tag != "bodyText" && n.tag != "floatingObjectList");
    upload
}

/// `rs_ink::core::objects::passthrough_object` over an [`Element`]: a
/// self-contained image/shape/line/text box an ink edit carries through
/// byte-identically. `Some(Some(file))` for an image (its content file id).
pub fn passthrough_object(o: &Element) -> Option<Option<String>> {
    if o.tag != "object" {
        return None;
    }
    _doc::objects::passthrough_object(&o.attrib, o.iter().skip(1).map(|n| (n.tag.as_str(), n.attrib.as_slice())))
}

/// The note's free-floating objects (`floatingObjectList` children, e.g. a
/// picture inserted on a phone, placed on page `pageIndex`) when an ink edit
/// may carry every one of them through byte-identically
/// (`rs_doc::objects::floating_inventory`: self-contained image/shape/line/
/// text box, a `pageIndex` inside the note, note-tree entries mirroring them).
/// Empty when the list is absent or empty (not validated further then, as
/// before floating objects were carried); `Err(Ink)` otherwise.
pub fn floating_objects(root: &Element) -> crate::err::R<Vec<&Element>> {
    let lists: Vec<&Element> = root.children.iter().filter(|c| c.tag == "floatingObjectList").collect();
    if lists.iter().all(|l| l.children.is_empty()) {
        return Ok(Vec::new());
    }
    let refuse = || crate::err::ink("floating objects are unsupported");
    let trees: Vec<&Element> = root.children.iter().filter(|c| c.tag == "noteTree").collect();
    let pages: Vec<&Element> = root.children.iter().filter(|c| c.tag == "pageList").collect();
    if lists.len() != 1 || trees.len() != 1 || pages.len() != 1 {
        return refuse();
    }
    let tree_lists: Vec<&Element> = trees[0].children.iter().filter(|c| c.tag == "floatingObjectList").collect();
    if tree_lists.len() != 1 {
        return refuse();
    }
    let ok = _doc::objects::floating_inventory(
        lists[0].children.iter().map(|o| {
            (o.tag.as_str(), o.attrib.as_slice(), o.iter().skip(1).map(|n| (n.tag.as_str(), n.attrib.as_slice())))
        }),
        tree_lists[0].children.iter().map(|e| (e.tag.as_str(), e.attrib.as_slice(), e.children.len())),
        pages[0].children.len(),
    );
    match ok {
        Some(_) => Ok(lists[0].children.iter().collect()),
        None => refuse(),
    }
}

/// Stroke objects of a page list, refusing anything that is neither a
/// top-level stroke nor a carried-through object (no object outside a layer
/// `objectList`, none nested). Returns (strokes, carried objects).
pub fn ink_objects(pages: &Element) -> crate::err::R<(Vec<&Element>, Vec<&Element>)> {
    let (mut strokes, mut others) = (Vec::new(), Vec::new());
    for list in pages.iter_tag("objectList") {
        for o in &list.children {
            if o.get("type") == Some("stroke") {
                strokes.push(o);
            } else if passthrough_object(o).is_some() {
                others.push(o);
            } else {
                return crate::err::ink("unsupported or ambiguous object inventory");
            }
        }
    }
    if pages.iter_tag("object").count() != strokes.len() + others.len() {
        return crate::err::ink("unsupported or ambiguous object inventory");
    }
    Ok((strokes, others))
}

/// Keep only the matching children of every `objectList` below `e` (upload
/// copies carry the edited objects only).
pub fn retain_objects(e: &mut Element, keep: &dyn Fn(&Element) -> bool) {
    if e.tag == "objectList" {
        e.children.retain(|o| keep(o));
        return;
    }
    for c in e.children.iter_mut() {
        retain_objects(c, keep);
    }
}
