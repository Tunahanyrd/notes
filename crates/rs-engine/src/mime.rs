//! `mimetypes.guess_type(name)[0]` as CPython 3.14 computes it: built-in
//! defaults overridden by the system `mime.types` files. Pure Rust.
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::pytables::{MIME_DEFAULTS, MIME_ENCODINGS_MAP, MIME_KNOWN_FILES, MIME_SUFFIX_MAP};

fn types_map() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map: HashMap<String, String> = MIME_DEFAULTS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        for file in MIME_KNOWN_FILES {
            let Ok(raw) = std::fs::read(file) else { continue };
            if !std::path::Path::new(file).is_file() {
                continue;
            }
            // Python opens these with encoding='utf-8'; undecodable files abort init there.
            let Ok(text) = String::from_utf8(raw) else { continue };
            for line in text.lines() {
                let mut words: Vec<&str> = line.split_whitespace().collect();
                if let Some(pos) = words.iter().position(|w| w.starts_with('#')) {
                    words.truncate(pos);
                }
                if words.is_empty() {
                    continue;
                }
                for suffix in &words[1..] {
                    map.insert(format!(".{}", suffix), words[0].to_string());
                }
            }
        }
        map
    })
}

/// posixpath.splitext
pub fn splitext(p: &str) -> (&str, &str) {
    let sep = p.rfind('/').map(|i| i as isize).unwrap_or(-1);
    if let Some(dot) = p.rfind('.') {
        if dot as isize > sep {
            let start = (sep + 1) as usize;
            if p[start..dot].chars().any(|c| c != '.') {
                return (&p[..dot], &p[dot..]);
            }
        }
    }
    (p, "")
}

/// `mimetypes.guess_type(name)[0]` for a plain file name.
pub fn guess_type(name: &str) -> Option<String> {
    let (base, ext) = splitext(name);
    let mut base = base.to_string();
    let mut ext = ext.to_string();
    loop {
        let lower = ext.to_lowercase();
        let Some((_, mapped)) = MIME_SUFFIX_MAP.iter().find(|(k, _)| *k == lower) else { break };
        let joined = format!("{}{}", base, mapped);
        let (b, e) = splitext(&joined);
        base = b.to_string();
        ext = e.to_string();
    }
    if MIME_ENCODINGS_MAP.iter().any(|(k, _)| *k == ext) {
        let (b, e) = splitext(&base);
        let (b, e) = (b.to_string(), e.to_string());
        base = b;
        ext = e;
    }
    let _ = base;
    types_map().get(&ext.to_lowercase()).cloned()
}
