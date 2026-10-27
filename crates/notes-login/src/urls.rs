//! `urllib.parse` (CPython 3.14) subsets used by the sign-in flow:
//! `urlsplit` with its username/password/hostname/port properties,
//! `parse_qsl(strict_parsing=True, keep_blank_values=True, errors='strict')`,
//! `urlencode`/`quote_plus`, `html.escape` and the urllib `Request` URL unwrap.

/// `ValueError` raised by urlsplit/port/parse_qsl/unquote (message is private).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitResult {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

/// Characters whose NFKC normalization contains one of `/?#@:` (Unicode
/// 16.0, generated from CPython 3.14 `unicodedata`); `_checknetloc` rejects a
/// non-ASCII netloc containing any of them.
const NFKC_NETLOC_REJECT: [u32; 19] = [
    0x2047, 0x2048, 0x2049, 0x2100, 0x2101, 0x2105, 0x2106, 0x2a74, 0xfe13, 0xfe16, 0xfe55, 0xfe56, 0xfe5f,
    0xfe6b, 0xff03, 0xff0f, 0xff1a, 0xff1f, 0xff20,
];

/// `urllib.parse.urlsplit(url)`
pub fn urlsplit(url: &str) -> Result<SplitResult, UrlError> {
    let url = url.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let mut url: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut scheme = String::new();
    let mut netloc: Option<String> = None;
    if let Some(i) = url.find(':') {
        let first = url.chars().next().unwrap();
        if i > 0 && first.is_ascii_alphabetic() && url[..i].chars().all(|c| SCHEME_CHARS.contains(c)) {
            scheme = url[..i].to_ascii_lowercase();
            url = url[i + 1..].to_string();
        }
    }
    if url.starts_with("//") {
        let rest = &url[2..];
        let delim = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let n = rest[..delim].to_string();
        url = rest[delim..].to_string();
        if n.contains('[') != n.contains(']') {
            return Err(UrlError);
        }
        if n.contains('[') && n.contains(']') {
            check_bracketed_netloc(&n)?;
        }
        netloc = Some(n);
    }
    let mut fragment = String::new();
    if let Some((u, f)) = url.split_once('#') {
        fragment = f.to_string();
        url = u.to_string();
    }
    let mut query = String::new();
    if let Some((u, q)) = url.split_once('?') {
        query = q.to_string();
        url = u.to_string();
    }
    if let Some(n) = &netloc {
        if !n.is_ascii() && n.chars().any(|c| NFKC_NETLOC_REJECT.contains(&(c as u32))) {
            return Err(UrlError);
        }
    }
    Ok(SplitResult { scheme, netloc: netloc.unwrap_or_default(), path: url, query, fragment })
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_port = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let hostname = match host_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err(UrlError);
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err(UrlError);
            }
            hostname
        }
        None => host_port.split_once(':').map(|(h, _)| h).unwrap_or(host_port),
    };
    if let Some(rest) = hostname.strip_prefix('v') {
        // \Av[a-fA-F0-9]+\..+\z
        let Some((hexpart, tail)) = rest.split_once('.') else { return Err(UrlError) };
        if hexpart.is_empty() || !hexpart.chars().all(|c| c.is_ascii_hexdigit()) || tail.is_empty() {
            return Err(UrlError);
        }
        Ok(())
    } else {
        // ipaddress.ip_address(): IPv6 (optionally with %scope) is accepted;
        // IPv4 in brackets and anything else is a ValueError.
        let address = hostname.split_once('%').map(|(a, _)| a).unwrap_or(hostname);
        if address.parse::<std::net::Ipv6Addr>().is_ok() && !hostname.ends_with('%') {
            Ok(())
        } else {
            Err(UrlError)
        }
    }
}

