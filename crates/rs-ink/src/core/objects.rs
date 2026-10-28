//! Non-stroke page objects the ink writers carry through untouched.
//!
//! Samsung stores images (`type="image"`), shapes (`shape`), lines (`line`)
//! and text boxes (`textBox`) inline in a layer's `objectList`; none of them
//! has a binary MIME part. An ink edit (append, first stroke, highlighter,
//! erase, move) only appends/removes/rewrites *stroke* objects of one layer,
//! recomputes that layer's and page's hashes from the children's stored
//! hashes, and may release the target page's canvas cache. Every other object
//! therefore stays byte-identical as long as it is self-contained: it must not
//! nest further objects, must not reference a canvas cache (`fileId`), and an
//! image must reference exactly one content file through its fill. This
//! module is the single admission rule; rs-ink's append (over its `Tree`) and
//! rs-ops' writers (over their `Element`) both adapt to it.
//!
//! Observed (structure only) in a private library copy: 45 page-layer images,
//! each `fillEffect type="image" imageId=<content file id>`, the content file
//! `refCount` equal to its image uses, object hash `SHA256(id + modifiedTime)`
//! for every object type. Text boxes on page layers were not observed; the
//! rule is the same and synthetic tests cover it.
use super::{doc_integer, is_canonical_uuid, sha256_hex};

/// Object types an ink edit may carry through unchanged.
pub const PASSTHROUGH_TYPES: [&str; 4] = ["image", "shape", "line", "textBox"];

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `Some(image_file)` when an object with these attributes and descendants
/// (pre-order, the object itself excluded; `(tag, attributes)`) is a
/// self-contained non-stroke object an ink edit may carry through. For an
/// image `image_file` is the content-file id its fill references; `None`
/// otherwise. `None` (outer) refuses the object.
pub fn passthrough_object<'a>(
    attrs: &[(String, String)],
    descendants: impl IntoIterator<Item = (&'a str, &'a [(String, String)])>,
) -> Option<Option<String>> {
    let kind = attr(attrs, "type")?;
    if !PASSTHROUGH_TYPES.contains(&kind) {
        return None;
    }
    let id = attr(attrs, "id").filter(|i| is_canonical_uuid(i))?;
    let modified = doc_integer(attr(attrs, "modifiedTime")).ok().filter(|m| *m >= 1)?;
    if attr(attrs, "hash") != Some(sha256_hex(format!("{id}{modified}").as_bytes()).as_str()) {
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

/// [`passthrough_object`] over an rs-ink [`super::xml::Tree`] node.
pub fn tree_passthrough(t: &super::xml::Tree, node: super::xml::NodeId) -> Option<Option<String>> {
    if t.tag(node) != "object" {
        return None;
    }
    let all = t.iter(node);
    passthrough_object(&t.nodes[node].attrs, all[1..].iter().map(|n| (t.tag(*n), t.nodes[*n].attrs.as_slice())))
}

#[cfg(test)]
mod tests {
    use super::*;

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
