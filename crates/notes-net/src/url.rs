//! URL handling exactly as `urllib.request.Request`, `urllib.parse.urlsplit`
//! and `http.client.HTTPConnection._get_hostport` do it (CPython 3.14).
use crate::error::NetError;
use crate::py;
use std::net::Ipv6Addr;

/// What `urllib.request.Request(url)` derives from a URL.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RequestUrl {
    /// `request.full_url` (fragment re-attached).
    pub full_url: String,
    /// `request.type` (lower-cased scheme).
    pub scheme: String,
    /// `request.host` (percent-decoded, may carry `:port`); `None`/empty when absent.
    pub host: Option<String>,
    /// `request.selector`
    pub selector: String,
}

/// `urllib.parse.unwrap`
fn unwrap(url: &str) -> &str {
    let mut url = py::strip(url);
    if url.starts_with('<') && url.ends_with('>') && url.len() >= 2 {
        url = py::strip(&url[1..url.len() - 1]);
    }
    if let Some(rest) = url.strip_prefix("URL:") {
        url = py::strip(rest);
    }
    url
}

/// `urllib.request.Request(url)`; the error is the constructor's ValueError.
pub(crate) fn request_url(url: &str) -> Result<RequestUrl, NetError> {
    let unwrapped = unwrap(url);
    // _splittag: rpartition('#')
    let (base, fragment) = match unwrapped.rfind('#') {
        Some(i) => (&unwrapped[..i], Some(&unwrapped[i + 1..])),
        None => (unwrapped, None),
    };
    // _splittype: '([^/:]+):(.*)'
    let colon = base.find([':', '/']);
    let (scheme, rest) = match colon {
        Some(i) if i > 0 && base[i..].starts_with(':') => (py::lower(&base[..i]), &base[i + 1..]),
        _ => {
            let shown = match fragment {
                Some(f) => format!("{base}#{f}"),
                None => base.to_string(),
            };
            return Err(NetError::value(format!("unknown url type: {}", py_repr(&shown))));
        }
    };
    // _splithost: '//([^/#?]*)(.*)'
    let (host, selector) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '#', '?']).unwrap_or(after.len());
            let host_port = &after[..end];
            let mut path = after[end..].to_string();
            if !path.is_empty() && !path.starts_with('/') {
                path.insert(0, '/');
            }
            (Some(host_port.to_string()), path)
        }
        None => (None, rest.to_string()),
    };
    let host = host.map(|h| if h.is_empty() { h } else { unquote(&h) });
    let full_url = match fragment {
        Some(f) => format!("{base}#{f}"),
        None => base.to_string(),
    };
    // Request.__init__ -> request_host() -> urlparse(full_url): may raise.
    if let Err(message) = urlsplit_check(&full_url) {
        return Err(NetError::value(message));
    }
    Ok(RequestUrl { full_url, scheme, host, selector })
}

/// A short `repr()` of a str for messages (single quotes, no escaping detail).
fn py_repr(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

const C0_OR_SPACE: &[char] = &[
    '\u{00}', '\u{01}', '\u{02}', '\u{03}', '\u{04}', '\u{05}', '\u{06}', '\u{07}', '\u{08}', '\t', '\n', '\u{0b}',
    '\u{0c}', '\r', '\u{0e}', '\u{0f}', '\u{10}', '\u{11}', '\u{12}', '\u{13}', '\u{14}', '\u{15}', '\u{16}', '\u{17}',
    '\u{18}', '\u{19}', '\u{1a}', '\u{1b}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}', ' ',
];

/// The parts of `urlsplit` that matter here: `(scheme, netloc)` after the
/// WHATWG clean-up, or the ValueError `urlsplit` raises.
pub(crate) fn urlsplit_netloc(url: &str) -> Result<(String, Option<String>), String> {
    let cleaned: String = url.trim_start_matches(C0_OR_SPACE).chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest: &str = &cleaned;
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next().unwrap_or(' ');
        if i > 0 && first.is_ascii_alphabetic()
            && rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = rest[..i].to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = None;
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let n = &after[..end];
        let open = n.contains('[');
        let close = n.contains(']');
        if open != close {
            return Err("Invalid IPv6 URL".into());
        }
        if open && close {
            check_bracketed_netloc(n)?;
        }
        // _checknetloc: NFKC expansion check, only for non-ASCII netlocs.
        // Every value reaching here is latin-1 decoded and no latin-1
        // character NFKC-normalises to one of '/?#@:'.
        netloc = Some(n.to_string());
    }
    Ok((scheme, netloc))
}

