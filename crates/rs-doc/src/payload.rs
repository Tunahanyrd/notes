//! Pure-Rust port of `tools.notes_payload.parse_payload` and helpers.
use crate::error::{Error, Result};
use crate::mime::{self, Hooks, Limits, Part};
use crate::util;
use crate::xml::{self, Element};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadUrl {
    pub id: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentFile {
    pub file_hash: String,
    pub source_id: Option<String>,
    pub metadata_hash: Option<String>,
    pub name: Option<String>,
    pub name_encoded: Option<String>,
    pub bind_number: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPreview {
    /// "title", "body" or "object"
    pub context: &'static str,
    pub text: String,
}

/// Everything `parse_payload` extracts. Part bodies are ranges into the input.
#[derive(Clone, Debug, Default)]
pub struct PayloadResult {
    pub parts: Vec<Part>,
    /// Indices into `parts` of the XML documents (`xml_documents`).
    pub xml_parts: Vec<usize>,
    pub xml_roots: Vec<String>,
    pub resource_ids: Vec<String>,
    pub download_urls: Vec<DownloadUrl>,
    pub content_files: Vec<ContentFile>,
    pub text_previews: Vec<TextPreview>,
    pub locked: bool,
    /// Parsed roots of the XML documents (same order as `xml_parts`).
    pub roots: Vec<Element>,
}

impl PayloadResult {
    /// `PayloadResult.text`
    pub fn text(&self) -> String {
        self.text_previews.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

pub fn is_xml_type(kind: &str) -> bool {
    mime::XML_TYPES.contains(&kind)
}

fn field(value: &str, limits: &Limits) -> Result<()> {
    if mime::field_ok(value, limits) {
        Ok(())
    } else {
        Err(Error::Payload("invalid metadata field"))
    }
}

fn is_py_space(c: char) -> bool {
    mime::py_is_space(c)
}

/// `_xml(raw)`: bounded, DTD-free UTF-8 XML with a supported root.
pub fn parse_xml(raw: &[u8], limits: &Limits) -> Result<Element> {
    if raw.len() > limits.max_xml_bytes {
        return Err(Error::Payload("XML exceeds size limit"));
    }
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    let text = std::str::from_utf8(raw).map_err(|_| Error::Payload("XML must be UTF-8"))?;
    if has_dtd(text) {
        return Err(Error::Payload("XML DTDs and entity declarations are prohibited"));
    }
    if let Some(decl) = declaration(text) {
        if let Some(enc) = declared_encoding(decl) {
            let folded = xml::casefold(enc);
            if !matches!(folded.as_ref(), "utf-8" | "utf8" | "us-ascii") {
                return Err(Error::Payload("unsupported XML encoding"));
            }
        }
    }
    let root = match xml::parse(text) {
        Ok(root) => root,
        Err(fail) => {
            if xml::structure_exceeded_before(text, fail.offset, limits.max_xml_depth, limits.max_xml_nodes) {
                return Err(Error::Payload("XML structural limit exceeded"));
            }
            return Err(Error::Payload("malformed XML"));
        }
    };
    let (count, depth) = root.shape();
    if depth as i64 > limits.max_xml_depth || count as i64 > limits.max_xml_nodes {
        return Err(Error::Payload("XML structural limit exceeded"));
    }
    let name = xml::local_casefold(&root.tag);
    if !matches!(name.as_str(), "comparenoteview" | "noteresource" | "getnoteinfoview") {
        return Err(Error::Payload("unsupported XML root"));
    }
    Ok(root)
}

/// `re.search(r'<!\s*(?:DOCTYPE|ENTITY)\b', text, re.IGNORECASE)`
fn has_dtd(text: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)<![\s\x1c-\x1f]*(?:DOCTYPE|ENTITY)\b").unwrap());
    text.contains("<!") && re.is_match(text)
}

/// `re.match(r'\s*<\?xml\b[^?]*\?>', text)`
fn declaration(text: &str) -> Option<&str> {
    let start = text.len() - text.trim_start_matches(is_py_space).len();
    let rest = &text[start..];
    let after = rest.strip_prefix("<?xml")?;
    match after.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' => return None,
        _ => {}
    }
    let q = after.find('?')?;
    if after[q + 1..].starts_with('>') {
        Some(&rest[..5 + q + 2])
    } else {
        None
    }
}

/// `re.search(r'encoding\s*=\s*([\'"])(.*?)\1', decl, re.IGNORECASE)[2]`
fn declared_encoding(decl: &str) -> Option<&str> {
    let lower = decl.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find("encoding") {
        let start = from + p;
        let mut i = start + 8;
        let b = decl.as_bytes();
        let skip = |i: &mut usize| {
            while *i < decl.len() {
                let c = decl[*i..].chars().next().unwrap();
                if !is_py_space(c) {
                    break;
                }
                *i += c.len_utf8();
            }
        };
        skip(&mut i);
        if i < b.len() && b[i] == b'=' {
            i += 1;
            skip(&mut i);
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i] as char;
                let body = &decl[i + 1..];
                let line = body.split('\n').next().unwrap_or("");
                if let Some(end) = line.find(quote) {
                    return Some(&body[..end]);
                }
            }
        }
        from = start + 1;
    }
    None
}

