//! Editor capability per note structure, on synthetic notes built from the
//! documented format (docs/EDITOR_FORMAT.md): pen notes with body text,
//! native empty text boxes, untitled notes, long annotation streams, and the
//! structures that stay read-only.
extern crate _editor as rs_editor;

use rs_editor::edit::{self, DocBackend, Now, Snapshot};
use rs_editor::etree::{self, Element};
use rs_editor::pyval::{PyInt, Val};
use rs_editor::rich;

const NOTE: &str = "6f1f6c35-6a40-4c1e-9a55-0a4fbb2f7a10";
const STROKE: &str = "0e5d4a3c-1111-4c1e-9a55-0a4fbb2f7a10";
const CONTENT: &str = "0e5d4a3c-2222-4c1e-9a55-0a4fbb2f7a10";
const STRING_ID: &str = "0e5d4a3c-3333-4c1e-9a55-0a4fbb2f7a10";

fn backend() -> DocBackend {
    DocBackend { limits: rs_doc::doc::DocLimits::default(), raw_body: Vec::new(), content_type: "application/xml".into() }
}

fn el(tag: &str, attrs: &[(&str, &str)]) -> Element {
    let mut e = Element::new(tag);
    for (k, v) in attrs {
        e.set(k, *v);
    }
    e
}

fn child_mut<'a>(e: &'a mut Element, name: &str) -> &'a mut Element {
    e.children.iter_mut().find(|c| c.local() == name).expect(name)
}

fn child<'a>(e: &'a Element, name: &str) -> &'a Element {
    e.children.iter().find(|c| c.local() == name).expect(name)
}

/// A plain text note from the research writer (title + body text objects).
fn plain(title: &str, body: &str) -> Element {
    let (_, xml) = rs_doc::doc::new_document(title, body, Some(NOTE), rs_doc::doc::NowArg::Int(1_700_000_000_000), 256 * 1024).unwrap();
    etree::parse(&xml).unwrap()
}

fn body_obj(root: &mut Element) -> &mut Element {
    child_mut(child_mut(root, "bodytext"), "object")
}

fn title_obj(root: &mut Element) -> &mut Element {
    child_mut(child_mut(child_mut(root, "attributes"), "title"), "object")
}

/// Turn a note into a pen note the way device notes look: one stroke object
/// in the page layer (mirrored in the note tree), its content file, the
/// attributes' string table and a native child order for the text objects.
fn add_pen_page(root: &mut Element) {
    {
        let page = child_mut(child_mut(root, "pagelist"), "page");
        let layer = child_mut(child_mut(page, "layerlist"), "layer");
        let mut list = Element::new("objectList");
        list.children.push(el("object", &[("type", "stroke"), ("id", STROKE), ("hash", "00"), ("contentFile", CONTENT)]));
        layer.children.push(list);
    }
    {
        let tree = child_mut(root, "notetree");
        let page = child_mut(child_mut(tree, "pagelist"), "page");
        let layer = child_mut(child_mut(page, "layerlist"), "layer");
        let mut list = Element::new("objectList");
        list.children.push(el("object", &[("id", STROKE), ("t", "1"), ("h", "00")]));
        layer.children.push(list);
        let mut files = Element::new("contentFileList");
        files.children.push(el("contentFile", &[("id", CONTENT), ("t", "1"), ("h", "00")]));
        tree.children.push(files);
    }
    let mut files = Element::new("contentFileList");
    files.children.push(el("contentFile", &[("id", CONTENT), ("fileName", "stroke.bin"), ("hash", "00")]));
    root.children.push(files);
    let attrs = child_mut(root, "attributes");
    let mut table = Element::new("stringIdList");
    table.children.push(el("stringId", &[("id", STRING_ID), ("value", "com.samsung.android.sdk.pen.pen.preload.FountainPen")]));
    attrs.children.push(table);
}

fn remove_text(obj: &mut Element) {
    obj.children.retain(|c| c.local() != "text");
}

fn snap(root: &Element, title: &str) -> Snapshot {
    Snapshot { title: Some(title.into()), resource_xml: etree::xml_bytes(root) }
}