/// `urlsplit(url)` raising ValueError?
pub(crate) fn urlsplit_check(url: &str) -> Result<(), String> {
    urlsplit_netloc(url).map(|_| ())
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    let host_and_port = match netloc.rfind('@') {
        Some(i) => &netloc[i + 1..],
        None => netloc,
    };
    let hostname = match host_and_port.find('[') {
        Some(i) => {
            if i > 0 {
                return Err("Invalid IPv6 URL".into());
            }
            let bracketed = &host_and_port[i + 1..];
            let (hostname, port) = match bracketed.find(']') {
                Some(j) => (&bracketed[..j], &bracketed[j + 1..]),
                None => (bracketed, ""),
            };
            if !port.is_empty() && !port.starts_with(':') {
                return Err("Invalid IPv6 URL".into());
            }
            hostname
        }
        None => host_and_port.split(':').next().unwrap_or(""),
    };
    check_bracketed_host(hostname)
}

fn check_bracketed_host(hostname: &str) -> Result<(), String> {
    if hostname.starts_with(['v', 'V']) {
        // \A[vV][a-fA-F0-9]+\..+\z
        let body = &hostname[1..];
        let hex_len = body.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        let tail = &body[hex_len..];
        let ok = hex_len > 0 && tail.starts_with('.') && tail.len() > 1 && !tail[1..].contains('\n');
        if !ok {
            return Err("IPvFuture address is invalid".into());
        }
        return Ok(());
    }
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("An IPv4 address cannot be in brackets".into());
    }
    if parse_ipv6_scoped(hostname).is_none() {
        return Err(format!("'{hostname}' does not appear to be an IPv4 or IPv6 address"));
    }
    Ok(())
}

/// `ipaddress.IPv6Address(text)` including an optional `%scope`.
pub(crate) fn parse_ipv6_scoped(text: &str) -> Option<Ipv6Addr> {
    let (addr, scope) = match text.find('%') {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    };
    if let Some(scope) = scope {
        if scope.is_empty() || scope.contains('%') {
            return None;
        }
    }
    addr.parse::<Ipv6Addr>().ok()
}

/// `urlsplit(url).hostname` (lower-cased, brackets and `%scope` handling as
/// `_hostinfo`), or the ValueError.
pub(crate) fn hostname(url: &str) -> Result<Option<String>, String> {
    let (_, netloc) = urlsplit_netloc(url)?;
    let netloc = netloc.unwrap_or_default();
    let host_and_port = match netloc.rfind('@') {
        Some(i) => &netloc[i + 1..],
        None => &netloc[..],
    };
    let host = match host_and_port.find('[') {
        Some(i) => {
            let bracketed = &host_and_port[i + 1..];
            match bracketed.find(']') {
                Some(j) => &bracketed[..j],
                None => bracketed,
            }
        }
        None => host_and_port.split(':').next().unwrap_or(""),
    };
    if host.is_empty() {
        return Ok(None);
    }
    // SplitResult.hostname: lower-cases, but keeps the zone id's case.
    let lowered = match host.find('%') {
        Some(i) => format!("{}{}", py::lower(&host[..i]), &host[i..]),
        None => py::lower(host),
    };
    Ok(Some(lowered))
}

