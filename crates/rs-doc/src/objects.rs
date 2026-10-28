//! Non-stroke objects an edit carries through untouched: the single admission
//! rule shared by the ink writers (rs-ink, rs-ops), the ink projection and the
//! text editor gate.
//!
//! Samsung stores images (`type="image"`), shapes (`shape`), lines (`line`)
//! and text boxes (`textBox`) inline: on a page layer's `objectList`, or — a
//! picture inserted on a phone — in the note-level `floatingObjectList`,
//! placed on page `pageIndex`. None of them has a binary MIME part. An ink or
//! text edit never rewrites them; they stay byte-identical as long as they are
//! self-contained: no nested objects, no canvas-cache reference (`fileId`),
//! and a picture names exactly one content file through its fill.
//!
//! Observed (structure only):
//! - page layers of a private library copy: 45 images, each `fillEffect
//!   type="image" imageId=<content file id>`, the content file `refCount`
//!   equal to its image uses, object hash `SHA256(id + modifiedTime)`;
//! - a phone-made test note (Notes 4.4.45, 2026-10-28): one floating image
//!   (`pageIndex`, `layoutType`, `ownerPageWidth/Height`, `rect`,
//!   `connectionPointList`, `lineColorEffect`, `lineStyleEffect`,
//!   `originalRect`, `path/segmentList`, `fillEffect type="image"` with
//!   `stretchOffset`/`tilingOffset`/`ninePatchRect`), its tree entry
//!   `noteTree/floatingObjectList/object h id t`, the same hash rule, and its
//!   content file at `refCount` 1; a page-layer shape and a page-layer text box
//!   (`spanOrderDataList`, `path`, `text`).
use crate::util::{hex, is_canonical_uuid, sha256};

/// Object types an edit may carry through unchanged.
pub const PASSTHROUGH_TYPES: [&str; 4] = ["image", "shape", "line", "textBox"];

/// Most free-floating objects one note may carry (the phone note had one).
pub const MAX_FLOATING_OBJECTS: usize = 4096;

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A non-negative decimal integer that fits in `i64` (`document.integer`).
fn integer(v: Option<&str>) -> Option<i64> {
    let v = v?;
    if v.is_empty() || v.len() > 19 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse::<i64>().ok()
}

/// `Some(image_file)` when an object with these attributes and descendants
/// (pre-order, the object itself excluded; `(tag, attributes)`) is a
/// self-contained non-stroke object an edit may carry through. For an image
/// `image_file` is the content-file id its fill references; `None` otherwise.
/// `None` (outer) refuses the object.
pub fn passthrough_object<'a>(
    attrs: &[(String, String)],
    descendants: impl IntoIterator<Item = (&'a str, &'a [(String, String)])>,
) -> Option<Option<String>> {
    let kind = attr(attrs, "type")?;
    if !PASSTHROUGH_TYPES.contains(&kind) {
        return None;
    }
    let id = attr(attrs, "id").filter(|i| is_canonical_uuid(i))?;
    let modified = integer(attr(attrs, "modifiedTime")).filter(|m| *m >= 1)?;
    if attr(attrs, "hash") != Some(hex(&sha256(format!("{id}{modified}").as_bytes())).as_str()) {
        return None;
    }
    if attrs.iter().any(|(k, _)| k == "fileId" || k == "imageId") {
        return None;
    }
    let mut images: Vec<String> = Vec::new();
    for (tag, a) in descendants {
        if tag == "object" || a.iter().any(|(k, _)| k == "fileId") {
            return None;
        }
        if let Some(image) = attr(a, "imageId") {
            if tag != "fillEffect" || attr(a, "type") != Some("image") || !is_sha256(image) {
                return None;
            }
            images.push(image.to_string());
        }
    }
    match (kind, images.len()) {
        ("image", 1) => Some(images.pop()),
        (_, 0) if kind != "image" => Some(None),
        _ => None,
    }
}

/// A free-floating object (`noteResource/floatingObjectList/object`): the
/// [`passthrough_object`] rule plus a `pageIndex` naming one of the note's
/// `page_count` pages. Nothing else about its placement is interpreted.
pub fn floating_object<'a>(
    attrs: &[(String, String)],
    descendants: impl IntoIterator<Item = (&'a str, &'a [(String, String)])>,
    page_count: usize,
) -> Option<Option<String>> {
    let index = integer(attr(attrs, "pageIndex"))?;
    if index as u64 >= page_count as u64 {
        return None;
    }
    passthrough_object(attrs, descendants)
}

