//! `DocumentSnapshot.from_dict` / `to_dict` over `Val` (samsung_notes/document.py).
use rs_doc::doc::DocumentSnapshot;
use rs_doc::util;
use rs_store::val::{py_eq, Val};

use crate::err::{Fail, Kind, R};

pub use rs_doc::doc::DocumentSnapshot as Snapshot;

/// `document.MAX_BODY_BYTES`
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

fn opt_str(v: &Option<String>) -> Val {
    v.as_deref().map(Val::s).unwrap_or(Val::Null)
}

fn opt_int(v: Option<i64>) -> Val {
    v.map(Val::Int).unwrap_or(Val::Null)
}

/// `DocumentSnapshot.to_dict()` (same key order as Python).
pub fn to_dict(s: &DocumentSnapshot) -> Val {
    Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        ("title", Val::s(&s.title)),
        ("body", Val::s(&s.body)),
        ("editable", Val::Bool(s.editable)),
        ("read_only_reason", opt_str(&s.read_only_reason)),
        ("local_id", opt_str(&s.local_id)),
        ("commit_id", opt_str(&s.commit_id)),
        ("created_ms", opt_int(s.created_ms)),
        ("modified_ms", opt_int(s.modified_ms)),
        ("server_id", opt_str(&s.server_id)),
        ("content_type", Val::s(&s.content_type)),
        ("resource_xml", Val::s(util::b64encode(&s.resource_xml))),
        ("raw_body", Val::s(util::b64encode(&s.raw_body))),
        ("raw_sha256", Val::s(util::hex(&util::sha256(&s.raw_body)))),
    ])
}

/// Authenticated raw body of a stored snapshot dict (the checks shared by
/// `DocumentSnapshot.from_dict` and `previews._payload_from_snapshot`).
pub fn authenticated_raw(value: &Val) -> R<Vec<u8>> {
    let Val::Dict(_) = value else { return Err(Fail::document("Kayıtlı belge sürümü desteklenmiyor.")) };
    if !value.get("schemaVersion").map(|v| py_eq(v, &Val::Int(1))).unwrap_or(false) {
        return Err(Fail::document("Kayıtlı belge sürümü desteklenmiyor."));
    }
    let encoded = match value.get("raw_body") {
        None => "",
        Some(Val::Str(s)) => s.as_str(),
        Some(_) => return Err(Fail::document("Kayıtlı belge boyutu geçersiz.")),
    };
    if encoded.chars().count() > (MAX_BODY_BYTES + 2) / 3 * 4 {
        return Err(Fail::document("Kayıtlı belge boyutu geçersiz."));
    }
    if !encoded.is_ascii() {
        return Err(Fail::value("string argument should contain only ASCII characters"));
    }
    let raw = util::b64decode_strict(encoded.as_bytes()).ok_or_else(|| Fail::value("Invalid base64-encoded string"))?;
    let digest = util::hex(&util::sha256(&raw));
    if !value.get("raw_sha256").map(|v| py_eq(v, &Val::s(digest.clone()))).unwrap_or(false) {
        return Err(Fail::document("Kayıtlı belgenin bütünlük kontrolü başarısız."));
    }
    Ok(raw)
}

/// `value['content_type']` as parse_document sees it.
pub fn content_type(value: &Val) -> R<String> {
    match value.get("content_type") {
        None => Err(Fail::key("'content_type'")),
        Some(Val::Str(s)) => Ok(s.clone()),
        Some(_) => Err(Fail::new(Kind::Payload, "invalid content type")),
    }
}

/// `DocumentSnapshot.from_dict(value)`
pub fn from_dict(value: &Val) -> R<DocumentSnapshot> {
    let raw = authenticated_raw(value)?;
    let content_type = content_type(value)?;
    let server_id = match value.get("server_id") {
        Some(Val::Str(s)) => Some(s.clone()),
        _ => None,
    };
    parse_document(&raw, &content_type, server_id.as_deref())
}

/// `document.parse_document(raw, content_type, server_id=...)`
pub fn parse_document(raw: &[u8], content_type: &str, server_id: Option<&str>) -> R<DocumentSnapshot> {
    rs_doc::doc::parse_document(raw, content_type, server_id).map_err(Fail::from)
}

/// `json.loads(row['base_document'])` for a stored snapshot column.
pub fn loads(text: &str) -> R<Val> {
    rs_store::val::loads(text).map_err(|e| Fail::value(format!("{:?}", e)))
}
