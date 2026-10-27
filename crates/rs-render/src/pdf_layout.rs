//! Pure-Rust PDF placement inventory (pdf_background.inspect_pdf_backgrounds /
//! pdf_page_indices) over already validated note XML documents.
use std::collections::{BTreeSet, HashMap, HashSet};

pub const MAX_PAGE_INDEX: i64 = 100_000;
pub const MAX_PLACEMENTS: usize = 16_384;
pub const MAX_DIMENSION: i64 = 32768;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfPlacement {
    pub page_index: i64,
    pub pdf_page_index: i64,
    pub width: i64,
    pub height: i64,
    pub rect: (i64, i64, i64, i64),
    pub annotation_objects: i64,
    pub sha256: String,
    pub download_url: Option<String>,
}

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn tag(node: &Node) -> String {
    node.tag_name().name().to_lowercase()
}

fn children<'a, 'i>(node: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(|n| n.is_element())
}

fn descendants<'a, 'i>(node: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    node.descendants().filter(|n| n.is_element())
}

/// Parse documents that already passed notes_payload._xml.
pub fn parse_all(documents: &[Vec<u8>]) -> Result<Vec<String>, ()> {
    documents.iter().map(|raw| {
        let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
        String::from_utf8(raw.to_vec()).map_err(|_| ())
    }).collect()
}

fn documents(texts: &[String]) -> Result<Vec<roxmltree::Document<'_>>, ()> {
    texts.iter().map(|t| roxmltree::Document::parse(t).map_err(|_| ())).collect()
}

fn integer(value: Option<&str>, maximum: i64, minimum: i64) -> Result<i64, &'static str> {
    let value = match value {
        Some(v) if (1..=10).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit()) => v,
        _ => return Err("PDF yerleşimi geçersiz."),
    };
    let result: i64 = value.parse().map_err(|_| "PDF yerleşimi geçersiz.")?;
    if !(minimum <= result && result <= maximum) {
        return Err("PDF yerleşimi boyut sınırını aşıyor.");
    }
    Ok(result)
}

fn attribute_key(attr: &roxmltree::Attribute) -> String {
    match attr.namespace() {
        Some(ns) => format!("{{{ns}}}{}", attr.name()),
        None => attr.name().to_string(),
    }
}

/// pdf_page_indices: pages that carry a non-empty pdfDataList.
pub fn pdf_pages(texts: &[String]) -> Result<BTreeSet<usize>, ()> {
    let docs = documents(texts)?;
    let resources: Vec<Node> = docs.iter().flat_map(|d| descendants(d.root_element())).filter(|n| tag(n) == "noteresource").collect();
    if resources.len() != 1 {
        return Ok(BTreeSet::new());
    }
    let lists: Vec<Node> = children(resources[0]).filter(|n| tag(n) == "pagelist").collect();
    if lists.len() != 1 {
        return Ok(BTreeSet::new());
    }
    Ok(children(lists[0]).filter(|n| tag(n) == "page").enumerate()
        .filter(|(_, page)| children(*page).any(|n| tag(&n) == "pdfdatalist" && children(n).next().is_some()))
        .map(|(i, _)| i).collect())
}

/// inspect_pdf_backgrounds after the locked check. Err(None) means the
/// re-parse of validated XML failed (generic message).
pub fn placements(texts: &[String], files: &HashSet<String>, urls: &HashMap<String, String>, max_pixels: i64)
                  -> Result<Vec<PdfPlacement>, Option<&'static str>> {
    let docs = documents(texts).map_err(|_| None)?;
    let resources: Vec<Node> = docs.iter().flat_map(|d| descendants(d.root_element())).filter(|n| tag(n) == "noteresource").collect();
    if resources.len() != 1 {
        return Err(Some("PDF not kaynağı belirsiz."));
    }
    let lists: Vec<Node> = children(resources[0]).filter(|n| tag(n) == "pagelist").collect();
    if lists.len() > 1 {
        return Err(Some("PDF sayfa listesi belirsiz."));
    }
    let pages: Vec<Node> = match lists.first() {
        Some(list) => children(*list).filter(|n| tag(n) == "page").collect(),
        None => Vec::new(),
    };
    let mut result = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let groups: Vec<Node> = children(*page).filter(|n| tag(n) == "pdfdatalist").collect();
        if groups.len() > 1 {
            return Err(Some("PDF yerleşim listesi belirsiz."));
        }
        let Some(group) = groups.first() else { continue };
        let width = integer(page.attribute("width"), MAX_DIMENSION, 1).map_err(Some)?;
        let height = integer(page.attribute("height"), MAX_DIMENSION, 1).map_err(Some)?;
        if width * height > max_pixels {
            continue;
        }
        let objects = descendants(*page).filter(|n| tag(n) == "object").count() as i64;
        for placement in children(*group) {
            if tag(&placement) != "pdfdata" {
                return Err(Some("PDF yerleşimi desteklenmiyor."));
            }
            let digest = placement.attribute("hash").unwrap_or("").to_lowercase();
            if !(digest.len() == 64 && digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))) || !files.contains(&digest) {
                return Err(Some("PDF dosyası referansı doğrulanamadı."));
            }
            let source_page = integer(placement.attribute("pageIndex"), MAX_PAGE_INDEX, 0).map_err(Some)?;
            let rects: Vec<Node> = children(placement).filter(|n| tag(n) == "pdfrect").collect();
            let keys: BTreeSet<String> = rects.first().map(|r| r.attributes().map(|a| attribute_key(&a)).collect()).unwrap_or_default();
            let expected: BTreeSet<String> = ["l", "t", "r", "b"].iter().map(|s| s.to_string()).collect();
            if rects.len() != 1 || keys != expected {
                return Err(Some("PDF yerleşimi henüz desteklenmiyor."));
            }
            let rect = rects[0];
            let left = integer(rect.attribute("l"), MAX_DIMENSION, 0).map_err(Some)?;
            let top = integer(rect.attribute("t"), MAX_DIMENSION, 0).map_err(Some)?;
            let right = integer(rect.attribute("r"), MAX_DIMENSION, 0).map_err(Some)?;
            let bottom = integer(rect.attribute("b"), MAX_DIMENSION, 0).map_err(Some)?;
            if !(0 <= left && left < right && right <= width) || !(0 <= top && top < bottom && bottom <= height) {
                return Err(Some("PDF yerleşimi sayfanın dışında."));
            }
            let download_url = urls.get(&digest).cloned();
            result.push(PdfPlacement { page_index: index as i64, pdf_page_index: source_page, width, height,
                rect: (left, top, right, bottom), annotation_objects: objects, sha256: digest, download_url });
            if result.len() > MAX_PLACEMENTS {
                return Err(Some("Çok fazla PDF yerleşimi var."));
            }
        }
    }
    Ok(result)
}