/// `_base64_utf8(value, limit)`
pub fn base64_utf8(value: &str, limit: usize) -> Result<String> {
    let compact: String = value.chars().filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n')).collect();
    if compact.chars().count() > ((limit + 2) / 3) * 4 {
        return Err(Error::Payload("encoded text exceeds limit"));
    }
    let invalid = || Error::Payload("invalid Base64 UTF-8 text");
    if !compact.is_ascii() {
        return Err(invalid());
    }
    let raw = util::b64decode_strict(compact.as_bytes()).ok_or_else(invalid)?;
    if raw.len() > limit {
        return Err(invalid());
    }
    String::from_utf8(raw).map_err(|_| invalid())
}

/// `decode_rich_text(value)`: outer Base64 plus annotation-delimited chunks.
pub fn decode_rich_text(value: &str, limits: &Limits) -> Result<String> {
    let rich = base64_utf8(value, limits.max_text_bytes)?;
    let mut out = String::new();
    let mut cursor = 0usize;
    let mut size = 0usize;
    while cursor < rich.len() {
        let marker = rich[cursor..].find("<?a").map(|p| p + cursor);
        let end = marker.unwrap_or(rich.len());
        let chunk = base64_utf8(&rich[cursor..end], limits.max_text_bytes)?;
        size += chunk.len();
        if size > limits.max_text_bytes {
            return Err(Error::Payload("text preview exceeds limit"));
        }
        out.push_str(&chunk);
        let Some(marker) = marker else { break };
        let Some(closing) = rich[marker + 3..].find("?>").map(|p| p + marker + 3) else {
            return Err(Error::Payload("malformed rich-text annotation"));
        };
        cursor = closing + 2;
    }
    Ok(out)
}

