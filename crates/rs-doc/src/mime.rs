//! Bounded multipart splitting and MIME header parsing (pure Rust).
//!
//! Mirrors `tools.notes_payload._headers/_content_type/_parts`, which use
//! `email.parser.BytesHeaderParser(policy=policy.default)`. The structured
//! headers we read (Content-Type, Content-Disposition,
//! Content-Transfer-Encoding) are validated with the same grammar as
//! `email._header_value_parser`; constructs that parser would accept but this
//! port does not model (comments, RFC 2231 sections, encoded words inside
//! quoted strings, a leading `From ` envelope line) are rejected (fail closed).
use crate::error::{Error, Result};

pub const XML_TYPES: [&str; 2] = ["application/xml", "text/xml"];

/// Limits read from the Python module at call time (tests patch them).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_body_bytes: usize,
    pub max_parts: i64,
    pub max_readonly_parts: i64,
    pub max_header_bytes: usize,
    pub max_xml_bytes: usize,
    pub max_xml_nodes: i64,
    pub max_xml_depth: i64,
    pub max_text_bytes: usize,
    pub max_field_bytes: usize,
    pub max_resources: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_body_bytes: 32 * 1024 * 1024,
            max_parts: 1024,
            max_readonly_parts: 16384,
            max_header_bytes: 16 * 1024,
            max_xml_bytes: 8 * 1024 * 1024,
            max_xml_nodes: 100_000,
            max_xml_depth: 64,
            max_text_bytes: 2 * 1024 * 1024,
            max_field_bytes: 16 * 1024,
            max_resources: 8192,
        }
    }
}

/// Parsed values of the three structured headers (`str(header)` form).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    pub content_type: Option<String>,
    pub disposition: Option<String>,
    pub transfer_encoding: Option<String>,
}

/// One multipart part; `body` is a byte range of the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub index: usize,
    pub name: Option<String>,
    pub filename: Option<String>,
    pub content_type: String,
    pub body: std::ops::Range<usize>,
}

/// Result of the narrow Samsung fast-path header form.
#[derive(Clone, Debug)]
pub struct FastMatch {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: String,
}

/// Hooks so a binding layer can route through patched module attributes.
pub trait Hooks {
    fn headers(&mut self, raw: &[u8], limits: &Limits) -> Result<Headers> {
        parse_headers(raw, limits)
    }
    fn fast_match(&mut self, raw: &[u8]) -> Result<Option<FastMatch>> {
        Ok(samsung_fast_match(raw))
    }
}

/// Pure-Rust default hooks.
pub struct NoHooks;
impl Hooks for NoHooks {}

// ---------------------------------------------------------------- helpers

const TSPECIALS: &[u8] = b"()<>@,:;\\\"[]/?=";

fn is_wsp(b: u8) -> bool {
    b == b' ' || b == b'\t'
}
fn token_end(b: u8) -> bool {
    TSPECIALS.contains(&b) || is_wsp(b)
}
fn attribute_end(b: u8) -> bool {
    token_end(b) || matches!(b, b'*' | b'\'' | b'%')
}
fn ext_attribute_end(b: u8) -> bool {
    token_end(b) || matches!(b, b'*' | b'\'')
}
fn non_printable(b: u8) -> bool {
    b <= 0x20 || b == 0x7f
}

/// Python `str.strip()` whitespace for ASCII.
pub fn py_is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\x1f' | '\u{85}')
        || (c as u32 > 0x7f && c.is_whitespace())
}
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_is_space)
}
fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_is_space)
}

/// Fail = defect (reject); Parse = HeaderParseError (recoverable in some places).
enum Fail {
    Parse,
    Defect,
}

struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
    out: String,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn at_end(&self) -> bool {
        self.i >= self.s.len()
    }
    fn copy_to(&mut self, j: usize) {
        // ASCII-only input (validated by the caller)
        self.out.push_str(std::str::from_utf8(&self.s[self.i..j]).unwrap());
        self.i = j;
    }
    /// [CFWS]: whitespace and (nested) comments, copied raw. Comments with
    /// quoted pairs are not modelled (fail closed).
    fn cfws(&mut self) -> std::result::Result<(), Fail> {
        loop {
            match self.peek() {
                Some(b) if is_wsp(b) => {
                    let mut j = self.i;
                    while j < self.s.len() && is_wsp(self.s[j]) {
                        j += 1;
                    }
                    self.copy_to(j);
                }
                Some(b'(') => {
                    let mut j = self.i;
                    let mut depth = 0usize;
                    loop {
                        let Some(&b) = self.s.get(j) else { return Err(Fail::Defect) };
                        match b {
                            b'(' => depth += 1,
                            b')' => {
                                depth -= 1;
                                if depth == 0 {
                                    j += 1;
                                    break;
                                }
                            }
                            b'\\' => return Err(Fail::Defect),
                            b' ' | b'\t' => {}
                            b if non_printable(b) => return Err(Fail::Defect),
                            _ => {}
                        }
                        j += 1;
                    }
                    self.copy_to(j);
                }
                _ => return Ok(()),
            }
        }
    }
    fn run(&mut self, end: fn(u8) -> bool) -> std::result::Result<(), Fail> {
        let mut j = self.i;
        while j < self.s.len() && !end(self.s[j]) {
            j += 1;
        }
        if j == self.i {
            return Err(Fail::Parse);
        }
        if self.s[self.i..j].iter().any(|&b| non_printable(b)) {
            return Err(Fail::Defect);
        }
        self.copy_to(j);
        Ok(())
    }
    /// get_token: [CFWS] 1*ttext [CFWS]
    fn token(&mut self) -> std::result::Result<(), Fail> {
        self.cfws()?;
        match self.peek() {
            Some(b) if token_end(b) => return Err(Fail::Parse),
            None => return Err(Fail::Parse),
            _ => {}
        }
        self.run(token_end)?;
        self.cfws()
    }
    /// get_quoted_string (bare string canonicalised like str(parse_tree)).
    fn quoted_string(&mut self) -> std::result::Result<String, Fail> {
        // caller guarantees s[i] == '"'
        self.i += 1;
        let mut content = String::new();
        loop {
            match self.peek() {
                None => return Err(Fail::Defect), // end of header inside quoted string
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(b) if is_wsp(b) => {
                    while let Some(b) = self.peek() {
                        if !is_wsp(b) {
                            break;
                        }
                        content.push(b as char);
                        self.i += 1;
                    }
                }
                Some(b'=') if self.s.get(self.i + 1) == Some(&b'?') => return Err(Fail::Defect),
                Some(_) => {
                    // _get_ptext_to_endchars(value, '"') on the current fragment
                    let start = self.i;
                    let mut end = start;
                    while end < self.s.len() && !is_wsp(self.s[end]) {
                        end += 1;
                    }
                    let fragment = &self.s[start..end];
                    let mut escape = false;
                    let mut pos = 0;
                    let mut stopped = false;
                    let mut vchars: Vec<u8> = Vec::new();
                    while pos < fragment.len() {
                        let c = fragment[pos];
                        if c == b'\\' {
                            if escape {
                                escape = false;
                            } else {
                                escape = true;
                                pos += 1;
                                continue;
                            }
                        }
                        if escape {
                            escape = false;
                        } else if c == b'"' {
                            stopped = true;
                            break;
                        }
                        vchars.push(c);
                        pos += 1;
                    }
                    if !stopped {
                        pos = fragment.len();
                    }
                    if vchars.iter().any(|&b| non_printable(b)) {
                        return Err(Fail::Defect);
                    }
                    content.push_str(std::str::from_utf8(&vchars).unwrap());
                    self.i = start + pos;
                }
            }
        }
        Ok(content)
    }
    /// get_parameter (non-extended subset) -> (attribute, param_value)
    fn parameter(&mut self) -> std::result::Result<(String, String), Fail> {
        // get_attribute
        self.cfws()?;
        match self.peek() {
            None => return Err(Fail::Parse),
            Some(b) if attribute_end(b) => return Err(Fail::Parse),
            _ => {}
        }
        let name_start = self.i;
        self.run(attribute_end)?;
        let name = std::str::from_utf8(&self.s[name_start..self.i]).unwrap().to_string();
        self.cfws()?;
        match self.peek() {
            None | Some(b';') => return Err(Fail::Defect), // name but no value
            Some(b'*') => return Err(Fail::Defect),        // RFC 2231: not modelled
            Some(b'=') => {}
            Some(_) => return Err(Fail::Parse),
        }
        self.copy_to(self.i + 1);
        self.cfws()?;
        match self.peek() {
            Some(b'\'') => Err(Fail::Defect),
            None => Err(Fail::Parse),
            Some(b'"') => {
                let value = self.quoted_string()?;
                self.cfws()?;
                Ok((name, value))
            }
            Some(b) if ext_attribute_end(b) => Err(Fail::Parse),
            Some(_) => {
                let v_start = self.i;
                self.run(ext_attribute_end)?;
                let value = std::str::from_utf8(&self.s[v_start..self.i]).unwrap().to_string();
                self.cfws()?;
                Ok((name, value))
            }
        }
    }
    /// parse_mime_parameters, rendered like str(MimeParameters)
    fn parameters(&mut self) -> std::result::Result<(), Fail> {
        let mark = self.out.len();
        let mut params: Vec<(String, String)> = Vec::new();
        while !self.at_end() {
            let start = self.i;
            match self.parameter() {
                Ok(p) => {
                    if params.iter().any(|(n, _)| *n == p.0) {
                        return Err(Fail::Defect); // duplicate parameter name
                    }
                    params.push(p);
                }
                Err(Fail::Defect) => return Err(Fail::Defect),
                Err(Fail::Parse) => {
                    self.i = start;
                    self.cfws()?;
                    if self.at_end() {
                        break;
                    }
                    return Err(Fail::Defect);
                }
            }
            match self.peek() {
                None => {}
                Some(b';') => self.copy_to(self.i + 1),
                Some(_) => return Err(Fail::Defect),
            }
        }
        self.out.truncate(mark);
        let rendered: Vec<String> = params
            .iter()
            .map(|(n, v)| if v.is_empty() { n.clone() } else { format!("{n}=\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")) })
            .collect();
        if !rendered.is_empty() {
            self.out.push(' ');
            self.out.push_str(&rendered.join("; "));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Kind {
    ContentType,
    Disposition,
    TransferEncoding,
}

fn validate(kind: Kind, value: &[u8]) -> std::result::Result<String, ()> {
    if value.is_empty() || !value.is_ascii() {
        return Err(());
    }
    let mut c = Cursor { s: value, i: 0, out: String::with_capacity(value.len() + 8) };
    let ok = (|| -> std::result::Result<(), Fail> {
        match kind {
            Kind::ContentType => {
                c.token()?;
                if c.peek() != Some(b'/') {
                    return Err(Fail::Defect);
                }
                c.copy_to(c.i + 1);
                c.token()?;
                if c.at_end() {
                    return Ok(());
                }
                if c.peek() != Some(b';') {
                    return Err(Fail::Defect);
                }
                c.copy_to(c.i + 1);
                c.parameters()
            }
            Kind::Disposition => {
                c.token()?;
                if c.at_end() {
                    return Ok(());
                }
                if c.peek() != Some(b';') {
                    return Err(Fail::Defect);
                }
                c.copy_to(c.i + 1);
                c.parameters()
            }
            Kind::TransferEncoding => {
                c.token()?;
                if c.at_end() {
                    Ok(())
                } else {
                    Err(Fail::Defect)
                }
            }
        }
    })();
    match ok {
        Ok(()) => Ok(c.out),
        Err(_) => Err(()),
    }
}

/// `tools.notes_payload._headers(raw)`.
pub fn parse_headers(raw: &[u8], limits: &Limits) -> Result<Headers> {
    if raw.len() > limits.max_header_bytes || raw.contains(&0) {
        return Err(Error::Payload("MIME headers exceed limits or are malformed"));
    }
    // Split like email.feedparser (NLCRE: \r\n | \r | \n), on raw + CRLFCRLF.
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut ended = false;
    while i < raw.len() {
        let b = raw[i];
        if b == b'\r' || b == b'\n' {
            let line = &raw[start..i];
            let eol = if b == b'\r' && raw.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
            if line.is_empty() {
                ended = true; // blank line ends the header block
                break;
            }
            lines.push(line);
            i += eol;
            start = i;
        } else {
            i += 1;
        }
    }
    if !ended && start < raw.len() {
        lines.push(&raw[start..]);
    }
    let malformed = Error::Payload("malformed MIME headers");
    // headerRE = ^(From |[\041-\071\073-\176]*:|[\t ])
    let mut fields: Vec<(&[u8], Vec<u8>)> = Vec::new();
    let mut current: Option<(&[u8], Vec<u8>)> = None;
    // lines matching headerRE, as collected by the feedparser
    let header_lines = lines
        .iter()
        .position(|l| !(l.starts_with(b"From ") || is_wsp(l[0]) || l.iter().position(|&b| !(0x21..=0x7e).contains(&b) || b == b':').is_some_and(|p| l[p] == b':')))
        .unwrap_or(lines.len());
    if header_lines < lines.len() {
        return Err(malformed); // MissingHeaderBodySeparatorDefect
    }
    for (lineno, line) in lines.iter().enumerate() {
        let first = line[0];
        if is_wsp(first) {
            match current.as_mut() {
                None => return Err(malformed), // FirstHeaderLineIsContinuationDefect
                Some((_, value)) => value.extend_from_slice(line),
            }
            continue;
        }
        if let Some(done) = current.take() {
            fields.push(done);
        }
        if line.starts_with(b"From ") {
            if lineno == 0 {
                continue; // unix-from envelope line: ignored
            }
            if lineno == header_lines - 1 {
                break; // pushed back as body
            }
            return Err(malformed); // MisplacedEnvelopeHeaderDefect
        }
        let colon = line.iter().position(|&b| !(0x21..=0x7e).contains(&b) || b == b':');
        match colon {
            Some(p) if line[p] == b':' => {
                if p == 0 {
                    return Err(malformed); // Missing header name.
                }
                current = Some((&line[..p], line[p + 1..].to_vec()));
            }
            _ => return Err(malformed), // MissingHeaderBodySeparatorDefect
        }
    }
    if let Some(done) = current.take() {
        fields.push(done);
    }
    let mut headers = Headers::default();
    for (key, kind) in [
        ("content-type", Kind::ContentType),
        ("content-disposition", Kind::Disposition),
        ("content-transfer-encoding", Kind::TransferEncoding),
    ] {
        let mut found = fields.iter().filter(|(n, _)| n.eq_ignore_ascii_case(key.as_bytes()));
        let first = found.next();
        if found.next().is_some() {
            return Err(Error::Payload("duplicate MIME header"));
        }
        if let Some((_, value)) = first {
            // header_source_parse + header_fetch_parse: lstrip, drop CR/LF
            let mut v: &[u8] = value;
            while let Some((&b, rest)) = v.split_first() {
                if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                    v = rest;
                } else {
                    break;
                }
            }
            let cleaned: Vec<u8> = v.iter().copied().filter(|&b| b != b'\r' && b != b'\n').collect();
            let decoded = validate(kind, &cleaned).map_err(|_| Error::Payload("malformed MIME header value"))?;
            match kind {
                Kind::ContentType => headers.content_type = Some(decoded),
                Kind::Disposition => headers.disposition = Some(decoded),
                Kind::TransferEncoding => headers.transfer_encoding = Some(decoded),
            }
        }
    }
    Ok(headers)
}

