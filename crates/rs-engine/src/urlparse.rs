//! The parts of CPython 3.14 `urllib.parse` the loopback API depends on.
//! Pure Rust. Strings are Python `str` values held as Rust `String`s.

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Split {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// `urllib.parse.urlsplit(url)`; `Err` where Python raises ValueError.
pub fn urlsplit(url: &str) -> Result<Split, ()> {
    let url: String = url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut out = Split::default();
    let mut rest: &str = &url;
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next().unwrap();
        if i > 0 && first.is_ascii() && first.is_ascii_alphabetic() && rest[..i].chars().all(|c| SCHEME_CHARS.contains(c)) {
            out.scheme = rest[..i].to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut rest = rest.to_string();
    if rest.starts_with("//") {
        let body = &rest[2..];
        let end = body.find(|c| c == '/' || c == '?' || c == '#').unwrap_or(body.len());
        let netloc = body[..end].to_string();
        rest = body[end..].to_string();
        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err(()); // Invalid IPv6 URL
        }
        if open && close {
            let host = netloc.split_once('[').map(|x| x.1).unwrap_or("");
            let host = host.split_once(']').map(|x| x.0).unwrap_or(host);
            if !bracketed_host_ok(host) {
                return Err(());
            }
        }
        out.netloc = netloc;
    }
    if let Some((a, b)) = rest.split_once('#') {
        out.fragment = b.to_string();
        rest = a.to_string();
    }
    if let Some((a, b)) = rest.split_once('?') {
        out.query = b.to_string();
        rest = a.to_string();
    }
    out.path = rest;
    Ok(out)
}

fn bracketed_host_ok(host: &str) -> bool {
    if let Some(v) = host.strip_prefix('v') {
        // \Av[a-fA-F0-9]+\..+\z
        let Some((hex, tail)) = v.split_once('.') else { return false };
        return !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty() && !tail.contains('\n');
    }
    let address = host.split_once('%').map(|x| x.0).unwrap_or(host);
    address.parse::<std::net::Ipv6Addr>().is_ok()
}

fn hexval(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// `_unquote_impl` on an ASCII run: percent-decode valid `%XX`, keep the rest.
fn unquote_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexval(b[i + 1]), hexval(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Python bytes.decode('utf-8', 'replace').
pub fn decode_replace(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `urllib.parse.unquote(s, errors=...)`: `strict=true` returns `Err` on invalid UTF-8.
pub fn unquote(s: &str, strict: bool) -> Result<String, ()> {
    if !s.contains('%') {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| -> Result<(), ()> {
        if !run.is_empty() {
            let bytes = unquote_bytes(run);
            if strict {
                out.push_str(std::str::from_utf8(&bytes).map_err(|_| ())?);
            } else {
                out.push_str(&decode_replace(&bytes));
            }
            run.clear();
        }
        Ok(())
    };
    for c in s.chars() {
        if c.is_ascii() {
            run.push(c);
        } else {
            flush(&mut run, &mut out)?;
            out.push(c);
        }
    }
    flush(&mut run, &mut out)?;
    Ok(out)
}

pub fn unquote_plus(s: &str) -> String {
    unquote(&s.replace('+', " "), false).unwrap_or_default()
}

/// `urllib.parse.parse_qs(qs, max_num_fields=n)`; `Err` = ValueError
/// ("Max number of fields exceeded"). Blank values are dropped.
pub fn parse_qs(qs: &str, max_num_fields: usize) -> Result<Vec<(String, Vec<String>)>, ()> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    if qs.is_empty() {
        return Ok(out);
    }
    if 1 + qs.matches('&').count() > max_num_fields {
        return Err(());
    }
    for pair in qs.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.split_once('=') {
            Some((n, v)) => (n, v),
            None => (pair, ""),
        };
        if value.is_empty() {
            continue;
        }
        let (name, value) = (unquote_plus(name), unquote_plus(value));
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some((_, values)) => values.push(value),
            None => out.push((name, vec![value])),
        }
    }
    Ok(out)
}

pub fn qs_get<'a>(params: &'a [(String, Vec<String>)], name: &str) -> Option<&'a Vec<String>> {
    params.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

/// `urllib.parse.quote(s)` (safe='/', UTF-8).
pub fn quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || "_.-~/".contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urllib_semantics() {
        assert_eq!(urlsplit("/api/notes?q=a#x").unwrap().path, "/api/notes");
        assert_eq!(urlsplit("/api/notes?q=a#x").unwrap().query, "q=a");
        assert_eq!(urlsplit("http://h:1/p?x").unwrap().path, "/p");
        assert!(urlsplit("http://[::1/p").is_err());
        assert_eq!(unquote("/%2e%2e/a%zz%C3%BC", false).unwrap(), "/../a%zzü");
        assert!(unquote("%ff", true).is_err());
        assert_eq!(unquote("%ff", false).unwrap(), "\u{fffd}");
        assert_eq!(parse_qs("q=a+b&q=%20&x&filter=", 10).unwrap(), vec![("q".into(), vec!["a b".into(), " ".into()])]);
        assert!(parse_qs("a=1&b=2&c=3", 2).is_err());
        assert_eq!(quote("Not ü.txt"), "Not%20%C3%BC.txt");
    }
}