/// `_url(value)` acceptance (`urlsplit` semantics); never fetches.
pub fn url_ok(value: &str) -> bool {
    if value.chars().any(|c| c.is_whitespace() || (c as u32) < 32 || c as u32 == 127) {
        return false;
    }
    let Some(colon) = value.find(':') else { return false };
    let scheme = &value[..colon];
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let rest = &value[colon + 1..];
    let Some(after) = rest.strip_prefix("//") else { return false };
    let delim = after.find(|c| matches!(c, '/' | '?' | '#')).unwrap_or(after.len());
    let netloc = &after[..delim];
    let path = &after[delim..];
    let (open, close) = (netloc.contains('['), netloc.contains(']'));
    if open != close {
        return false;
    }
    if !netloc.is_ascii() {
        return false; // NFKC netloc checks are not modelled (fail closed)
    }
    if netloc.contains('@') {
        return false; // username present
    }
    let (hostname, port) = if open {
        let (before, bracketed) = netloc.split_once('[').unwrap();
        if !before.is_empty() {
            return false;
        }
        let (host, after_host) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        if !after_host.is_empty() && !after_host.starts_with(':') {
            return false;
        }
        if host.starts_with('v') {
            let ok = host.len() > 1 && {
                let body = &host[1..];
                match body.split_once('.') {
                    Some((hexpart, rest)) => !hexpart.is_empty() && hexpart.bytes().all(|b| b.is_ascii_hexdigit()) && !rest.is_empty(),
                    None => false,
                }
            };
            if !ok {
                return false;
            }
        } else if host.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        let port = after_host.split_once(':').map(|(_, p)| p).unwrap_or("");
        (host, port)
    } else {
        match netloc.split_once(':') {
            Some((h, p)) => (h, p),
            None => (netloc, ""),
        }
    };
    if hostname.is_empty() {
        return false;
    }
    if !port.is_empty() {
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if port.trim_start_matches('0') != "443" {
            return false;
        }
    }
    // fragment: first '#' after the netloc, non-empty => rejected
    if let Some(h) = path.find('#') {
        if !path[h + 1..].is_empty() {
            return false;
        }
    }
    true
}