// ------------------------------------------------- email.message accessors

fn parseparam(s: &str) -> Vec<String> {
    // email.message._parseparam
    let mut s = format!(";{s}");
    let mut plist = Vec::new();
    while s.starts_with(';') {
        s.remove(0);
        let bytes = s.as_bytes();
        let count = |end: usize, pat: &str| s[..end].matches(pat).count();
        let mut end = s.find(';').map(|e| e as isize).unwrap_or(-1);
        while end > 0 && (count(end as usize, "\"") as isize - count(end as usize, "\\\"") as isize) % 2 != 0 {
            end = match s[end as usize + 1..].find(';') {
                Some(p) => (end as usize + 1 + p) as isize,
                None => -1,
            };
        }
        let end = if end < 0 { bytes.len() } else { end as usize };
        let mut f = s[..end].to_string();
        if let Some(i) = f.find('=') {
            f = format!("{}={}", py_strip(&f[..i]).to_lowercase(), py_strip(&f[i + 1..]));
        }
        plist.push(py_strip(&f).to_string());
        s = s[end..].to_string();
    }
    plist
}

/// email.utils.unquote
pub fn utils_unquote(s: &str) -> String {
    if s.chars().count() > 1 {
        if s.starts_with('"') && s.ends_with('"') {
            return s[1..s.len() - 1].replace("\\\\", "\\").replace("\\\"", "\"");
        }
        if s.starts_with('<') && s.ends_with('>') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

fn utils_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Message._get_params_preserve + utils.decode_params (no RFC 2231 names).
fn params(value: &str) -> std::result::Result<Vec<(String, String)>, ()> {
    let mut out = Vec::new();
    for (i, p) in parseparam(value).into_iter().enumerate() {
        let (name, val) = match p.split_once('=') {
            Some((n, v)) => (py_strip(n).to_string(), py_strip(v).to_string()),
            None => (py_strip(&p).to_string(), String::new()),
        };
        if i == 0 {
            out.push((name, val));
        } else {
            if name.contains('*') {
                return Err(()); // RFC 2231 continuation: not modelled
            }
            out.push((name, format!("\"{}\"", utils_quote(&utils_unquote(&val)))));
        }
    }
    Ok(out)
}

fn get_param(value: Option<&str>, param: &str) -> std::result::Result<Option<String>, ()> {
    let Some(value) = value else { return Ok(None) };
    for (k, v) in params(value)? {
        if k.to_lowercase() == param.to_lowercase() {
            return Ok(Some(utils_unquote(&v)));
        }
    }
    Ok(None)
}

impl Headers {
    /// `Message.get_content_type()` (without .lower(): callers lower it).
    pub fn content_type(&self) -> String {
        match &self.content_type {
            None => "text/plain".to_string(),
            Some(v) => {
                let main = py_strip(v.split(';').next().unwrap_or("")).to_lowercase();
                if main.matches('/').count() != 1 {
                    "text/plain".to_string()
                } else {
                    main
                }
            }
        }
    }
    pub fn boundary(&self) -> Result<Option<String>> {
        let v = get_param(self.content_type.as_deref(), "boundary").map_err(|_| Error::Payload("invalid MIME boundary"))?;
        Ok(v.map(|b| py_rstrip(&utils_unquote(&b)).to_string()))
    }
    pub fn param(&self, header: &str, name: &str) -> std::result::Result<Option<String>, ()> {
        let value = if header.eq_ignore_ascii_case("content-disposition") {
            self.disposition.as_deref()
        } else if header.eq_ignore_ascii_case("content-type") {
            self.content_type.as_deref()
        } else {
            None
        };
        get_param(value, name)
    }
    pub fn filename(&self) -> std::result::Result<Option<String>, ()> {
        let found = match get_param(self.disposition.as_deref(), "filename")? {
            Some(v) => Some(v),
            None => get_param(self.content_type.as_deref(), "name")?,
        };
        Ok(found.map(|v| py_strip(&utils_unquote(&v)).to_string()))
    }
    /// `str(headers.get('Content-Transfer-Encoding', 'binary')).lower().strip()`
    pub fn transfer_encoding(&self) -> String {
        let v = self.transfer_encoding.as_deref().unwrap_or("binary");
        py_strip(&v.to_lowercase()).to_string()
    }
}

// ---------------------------------------------------------------- parts

/// `_SAMSUNG_PART_HEADERS.fullmatch(raw)`.
pub fn samsung_fast_match(raw: &[u8]) -> Option<FastMatch> {
    fn ident(raw: &[u8], i: &mut usize, max: usize) -> Option<String> {
        let start = *i;
        while *i < raw.len() && (raw[*i].is_ascii_alphanumeric() || matches!(raw[*i], b'_' | b'.' | b'-')) {
            *i += 1;
        }
        let n = *i - start;
        if n == 0 || n > max {
            return None;
        }
        Some(String::from_utf8(raw[start..*i].to_vec()).unwrap())
    }
    fn lit(raw: &[u8], i: &mut usize, s: &[u8]) -> bool {
        if raw[*i..].starts_with(s) {
            *i += s.len();
            true
        } else {
            false
        }
    }
    let mut i = 0usize;
    if !lit(raw, &mut i, b"Content-Disposition: form-data; name=\"") {
        return None;
    }
    let name = ident(raw, &mut i, 128)?;
    if !lit(raw, &mut i, b"\"") {
        return None;
    }
    let mut filename = None;
    if raw[i..].starts_with(b"; filename=\"") {
        let save = i;
        i += 12;
        let ok = ident(raw, &mut i, 256).and_then(|f| if lit(raw, &mut i, b"\"") { Some(f) } else { None });
        match ok {
            Some(f) => filename = Some(f),
            None => i = save,
        }
    }
    if !lit(raw, &mut i, b"\r\nContent-Type: application/") {
        return None;
    }
    let content_type = if lit(raw, &mut i, b"octet-stream") {
        "application/octet-stream"
    } else if lit(raw, &mut i, b"xml") {
        "application/xml"
    } else {
        return None;
    };
    if !lit(raw, &mut i, b"\r\nContent-Length: ") {
        return None;
    }
    let digits = raw[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 10 || i + digits != raw.len() {
        return None;
    }
    Some(FastMatch { name, filename, content_type: content_type.to_string() })
}

/// `_field(value)`: bounded metadata without control characters.
pub fn field_ok(value: &str, limits: &Limits) -> bool {
    value.len() <= limits.max_field_bytes && !value.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
}

/// `_content_type(value)`.
pub fn content_type_headers<H: Hooks>(value: &str, limits: &Limits, hooks: &mut H) -> Result<Headers> {
    if value.chars().count() > 2048 || value.chars().any(|c| (c as u32) < 32 || (c as u32) > 126) {
        return Err(Error::Payload("invalid content type"));
    }
    let raw = format!("Content-Type: {value}");
    let parsed = hooks.headers(raw.as_bytes(), limits)?;
    if parsed.content_type.as_deref().map_or(true, str::is_empty) {
        return Err(Error::Payload("missing content type"));
    }
    Ok(parsed)
}

fn boundary_ok(b: &str) -> bool {
    !b.is_empty()
        && b.chars().count() <= 70
        && b.bytes().all(|c| c.is_ascii_alphanumeric() || b"'()+_,./:=? -".contains(&c))
        && !b.ends_with(' ')
}

struct Marker {
    start: usize,
    end: usize,
    close: bool,
}

/// finditer of `(?:\A|\r?\n)--BOUNDARY(?P<close>--)?[ \t]*(?:\r?\n|\Z)`
fn markers(body: &[u8], boundary: &str, max: usize) -> Result<Vec<Marker>> {
    let pattern = format!(r"(?-u)(?:\A|\r?\n)--{}(?:--)?[ \t]*(?:\r?\n|\z)", regex::escape(boundary));
    let re = regex::bytes::Regex::new(&pattern).map_err(|_| Error::Payload("invalid MIME boundary"))?;
    let dash_len = boundary.len() + 2;
    let mut out: Vec<Marker> = Vec::new();
    for m in re.find_iter(body) {
        let text = m.as_bytes();
        let lead = if text.starts_with(b"\r\n") { 2 } else if text.starts_with(b"\n") { 1 } else { 0 };
        let close = text[lead + dash_len..].starts_with(b"--");
        out.push(Marker { start: m.start(), end: m.end(), close });
        if out.len() > max {
            return Err(Error::Payload("too many MIME parts"));
        }
    }
    Ok(out)
}

fn header_split(segment: &[u8], window: usize) -> Option<(usize, usize)> {
    let seg = &segment[..segment.len().min(window)];
    let mut p = 0usize;
    while p < seg.len() {
        let q = if seg[p] == b'\r' && seg.get(p + 1) == Some(&b'\n') {
            p + 2
        } else if seg[p] == b'\n' {
            p + 1
        } else {
            p += 1;
            continue;
        };
        if seg.get(q) == Some(&b'\r') && seg.get(q + 1) == Some(&b'\n') {
            return Some((p, q + 2));
        }
        if seg.get(q) == Some(&b'\n') {
            return Some((p, q + 1));
        }
        p += 1;
    }
    None
}

/// `tools.notes_payload._parts(body, content_type, max_parts=...)`;
/// `max_parts` must already be validated (1..=MAX_READONLY_PARTS).
pub fn parts<H: Hooks>(body: &[u8], content_type: &str, max_parts: i64, limits: &Limits, hooks: &mut H) -> Result<Vec<Part>> {
    if !(1..=limits.max_readonly_parts).contains(&max_parts) {
        return Err(Error::Payload("invalid MIME part bound"));
    }
    let top = content_type_headers(content_type, limits, hooks)?;
    let kind = top.content_type().to_lowercase();
    if XML_TYPES.contains(&kind.as_str()) {
        return Ok(vec![Part { index: 0, name: None, filename: None, content_type: kind, body: 0..body.len() }]);
    }
    if kind != "multipart/form-data" && kind != "multipart/mixed" {
        return Err(Error::Payload("unsupported payload content type"));
    }
    let boundary = top.boundary()?;
    let boundary = match boundary {
        Some(b) if boundary_ok(&b) => b,
        _ => return Err(Error::Payload("invalid MIME boundary")),
    };
    let found = markers(body, &boundary, max_parts as usize + 1)?;
    if found.len() < 2 || found[0].close || !found[found.len() - 1].close {
        return Err(Error::Payload("incomplete multipart body"));
    }
    if found[..found.len() - 1].iter().any(|m| m.close) {
        return Err(Error::Payload("unexpected closing MIME boundary"));
    }
    let mut out: Vec<Part> = Vec::with_capacity(found.len() - 1);
    for pair in found.windows(2) {
        let (seg_start, seg_end) = (pair[0].end, pair[1].start.max(pair[0].end));
        let segment = &body[seg_start..seg_end];
        let Some((split_start, split_end)) = header_split(segment, limits.max_header_bytes + 4) else {
            return Err(Error::Payload("missing or oversized MIME headers"));
        };
        let header_bytes = &segment[..split_start];
        let range = seg_start + split_end..seg_end;
        if let Some(fast) = hooks.fast_match(header_bytes)? {
            out.push(Part { index: out.len(), name: Some(fast.name), filename: fast.filename, content_type: fast.content_type, body: range });
            continue;
        }
        let headers = hooks.headers(header_bytes, limits)?;
        let encoding = headers.transfer_encoding();
        if !matches!(encoding.as_str(), "binary" | "8bit" | "7bit") {
            return Err(Error::Payload("unsupported MIME transfer encoding"));
        }
        let kind = headers.content_type().to_lowercase();
        if kind.starts_with("multipart/") {
            return Err(Error::Payload("nested multipart is unsupported"));
        }
        let disposition = Error::Payload("invalid MIME disposition");
        let name = headers.param("content-disposition", "name").map_err(|_| Error::Payload("invalid MIME disposition"))?;
        if let Some(n) = &name {
            if !field_ok(n, limits) {
                return Err(disposition);
            }
        }
        let filename = headers.filename().map_err(|_| Error::Payload("invalid MIME disposition"))?;
        if let Some(f) = &filename {
            if !field_ok(f, limits) {
                return Err(disposition);
            }
        }
        out.push(Part { index: out.len(), name, filename, content_type: kind, body: range });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_and_parts() {
        let body = b"--b\r\nContent-Type: application/xml\r\n\r\n<x/>\r\n--b--\r\n";
        let p = parts(body, "multipart/form-data; boundary=b", 10, &Limits::default(), &mut NoHooks).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(&body[p[0].body.clone()], b"<x/>");
        assert_eq!(p[0].content_type, "application/xml");
    }

    #[test]
    fn quoted_params() {
        let h = parse_headers(b"Content-Disposition: form-data; name=\"a\\b\"; filename=\" <x> \"", &Limits::default()).unwrap();
        assert_eq!(h.param("content-disposition", "name").unwrap().as_deref(), Some("ab"));
        assert_eq!(h.filename().unwrap().as_deref(), Some("<x>"));
    }
}