impl SplitResult {
    fn userinfo(&self) -> (Option<String>, Option<String>) {
        match self.netloc.rsplit_once('@') {
            Some((info, _)) => match info.split_once(':') {
                Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
                None => (Some(info.to_string()), None),
            },
            None => (None, None),
        }
    }
    fn hostinfo(&self) -> (String, Option<String>) {
        let hostinfo = self.netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(&self.netloc);
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (h, p) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (h, p.split_once(':').map(|(_, p)| p).unwrap_or(""))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (hostname.to_string(), if port.is_empty() { None } else { Some(port.to_string()) })
    }
    pub fn username(&self) -> Option<String> {
        self.userinfo().0
    }
    pub fn password(&self) -> Option<String> {
        self.userinfo().1
    }
    /// `.hostname` (lowercased, `%zone` preserved)
    pub fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            return None;
        }
        Some(match hostname.split_once('%') {
            Some((h, zone)) => format!("{}%{}", h.to_lowercase(), zone),
            None => hostname.to_lowercase(),
        })
    }
    /// `.port` — raises ValueError for a non-numeric or out-of-range port.
    pub fn port(&self) -> Result<Option<u32>, UrlError> {
        match self.hostinfo().1 {
            None => Ok(None),
            Some(p) => {
                if !p.chars().all(|c| c.is_ascii_digit()) {
                    return Err(UrlError);
                }
                let trimmed = p.trim_start_matches('0');
                if trimmed.len() > 5 {
                    return Err(UrlError);
                }
                let value: u32 = if trimmed.is_empty() { 0 } else { trimmed.parse().map_err(|_| UrlError)? };
                if value > 65535 {
                    return Err(UrlError);
                }
                Ok(Some(value))
            }
        }
    }
}

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `_unquote_impl` on an ASCII run, then `.decode('utf-8', 'strict')`.
fn unquote_ascii_run(run: &str) -> Result<String, UrlError> {
    let b = run.as_bytes();
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
    String::from_utf8(out).map_err(|_| UrlError)
}

/// `urllib.parse.unquote(s, errors='strict')`
pub fn unquote_strict(s: &str) -> Result<String, UrlError> {
    if !s.contains('%') {
        return Ok(s.to_string());
    }
    let mut out = String::new();
    let mut run_start = 0;
    let mut ascii = None::<bool>;
    for (i, c) in s.char_indices() {
        let is_ascii = c.is_ascii();
        match ascii {
            Some(prev) if prev != is_ascii => {
                let run = &s[run_start..i];
                if prev { out.push_str(&unquote_ascii_run(run)?) } else { out.push_str(run) }
                run_start = i;
            }
            _ => {}
        }
        ascii = Some(is_ascii);
    }
    if let Some(prev) = ascii {
        let run = &s[run_start..];
        if prev { out.push_str(&unquote_ascii_run(run)?) } else { out.push_str(run) }
    }
    Ok(out)
}

/// `urllib.parse.unquote_plus(s, errors='strict')`
pub fn unquote_plus_strict(s: &str) -> Result<String, UrlError> {
    unquote_strict(&s.replace('+', " "))
}

/// `parse_qsl(qs, keep_blank_values=True, strict_parsing=True,
/// errors='strict', max_num_fields=max_fields)`
pub fn parse_qsl_strict(qs: &str, max_fields: usize) -> Result<Vec<(String, String)>, UrlError> {
    if qs.is_empty() {
        return Ok(Vec::new());
    }
    if max_fields < 1 + qs.matches('&').count() {
        return Err(UrlError);
    }
    let mut pairs = Vec::new();
    for name_value in qs.split('&') {
        let Some((name, value)) = name_value.split_once('=') else { return Err(UrlError) };
        let name = unquote_plus_strict(name)?;
        let value = unquote_plus_strict(value)?;
        pairs.push((name, value));
    }
    Ok(pairs)
}

/// `urllib.parse.quote_plus(s, safe='')`
pub fn quote_plus(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(*b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// `urllib.parse.urlencode(pairs)` for already-`str()`ed values.
pub fn urlencode(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `html.escape(s, quote=True)`
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// `urllib.request.Request(url).full_url` normalization (`unwrap`): strip
/// whitespace, `<...>` and a `URL:` prefix.
pub fn request_unwrap(url: &str) -> String {
    let mut u = crate::runtime::py_strip(url).to_string();
    if u.starts_with('<') && u.ends_with('>') && u.len() >= 2 {
        u = crate::runtime::py_strip(&u[1..u.len() - 1]).to_string();
    }
    if let Some(rest) = u.strip_prefix("URL:") {
        u = crate::runtime::py_strip(rest).to_string();
    }
    u
}