fn now(root: &Element) -> Now {
    Now::Int(PyInt::from_i128(rich::integer(root.get("syncModifiedTime")).unwrap() + 1000))
}

fn model(text: &str, spans: Vec<Val>, paragraphs: Vec<Val>) -> Val {
    Val::Dict(vec![
        (Val::str("schemaVersion"), Val::int(1)),
        (Val::str("text"), Val::str(text)),
        (Val::str("spans"), Val::List(spans)),
        (Val::str("paragraphs"), Val::List(paragraphs)),
    ])
}

fn span(start: i128, end: i128, kind: &str, value: Val) -> Val {
    Val::Dict(vec![
        (Val::str("start"), Val::int(start)),
        (Val::str("end"), Val::int(end)),
        (Val::str("kind"), Val::str(kind)),
        (Val::str("value"), value),
        (Val::str("interval"), Val::int(3)),
    ])
}

fn projection(root: &Element, title: &str) -> edit::Projection {
    edit::editor_projection(Ok(&snap(root, title)), &backend()).unwrap()
}

/// Everything but the body text object and the bookkeeping attributes an
/// edit advances, in canonical form.
fn carried(root: &Element) -> Vec<String> {
    root.children
        .iter()
        .filter(|c| c.local() != "bodytext")
        .map(|c| {
            let mut c = c.clone();
            for k in ["modifiedTime", "fileRevision", "displayModifiedTime", "t", "h"] {
                if c.local() == "attributes" {
                    c.pop_attr(k);
                }
            }
            if c.local() == "notetree" {
                for n in c.children.iter_mut().filter(|n| n.local() == "bodytext") {
                    n.pop_attr("t");
                    n.pop_attr("h");
                }
            }
            etree::canonicalize(&c)
        })
        .collect()
}

fn reread_body(xml: &[u8]) -> rich::Rich {
    let root = etree::parse(xml).unwrap();
    edit::object_rich(child(child(&root, "bodytext"), "object")).unwrap()
}

#[test]
fn pen_note_body_text_is_editable_and_pages_are_carried() {
    let mut root = plain("Kalem", "Satır bir\nSatır iki");
    add_pen_page(&mut root);
    let p = projection(&root, "Kalem");
    assert!(p.title_edit && p.text_edit, "{:?}", p.reason);
    let edited = model("Satır bir\nSatır iki ve ek", vec![span(0, 5, "bold", Val::Bool(true))], vec![]);
    let out = edit::prepare_editor_edit(&snap(&root, "Kalem"), &Val::str("Kalem"), &edited, &now(&root), &backend()).unwrap();
    let new_root = etree::parse(&out.full_xml).unwrap();
    assert_eq!(carried(&root), carried(&new_root), "pages, note tree, content files and string table must be unchanged");
    assert!(rs_editor::pyval::py_eq(&reread_body(&out.full_xml).model, &rich::normalize_model(&edited).unwrap()));
    // The upload sends no page/content data and asks only for the body.
    let upload = etree::parse(&out.upload_xml).unwrap();
    for name in ["pagelist", "contentfilelist"] {
        let section = child(&upload, name);
        assert!(section.children.is_empty() && section.attrib.is_empty(), "{name}");
    }
    assert_eq!(out.requested_ids.len(), 1);
    // The plain (non-rich) path keeps its stricter gate.
    let plain_gate = rs_doc::doc::plain_structure(&rs_doc::xml::fromstring(&etree::xml_bytes(&root)).unwrap(),
        &rs_doc::mime::Limits::default(), 256 * 1024);
    assert!(matches!(plain_gate, Err(rs_doc::Error::Document(m)) if m.contains("Çizim veya ek nesne")));
}

