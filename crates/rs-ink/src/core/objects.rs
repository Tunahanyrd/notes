//! Non-stroke objects the ink writers carry through untouched.
//!
//! The admission rule itself lives in `rs_doc::objects` (shared with rs-ops
//! and the text editor gate): self-contained images, shapes, lines and text
//! boxes on page layers, and the same object types in the note-level
//! `floatingObjectList` (a picture inserted on a phone) when their
//! `pageIndex` names a page and their note-tree entries mirror them. An ink
//! edit (append, first stroke, highlighter, erase, move) only
//! appends/removes/rewrites *stroke* objects of one layer, recomputes that
//! layer's and page's hashes from the children's stored hashes and may
//! release the target page's canvas cache, so every carried object stays
//! byte-identical. This module adapts the rule to rs-ink's [`super::xml::Tree`].
use super::{ink, R};

pub use rs_doc::objects::{floating_object, passthrough_object, PASSTHROUGH_TYPES};

/// [`passthrough_object`] over an rs-ink [`super::xml::Tree`] node.
pub fn tree_passthrough(t: &super::xml::Tree, node: super::xml::NodeId) -> Option<Option<String>> {
    if t.tag(node) != "object" {
        return None;
    }
    let all = t.iter(node);
    passthrough_object(&t.nodes[node].attrs, all[1..].iter().map(|n| (t.tag(*n), t.nodes[*n].attrs.as_slice())))
}

/// The note's free-floating objects (`floatingObjectList` children) when
/// every one of them can be carried through: [`floating_object`] for each and
/// the note-tree mirror (`noteTree/floatingObjectList`) entry for entry.
/// Returns the objects (empty when the list is absent or empty).
pub fn tree_floating(t: &super::xml::Tree, root: super::xml::NodeId) -> R<Vec<super::xml::NodeId>> {
    let lists: Vec<_> = t.children(root).iter().copied().filter(|n| t.tag(*n) == "floatingObjectList").collect();
    let tree = t.one(root, "noteTree")?;
    let tree_lists: Vec<_> = t.children(tree).iter().copied().filter(|n| t.tag(*n) == "floatingObjectList").collect();
    if lists.len() > 1 || tree_lists.len() > 1 {
        return ink("duplicate floating object list");
    }
    let objects: Vec<_> = lists.first().map(|l| t.children(*l).to_vec()).unwrap_or_default();
    let entries: Vec<_> = tree_lists.first().map(|l| t.children(*l).to_vec()).unwrap_or_default();
    let page_count = t.len(t.one(root, "pageList")?);
    let ok = rs_doc::objects::floating_inventory(
        objects.iter().map(|o| {
            let all = t.iter(*o);
            let descendants: Vec<(&str, &[(String, String)])> =
                all[1..].iter().map(|n| (t.tag(*n), t.nodes[*n].attrs.as_slice())).collect();
            (t.tag(*o), t.nodes[*o].attrs.as_slice(), descendants)
        }),
        entries.iter().map(|e| (t.tag(*e), t.nodes[*e].attrs.as_slice(), t.len(*e))),
        page_count,
    );
    match ok {
        Some(_) => Ok(objects),
        None => ink("floating objects cannot be carried"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sha256_hex;

    fn a(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const ID: &str = "0b5d3c3e-1f0a-4c55-9d43-0a1b2c3d4e5f";

    fn base(kind: &str) -> Vec<(String, String)> {
        let hash = sha256_hex(format!("{ID}1700000000000").as_bytes());
        a(&[("type", kind), ("id", ID), ("modifiedTime", "1700000000000"), ("hash", &hash)])
    }

    #[test]
    fn admits_self_contained_objects() {
        let fill = a(&[("type", "image"), ("imageId", &"a".repeat(64))]);
        assert_eq!(passthrough_object(&base("image"), [("fillEffect", fill.as_slice())]), Some(Some("a".repeat(64))));
        for kind in ["shape", "line", "textBox"] {
            assert_eq!(passthrough_object(&base(kind), [("rect", &[][..])]), Some(None), "{kind}");
        }
    }

    #[test]
    fn refuses_nesting_cache_references_and_unknown_types() {
        let fill = a(&[("type", "image"), ("imageId", &"a".repeat(64))]);
        let nested: Vec<(String, String)> = Vec::new();
        assert_eq!(passthrough_object(&base("image"), [("fillEffect", fill.as_slice()), ("object", nested.as_slice())]), None);
        assert_eq!(passthrough_object(&base("image"), []), None, "image without fill");
        assert_eq!(passthrough_object(&base("shape"), [("fillEffect", fill.as_slice())]), None, "image-filled shape");
        let cache = a(&[("fileId", "3")]);
        assert_eq!(passthrough_object(&base("textBox"), [("pair", cache.as_slice())]), None);
        assert_eq!(passthrough_object(&base("stroke"), []), None);
        assert_eq!(passthrough_object(&base("customObject"), []), None);
        let mut wrong = base("shape");
        wrong[3].1 = "0".repeat(64);
        assert_eq!(passthrough_object(&wrong, []), None, "hash must be SHA256(id + modifiedTime)");
    }
}
