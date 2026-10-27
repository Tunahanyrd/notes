//! `previews._payload_from_snapshot` / `_inventory_from_snapshot`: the cached
//! snapshot's raw bytes are authenticated (schema, size, SHA256) and the MIME
//! payload is parsed exactly once per inventory.
use notes_host::{Fail, Kind, Val};
use rs_doc::payload::PayloadResult;
use rs_ink::core::payload::{InkInventory, ParsedPayload};
use rs_ink::core::InkErr;

/// A parsed payload plus the body its part ranges point into.
pub struct Parsed {
    pub body: Vec<u8>,
    pub result: PayloadResult,
}

impl Parsed {
    /// `PayloadResult.xml_documents`
    pub fn xml_documents(&self) -> Vec<Vec<u8>> {
        self.result.xml_parts.iter().map(|i| self.body[self.result.parts[*i].body.clone()].to_vec()).collect()
    }

    pub fn ink_input(&self) -> ParsedPayload {
        ParsedPayload::from_rs_doc(&self.result, &self.body)
    }
}

/// `tools.notes_payload.parse_payload(raw, content_type, max_parts=MAX_READONLY_PARTS)`
pub fn parse_payload(raw: Vec<u8>, content_type: &str) -> Result<Parsed, Fail> {
    let limits = rs_doc::mime::Limits::default();
    let result = rs_doc::payload::parse_payload(&raw, content_type, Some(limits.max_readonly_parts), &limits,
                                                &mut rs_doc::mime::NoHooks)?;
    Ok(Parsed { body: raw, result })
}

/// `previews._payload_from_snapshot(value)`
pub fn payload_from_snapshot(value: &Val) -> Result<Parsed, Fail> {
    let raw = notes_host::snapshot::authenticated_raw(value)?;
    let content_type = notes_host::snapshot::content_type(value)?;
    parse_payload(raw, &content_type)
}

/// rs-ink core error -> the Python exception class it stands for.
pub fn ink_fail(e: InkErr) -> Fail {
    match e {
        InkErr::Ink(m) => Fail::new(Kind::Ink, m),
        InkErr::Document(m) => Fail::document(m),
        InkErr::Payload(m) => Fail::new(Kind::Payload, m),
        InkErr::Struct(m) => Fail::new(Kind::Other, m),
        InkErr::Parse(m) => Fail::new(Kind::XmlParse, m),
        InkErr::KeyError(m) => Fail::key(m),
        InkErr::ValueError(m) => Fail::value(m),
        InkErr::TypeError(m) => Fail::type_(m),
        InkErr::Overflow(m) => Fail::new(Kind::Overflow, m),
        InkErr::Unicode(..) => Fail::new(Kind::Unicode, "'ascii' codec can't decode bytes"),
        InkErr::Foreign => Fail::new(Kind::Other, "foreign error"),
    }
}

/// `ink.inspect_ink_payload(parsed)`
pub fn inspect_ink_payload(parsed: &Parsed) -> Result<InkInventory, Fail> {
    rs_ink::core::payload::inspect_ink_payload(&parsed.ink_input()).map_err(ink_fail)
}

/// `previews._inventory_from_snapshot(value)`
pub fn inventory_from_snapshot(value: &Val) -> Result<InkInventory, Fail> {
    inspect_ink_payload(&payload_from_snapshot(value)?)
}