#[test]
fn native_empty_body_gets_text_and_clearing_writes_it_natively() {
    let mut root = plain("Boş gövde", "x");
    add_pen_page(&mut root);
    remove_text(body_obj(&mut root));
    let p = projection(&root, "Boş gövde");
    assert!(p.text_edit, "{:?}", p.reason);
    assert_eq!(rich::model_text(p.model.as_ref().unwrap()), "");
    let edited = model("Merhaba", vec![], vec![]);
    let out = edit::prepare_editor_edit(&snap(&root, "Boş gövde"), &Val::str("Boş gövde"), &edited, &now(&root), &backend()).unwrap();
    let new_root = etree::parse(&out.full_xml).unwrap();
    let body = child(child(&new_root, "bodytext"), "object");
    assert_eq!(body.children.last().unwrap().local(), "text", "native order: <text> is the last child");
    assert_eq!(rich::model_text(&reread_body(&out.full_xml).model), "Merhaba");
    assert_eq!(carried(&root), carried(&new_root));
    // Clearing it again writes the native empty text box (no <text>).
    let cleared = edit::prepare_editor_edit(&snap(&new_root, "Boş gövde"), &Val::str("Boş gövde"), &model("", vec![], vec![]),
        &now(&new_root), &backend()).unwrap();
    let cleared_root = etree::parse(&cleared.full_xml).unwrap();
    assert!(!child(child(&cleared_root, "bodytext"), "object").children.iter().any(|c| c.local() == "text"));
    assert!(projection(&cleared_root, "Boş gövde").text_edit);
}

#[test]
fn untitled_note_title_is_editable() {
    let mut root = plain("", "Gövde");
    add_pen_page(&mut root);
    remove_text(title_obj(&mut root));
    let p = projection(&root, "");
    assert!(p.title_edit && p.text_edit, "{:?}", p.reason);
    let out = edit::prepare_title_edit(&snap(&root, ""), &Val::str("Yeni başlık"), &now(&root), &backend()).unwrap();
    let new_root = etree::parse(&out.full_xml).unwrap();
    let title = child(child(child(&new_root, "attributes"), "title"), "object");
    assert_eq!(rich::model_text(&edit::object_rich(title).unwrap().model), "Yeni başlık");
    assert_eq!(title.children.last().unwrap().local(), "text");
    // Body and pages are untouched by a title edit.
    assert_eq!(etree::canonicalize(child(&root, "bodytext")), etree::canonicalize(child(&new_root, "bodytext")));
    assert_eq!(etree::canonicalize(child(&root, "pagelist")), etree::canonicalize(child(&new_root, "pagelist")));
    // Clearing the title writes a native untitled text box.
    let cleared = edit::prepare_title_edit(&snap(&new_root, "Yeni başlık"), &Val::str(""), &now(&new_root), &backend()).unwrap();
    let cleared_root = etree::parse(&cleared.full_xml).unwrap();
    assert!(!child(child(child(&cleared_root, "attributes"), "title"), "object").children.iter().any(|c| c.local() == "text"));
}

#[test]
fn long_device_streams_with_one_parsing_state_per_paragraph() {
    let lines = 6000;
    let text: String = (0..lines).map(|i| format!("s{i}")).collect::<Vec<_>>().join("\n");
    let paragraphs: Vec<Val> = (0..lines)
        .map(|i| Val::Dict(vec![(Val::str("index"), Val::int(i)), (Val::str("parsingState"), Val::Bool(false))]))
        .collect();
    let m = model(&text, vec![], paragraphs.clone());
    let rich_body = rich::encode_rich_text(&m, &[]).unwrap();
    let mut root = plain("Uzun", "x");
    add_pen_page(&mut root);
    edit::put_rich(body_obj(&mut root), &rich_body, false).unwrap();
    let p = projection(&root, "Uzun");
    assert!(p.text_edit, "{:?}", p.reason);
    // Re-encoding with no edit is byte-identical for our canonical stream.
    let back = reread_body(&etree::xml_bytes(&root));
    assert_eq!(rich::encode_rich_text(&back.model, &back.sections).unwrap().encoded, rich_body.encoded);
    // Above the bound the body stays read-only with the limit reason.
    let many: Vec<Val> = (0..(rich::MAX_ANNOTATIONS as i128 + 1))
        .map(|i| span(i, i, "bold", Val::Bool(true)))
        .collect();
    let huge = model(&"a".repeat(rich::MAX_ANNOTATIONS + 2), many, vec![]);
    assert!(matches!(rich::normalize_model(&huge), Err(rich::Error::Document(m)) if m == "Biçimlendirme sınırı aşıldı."));
}

