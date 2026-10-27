//! Samsung rich-text stream codec (pure Rust port of the codec half of
//! samsung_notes/editor_document.py): span/paragraph/section annotations,
//! normalize_model, read/encode, remap_sections, model fingerprint.
use std::collections::{BTreeMap, HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::pyval::*;

pub const MAX_ANNOTATIONS: usize = 4096;
pub const MAX_ENCODED: usize = 4 * 1024 * 1024;
pub const MAX_TEXT: usize = 256 * 1024;
pub const SPAN_TYPES: &[(&str, i64)] = &[("foregroundColor", 1), ("fontSize", 3), ("bold", 5), ("italic", 6),
    ("underline", 7), ("hyperText", 9), ("backgroundColor", 17), ("strikethrough", 20)];
pub const ALIGNMENTS: &[(&str, i64)] = &[("left", 0), ("right", 1), ("center", 2), ("justify", 3)];
pub const BULLETS: &[(&str, i64)] = &[("none", 0), ("check", 2), ("number", 4), ("alphabet", 6), ("roman", 7),
    ("disc", 8), ("whiteCircle", 9), ("upperAlphabet", 10), ("blackSquare", 11), ("whiteSquare", 12)];
const DIRECTIONS: &[(&str, i64)] = &[("none", 0), ("ltr", 1), ("rtl", 2)];

/// Error classes raised by the editor port (mapped to Python exceptions).
#[derive(Debug)]
pub enum Error {
    /// samsung_notes.document.DocumentError
    Document(String),
    Value(String),
    Type(String),
    Overflow(String),
    Attribute(String),
    /// xml.etree.ElementTree.ParseError
    Parse(String),
    /// tools.notes_payload.PayloadError (a ValueError subclass)
    Payload(String),
    /// Error raised by a not-yet-ported dependency; re-raised unchanged.
    External { kind: ExtKind, message: String, payload: Box<dyn std::any::Any + Send> },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExtKind { Document, Value, Type, Parse, Other }

impl Error {
    /// `except ValueError` (DocumentError is a ValueError subclass).
    pub fn is_value_error(&self) -> bool {
        matches!(self, Error::Document(_) | Error::Value(_) | Error::Payload(_)
            | Error::External { kind: ExtKind::Document | ExtKind::Value, .. })
    }
    pub fn is_document_error(&self) -> bool {
        matches!(self, Error::Document(_) | Error::External { kind: ExtKind::Document, .. })
    }
    /// `except (DocumentError, ValueError, TypeError, ET.ParseError)`.
    pub fn is_format_error(&self) -> bool {
        self.is_value_error() || matches!(self, Error::Type(_) | Error::Parse(_)
            | Error::External { kind: ExtKind::Type | ExtKind::Parse, .. })
    }
    pub fn message(&self) -> String {
        match self {
            Error::Document(m) | Error::Value(m) | Error::Type(m) | Error::Overflow(m)
            | Error::Attribute(m) | Error::Parse(m) | Error::Payload(m) => m.clone(),
            Error::External { message, .. } => message.clone(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<rs_doc::Error> for Error {
    fn from(e: rs_doc::Error) -> Self {
        match e {
            rs_doc::Error::Payload(m) => Error::Payload(m.to_string()),
            rs_doc::Error::Document(m) => Error::Document(m.into_owned()),
            rs_doc::Error::Value(m) => Error::Value(m.into_owned()),
            rs_doc::Error::Type(m) => Error::Type(m.into_owned()),
            rs_doc::Error::XmlParse(m) => Error::Parse(m),
            rs_doc::Error::Io(e) => Error::External { kind: ExtKind::Other, message: e.to_string(), payload: Box::new(()) },
            rs_doc::Error::External(payload) => Error::External { kind: ExtKind::Other, message: "external error".into(), payload },
        }
    }
}

pub fn doc<T>(message: &str) -> Result<T> {
    Err(Error::Document(message.to_string()))
}

// ---- samsung_notes.document helpers (local copies until rs-doc is stable) --

/// document.text_value
pub fn text_value(value: &Val) -> Result<String> {
    let bad = || Error::Document("Metin geçersiz veya desteklenen boyutu aşıyor.".into());
    let Val::Str(s) = value else { return Err(bad()) };
    if s.len() > MAX_TEXT || s.chars().any(|c| ((c as u32) < 32 && !matches!(c, '\n' | '\r' | '\t'))
        || c == '\u{fffe}' || c == '\u{ffff}') {
        return Err(bad());
    }
    Ok(s.clone())
}

pub fn text_value_str(s: &str) -> Result<String> {
    text_value(&Val::Str(s.to_string()))
}

/// document.encode_text
pub fn encode_text(value: &str) -> Result<String> {
    let value = text_value_str(value)?;
    Ok(b64encode(b64encode(value.as_bytes()).as_bytes()))
}

/// document.integer (no default)
pub fn integer(value: Option<&str>) -> Result<i128> {
    let bad = || Error::Document("Belge zaman veya sürüm bilgisi geçersiz.".into());
    let s = value.ok_or_else(bad)?;
    if s.is_empty() || s.len() > 19 || !s.bytes().all(|b| b.is_ascii_digit()) { return Err(bad()); }
    let v: i128 = s.parse().map_err(|_| bad())?;
    if v > i64::MAX as i128 { return Err(bad()); }
    Ok(v)
}

// ---- primitives -----------------------------------------------------------

pub fn utf16_length(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

pub fn b64(value: &str) -> String {
    b64encode(value.as_bytes())
}

/// editor_document._decode
pub fn decode(value: &str) -> Result<String> {
    if value.chars().count() > MAX_ENCODED {
        return doc("Biçimlendirme verisi boyut sınırını aşıyor.");
    }
    let joined: String = value.chars().filter(|c| !py_isspace(*c)).collect();
    let bytes = b64decode_strict(&joined).ok_or_else(|| Error::Document("Biçimlendirme kodlaması geçersiz.".into()))?;
    String::from_utf8(bytes).map_err(|_| Error::Document("Biçimlendirme kodlaması geçersiz.".into()))
}

/// UTF-16 offset -> byte index in `text` for every code point boundary.
pub struct Boundaries(Vec<Option<usize>>);

impl Boundaries {
    pub fn new(text: &str) -> Self {
        let mut map = vec![Some(0)];
        for (index, c) in text.char_indices() {
            if c.len_utf16() == 2 { map.push(None); }
            map.push(Some(index + c.len_utf8()));
        }
        Boundaries(map)
    }
    pub fn byte(&self, offset: i64) -> Option<usize> {
        if offset < 0 { return None; }
        self.0.get(offset as usize).copied().flatten()
    }
    pub fn contains(&self, offset: i64) -> bool {
        self.byte(offset).is_some()
    }
}

fn contains_val(b: &Boundaries, v: &Val) -> Option<i64> {
    let x = v.exact_i64()?;
    if b.contains(x) { Some(x) } else { None }
}

/// editor_document._paragraph_ranges
pub fn paragraph_ranges(text: &str) -> Vec<(i64, i64)> {
    let (mut start, mut offset, mut ranges) = (0i64, 0i64, Vec::new());
    for c in text.chars() {
        if c == '\r' || c == '\n' {
            ranges.push(if offset == 0 { (-1, -1) } else { (start, offset) });
            if offset != 0 { start = offset; }
        }
        offset += c.len_utf16() as i64;
    }
    ranges.push((start, offset));
    ranges
}

fn lookup(table: &[(&'static str, i64)], name: &str) -> Option<i64> {
    table.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn rlookup(table: &[(&'static str, i64)], value: i64) -> Option<&'static str> {
    table.iter().find(|(_, v)| *v == value).map(|(k, _)| *k)
}

/// Python `float(value)` for model values; Err(true) means OverflowError.
fn py_float(value: &Val) -> std::result::Result<f64, bool> {
    match value {
        Val::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Val::Int(i, _) => {
            let f = i.to_f64();
            if f.is_infinite() { Err(true) } else { Ok(f) }
        }
        Val::Float(f) => Ok(*f),
        Val::Str(s) => parse_py_float(s).ok_or(false),
        Val::Other(Some(f)) => Ok(*f),
        _ => Err(false),
    }
}

fn hyper_data_ok(data: &str) -> bool {
    data.chars().count() <= 4096
        && !data.chars().any(|c| c == ',' || c == '\'' || (c as u32) <= 0x1f)
        && !data.contains("?>") && !data.contains("<?")
}

fn ascii_digits(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// `_span_value(kind, value)` (model value -> wire string).
pub fn span_value_model(kind: &Val, value: &Val) -> Result<String> {
    let kind = kind.as_str().unwrap_or("\u{0}");
    match kind {
        "bold" | "italic" | "underline" | "strikethrough" => match value {
            Val::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
            _ => doc("Metin biçimi açık/kapalı olmalı."),
        },
        "fontSize" => {
            let number = match py_float(value) {
                Ok(n) => n,
                Err(true) => return Err(Error::Overflow("int too large to convert to float".into())),
                Err(false) => return doc("Yazı boyutu geçersiz."),
            };
            if matches!(value, Val::Bool(_)) || !number.is_finite() || !(number > 0.0 && number <= 1000.0) {
                return doc("Yazı boyutu geçersiz.");
            }
            Ok(py_float_repr(number))
        }
        "hyperText" => {
            let bad = || Error::Document("Bağlantı biçimi geçersiz.".into());
            let d = value.dict().ok_or_else(bad)?;
            if !same_keys(d, &["type", "dateTimeType", "data"]) { return Err(bad()); }
            let t = dget(d, "type").unwrap().exact_i64().filter(|v| (0..10).contains(v)).ok_or_else(bad)?;
            let dt = dget(d, "dateTimeType").unwrap().exact_i64().filter(|v| (0..22).contains(v)).ok_or_else(bad)?;
            match dget(d, "data").unwrap() {
                Val::None => Ok(format!("{t},{dt}")),
                Val::Str(s) if hyper_data_ok(s) => Ok(format!("{t},{dt},{s}")),
                _ => Err(bad()),
            }
        }
        "foregroundColor" | "backgroundColor" => {
            let s = match value { Val::Str(s) => s.as_str(), _ => "" };
            let hex = s.strip_prefix('#').unwrap_or("");
            if !s.starts_with('#') || !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return doc("Renk #AARRGGBB veya #RRGGBB biçiminde olmalı.");
            }
            let digits = if hex.len() == 8 { hex.to_string() } else { format!("FF{hex}") };
            let number = i64::from_str_radix(&digits, 16).unwrap();
            Ok((if number < 1 << 31 { number } else { number - (1 << 32) }).to_string())
        }
        _ => doc("Bu metin biçimi henüz güvenle düzenlenemiyor."),
    }
}

/// `_span_value(kind, value, wire=True)` (wire string -> model value).
pub fn span_value_wire(kind: &str, value: &str) -> Result<Val> {
    match kind {
        "bold" | "italic" | "underline" | "strikethrough" => match value {
            "true" => Ok(Val::Bool(true)),
            "false" => Ok(Val::Bool(false)),
            _ => doc("Geçersiz metin biçimi."),
        },
        "fontSize" => {
            let number = parse_py_float(value).ok_or_else(|| Error::Document("Yazı boyutu geçersiz.".into()))?;
            if !number.is_finite() || !(number > 0.0 && number <= 1000.0) { return doc("Yazı boyutu geçersiz."); }
            Ok(Val::Float(number))
        }
        "hyperText" => {
            let parts: Vec<&str> = value.splitn(3, ',').collect();
            let ok = parts.len() >= 2 && ascii_digits(parts[0], 1, 2) && ascii_digits(parts[1], 1, 2)
                && (parts.len() == 2 || !parts[2].contains(',') && !parts[2].contains('\''));
            if !ok { return doc("Bağlantı biçimi desteklenmiyor."); }
            let data = if parts.len() == 3 { Val::str(parts[2]) } else { Val::None };
            let result = Val::Dict(vec![
                (Val::str("type"), Val::int(parts[0].parse().unwrap())),
                (Val::str("dateTimeType"), Val::int(parts[1].parse().unwrap())),
                (Val::str("data"), data)]);
            span_value_model(&Val::str("hyperText"), &result)?;
            Ok(result)
        }
        "foregroundColor" | "backgroundColor" => {
            let bad = || Error::Document("Metin rengi geçersiz.".into());
            let number = parse_py_int(value).and_then(|v| v.to_i64()).ok_or_else(bad)?;
            if !(-(1i64 << 31)..(1i64 << 31)).contains(&number) { return Err(bad()); }
            Ok(Val::Str(format!("#{:08X}", (number as u64) & 0xffff_ffff)))
        }
        _ => doc("Bu metin biçimi henüz güvenle düzenlenemiyor."),
    }
}

/// `_paragraph_value(kind, value)` (model -> wire).
pub fn paragraph_value_model(kind: &str, value: &Val) -> Result<String> {
    match kind {
        "alignment" => match value.as_str().and_then(|s| lookup(ALIGNMENTS, s)) {
            Some(v) => Ok(v.to_string()),
            None => doc("Paragraf hizası geçersiz."),
        },
        "parsingState" => match value {
            Val::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
            _ => doc("Paragraf metadata bilgisi geçersiz."),
        },
        "indentLevel" => {
            let bad = || Error::Document("Girinti biçimi geçersiz.".into());
            let d = value.dict().ok_or_else(bad)?;
            if !same_keys(d, &["level", "direction"]) { return Err(bad()); }
            let level = dget(d, "level").unwrap().exact_i64().filter(|v| (0..=32).contains(v)).ok_or_else(bad)?;
            let direction = dget(d, "direction").unwrap().as_str().and_then(|s| lookup(DIRECTIONS, s)).ok_or_else(bad)?;
            Ok(format!("{level},{direction}"))
        }
        "bullet" => {
            let d = match value.dict() {
                Some(d) if !extra_keys(d, &["kind", "number", "checked", "typeList"]) => d,
                _ => return doc("Liste biçimi geçersiz."),
            };
            let name = match dget(d, "kind").and_then(Val::as_str) {
                Some(n) if lookup(BULLETS, n).is_some() => n,
                _ => return doc("Liste biçimi geçersiz."),
            };
            let second = if name == "check" {
                match dget(d, "checked") {
                    Some(Val::Bool(b)) if !dhas(d, "number") => if *b { "true".to_string() } else { "false".to_string() },
                    _ => return doc("Onay kutusu değeri geçersiz."),
                }
            } else {
                let number = dget(d, "number").cloned().unwrap_or(Val::int(0));
                match number.exact_i64() {
                    Some(n) if (0..(1i64 << 31)).contains(&n) && !dhas(d, "checked") => n.to_string(),
                    _ => return doc("Liste numarası geçersiz."),
                }
            };
            let empty = Val::List(vec![]);
            let values = dget(d, "typeList").unwrap_or(&empty);
            let list = match values.list() {
                Some(l) if l.len() <= 32 => l,
                _ => return doc("Liste işareti dizisi desteklenmiyor."),
            };
            let mut numbers = Vec::new();
            for v in list {
                match v.exact_i64() {
                    Some(n) if rlookup(BULLETS, n).is_some() => numbers.push(n.to_string()),
                    _ => return doc("Liste işareti dizisi desteklenmiyor."),
                }
            }
            let suffix = if numbers.is_empty() { String::new() } else { format!(",{{{}}}", numbers.join(",")) };
            Ok(format!("{},{}{}", lookup(BULLETS, name).unwrap(), second, suffix))
        }
        _ => doc("Bu paragraf biçimi henüz güvenle düzenlenemiyor."),
    }
}

/// `_paragraph_value(kind, value, wire=True)` (wire -> model).
pub fn paragraph_value_wire(kind: &str, value: &str) -> Result<Val> {
    match kind {
        "alignment" => match ALIGNMENTS.iter().find(|(_, v)| v.to_string() == value) {
            Some((k, _)) => Ok(Val::str(*k)),
            None => doc("Paragraf hizası desteklenmiyor."),
        },
        "parsingState" => match value.to_lowercase().as_str() {
            "true" => Ok(Val::Bool(true)),
            "false" => Ok(Val::Bool(false)),
            _ => doc("Paragraf metadata bilgisi geçersiz."),
        },
        "indentLevel" => {
            let attempt = || -> Option<Val> {
                let parts: Vec<&str> = value.split(',').collect();
                if parts.len() != 2 { return None; }
                let level = parse_py_int(parts[0])?;
                let direction = parse_py_int(parts[1])?.to_i64()?;
                let name = rlookup(DIRECTIONS, direction)?;
                let result = Val::Dict(vec![(Val::str("level"), Val::Int(level, true)), (Val::str("direction"), Val::str(name))]);
                paragraph_value_model(kind, &result).ok()?;
                Some(result)
            };
            attempt().ok_or_else(|| Error::Document("Girinti biçimi desteklenmiyor.".into()))
        }
        "bullet" => {
            let unsupported = || Error::Document("Liste biçimi desteklenmiyor.".into());
            // (\d+),(true|false|-?\d+)(?:,\{([0-9,]*)\})?
            let (first, rest) = value.split_once(',').ok_or_else(unsupported)?;
            if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) { return Err(unsupported()); }
            let (second, type_list) = match rest.find(",{") {
                Some(i) if rest.ends_with('}') => {
                    let inner = &rest[i + 2..rest.len() - 1];
                    if !inner.bytes().all(|b| b.is_ascii_digit() || b == b',') { return Err(unsupported()); }
                    (&rest[..i], Some(inner))
                }
                _ => (rest, None),
            };
            let second_ok = second == "true" || second == "false" || {
                let digits = second.strip_prefix('-').unwrap_or(second);
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            };
            if !second_ok { return Err(unsupported()); }
            let name = parse_py_int(first).and_then(|n| n.to_i64()).and_then(|n| rlookup(BULLETS, n))
                .ok_or_else(|| Error::Document("Bu liste işareti henüz desteklenmiyor.".into()))?;
            let mut result = vec![(Val::str("kind"), Val::str(name))];
            if name == "check" {
                if second != "true" && second != "false" { return doc("Onay kutusu değeri geçersiz."); }
                result.push((Val::str("checked"), Val::Bool(second == "true")));
            } else {
                let number = parse_py_int(second).ok_or_else(|| Error::Document("Liste numarası geçersiz.".into()))?;
                result.push((Val::str("number"), Val::Int(number, true)));
            }
            if let Some(inner) = type_list {
                let mut items = Vec::new();
                if !inner.is_empty() {
                    for piece in inner.split(',') {
                        let v = parse_py_int(piece).ok_or_else(|| Error::Value(format!("invalid literal for int() with base 10: '{piece}'")))?;
                        items.push(Val::Int(v, true));
                    }
                }
                result.push((Val::str("typeList"), Val::List(items)));
            }
            let result = Val::Dict(result);
            paragraph_value_model(kind, &result)?;
            Ok(result)
        }
        _ => doc("Bu paragraf biçimi henüz güvenle düzenlenemiyor."),
    }
}

// ---- normalize_model ------------------------------------------------------

/// Accessors for a normalized model (always the dict shape built below).
pub fn model_text(model: &Val) -> &str {
    dget(model.dict().unwrap(), "text").and_then(Val::as_str).unwrap_or("")
}
pub fn model_spans(model: &Val) -> &[Val] {
    dget(model.dict().unwrap(), "spans").and_then(Val::list).map(|v| v.as_slice()).unwrap_or(&[])
}
pub fn model_paragraphs(model: &Val) -> &[Val] {
    dget(model.dict().unwrap(), "paragraphs").and_then(Val::list).map(|v| v.as_slice()).unwrap_or(&[])
}

pub fn normalize_model(model: &Val) -> Result<Val> {
    let bad_version = || Error::Document("Editör belgesi sürümü geçersiz.".into());
    let d = model.dict().ok_or_else(bad_version)?;
    if extra_keys(d, &["schemaVersion", "text", "spans", "paragraphs"])
        || !py_eq(dget(d, "schemaVersion").unwrap_or(&Val::None), &Val::int(1)) {
        return Err(bad_version());
    }
    let text = text_value(dget(d, "text").unwrap_or(&Val::None))?;
    let boundaries = Boundaries::new(&text);
    let empty = Val::List(vec![]);
    let spans = dget(d, "spans").unwrap_or(&empty);
    let paragraphs = dget(d, "paragraphs").unwrap_or(&empty);
    let (Some(spans), Some(paragraphs)) = (spans.list(), paragraphs.list()) else {
        return doc("Biçimlendirme sınırı aşıldı.");
    };
    if spans.len() + paragraphs.len() > MAX_ANNOTATIONS { return doc("Biçimlendirme sınırı aşıldı."); }
    let mut out_spans = Vec::new();
    let mut by_kind: HashMap<String, Vec<(i64, i64)>> = HashMap::new();
    for span in spans {
        let s = match span.dict() {
            Some(s) if !extra_keys(s, &["start", "end", "kind", "value", "interval"]) => s,
            _ => return doc("Metin aralığı geçersiz."),
        };
        let none = Val::None;
        let kind = dget(s, "kind").unwrap_or(&none);
        let three = Val::int(3);
        let interval = dget(s, "interval").unwrap_or(&three);
        let a = dget(s, "start").and_then(|v| contains_val(&boundaries, v));
        let b = dget(s, "end").and_then(|v| contains_val(&boundaries, v));
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) if a <= b => (a, b),
            _ => return doc("Metin aralığı UTF-16 karakter sınırlarına uymuyor."),
        };
        let interval = match interval.exact_i64() {
            Some(i) if (0..4).contains(&i) => i,
            _ => return doc("Metin aralığı genişletme türü geçersiz."),
        };
        let wire = span_value_model(kind, dget(s, "value").unwrap_or(&none))?;
        let kind = kind.as_str().unwrap();
        let value = span_value_wire(kind, &wire)?;
        let ranges = by_kind.entry(kind.to_string()).or_default();
        if ranges.iter().any(|&(x, y)| a.max(x) < b.min(y)) {
            return doc("Aynı metin biçiminde çakışan aralıklar desteklenmiyor.");
        }
        ranges.push((a, b));
        out_spans.push(Val::Dict(vec![(Val::str("start"), Val::int(a as i128)), (Val::str("end"), Val::int(b as i128)),
            (Val::str("kind"), Val::str(kind)), (Val::str("value"), value), (Val::str("interval"), Val::int(interval as i128))]));
    }
    let ranges = paragraph_ranges(&text);
    let mut seen = HashSet::new();
    let mut out_paragraphs = Vec::new();
    for paragraph in paragraphs {
        let p = match paragraph.dict() {
            Some(p) if !extra_keys(p, &["index", "alignment", "parsingState", "indentLevel", "bullet"]) => p,
            _ => return doc("Paragraf biçimi desteklenmiyor."),
        };
        let index = match dget(p, "index").and_then(Val::exact_i64) {
            Some(i) if i >= 0 && (i as usize) < ranges.len() && !seen.contains(&i) => i,
            _ => return doc("Paragraf konumu geçersiz."),
        };
        let range = ranges[index as usize];
        if range.0 < 0 || range.0 == range.1 { return doc("Boş paragraf biçimlendirmesi henüz desteklenmiyor."); }
        seen.insert(index);
        let mut item = vec![(Val::str("index"), Val::int(index as i128))];
        for (k, v) in p {
            let kind = k.as_str().unwrap();
            if kind != "index" {
                let wire = paragraph_value_model(kind, v)?;
                item.push((k.clone(), paragraph_value_wire(kind, &wire)?));
            }
        }
        out_paragraphs.push(Val::Dict(item));
    }
    let output = Val::Dict(vec![(Val::str("schemaVersion"), Val::int(1)), (Val::str("text"), Val::Str(text)),
        (Val::str("spans"), Val::List(out_spans)), (Val::str("paragraphs"), Val::List(out_paragraphs))]);
    if json_dumps(&output, Some(2), false).len() > 1024 * 1024 {
        return doc("Editör belgesi kayıt sınırını aşıyor.");
    }
    Ok(output)
}

pub fn editor_model_fingerprint(model: &Val) -> Result<String> {
    let normalized = normalize_model(model)?;
    Ok(hex(&Sha256::digest(json_dumps(&normalized, None, true).as_bytes())))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- RichText ---------------------------------------------------------------

pub type SectionT = (i64, i64, String);

#[derive(Clone, Debug)]
pub struct Rich {
    pub encoded: String,
    /// Normalized model (dict Val).
    pub model: Val,
    pub span_order: Vec<String>,
    pub zero_length: Vec<String>,
    pub sections: Vec<SectionT>,
}

/// A side-car record: Ok(str) or Err(type name of a non-str object).
pub type Record = std::result::Result<String, String>;

fn record_str(record: &Record) -> Result<&str> {
    record.as_deref().map_err(|t| Error::Attribute(format!("'{t}' object has no attribute 'split'")))
}

fn read_span_record(record: &Record) -> Result<Vec<(Val, Val)>> {
    let record = record_str(record)?;
    let bad = || Error::Document("Metin biçimi sırası desteklenmiyor.".into());
    let parts: Vec<&str> = record.splitn(5, ',').collect();
    if parts.len() != 5 { return Err(bad()); }
    let kind = SPAN_TYPES.iter().find(|(_, v)| v.to_string() == parts[0]).map(|(k, _)| *k).ok_or_else(bad)?;
    let a = parse_py_int(parts[1]).ok_or_else(bad)?;
    let b = parse_py_int(parts[2]).ok_or_else(bad)?;
    let interval = parse_py_int(parts[3]).ok_or_else(bad)?;
    let value = span_value_wire(kind, parts[4]).map_err(|e| if e.is_value_error() { bad() } else { e })?;
    Ok(vec![(Val::str("start"), Val::Int(a, true)), (Val::str("end"), Val::Int(b, true)), (Val::str("kind"), Val::str(kind)),
        (Val::str("interval"), Val::Int(interval, true)), (Val::str("value"), value)])
}

/// `_span_record(span)` for a span dict.
fn span_record(span: &[(Val, Val)]) -> Result<String> {
    let kind = dget(span, "kind").and_then(Val::as_str).unwrap_or("");
    let id = lookup(SPAN_TYPES, kind).ok_or_else(|| Error::Value(format!("KeyError: {kind:?}")))?;
    let num = |k: &str| match dget(span, k) { Some(Val::Int(i, _)) => i.0.clone(), _ => String::new() };
    Ok(format!("{},{},{},{},{}", id, num("start"), num("end"), num("interval"),
        span_value_model(dget(span, "kind").unwrap(), dget(span, "value").unwrap_or(&Val::None))?))
}

struct Open { group: &'static str, kind: String, value: Val, raw: String, interval: Option<PyInt>, start: i64, seq: usize }

fn token_at(s: &str, pos: usize) -> Option<(usize, &'static str, String, Option<String>)> {
    // \s+'(span|paragraph|sectionData)/([A-Za-z]+)'(?:='([^']*)')?
    let rest = &s[pos..];
    let ws: usize = rest.chars().take_while(|c| py_isspace(*c)).map(char::len_utf8).sum();
    if ws == 0 { return None; }
    let mut r = rest[ws..].strip_prefix('\'')?;
    let group = ["span", "paragraph", "sectionData"].into_iter().find(|g| r.starts_with(&format!("{g}/")))?;
    r = &r[group.len() + 1..];
    let n = r.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    if n == 0 { return None; }
    let kind = r[..n].to_string();
    r = r[n..].strip_prefix('\'')?;
    let mut value = None;
    if let Some(v) = r.strip_prefix("='") {
        if let Some(end) = v.find('\'') {
            value = Some(v[..end].to_string());
            r = &v[end + 1..];
        }
    }
    Some((s.len() - r.len(), group, kind, value))
}

fn section_raw_ok(value: &str) -> bool {
    ascii_digits(value, 1, 10) && value.parse::<u64>().unwrap() < 1 << 31
}

pub fn read_rich_text(encoded: &str, span_order: &[Record], zero_length: &[Record]) -> Result<Rich> {
    if span_order.len() + zero_length.len() > MAX_ANNOTATIONS * 2 { return doc("Biçimlendirme sırası sınırı aşıldı."); }
    let stream = decode(encoded)?;
    let mut chunks = String::new();
    let mut opens: Vec<Open> = Vec::new();
    let mut completed: Vec<(Open, i64)> = Vec::new();
    let (mut position, mut cursor, mut serial) = (0i64, 0usize, 0usize);
    while cursor < stream.len() {
        let marker = stream[cursor..].find("<?a").map(|i| i + cursor);
        let end = marker.unwrap_or(stream.len());
        let chunk = decode(&stream[cursor..end])?;
        position += utf16_length(&chunk) as i64;
        chunks.push_str(&chunk);
        if position > 256 * 1024 { return doc("Metin sınırı aşıldı."); }
        let Some(marker) = marker else { break };
        let close = stream[marker + 3..].find("?>").map(|i| i + marker + 3)
            .ok_or_else(|| Error::Document("Biçimlendirme etiketi kapanmamış.".into()))?;
        let token_text = &stream[marker + 3..close];
        let mut offset = 0;
        while offset < token_text.len() {
            let Some((next, group, kind, value)) = token_at(token_text, offset) else {
                return doc("Bilinmeyen biçimlendirme sözdizimi.");
            };
            offset = next;
            if group == "sectionData" && kind != "sectionData" { return doc("Bilinmeyen biçimlendirme sözdizimi."); }
            if let Some(value) = value {
                let mut interval = None;
                let parsed = if group == "sectionData" {
                    if !section_raw_ok(&value) { return doc("Sayfa bölümü verisi desteklenmiyor."); }
                    Val::Str(value.clone())
                } else if group == "span" {
                    let bad = || Error::Document("Metin biçimi değeri geçersiz.".into());
                    let (i, raw) = value.split_once(',').ok_or_else(bad)?;
                    interval = Some(parse_py_int(i).ok_or_else(bad)?);
                    span_value_wire(&kind, raw)?
                } else {
                    paragraph_value_wire(&kind, &value)?
                };
                opens.push(Open { group, kind, value: parsed, raw: value, interval, start: position, seq: serial });
                serial += 1;
            } else {
                let mut matched = None;
                if group == "sectionData" {
                    matched = (0..opens.len()).rev().find(|&i| opens[i].group == "sectionData"
                        && opens[i].start + opens[i].raw.parse::<i64>().unwrap() == position);
                }
                if matched.is_none() {
                    matched = (0..opens.len()).rev().find(|&i| opens[i].group == group && opens[i].kind == kind);
                }
                let Some(i) = matched else { return doc("Başlangıcı olmayan biçimlendirme etiketi."); };
                let item = opens.remove(i);
                completed.push((item, position));
            }
            if opens.len() + completed.len() > MAX_ANNOTATIONS { return doc("Biçimlendirme sınırı aşıldı."); }
        }
        cursor = close + 2;
    }
    if !opens.is_empty() { return doc("Kapanmamış biçimlendirme aralığı düzenlenemez."); }
    let text = chunks;
    let ranges = paragraph_ranges(&text);
    let mut spans: Vec<Vec<(Val, Val)>> = Vec::new();
    let mut paragraph_map: Vec<(usize, Vec<(Val, Val)>)> = Vec::new();
    let mut sections: Vec<(usize, i64, i64, String)> = Vec::new();
    for (item, end) in completed {
        match item.group {
            "span" => spans.push(vec![(Val::str("start"), Val::int(item.start as i128)), (Val::str("end"), Val::int(end as i128)),
                (Val::str("kind"), Val::Str(item.kind)), (Val::str("value"), item.value),
                (Val::str("interval"), Val::Int(item.interval.unwrap(), true))]),
            "sectionData" => sections.push((item.seq, item.start, end, item.raw)),
            _ => {
                let matches: Vec<usize> = ranges.iter().enumerate()
                    .filter(|(_, r)| r.0 == item.start && r.0 >= 0).map(|(i, _)| i).collect();
                if matches.len() != 1 { return doc("Paragraf aralığı native sınırlarla uyuşmuyor."); }
                let index = matches[0];
                let slot = match paragraph_map.iter().position(|(i, _)| *i == index) {
                    Some(p) => p,
                    None => {
                        paragraph_map.push((index, vec![(Val::str("index"), Val::int(index as i128))]));
                        paragraph_map.len() - 1
                    }
                };
                let paragraph = &mut paragraph_map[slot].1;
                if let Some(existing) = dget(paragraph, &item.kind) {
                    if !py_eq(existing, &item.value) { return doc("Tekrarlanan paragraf biçimi."); }
                    continue;
                }
                paragraph.push((Val::Str(item.kind), item.value));
            }
        }
    }
    for record in zero_length {
        let span = read_span_record(record)?;
        if !py_eq(dget(&span, "start").unwrap(), dget(&span, "end").unwrap()) {
            return doc("Sıfır uzunluklu metin biçimi geçersiz.");
        }
        spans.push(span);
    }
    if !span_order.is_empty() {
        let canonical: Vec<String> = span_order.iter().map(|r| span_record(&read_span_record(r)?)).collect::<Result<_>>()?;
        let mut records: HashMap<String, usize> = HashMap::new();
        for (i, s) in spans.iter().enumerate() { records.insert(span_record(s)?, i); }
        let canonical_set: HashSet<&String> = canonical.iter().collect();
        let record_set: HashSet<&String> = records.keys().collect();
        if records.len() != spans.len() || canonical.len() != spans.len() || record_set != canonical_set {
            return doc("Metin biçimi sıralama metadata bilgisi uyuşmuyor.");
        }
        spans = canonical.iter().map(|r| spans[records[r]].clone()).collect();
    }
    sections.sort_by_key(|s| s.0);
    let sections: Vec<SectionT> = sections.into_iter().map(|(_, a, b, r)| (a, b, r)).collect();
    check_sections_typed(&text, &sections)?;
    let model = normalize_model(&Val::Dict(vec![(Val::str("schemaVersion"), Val::int(1)), (Val::str("text"), Val::Str(text)),
        (Val::str("spans"), Val::List(spans.into_iter().map(Val::Dict).collect())),
        (Val::str("paragraphs"), Val::List(paragraph_map.into_iter().map(|(_, p)| Val::Dict(p)).collect()))]))?;
    Ok(Rich { encoded: encoded.to_string(), model, span_order: span_order.iter().map(|r| r.clone().unwrap_or_default()).collect(),
        zero_length: zero_length.iter().map(|r| r.clone().unwrap_or_default()).collect(), sections })
}

/// One item of a sections argument: a (start, end, raw) triple, or an item
/// that fails tuple unpacking (ValueError message).
pub enum SecIn { Ok(Val, Val, Val), Unpack(String) }

pub fn unpack_message(got: usize, known: bool) -> String {
    if got < 3 { format!("not enough values to unpack (expected 3, got {got})") }
    else if known { format!("too many values to unpack (expected 3, got {got})") }
    else { "too many values to unpack (expected 3)".to_string() }
}

/// `_check_sections` for arbitrary (start, end, raw) values.
pub fn check_sections(text: &str, sections: &[SecIn]) -> Result<Vec<SectionT>> {
    if sections.len() > MAX_ANNOTATIONS { return doc("Sayfa bölümü sınırı aşıldı."); }
    let boundaries = Boundaries::new(text);
    let mut previous = 0i64;
    let mut out = Vec::new();
    for item in sections {
        let (start, end, raw) = match item {
            SecIn::Ok(a, b, c) => (a, b, c),
            SecIn::Unpack(message) => return Err(Error::Value(message.clone())),
        };
        let s = contains_val(&boundaries, start);
        let e = contains_val(&boundaries, end);
        match (s, e, raw) {
            (Some(s), Some(e), Val::Str(r)) if previous <= s && s <= e && section_raw_ok(r) => {
                previous = e;
                out.push((s, e, r.clone()));
            }
            _ => return doc("Sayfa bölümleri metinle uyuşmuyor."),
        }
    }
    Ok(out)
}

pub fn sections_to_vals(sections: &[SectionT]) -> Vec<SecIn> {
    sections.iter().map(|(a, b, r)| SecIn::Ok(Val::int(*a as i128), Val::int(*b as i128), Val::Str(r.clone()))).collect()
}

fn check_sections_typed(text: &str, sections: &[SectionT]) -> Result<()> {
    check_sections(text, &sections_to_vals(sections)).map(|_| ())
}

/// remap_sections for typed slices (see the Python docstring).
pub fn remap_sections(old_text: &str, new_text: &str, sections: &[SectionT]) -> Result<Vec<SectionT>> {
    if sections.is_empty() || old_text == new_text { return Ok(sections.to_vec()); }
    let positions: Vec<(i64, i64)> = sections.iter().map(|(a, b, _)| (*a, *b)).collect();
    let moved = remap_offsets(old_text, new_text, &positions);
    let out: Vec<SectionT> = sections.iter().zip(moved).map(|((start, end, raw), (ns, ne))| {
        (ns, ne, if ne - ns == end - start { raw.clone() } else { (ne - ns).to_string() })
    }).collect();
    check_sections_typed(new_text, &out)?;
    Ok(out)
}

/// The boundary mapping of remap_sections for (start, end) pairs.
pub fn remap_offsets(old_text: &str, new_text: &str, positions: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let a: Vec<u16> = old_text.encode_utf16().collect();
    let b: Vec<u16> = new_text.encode_utf16().collect();
    let mut prefix = 0usize;
    let mut limit = a.len().min(b.len());
    while prefix < limit && a[prefix] == b[prefix] { prefix += 1; }
    let mut suffix = 0usize;
    limit -= prefix;
    while suffix < limit && a[a.len() - suffix - 1] == b[b.len() - suffix - 1] { suffix += 1; }
    let (old_len, new_len) = (a.len(), b.len());
    let low = |x: usize, t: &[u16]| 0 < x && x < t.len() && (0xDC00..=0xDFFF).contains(&t[x]);
    while prefix > 0 && (low(prefix, &a) || low(prefix, &b)) { prefix -= 1; }
    while suffix > 0 && (low(old_len - suffix, &a) || low(new_len - suffix, &b)) { suffix -= 1; }
    let (old_len, new_len, prefix, suffix) = (old_len as i64, new_len as i64, prefix as i64, suffix as i64);
    let middle = new_len - suffix - prefix;
    let shift = |x: i64| -> i64 {
        if x == old_len { return new_len; }
        if x <= prefix { return x; }
        if x >= old_len - suffix { return x + new_len - old_len; }
        prefix + (x - prefix).min(middle)
    };
    positions.iter().map(|(start, end)| (shift(*start), shift(*end))).collect()
}

/// encode_rich_text for an already normalized model and checked slices.
pub fn encode_normalized(model: Val, sections: Vec<SectionT>) -> Result<Rich> {
    let text = model_text(&model).to_string();
    let boundaries = Boundaries::new(&text);
    #[derive(Default)]
    struct Slot { close: Vec<String>, open: Vec<String>, zero: Vec<String> }
    let mut events: BTreeMap<i64, Slot> = BTreeMap::new();
    let mut order = Vec::new();
    let mut zeros = Vec::new();
    fn add(events: &mut BTreeMap<i64, Slot>, start: i64, end: i64, tag: &str, value: &str) {
        events.entry(start).or_default().open.push(format!(" '{tag}'='{value}'"));
        events.entry(end).or_default().close.push(format!(" '{tag}'"));
    }
    for span in model_spans(&model) {
        let s = span.dict().unwrap();
        let record = span_record(s)?;
        order.push(record.clone());
        let start = dget(s, "start").unwrap().exact_i64().unwrap();
        let end = dget(s, "end").unwrap().exact_i64().unwrap();
        if start == end { zeros.push(record); continue; }
        let kind = dget(s, "kind").unwrap();
        let interval = dget(s, "interval").unwrap().exact_i64().unwrap();
        let value = span_value_model(kind, dget(s, "value").unwrap())?;
        add(&mut events, start, end, &format!("span/{}", kind.as_str().unwrap()), &format!("{interval},{value}"));
    }
    let ranges = paragraph_ranges(&text);
    for paragraph in model_paragraphs(&model) {
        let p = paragraph.dict().unwrap();
        let (start, end) = ranges[dget(p, "index").unwrap().exact_i64().unwrap() as usize];
        for (k, v) in p {
            let kind = k.as_str().unwrap();
            if kind != "index" {
                add(&mut events, start, end, &format!("paragraph/{kind}"), &paragraph_value_model(kind, v)?);
            }
        }
    }
    for (start, end, raw) in &sections {
        if start == end {
            events.entry(*start).or_default().zero.push(format!(" 'sectionData/sectionData'='{raw}' 'sectionData/sectionData'"));
        } else {
            add(&mut events, *start, *end, "sectionData/sectionData", raw);
        }
    }
    let mut stream = String::new();
    let mut previous = 0i64;
    for (offset, event) in &events {
        if *offset > previous {
            stream.push_str(&b64(&text[boundaries.byte(previous).unwrap()..boundaries.byte(*offset).unwrap()]));
        }
        let tokens: String = event.close.iter().chain(event.open.iter()).map(String::as_str).collect();
        if !tokens.is_empty() { stream.push_str("<?a"); stream.push_str(&tokens); stream.push_str("?>"); }
        for pair in &event.zero { stream.push_str("<?a"); stream.push_str(pair); stream.push_str("?>"); }
        previous = *offset;
    }
    if previous < utf16_length(&text) as i64 {
        stream.push_str(&b64(&text[boundaries.byte(previous).unwrap()..]));
    }
    let rich = Rich { encoded: b64(&stream), model, span_order: order, zero_length: zeros, sections };
    if !rich.sections.is_empty() {
        let records = |v: &[String]| -> Vec<Record> { v.iter().cloned().map(Ok).collect() };
        let again = read_rich_text(&rich.encoded, &records(&rich.span_order), &records(&rich.zero_length))?;
        if again.sections != rich.sections || !py_eq(&again.model, &rich.model) {
            return doc("Sayfa bölümleri güvenle yeniden yazılamıyor.");
        }
    }
    Ok(rich)
}

/// encode_rich_text(model, sections) with typed slices.
pub fn encode_rich_text(model: &Val, sections: &[SectionT]) -> Result<Rich> {
    let model = normalize_model(model)?;
    let sections = check_sections(model_text(&model), &sections_to_vals(sections))?;
    encode_normalized(model, sections)
}