/// The floating objects' `(id, modifiedTime, hash)` against their note-tree
/// entries' `(id, t, h)`: same order, unique canonical ids, leaf entries.
pub fn floating_mirror_ok(objects: &[[Option<&str>; 3]], tree: &[[Option<&str>; 3]], tree_leaves: bool) -> bool {
    let mut ids: Vec<&str> = objects.iter().filter_map(|o| o[0]).collect();
    ids.sort_unstable();
    ids.dedup();
    tree_leaves && objects.len() <= MAX_FLOATING_OBJECTS && objects == tree && ids.len() == objects.len()
}

/// [`floating_object`] for every child of the resource's floating list and
/// the [`floating_mirror_ok`] check against the tree's, over any DOM:
/// `objects`/`tree` are `(tag, attributes, descendants pre-order, child count)`.
/// Returns the image content-file ids (one per picture, in order), or None
/// when the list cannot be carried. Both lists empty/absent: `Some([])`.
pub fn floating_inventory<'a, D>(
    objects: impl IntoIterator<Item = (&'a str, &'a [(String, String)], D)>,
    tree: impl IntoIterator<Item = (&'a str, &'a [(String, String)], usize)>,
    page_count: usize,
) -> Option<Vec<String>>
where
    D: IntoIterator<Item = (&'a str, &'a [(String, String)])>,
{
    let mut keys = Vec::new();
    let mut files = Vec::new();
    for (tag, attrs, descendants) in objects {
        if tag != "object" {
            return None;
        }
        if let Some(file) = floating_object(attrs, descendants, page_count)? {
            files.push(file);
        }
        keys.push([attr(attrs, "id"), attr(attrs, "modifiedTime"), attr(attrs, "hash")]);
    }
    let mut leaves = true;
    let mut entries = Vec::new();
    for (tag, attrs, children) in tree {
        leaves &= tag == "object" && children == 0;
        entries.push([attr(attrs, "id"), attr(attrs, "t"), attr(attrs, "h")]);
    }
    floating_mirror_ok(&keys, &entries, leaves).then_some(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const ID: &str = "0b5d3c3e-1f0a-4c55-9d43-0a1b2c3d4e5f";

    fn base(kind: &str) -> Vec<(String, String)> {
        let hash = hex(&sha256(format!("{ID}1700000000000").as_bytes()));
        a(&[("type", kind), ("id", ID), ("modifiedTime", "1700000000000"), ("hash", &hash)])
    }

    #[test]
    fn floating_object_needs_a_page_index_inside_the_note() {
        let fill = a(&[("type", "image"), ("imageId", &"a".repeat(64))]);
        let mut attrs = base("image");
        assert_eq!(floating_object(&attrs, [("fillEffect", fill.as_slice())], 3), None, "no pageIndex");
        attrs.push(("pageIndex".into(), "2".into()));
        assert_eq!(floating_object(&attrs, [("fillEffect", fill.as_slice())], 3), Some(Some("a".repeat(64))));
        assert_eq!(floating_object(&attrs, [("fillEffect", fill.as_slice())], 2), None, "page out of range");
        attrs.last_mut().unwrap().1 = "-1".into();
        assert_eq!(floating_object(&attrs, [("fillEffect", fill.as_slice())], 3), None);
    }

    #[test]
    fn floating_inventory_checks_the_tree_mirror() {
        let fill = a(&[("type", "image"), ("imageId", &"a".repeat(64))]);
        let mut attrs = base("image");
        attrs.push(("pageIndex".into(), "0".into()));
        let hash = attr(&attrs, "hash").unwrap().to_string();
        let entry = a(&[("id", ID), ("t", "1700000000000"), ("h", &hash)]);
        let objects = || vec![("object", attrs.as_slice(), vec![("fillEffect", fill.as_slice())])];
        assert_eq!(floating_inventory(objects(), vec![("object", entry.as_slice(), 0)], 1), Some(vec!["a".repeat(64)]));
        assert_eq!(floating_inventory(objects(), vec![], 1), None, "tree entry missing");
        assert_eq!(floating_inventory(objects(), vec![("object", entry.as_slice(), 1)], 1), None, "tree entry not a leaf");
        let stale = a(&[("id", ID), ("t", "1"), ("h", &hash)]);
        assert_eq!(floating_inventory(objects(), vec![("object", stale.as_slice(), 0)], 1), None, "stale tree time");
        let empty: Vec<(&str, &[(String, String)], Vec<(&str, &[(String, String)])>)> = Vec::new();
        assert_eq!(floating_inventory(empty, vec![], 1), Some(vec![]));
        let group = vec![("object", attrs.as_slice(), vec![("fillEffect", fill.as_slice()), ("object", &[][..])])];
        assert_eq!(floating_inventory(group, vec![("object", entry.as_slice(), 0)], 1), None, "nested object");
    }
}