#[test]
fn page_slices_follow_an_edit_in_a_pen_note() {
    let text = "Birinci sayfa\nİkinci sayfa";
    let first = rich::utf16_length("Birinci sayfa\n") as i64;
    let total = rich::utf16_length(text) as i64;
    let sections = vec![(0, first, first.to_string()), (first, total, (total - first).to_string())];
    let rich_body = rich::encode_rich_text(&model(text, vec![], vec![]), &sections).unwrap();
    let mut root = plain("Bölümlü", "x");
    add_pen_page(&mut root);
    edit::put_rich(body_obj(&mut root), &rich_body, false).unwrap();
    assert!(projection(&root, "Bölümlü").text_edit);
    let edited = model("Birinci sayfa ek\nİkinci sayfa", vec![], vec![]);
    let out = edit::prepare_editor_edit(&snap(&root, "Bölümlü"), &Val::str("Bölümlü"), &edited, &now(&root), &backend()).unwrap();
    let back = reread_body(&out.full_xml);
    let grown = first + 3;
    assert_eq!(back.sections, vec![(0, grown, grown.to_string()), (grown, total + 3, (total - first).to_string())]);
}

fn reason(root: &Element) -> Option<String> {
    let p = projection(root, "Kapalı");
    assert!(!p.text_edit);
    p.reason
}

#[test]
fn structures_that_stay_read_only_have_plain_reasons() {
    // Free-floating objects.
    let mut root = plain("Kapalı", "metin");
    add_pen_page(&mut root);
    let mut floating = Element::new("floatingObjectList");
    floating.children.push(el("object", &[("type", "image"), ("id", STRING_ID)]));
    root.children.push(floating);
    assert_eq!(reason(&root).as_deref(), Some(rs_doc::doc::EDITOR_FLOATING));
    // Voice recordings / attachments anywhere.
    let mut root = plain("Kapalı", "metin");
    add_pen_page(&mut root);
    child_mut(&mut root, "attributes").children.push(Element::new("voice"));
    assert_eq!(reason(&root).as_deref(), Some(rs_doc::doc::EDITOR_MEDIA));
    // An object outside the page list (e.g. next to the body text).
    let mut root = plain("Kapalı", "metin");
    add_pen_page(&mut root);
    child_mut(&mut root, "bodytext").children.push(el("object", &[("type", "image"), ("id", STRING_ID)]));
    assert!(reason(&root).is_some());
    // Page objects that disagree with the note tree's inventory.
    let mut root = plain("Kapalı", "metin");
    add_pen_page(&mut root);
    let tree = child_mut(&mut root, "notetree");
    let page = child_mut(child_mut(tree, "pagelist"), "page");
    let layer = child_mut(child_mut(page, "layerlist"), "layer");
    child_mut(layer, "objectlist").children[0].set("id", CONTENT);
    assert_eq!(reason(&root).as_deref(), Some(rs_doc::doc::EDITOR_INCONSISTENT));
    // Unknown annotation grammar keeps the body read-only.
    let mut root = plain("Kapalı", "metin");
    add_pen_page(&mut root);
    let stream = format!("{}<?a 'span/fontName'='3,Roboto'?>{}<?a 'span/fontName'?>", rich::b64("me"), rich::b64("tin"));
    let t = body_obj(&mut root).children.iter_mut().find(|c| c.local() == "text").unwrap();
    t.text = Some(rich::b64(&stream));
    assert!(reason(&root).is_some());
}

#[test]
fn floating_object_list_mirrors_the_writer() {
    // The research writer has no floatingObjectList; add an empty one like
    // Cloud downsync does and check it does not block editing.
    let mut root = plain("Boş liste", "metin");
    root.children.push(Element::new("floatingObjectList"));
    add_pen_page(&mut root);
    assert!(projection(&root, "Boş liste").text_edit);
}
