//! `http.client.parse_headers`: the header block decoded as iso-8859-1 and
//! fed through `email.feedparser` with the compat32 policy, then read with
//! `email.message.Message.get` / `get_content_type`.
use crate::py;

/// `http.client.HTTPMessage` (only the lookups the callers use).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    headers: Vec<(String, String)>,
}

/// `headerRE = re.compile(r'^(From |[\041-\071\073-\176]*:|[\t ])')`
fn header_re(line: &str) -> bool {
    if line.starts_with("From ") || line.starts_with('\t') || line.starts_with(' ') {
        return true;
    }
    for c in line.chars() {
        if c == ':' {
            return true;
        }
        if !matches!(c, '\u{21}'..='\u{39}' | '\u{3b}'..='\u{7e}') {
            return false;
        }
    }
    false
}

/// Universal-newline split keeping the separators (`StringIO(newline='').readlines()`).
fn split_lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..=i]);
                start = i + 1;
            }
            b'\r' => {
                let end = if bytes.get(i + 1) == Some(&b'\n') { i + 1 } else { i };
                lines.push(&text[start..=end]);
                i = end;
                start = end + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `Compat32.header_source_parse`
fn source_parse(lines: &[&str]) -> (String, String) {
    let first = lines[0];
    let i = first.find(':').unwrap_or(first.len());
    let name = first[..i].to_string();
    let mut value = first.get(i + 1..).unwrap_or("").trim_start_matches([' ', '\t']).to_string();
    for more in &lines[1..] {
        value.push_str(more);
    }
    let value = value.trim_end_matches(['\r', '\n']).to_string();
    (name, value)
}

impl Message {
    /// Parse the raw header lines http.client read (including the final
    /// blank line when present).
    pub(crate) fn parse(raw_lines: &[Vec<u8>]) -> Message {
        let joined: Vec<u8> = raw_lines.concat();
        let text = py::latin1(&joined);
        let lines = split_lines(&text);
        // FeedParser._parsegen: collect header lines until one does not match.
        let mut header_lines: Vec<&str> = Vec::new();
        for line in &lines {
            if !header_re(line) {
                break; // the blank separator, or the first body line (defect)
            }
            header_lines.push(line);
        }
        // FeedParser._parse_headers
        let mut headers = Vec::new();
        let mut last: Vec<&str> = Vec::new();
        let count = header_lines.len();
        for (lineno, line) in header_lines.iter().enumerate() {
            if line.starts_with(' ') || line.starts_with('\t') {
                if last.is_empty() {
                    continue; // FirstHeaderLineIsContinuationDefect
                }
                last.push(line);
                continue;
            }
            if !last.is_empty() {
                headers.push(source_parse(&last));
                last.clear();
            }
            if line.starts_with("From ") {
                if lineno == 0 {
                    continue; // unix-from envelope
                } else if lineno == count - 1 {
                    break; // pushed back as the first body line
                } else {
                    continue; // MisplacedEnvelopeHeaderDefect
                }
            }
            match line.find(':') {
                Some(0) | None => continue, // "Missing header name."
                Some(_) => last.push(line),
            }
        }
        if !last.is_empty() {
            headers.push(source_parse(&last));
        }
        Message { headers }
    }

    /// `Message.get(name)`: the first header with that name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&str> {
        let wanted = py::lower(name);
        self.headers.iter().find(|(k, _)| py::lower(k) == wanted).map(|(_, v)| v.as_str())
    }

    /// `name in message`
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// `Message.get_content_type()`
    pub fn content_type(&self) -> String {
        let Some(value) = self.get("content-type") else {
            return "text/plain".to_string();
        };
        let main = match value.find(';') {
            Some(i) => &value[..i],
            None => value,
        };
        let ctype = py::lower(py::strip(main));
        if ctype.matches('/').count() != 1 {
            return "text/plain".to_string();
        }
        ctype
    }

    /// All `(name, value)` pairs in order.
    pub fn items(&self) -> &[(String, String)] {
        &self.headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[u8]) -> Message {
        let lines: Vec<Vec<u8>> = s.split_inclusive(|&b| b == b'\n').map(|l| l.to_vec()).collect();
        Message::parse(&lines)
    }

    #[test]
    fn email_parser_semantics() {
        let m = parse(b"Content-Type:   Application/JSON ; charset=x  \r\nX: a\r\n b\r\nLocation: /n\r\nlocation: /second\r\n\r\n");
        assert_eq!(m.get("content-type"), Some("Application/JSON ; charset=x  "));
        assert_eq!(m.content_type(), "application/json");
        assert_eq!(m.get("x"), Some("a\r\n b"));
        assert_eq!(m.get("LOCATION"), Some("/n"));
        let m = parse(b"Bad Header: 1\r\nContent-Type: text/html\r\n\r\n");
        assert_eq!(m.get("content-type"), None);
        assert_eq!(m.content_type(), "text/plain");
        let m = parse(b"Content-Type: a/b/c\r\n\r\n");
        assert_eq!(m.content_type(), "text/plain");
        let m = parse(b"Content-Type: x\rY: z\r\n\r\n");
        assert_eq!(m.get("content-type"), Some("x"));
        assert_eq!(m.get("y"), Some("z"));
        let m = parse(b"Content-Type: \xe9/x\r\n\r\n");
        assert_eq!(m.content_type(), "\u{e9}/x");
    }
}