/// `urllib.parse.unquote(s)` (utf-8, errors='replace').
pub(crate) fn unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut ascii_run: Vec<u8> = Vec::new();
    let flush = |run: &mut Vec<u8>, out: &mut String| {
        if run.is_empty() {
            return;
        }
        let mut bytes = Vec::with_capacity(run.len());
        let mut i = 0;
        while i < run.len() {
            if run[i] == b'%' && i + 2 < run.len() {
                let hex = &run[i + 1..i + 3];
                if let (Some(h), Some(l)) = ((hex[0] as char).to_digit(16), (hex[1] as char).to_digit(16)) {
                    bytes.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
            }
            bytes.push(run[i]);
            i += 1;
        }
        out.push_str(&String::from_utf8_lossy(&bytes));
        run.clear();
    };
    for c in s.chars() {
        if c.is_ascii() {
            ascii_run.push(c as u8);
        } else {
            flush(&mut ascii_run, &mut out);
            out.push(c);
        }
    }
    flush(&mut ascii_run, &mut out);
    out
}

/// `HTTPConnection._get_hostport(host, None)`; errors are `InvalidURL`.
/// The port is Python's `int()` of the text and may be out of range.
pub(crate) fn host_port(host: &str, default_port: u16) -> Result<(String, i128), NetError> {
    let mut host = host.to_string();
    let mut port = default_port as i128;
    let i = host.rfind(':');
    let j = host.rfind(']');
    if let Some(i) = i {
        if j.is_none_or(|j| i > j) {
            let text = &host[i + 1..];
            match py::int10(text) {
                Some(p) => port = p,
                None => {
                    if !text.is_empty() {
                        return Err(NetError::escape("InvalidURL", format!("nonnumeric port: '{text}'")));
                    }
                }
            }
            host.truncate(i);
        }
    }
    if host.starts_with('[') && host.ends_with(']') && host.len() >= 2 {
        host = host[1..host.len() - 1].to_string();
    }
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_parsing_matches_urllib() {
        let r = request_url(" https://api.example.com/a?b=1#frag ").unwrap();
        assert_eq!(r.scheme, "https");
        assert_eq!(r.host.as_deref(), Some("api.example.com"));
        assert_eq!(r.selector, "/a?b=1");
        assert_eq!(r.full_url, "https://api.example.com/a?b=1#frag");
        let r = request_url("HTTP://h:8080?x").unwrap();
        assert_eq!((r.scheme.as_str(), r.host.as_deref(), r.selector.as_str()), ("http", Some("h:8080"), "/?x"));
        let r = request_url("https://h").unwrap();
        assert_eq!(r.selector, "");
        assert!(request_url("/no/scheme").is_err());
        assert!(request_url("https://[::1/").is_err());
        assert!(request_url("https://[1.2.3.4]/").is_err());
        assert!(request_url("https://[::1]:8443/").is_ok());
        let r = request_url("https://%61pi.example/").unwrap();
        assert_eq!(r.host.as_deref(), Some("api.example"));
        assert!(request_url("mailto:x").is_ok());
    }

    #[test]
    fn hostport_matches_http_client() {
        assert_eq!(host_port("h:8443", 443).unwrap(), ("h".into(), 8443));
        assert_eq!(host_port("h:", 443).unwrap(), ("h".into(), 443));
        assert_eq!(host_port("[::1]:9", 443).unwrap(), ("::1".into(), 9));
        assert_eq!(host_port("[::1]", 443).unwrap(), ("::1".into(), 443));
        assert!(host_port("h:x", 443).is_err());
    }

    #[test]
    fn hostname_and_unquote() {
        assert_eq!(hostname("https://User@API.Example.com:1/x").unwrap().as_deref(), Some("api.example.com"));
        assert_eq!(hostname("https://[FE80::1%25Eth0]/").unwrap().as_deref(), Some("fe80::1%25Eth0"));
        assert_eq!(hostname("https:///x").unwrap(), None);
        assert_eq!(unquote("a%2Fb%zz%4"), "a/b%zz%4");
        assert_eq!(unquote("%C3%A9x"), "\u{e9}x");
        assert_eq!(unquote("%FF"), "\u{fffd}");
    }
}
