//! Shared full-snapshot stroke decoding and XML metadata binding.
//! Renderer admission is deliberately separate from this structural check.
use super::binary::{decode_stroke_resource, i32_at, py_slice, Record};
use super::prepare::{bind_stroke_metadata, Binding};
use super::xml::Tree;
use std::collections::{HashMap, HashSet};

/// Bump when these full-snapshot validation semantics change.
pub const VERSION: &str = "render-stroke-bind/1";
pub struct BoundRecord {
    pub record: Record,
    pub binding: Binding,
    pub rotation: i32,
}

/// Identical full-note decoding/binding used by the preview renderer. The
/// caller owns pen admission and rendering decisions.
pub fn decode_records(
    resource: &[u8],
    bodies: &[Vec<u8>],
) -> Result<HashMap<String, BoundRecord>, String> {
    let mut records = HashMap::new();
    visit_records(resource, bodies, |id, record| { records.insert(id, record); })?;
    Ok(records)
}

/// Run the same decoding and binding checks without retaining decoded strokes.
pub fn validate_records<T: AsRef<[u8]>>(resource: &[u8], bodies: &[T]) -> Result<(), String> {
    visit_records(resource, bodies, |_, _| {})
}

fn visit_records<T: AsRef<[u8]>>(resource: &[u8], bodies: &[T], mut visit: impl FnMut(String, BoundRecord)) -> Result<(), String> {
    let tree = Tree::parse(resource).map_err(|e| format!("{e:?}"))?;
    let root = tree.root;
    let child = |name: &str| {
        tree.children(root)
            .iter()
            .copied()
            .find(|c| tree.local(*c).to_lowercase() == name)
    };
    let mut objects = HashMap::new();
    if let Some(list) = child("pagelist") {
        for page in tree.children(list) {
            for node in tree.iter(*page) {
                if tree.tag(node) == "object" {
                    if let Some(id) = tree.get(node, "id") {
                        objects.insert(id.to_string(), node);
                    }
                }
            }
        }
    }
    let table = child("attributes")
        .and_then(|a| tree.find(a, "stringIdList"))
        .map(|n| (&tree, n));
    let mut identities = HashSet::new();
    for body in bodies {
        let record = decode_stroke_resource(body.as_ref()).map_err(|e| format!("{e:?}"))?;
        let base = &record.frames[0];
        let start = base
            .fixed_start
            .checked_add(6)
            .ok_or("invalid frame offset")?;
        let end = base
            .fixed_start
            .checked_add(42)
            .ok_or("invalid frame offset")?;
        let identity = py_slice(&base.raw, start, Some(end));
        let identity = match std::str::from_utf8(identity) {
            Ok(s) if s.is_ascii() => s.to_string(),
            _ => return Err("'ascii' codec can't decode bytes".into()),
        };
        let Some(&node) = objects.get(&identity) else {
            return Err("Çizgi kaynağı notla eşleşmiyor.".into());
        };
        if !identities.insert(identity.clone()) {
            return Err("Çizgi kaynağı notla eşleşmiyor.".into());
        }
        let binding = bind_stroke_metadata(&record.frames, (&tree, node), table)
            .map_err(|e| format!("{e:?}"))?;
        let rotation_offset = base
            .flexible_start
            .checked_sub(5)
            .ok_or("invalid frame offset")?;
        let rotation = i32_at(&base.raw, rotation_offset).map_err(|e| format!("{e:?}"))?;
        visit(
            identity,
            BoundRecord {
                record,
                binding,
                rotation,
            },
        );
    }
    Ok(())
}
