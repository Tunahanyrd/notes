//! Deterministic mutation fuzzing of the Cloud response parsers (metadata
//! XML, JSON, compare/upload guards, change feed and note tree): every input
//! must be Ok or Err, never a panic. Seeds are the synthetic responses
//! recorded in tests/fixtures/oracle plus the notes-protocol fixtures.
#[path = "../../notes-host/tests/support/fuzzkit.rs"]
mod fuzzkit;

use std::io::Read;

use notes_cloud::{guards, json, metadata};

fn collect(value: &serde_json::Value, out: &mut Vec<Vec<u8>>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if let (true, serde_json::Value::String(s)) = (matches!(k.as_str(), "body" | "response" | "raw" | "payload"), v) {
                    if s.len() > 20 {
                        let bytes = fuzzkit::b64_decode(s.as_bytes()).unwrap_or_else(|| s.as_bytes().to_vec());
                        if !out.contains(&bytes) {
                            out.push(bytes);
                        }
                    }
                }
                collect(v, out);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
        _ => {}
    }
}

fn seeds() -> Vec<(Vec<u8>, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut bodies = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(root.join("tests/fixtures/oracle")).unwrap().flatten().map(|e| e.path()).collect();
    names.sort();
    for p in names {
        let mut text = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(&p).unwrap()).read_to_string(&mut text).unwrap();
        collect(&serde_json::from_str(&text).unwrap(), &mut bodies);
    }
    for f in ["changes.xml", "tree.xml", "entrypoint.json"] {
        bodies.push(std::fs::read(root.join("../notes-protocol/tests/fixtures").join(f)).unwrap());
    }
    // Entity expansion must stay bounded wherever a DTD is accepted.
    bodies.push(
        br#"<?xml version="1.0"?><!DOCTYPE V [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;"><!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;"><!ENTITY e "&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;"><!ENTITY f "&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;"><!ENTITY g "&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;"><!ENTITY h "&g;&g;&g;&g;&g;&g;&g;&g;&g;&g;">]><V><created><view><hash>&h;</hash></view></created></V>"#
            .to_vec(),
    );
    bodies.into_iter().map(|b| (b, String::new())).collect()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let a = text.find(open)? + open.len();
    let b = text[a..].find(close)? + a;
    Some(&text[a..b])
}

fn ids(text: &str) -> Vec<Option<String>> {
    text.split("id=\"").skip(1).filter_map(|s| s.split('"').next()).map(|s| Some(s.to_string())).take(8).collect()
}

fn target(raw: &[u8], _: &str) {
    let _ = metadata::parse_folder_changes(raw);
    let _ = metadata::parse_note_list(raw);
    let _ = json::loads(raw);
    let _ = guards::default_view(raw);
    let _ = guards::find_upload_entry(raw, "0123456789abcdef");
    let _ = notes_cloud::etree::parse_bytes(raw, false);
    let text = String::from_utf8_lossy(raw);
    let _ = notes_protocol::parse_note_changes(&text);
    let _ = notes_protocol::tree::parse_note_tree(&text);
    let commit = between(&text, "<commitId>", "</commitId>");
    let ids = ids(&text);
    let id = |i: usize| ids.get(i).cloned().flatten();
    let _ = guards::compare_guard(Some(raw), commit, &ids);
    let _ = guards::compare_ink_guard(Some(raw), commit, id(0).as_deref(), id(1).as_deref(), id(2).as_deref(), &ids[ids.len().min(3)..]);
    let _ = guards::compare_page_guard(Some(raw), commit, &ids);
    let _ = guards::compare_new_pages_guard(Some(raw), commit, &ids, &ids, id(0).as_deref());
    let layers: Vec<(Option<String>, Option<String>)> = ids.chunks(2).map(|c| (c[0].clone(), c.get(1).cloned().flatten())).collect();
    let _ = guards::compare_page_ops_guard(Some(raw), commit, &ids, &layers, &ids);
}

#[test]
fn hostile_cloud_responses_never_panic() {
    let failures = fuzzkit::run("cloud", 0x2545_f491_4f6c_dd1d, 1500, seeds(), target);
    assert!(failures.is_empty(), "panics at: {failures:?}");
}
