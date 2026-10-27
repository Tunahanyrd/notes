//! Small pieces of CPython 3.14 semantics the HTTP layer depends on:
//! `str` whitespace, `int()` parsing, `str.capitalize/title/lower`,
//! latin-1 decoding and `json.dumps` (ensure_ascii) output.

/// `Py_UNICODE_ISSPACE`: the characters `str.strip()/split()` treat as whitespace.
pub(crate) fn is_space(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{1f}' | ' '
        | '\u{85}' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}'
        | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

/// `str.strip()` without arguments.
pub(crate) fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `bytes.decode('iso-8859-1')`
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// `str.encode('latin-1')`; `None` is a UnicodeEncodeError.
pub(crate) fn encode_latin1(s: &str) -> Option<Vec<u8>> {
    s.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect()
}

/// `str.lower()`
pub(crate) fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// `str.upper()`
pub(crate) fn upper(s: &str) -> String {
    s.to_uppercase()
}

/// `str.capitalize()` (first character upper/title case, the rest lower case).
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.push_str(&chars.as_str().to_lowercase());
            out
        }
    }
}

/// `str.title()`: a character after a cased character is lowered, any other
/// one is upper-cased (`"X-sc-e2ee-state".title() == "X-Sc-E2Ee-State"`).
pub(crate) fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut previous_cased = false;
    for c in s.chars() {
        if previous_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        previous_cased = c.is_lowercase() || c.is_uppercase();
    }
    out
}

/// Digits with single underscores between them (PEP 515), as `int()` accepts.
fn digits_value(body: &str, radix: u32, allow_leading_underscore: bool) -> Option<i128> {
    if body.is_empty() {
        return None;
    }
    let mut value: i128 = 0;
    let mut previous_underscore = !allow_leading_underscore;
    let mut any_digit = false;
    for (index, c) in body.chars().enumerate() {
        if c == '_' {
            if previous_underscore || (index == 0 && !allow_leading_underscore) {
                return None;
            }
            previous_underscore = true;
            continue;
        }
        let digit = c.to_digit(radix)?;
        // Saturate: callers only compare against small bounds.
        value = value.saturating_mul(radix as i128).saturating_add(digit as i128);
        previous_underscore = false;
        any_digit = true;
    }
    if previous_underscore || !any_digit {
        return None;
    }
    Some(value)
}

/// `int(s)` for a `str` (base 10). `None` is a ValueError. Values beyond
/// `i128` saturate (only ever compared with small bounds).
pub(crate) fn int10(s: &str) -> Option<i128> {
    let s = strip(s);
    let (negative, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    if !body.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let v = digits_value(body, 10, false)?;
    Some(if negative { -v } else { v })
}

/// `int(line, 16)` for `bytes` (http.client chunk sizes). ASCII whitespace is
/// stripped, a sign and a `0x` prefix are allowed, underscores per PEP 515.
pub(crate) fn int16_bytes(line: &[u8]) -> Option<i128> {
    if line.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(line).ok()?;
    let text = text.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}'));
    let (negative, body) = match text.as_bytes().first() {
        Some(b'+') => (false, &text[1..]),
        Some(b'-') => (true, &text[1..]),
        _ => (false, text),
    };
    let (body, prefixed) = if body.len() >= 2 && (body.starts_with("0x") || body.starts_with("0X")) {
        (&body[2..], true)
    } else {
        (body, false)
    };
    if !body.chars().all(|c| c.is_ascii_hexdigit() || c == '_') {
        return None;
    }
    let v = digits_value(body, 16, prefixed)?;
    Some(if negative { -v } else { v })
}

/// `json.dumps(s)` string literal (ensure_ascii=True).
pub(crate) fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A JSON value of the shapes `send_once` writes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum J {
    Int(i64),
    Bool(bool),
    Str(String),
}

fn json_value(out: &mut String, v: &J) {
    match v {
        J::Int(i) => out.push_str(&i.to_string()),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Str(s) => json_str(out, s),
    }
}

/// `json.dumps(obj)` (default separators `', '` and `': '`).
pub(crate) fn dumps_compact(obj: &[(&str, J)]) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in obj.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        json_str(&mut out, k);
        out.push_str(": ");
        json_value(&mut out, v);
    }
    out.push('}');
    out
}

/// `json.dumps(obj, indent=2)` (item separator `','`, key separator `': '`).
pub(crate) fn dumps_indent2(obj: &[(&str, J)]) -> String {
    if obj.is_empty() {
        return "{}".to_string();
    }
    let mut out = String::from("{\n");
    for (i, (k, v)) in obj.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  ");
        json_str(&mut out, k);
        out.push_str(": ");
        json_value(&mut out, v);
    }
    out.push_str("\n}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_int_rules() {
        assert_eq!(int10(" 42 "), Some(42));
        assert_eq!(int10("+4_2"), Some(42));
        assert_eq!(int10("-1"), Some(-1));
        assert_eq!(int10("4__2"), None);
        assert_eq!(int10("_42"), None);
        assert_eq!(int10("42_"), None);
        assert_eq!(int10(""), None);
        assert_eq!(int10("0x10"), None);
        assert_eq!(int10("\u{a0}7\u{85}"), Some(7));
        assert_eq!(int16_bytes(b"1a\r\n"), Some(26));
        assert_eq!(int16_bytes(b"0x_1F "), Some(31));
        assert_eq!(int16_bytes(b"_1f"), None);
        assert_eq!(int16_bytes(b"-5"), Some(-5));
        assert_eq!(int16_bytes(b""), None);
        assert_eq!(int16_bytes(b"0x"), None);
        assert_eq!(int16_bytes(b"1\x00"), None);
    }

    #[test]
    fn python_case_rules() {
        assert_eq!(title("X-sc-e2ee-state"), "X-Sc-E2Ee-State");
        assert_eq!(title("Content-type"), "Content-Type");
        assert_eq!(capitalize("x-SC-app-ID"), "X-sc-app-id");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn json_matches_python() {
        let obj = [("a", J::Int(1)), ("b", J::Bool(true)), ("c", J::Str("x\u{e9}\"\u{1f600}\u{7f}".into()))];
        assert_eq!(dumps_compact(&obj), "{\"a\": 1, \"b\": true, \"c\": \"x\\u00e9\\\"\\ud83d\\ude00\\u007f\"}");
        assert_eq!(dumps_indent2(&obj[..1]), "{\n  \"a\": 1\n}");
    }
}
