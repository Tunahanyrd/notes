//! CPython `json` over [`Val`]: byte-identical `json.dumps` in the three
//! styles the cloud layer writes (indent=2 receipts, compact separators,
//! default) and `json.loads(bytes)`.
use notes_host::{Fail, Val};
use rs_store::val::float_repr;
use std::fmt::Write as _;

pub struct Style<'a> {
    pub indent: Option<usize>,
    pub item_sep: &'a str,
    pub key_sep: &'a str,
    pub ensure_ascii: bool,
}

/// `json.dumps(v, indent=2)`
pub const INDENT2: Style<'static> = Style { indent: Some(2), item_sep: ",", key_sep: ": ", ensure_ascii: true };
/// `json.dumps(v, separators=(',', ':'))`
pub const COMPACT: Style<'static> = Style { indent: None, item_sep: ",", key_sep: ":", ensure_ascii: true };
/// `json.dumps(v)`
pub const DEFAULT: Style<'static> = Style { indent: None, item_sep: ", ", key_sep: ": ", ensure_ascii: true };

fn encode_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (ensure_ascii && (c as u32) > 0x7e) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{:04x}", unit);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline(out: &mut String, style: &Style, level: usize) {
    if let Some(n) = style.indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

fn float_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        float_repr(f)
    }
}

fn write_value(out: &mut String, v: &Val, style: &Style, level: usize) {
    match v {
        Val::Null => out.push_str("null"),
        Val::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Val::Int(i) => {
            let _ = write!(out, "{}", i);
        }
        Val::BigInt(s) => out.push_str(s),
        Val::Float(f) => out.push_str(&float_text(*f)),
        Val::Str(s) => encode_str(out, s, style.ensure_ascii),
        // bytes are never JSON-serialised by the oracle; keep a visible marker.
        Val::Bytes(_) => out.push_str("null"),
        Val::List(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(style.item_sep);
                }
                newline(out, style, level + 1);
                write_value(out, item, style, level + 1);
            }
            newline(out, style, level);
            out.push(']');
        }
        Val::Dict(items) => {
            if items.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(style.item_sep);
                }
                newline(out, style, level + 1);
                encode_str(out, k, style.ensure_ascii);
                out.push_str(style.key_sep);
                write_value(out, item, style, level + 1);
            }
            newline(out, style, level);
            out.push('}');
        }
    }
}

pub fn dumps(v: &Val, style: &Style) -> String {
    let mut out = String::new();
    write_value(&mut out, v, style, 0);
    out
}

/// `(json.dumps(value, indent=2) + '\n').encode()`
pub fn receipt_bytes(v: &Val) -> Vec<u8> {
    let mut s = dumps(v, &INDENT2);
    s.push('\n');
    s.into_bytes()
}

/// `json.loads(raw)` for bytes (UTF-8, optional BOM). Errors are ValueError
/// (`json.JSONDecodeError` / `UnicodeDecodeError`).
pub fn loads(raw: &[u8]) -> Result<Val, Fail> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(raw);
    let text = std::str::from_utf8(raw).map_err(|e| Fail::new(notes_host::Kind::Unicode, e.to_string()))?;
    rs_store::val::loads(text).map_err(|e| match e {
        rs_store::val::JsonError::Decode(m) | rs_store::val::JsonError::Value(m) => Fail::value(m),
        rs_store::val::JsonError::Type(m) => Fail::type_(m),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_style_matches_python() {
        let v = Val::dict(vec![
            ("a", Val::Int(1)),
            ("b", Val::List(vec![])),
            ("c", Val::Dict(vec![])),
            ("d", Val::List(vec![Val::Float(1.5), Val::s("ç")])),
        ]);
        assert_eq!(
            dumps(&v, &INDENT2),
            "{\n  \"a\": 1,\n  \"b\": [],\n  \"c\": {},\n  \"d\": [\n    1.5,\n    \"\\u00e7\"\n  ]\n}"
        );
        assert_eq!(dumps(&v, &COMPACT), "{\"a\":1,\"b\":[],\"c\":{},\"d\":[1.5,\"\\u00e7\"]}");
        assert_eq!(dumps(&v, &DEFAULT), "{\"a\": 1, \"b\": [], \"c\": {}, \"d\": [1.5, \"\\u00e7\"]}");
    }
}