fn boolean(value: Option<&str>) -> Result<bool> {
    match value {
        None => Ok(false),
        Some(v) => match xml::casefold(v).as_ref() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(Error::Payload("invalid encryption or lock flag")),
        },
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Walk one parsed XML document, accumulating into `out` (parse_payload loop body).
pub fn walk_root(root: &Element, out: &mut PayloadResult, preview_bytes: &mut usize, limits: &Limits) -> Result<()> {
    let mut stack: Vec<(&Element, usize)> = vec![(root, 0)];
    let mut ancestors: Vec<String> = Vec::new();
    while let Some((element, depth)) = stack.pop() {
        ancestors.truncate(depth);
        let tag = xml::local_casefold(&element.tag);
        let mut attrs: Vec<(String, &str)> = Vec::with_capacity(element.attrs.len());
        for (k, v) in &element.attrs {
            let key = xml::local_casefold(k);
            if attrs.iter().any(|(existing, _)| *existing == key) {
                return Err(Error::Payload("ambiguous XML attributes"));
            }
            field(v, limits)?;
            attrs.push((key, v.as_str()));
        }
        let get = |name: &str| attrs.iter().find(|(k, _)| k == name).map(|(_, v)| *v);
        match tag.as_str() {
            "noteresource" => {
                if boolean(get("isencrypted"))? {
                    return Err(Error::Payload("encrypted note resources are unsupported"));
                }
                out.locked = out.locked || boolean(get("islocked"))?;
                if let Some(id) = get("id") {
                    if !id.is_empty() {
                        out.resource_ids.push(id.to_string());
                    }
                }
            }
            _ => {}
        }
        if tag == "notetree" && boolean(get("encrypted"))? {
            return Err(Error::Payload("EDP encrypted note trees are unsupported"));
        }
        if tag == "downloadurl" {
            let mut id: Option<String> = None;
            let mut url: Option<String> = None;
            for child in &element.children {
                let name = xml::local_casefold(&child.tag);
                if name == "id" || name == "url" {
                    let slot = if name == "id" { &mut id } else { &mut url };
                    if slot.is_some() || !child.children.is_empty() {
                        return Err(Error::Payload("ambiguous download URL entry"));
                    }
                    let text = child.text.clone().unwrap_or_default();
                    field(&text, limits)?;
                    *slot = Some(text);
                }
            }
            let identity = id.unwrap_or_default();
            let url = url.unwrap_or_default();
            if identity.is_empty() || url.is_empty() || out.download_urls.iter().any(|u| u.id == identity) {
                return Err(Error::Payload("invalid or duplicate download URL entry"));
            }
            if !url_ok(&url) {
                return Err(Error::Payload("invalid resource URL"));
            }
            out.download_urls.push(DownloadUrl { id: identity, url });
        }
        if tag == "contentfile" {
            let hashes: Vec<&str> = element
                .children
                .iter()
                .filter(|c| xml::local_casefold(&c.tag) == "filehash")
                .map(|c| c.text.as_deref().unwrap_or(""))
                .collect();
            if hashes.len() > 1 {
                return Err(Error::Payload("ambiguous content-file hash"));
            }
            let file_hash = hashes.first().copied().unwrap_or_else(|| get("id").unwrap_or(""));
            if !is_sha256_hex(file_hash) {
                return Err(Error::Payload("invalid content-file SHA256"));
            }
            if let Some(id) = get("id") {
                if !id.is_empty() && xml::casefold(id) != xml::casefold(file_hash) {
                    return Err(Error::Payload("content-file identifier does not match file hash"));
                }
            }
            let encoded = get("name");
            let name = match encoded {
                Some(e) => Some(base64_utf8(e, limits.max_field_bytes)?),
                None => None,
            };
            out.content_files.push(ContentFile {
                file_hash: file_hash.to_ascii_lowercase(),
                source_id: get("id").map(str::to_string),
                metadata_hash: get("hash").map(str::to_string),
                name,
                name_encoded: encoded.map(str::to_string),
                bind_number: get("bindnumber").map(str::to_string),
            });
        }
        if tag == "text" && ancestors.last().map(String::as_str) == Some("object") {
            if !element.children.is_empty() {
                return Err(Error::Payload("unexpected nested text element"));
            }
            let text = decode_rich_text(element.text.as_deref().unwrap_or(""), limits)?;
            *preview_bytes += text.len();
            if *preview_bytes > limits.max_text_bytes {
                return Err(Error::Payload("total text preview exceeds limit"));
            }
            let context = if ancestors.iter().any(|a| a == "title") {
                "title"
            } else if ancestors.iter().any(|a| a == "bodytext") {
                "body"
            } else {
                "object"
            };
            out.text_previews.push(TextPreview { context, text });
        }
        let most = out.download_urls.len().max(out.content_files.len()).max(out.resource_ids.len()).max(out.text_previews.len());
        if most > limits.max_resources {
            return Err(Error::Payload("too many payload resources"));
        }
        ancestors.push(tag);
        for child in element.children.iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    Ok(())
}

/// Validate `max_parts` like `_parts` (`type(max_parts) is int` checked by callers).
pub fn check_body(body: &[u8], limits: &Limits) -> Result<()> {
    if body.is_empty() || body.len() > limits.max_body_bytes {
        return Err(Error::Payload("payload is empty or exceeds size limit"));
    }
    Ok(())
}

/// Run the XML part of parse_payload over already split parts.
pub fn payload_from_parts(body: &[u8], parts: Vec<Part>, limits: &Limits) -> Result<PayloadResult> {
    let mut out = PayloadResult::default();
    let mut preview_bytes = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if !is_xml_type(&part.content_type) {
            continue;
        }
        let root = parse_xml(&body[part.body.clone()], limits)?;
        out.xml_parts.push(i);
        out.xml_roots.push(xml::local_casefold(&root.tag));
        walk_root(&root, &mut out, &mut preview_bytes, limits)?;
        out.roots.push(root);
    }
    if out.xml_parts.is_empty() {
        return Err(Error::Payload("payload contains no supported XML document"));
    }
    out.parts = parts;
    Ok(out)
}

/// `parse_payload(body, content_type, max_parts=...)`.
pub fn parse_payload<H: Hooks>(body: &[u8], content_type: &str, max_parts: Option<i64>, limits: &Limits, hooks: &mut H) -> Result<PayloadResult> {
    check_body(body, limits)?;
    let parts = mime::parts(body, content_type, max_parts.unwrap_or(limits.max_parts), limits, hooks)?;
    payload_from_parts(body, parts, limits)
}

/// Same, with the default limits and no hooks.
pub fn parse(body: &[u8], content_type: &str, max_parts: Option<i64>) -> Result<PayloadResult> {
    parse_payload(body, content_type, max_parts, &Limits::default(), &mut mime::NoHooks)
}
