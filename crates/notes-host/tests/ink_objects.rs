//! Pen everywhere: every ink writer (append recipes 2/3, first stroke,
//! highlighter, erase, move) and the projection accept notes whose page layers
//! also carry images, shapes, lines and text boxes, and carry those objects
//! through byte-identically (same element, same objectList position, same tree
//! entry, same image content-file entry). Synthetic data only: the synthetic
//! Fountain note of `tests/fixtures/ink_objects_base.json` with injected
//! objects (no real note, no network).
use notes_host::{areas, snapshot, Val};
use rs_ink::core::binary::RawSamples;
use rs_ink::core::prepare::{payload_parts, prepare_append, AppendRecipe, AppendSnapshot};
use rs_ops::util::{hex, parent_hash};
use rs_ops::xml::{self, Element};

const BOUNDARY: &str = "RenderTest";
const IMAGE: &str = "c0000000-0000-4000-8000-000000000001";
const SHAPE: &str = "c0000000-0000-4000-8000-000000000002";
const LINE: &str = "c0000000-0000-4000-8000-000000000003";
const TEXT: &str = "c0000000-0000-4000-8000-000000000004";
const IMAGE2: &str = "c0000000-0000-4000-8000-000000000005";
const PAGE2: &str = "c0000000-0000-4000-8000-0000000000a1";
const LAYER2: &str = "c0000000-0000-4000-8000-0000000000a2";
const SECOND: &str = "b0000000-0000-4000-8000-000000000002";
const NEW: &str = "b0000000-0000-4000-8000-000000000003";
const T: i64 = 1_699_999_000_000_000;

fn sha(text: &str) -> String {
    rs_ink::core::sha256_hex(text.as_bytes())
}

fn base() -> Val {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ink_objects_base.json")).unwrap();
    rs_store::val::loads(&text).unwrap()
}

type Part = (String, Option<String>, String, Vec<u8>);

fn parts(note: &Val) -> Vec<Part> {
    let snap = snapshot::from_dict(note).unwrap();
    payload_parts(&snap.raw_body, &snap.content_type, &Default::default())
        .unwrap()
        .into_iter()
        .map(|p| (p.name.unwrap(), p.filename, p.content_type, p.body))
        .collect()
}

fn multipart(parts: &[Part]) -> Vec<u8> {
    let mut raw = Vec::new();
    for (name, filename, content_type, body) in parts {
        raw.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes());
        if let Some(f) = filename {
            raw.extend_from_slice(format!("; filename=\"{f}\"").as_bytes());
        }
        raw.extend_from_slice(format!("\r\nContent-Type: {content_type}\r\n\r\n").as_bytes());
        raw.extend_from_slice(body);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    raw
}

fn without_declaration(xml: &[u8]) -> Vec<u8> {
    let start = xml.iter().position(|b| *b == b'<').unwrap();
    let mut xml = xml[start..].to_vec();
    if xml.starts_with(b"<?xml") {
        let end = xml.windows(2).position(|w| w == b"?>").unwrap() + 2;
        xml = xml[end..].to_vec();
    }
    xml
}

/// A document as the Cloud returns it: `resource` plus the stroke parts.
fn document(resource: &[u8], strokes: Vec<Part>, commit: &str) -> Val {
    let mut view = b"<CompareNoteView>".to_vec();
    view.extend_from_slice(&without_declaration(resource));
    view.extend_from_slice(format!("<commitId>{commit}</commitId></CompareNoteView>").as_bytes());
    let mut out: Vec<Part> = vec![("noteResource".into(), None, "application/xml".into(), view)];
    out.extend(strokes);
    let content_type = format!("multipart/form-data;boundary={BOUNDARY}");
    snapshot::to_dict(&snapshot::parse_document(&multipart(&out), &content_type, Some("synthetic-server")).unwrap())
}

fn stroke_parts(note: &Val) -> Vec<Part> {
    parts(note).into_iter().filter(|p| p.0 == "strokeResource").collect()
}

fn resource(note: &Val) -> Element {
    xml::parse_bytes(&snapshot::from_dict(note).unwrap().resource_xml).unwrap()
}

fn samples(points: &[(f64, f64)]) -> RawSamples {
    let n = points.len();
    let pressures: Vec<f64> = (0..n).map(|i| 0.4 + 0.1 * (i % 3) as f64).collect();
    let times: Vec<i64> = (0..n as i64).map(|i| i * 8).collect();
    RawSamples::from_parts(points, &pressures, &times)
}

/// The base note with a second stroke (our recipe 2 append), read back.
fn two_strokes() -> Val {
    let note = base();
    let snap = snapshot::from_dict(&note).unwrap();
    let source = first_stroke(&resource(&note));
    let edit = prepare_append(&AppendSnapshot::from_doc(&snap), &source, SECOND, &samples(&[(40.0, 40.0), (60.0, 48.0), (80.0, 70.0)]),
                              1_700_000_000_001, AppendRecipe::V2, None).unwrap();
    let mut strokes = stroke_parts(&note);
    let (name, filename, body) = edit.binary_parts[0].clone();
    strokes.push((name, Some(filename), "application/octet-stream".into(), body));
    document(&edit.full_xml, strokes, "synthetic-commit-2")
}

fn first_stroke(root: &Element) -> String {
    root.iter_tag("object").find(|o| o.get("type") == Some("stroke")).unwrap().get("id").unwrap().to_string()
}

fn object(kind: &str, id: &str, extra: &[(&str, &str)], children: Vec<Element>) -> Element {
    let t = T.to_string();
    let mut o = Element::new("object", &[("type", kind), ("id", id), ("hash", &sha(&format!("{id}{T}"))), ("modifiedTime", &t)]);
    for (k, v) in extra {
        o.set(k, v);
    }
    o.children = children;
    o
}

fn rect(tag: &str, l: &str, t: &str, r: &str, b: &str) -> Element {
    Element::new(tag, &[("l", l), ("t", t), ("r", r), ("b", b)])
}

fn image(id: &str, file: &str) -> Element {
    let mut fill = Element::new("fillEffect", &[("type", "image"), ("imageId", file), ("imageType", "0"), ("transparency", "0")]);
    fill.children.push(rect("stretchOffset", "0.0", "0.0", "0.0", "0.0"));
    object("image", id, &[("shapeType", "4"), ("layoutType", "0"), ("isTransparency", "false")],
           vec![rect("rect", "100.0", "200.0", "500.0", "600.0"), rect("originalRect", "100.0", "200.0", "500.0", "600.0"), fill])
}

fn tree_entry(o: &Element) -> Element {
    Element::new("object", &[("id", o.get("id").unwrap()), ("t", o.get("modifiedTime").unwrap()), ("h", o.get("hash").unwrap())])
}

fn content_file(file: &str, bind: &str) -> (Element, Element) {
    let name = rs_doc::util::b64encode("image.jpg".as_bytes());
    let t = T.to_string();
    // Samsung stores the flag as "True" on some files; the digest covers that spelling.
    let hash = sha(&format!("{file}{bind}image.jpg1{T}True"));
    let mut entry = Element::new("contentFile", &[("bindNumber", bind), ("createdTime", &t), ("hash", &hash), ("id", file),
        ("isFileAttached", "True"), ("modifiedTime", &t), ("name", &name), ("refCount", "1")]);
    let mut fh = Element::new("fileHash", &[]);
    fh.text = Some(file.to_string());
    entry.children.push(fh);
    (entry, Element::new("contentFile", &[("id", file), ("t", &t), ("h", &hash)]))
}

fn rehash(page: &mut Element, tree_page: &mut Element) {
    let pid = page.get("id").unwrap().to_string();
    let mut layer_hashes = Vec::new();
    let layers = page.find_mut("layerList").unwrap();
    let tree_layers = tree_page.find_mut("layerList").unwrap();
    for (layer, tl) in layers.children.iter_mut().zip(tree_layers.children.iter_mut()) {
        let lid = layer.get("id").unwrap().to_string();
        let mt: i64 = layer.get("modifiedTime").unwrap().parse().unwrap();
        let children: Vec<[u8; 32]> = layer.find("objectList").map(|l| l.children.iter().map(|o| {
            let raw = rs_ops::util::fromhex(o.get("hash")).unwrap();
            <[u8; 32]>::try_from(raw).unwrap()
        }).collect()).unwrap_or_default();
        let h = parent_hash(Some(&lid), mt, &children).unwrap();
        layer.set("hash", &hex(&h));
        tl.set("h", &hex(&h));
        layer_hashes.push(h);
    }
    let mt: i64 = page.get("modifiedTime").unwrap().parse().unwrap();
    let h = hex(&parent_hash(Some(&pid), mt, &layer_hashes).unwrap());
    page.set("hash", &h);
    tree_page.set("h", &h);
}

/// Two strokes plus an image (before them), a foreign shape, a line and a
/// text box (after them) on page 1; page 2 carries only an image.
fn with_objects() -> Val {
    let note = two_strokes();
    let root = objects_resource(&sha("synthetic image 1"), &sha("synthetic image 2"), false);
    document(&xml::xml_bytes(&root), stroke_parts(&note), "synthetic-commit-3")
}

/// `with_objects`' resource for the given picture file hashes; `blank` adds an
/// empty third page.
fn objects_resource(file1: &str, file2: &str, blank: bool) -> Element {
    let note = two_strokes();
    let mut root = resource(&note);
    let (file1, file2) = (file1.to_string(), file2.to_string());
    let ti = root.find_index("noteTree").unwrap();
    let pi = root.find_index("pageList").unwrap();
    // Page 1 objects.
    let shape = object("shape", SHAPE, &[("shapeType", "4"), ("deviceEdited", "true")],
                       vec![rect("rect", "600.0", "100.0", "700.0", "160.0")]);
    let line = object("line", LINE, &[("connectorType", "0")], vec![Element::new("beginPos", &[("x", "10.0"), ("y", "900.0")])]);
    let mut text = Element::new("text", &[]);
    text.text = Some(rs_doc::util::b64encode(rs_doc::util::b64encode("Kutu".as_bytes()).as_bytes()));
    let textbox = object("textBox", TEXT, &[("shapeType", "4"), ("textGravity", "0")], vec![rect("rect", "50.0", "1000.0", "400.0", "1100.0"), text]);
    {
        let page = &mut root.children[pi].children[0];
        let list = page.find_mut("layerList").unwrap().children[0].find_mut("objectList").unwrap();
        list.children.insert(0, image(IMAGE, &file1));
        list.children.extend([shape, line, textbox]);
        let order: Vec<Element> = list.children.iter().map(tree_entry).collect();
        let tp = &mut root.children[ti].children[0].children[0];
        tp.find_mut("layerList").unwrap().children[0].find_mut("objectList").unwrap().children = order;
    }
    // Page 2: image only, with its drawn rectangle.
    let mut page2 = root.children[pi].children[0].clone();
    page2.set("id", PAGE2);
    page2.children.retain(|c| c.tag == "layerList" || c.tag == "drawnRect");
    {
        let layer = &mut page2.find_mut("layerList").unwrap().children[0];
        layer.set("id", LAYER2);
        layer.find_mut("objectList").unwrap().children = vec![image(IMAGE2, &file2)];
        let drawn = page2.find_mut("drawnRect").unwrap();
        for (k, v) in [("l", "100.0"), ("t", "200.0"), ("r", "500.0"), ("b", "600.0")] {
            drawn.set(k, v);
        }
    }
    let mut tree_page2 = Element::new("page", &[("id", PAGE2), ("t", page2.get("modifiedTime").unwrap()), ("h", "")]);
    {
        let layer = &page2.find("layerList").unwrap().children[0];
        let tl = tree_page2.sub_element("layerList", &[]).sub_element("layer", &[("id", LAYER2), ("t", layer.get("modifiedTime").unwrap()), ("h", "")]);
        tl.sub_element("objectList", &[]).children.push(tree_entry(&layer.find("objectList").unwrap().children[0]));
    }
    rehash(&mut page2, &mut tree_page2);
    {
        let (page, tree_page) = {
            let (left, right) = root.children.split_at_mut(pi);
            (&mut right[0].children[0], &mut left[ti].children[0].children[0])
        };
        rehash(page, tree_page);
    }
    root.children[pi].children.push(page2);
    root.children[ti].children[0].children.push(tree_page2);
    // Content files of both images (tree list between pageList and bodyText).
    let (e1, t1) = content_file(&file1, "3");
    let (e2, t2) = content_file(&file2, "4");
    let fi = root.find_index("contentFileList").unwrap();
    root.children[fi].children = vec![e1, e2];
    let mut tree_files = Element::new("contentFileList", &[]);
    tree_files.children = vec![t1, t2];
    root.children[ti].children.insert(1, tree_files);
    if blank {
        // Page 3: an empty sheet (same structure, no objects).
        let mut page3 = root.children[pi].children[1].clone();
        page3.set("id", "c0000000-0000-4000-8000-0000000000b1");
        page3.children.retain(|c| c.tag == "layerList");
        let layer = &mut page3.find_mut("layerList").unwrap().children[0];
        layer.set("id", "c0000000-0000-4000-8000-0000000000b2");
        layer.find_mut("objectList").unwrap().children.clear();
        let mut tree_page3 = Element::new("page", &[("id", "c0000000-0000-4000-8000-0000000000b1"), ("t", page3.get("modifiedTime").unwrap()), ("h", "")]);
        {
            let layer = &page3.find("layerList").unwrap().children[0];
            let tl = tree_page3.sub_element("layerList", &[]).sub_element("layer", &[("id", "c0000000-0000-4000-8000-0000000000b2"), ("t", layer.get("modifiedTime").unwrap()), ("h", "")]);
            tl.sub_element("objectList", &[]);
        }
        rehash(&mut page3, &mut tree_page3);
        root.children[pi].children.push(page3);
        let tree_pages = root.children[ti].children.iter_mut().find(|c| c.tag == "pageList").unwrap();
        tree_pages.children.push(tree_page3);
    }
    root
}

/// Regenerates `crates/notes-preview/tests/fixtures/objects_note.json`: the
/// `with_objects` note (plus an empty third page) whose pictures are real
/// synthetic PNGs, with the Cloud's download URL entries, for the page
/// composite tests. Synthetic data only.
#[test]
#[ignore = "fixture generator"]
fn regen_preview_objects_fixture() {
    let red = rs_render::pixels::rgba_png(4, 4, &[230, 30, 30, 255].repeat(16)).unwrap();
    let blue = rs_render::pixels::rgba_png(4, 4, &[30, 60, 230, 255].repeat(16)).unwrap();
    let (file1, file2) = (rs_ink::core::sha256_hex(&red), rs_ink::core::sha256_hex(&blue));
    let mut root = objects_resource(&file1, &file2, true);
    // Page 2 also names a Samsung canvas cache (whose bytes the tests serve
    // as an undecodable file): the local drawing must stand in for it.
    let cache = rs_ink::core::sha256_hex(b"synthetic canvas cache");
    {
        let pi = root.find_index("pageList").unwrap();
        let mut map = Element::new("canvasCacheDataMap", &[]);
        map.sub_element("pair", &[("key", "0"), ("fileId", "5"), ("width", "1080"), ("height", "1527"), ("isDarkMode", "false")]);
        root.children[pi].children[1].children.push(map);
        let (entry, tree) = content_file(&cache, "5");
        let fi = root.find_index("contentFileList").unwrap();
        root.children[fi].children.push(entry);
        let ti = root.find_index("noteTree").unwrap();
        root.children[ti].children.iter_mut().find(|c| c.tag == "contentFileList").unwrap().children.push(tree);
    }
    let note = two_strokes();
    let mut view = b"<CompareNoteView>".to_vec();
    view.extend_from_slice(&without_declaration(&xml::xml_bytes(&root)));
    view.extend_from_slice(b"<commitId>synthetic-commit-4</commitId><downloadUrls>");
    for f in [&file1, &file2, &cache] {
        view.extend_from_slice(format!("<downloadUrl><id>{f}</id><url>https://ew1a-api.samsungcloud.com/file?signature={}</url></downloadUrl>", &f[..8]).as_bytes());
    }
    view.extend_from_slice(b"</downloadUrls></CompareNoteView>");
    let mut out: Vec<Part> = vec![("noteResource".into(), None, "application/xml".into(), view)];
    out.extend(stroke_parts(&note));
    let content_type = format!("multipart/form-data;boundary={BOUNDARY}");
    let doc = snapshot::to_dict(&snapshot::parse_document(&multipart(&out), &content_type, Some("synthetic-server")).unwrap());
    let fixture = Val::dict(vec![
        ("snapshot", doc),
        ("images", Val::dict(vec![(file1.as_str(), Val::s(rs_doc::util::b64encode(&red))), (file2.as_str(), Val::s(rs_doc::util::b64encode(&blue)))])),
    ]);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../notes-preview/tests/fixtures/objects_note.json");
    std::fs::write(path, rs_store::val::dumps(&fixture, true, true).unwrap()).unwrap();
}

/// Every carried object (by id) as serialized XML, in document order, plus
/// its tree entry and the content-file entries.
fn carried(root: &Element) -> Vec<String> {
    let mut out = Vec::new();
    for o in root.find("pageList").unwrap().iter_tag("object") {
        if matches!(o.get("type"), Some("image" | "shape" | "line" | "textBox")) {
            out.push(String::from_utf8(xml::xml_bytes(o)).unwrap());
        }
    }
    assert_eq!(out.len(), 5, "fixture carries five objects");
    let tree = root.find("noteTree").unwrap();
    for o in tree.iter_tag("object") {
        if [IMAGE, SHAPE, LINE, TEXT, IMAGE2].contains(&o.get("id").unwrap_or("")) {
            out.push(String::from_utf8(xml::xml_bytes(o)).unwrap());
        }
    }
    for f in root.find("contentFileList").unwrap().children.iter().chain(tree.find("contentFileList").unwrap().children.iter()) {
        out.push(String::from_utf8(xml::xml_bytes(f)).unwrap());
    }
    out
}

/// Object ids of a page's layer, in order.
fn order(root: &Element, page: usize) -> Vec<String> {
    root.find("pageList").unwrap().children[page].iter_tag("object").map(|o| o.get("id").unwrap().to_string()).collect()
}

/// The upload carries the complete note tree (every page, layer and object
/// entry, carried ones included) even though its page list is pruned.
fn tree_intact(full: &[u8], upload: &[u8]) {
    let tree = |x: &[u8]| xml::xml_bytes(xml::parse_bytes(x).unwrap().find("noteTree").unwrap());
    assert_eq!(tree(full), tree(upload), "upload note tree differs from the edited document");
}

fn ids(edit_upload: &[u8]) -> Vec<String> {
    let root = xml::parse_bytes(edit_upload).unwrap();
    root.find("pageList").unwrap().iter_tag("object").map(|o| o.get("id").unwrap().to_string()).collect()
}

fn caps(projection: &Val) -> Val {
    projection.get("capabilities").cloned().unwrap()
}

fn page_ink(projection: &Val, index: usize) -> Val {
    let Some(Val::List(pages)) = projection.get("pages") else { panic!("no pages: {projection:?}") };
    pages[index].get("ink").cloned().unwrap()
}

fn on(v: &Val, key: &str) -> bool {
    v.get(key).is_some_and(Val::truthy)
}

#[test]
fn projection_enables_pen_eraser_and_first_stroke_beside_carried_objects() {
    // Before: "Bu notta resim var; kalem henüz desteklenmiyor." and no pages.
    let note = with_objects();
    let p = areas::project_ink(&note, false).unwrap();
    assert_eq!(p.get("reason"), Some(&Val::Null), "{p:?}");
    for key in ["appendInk", "eraseInk", "firstStroke", "highlighter"] {
        assert!(on(&caps(&p), key), "{key}: {p:?}");
    }
    let (first, second) = (page_ink(&p, 0), page_ink(&p, 1));
    assert!(on(&first, "appendInk") && on(&first, "eraseInk") && on(&first, "highlighter"), "{first:?}");
    assert!(on(&second, "firstStroke") && on(&second, "highlighter") && !on(&second, "appendInk"), "{second:?}");
    // Only strokes are listed (and erasable); carried objects never are.
    let Some(Val::List(pages)) = p.get("pages") else { panic!() };
    let Some(Val::List(strokes)) = pages[0].get("strokes") else { panic!() };
    assert_eq!(strokes.len(), 2);
}

#[test]
fn projection_stays_closed_for_objects_it_cannot_carry() {
    let note = with_objects();
    let mut root = resource(&note);
    let pi = root.find_index("pageList").unwrap();
    // Nest an object inside the shape: grouped content is never carried.
    let list = root.children[pi].children[0].find_mut("layerList").unwrap().children[0].find_mut("objectList").unwrap();
    let shape = list.children.iter_mut().find(|o| o.get("id") == Some(SHAPE)).unwrap();
    shape.children.push(Element::new("object", &[("type", "stroke")]));
    let p = areas::project_ink(&document(&xml::xml_bytes(&root), stroke_parts(&note), "synthetic-commit-4"), false).unwrap();
    assert!(!on(&caps(&p), "appendInk") && !on(&caps(&p), "eraseInk"), "{p:?}");
    assert_eq!(p.get("reason").and_then(Val::as_str), Some("Bu notta şekil veya metin kutusu gibi nesneler var; kalem henüz desteklenmiyor."));
}

#[test]
fn append_recipes_2_and_3_carry_every_other_object_byte_identically() {
    let note = with_objects();
    let before = resource(&note);
    let snap = snapshot::from_dict(&note).unwrap();
    let source = first_stroke(&before);
    for recipe in [AppendRecipe::V2, AppendRecipe::V3 { color_argb: 0xffff0000, pen_size: 3.5 }] {
        let edit = prepare_append(&AppendSnapshot::from_doc(&snap), &source, NEW, &samples(&[(200.0, 300.0), (260.0, 330.0)]),
                                  1_800_000_000_000, recipe, None)
            .unwrap_or_else(|e| panic!("{recipe:?}: {e:?}"));
        let after = xml::parse_bytes(&edit.full_xml).unwrap();
        assert_eq!(carried(&after), carried(&before), "{recipe:?}");
        let mut expected = order(&before, 0);
        expected.push(NEW.into());
        assert_eq!(order(&after, 0), expected, "{recipe:?}: object order");
        assert_eq!(order(&after, 1), order(&before, 1));
        assert_eq!(ids(&edit.upload_xml), vec![NEW.to_string()], "only the new stroke travels");
        tree_intact(&edit.full_xml, &edit.upload_xml);
        assert!(edit.content_file_ids.is_empty(), "image files are never released");
        assert_eq!(edit.requested_ids.len(), 3);
    }
    // Recipe 1 keeps its original stroke-only profile.
    let legacy = prepare_append(&AppendSnapshot::from_doc(&snap), &source, NEW, &samples(&[(200.0, 300.0), (260.0, 330.0)]),
                                1_800_000_000_000, AppendRecipe::V1, None);
    assert!(legacy.is_err());
}

fn deps() -> notes_host::deps::PureDeps {
    notes_host::deps::PureDeps::default()
}

#[test]
fn erase_and_move_touch_only_strokes() {
    let note = with_objects();
    let before = resource(&note);
    let snap = snapshot::from_dict(&note).unwrap();
    let page = before.find("pageList").unwrap().children[0].get("id").unwrap().to_string();
    let layer = before.find("pageList").unwrap().children[0].find("layerList").unwrap().children[0].get("id").unwrap().to_string();
    let strokes: Vec<String> = before.find("pageList").unwrap().children[0].iter_tag("object")
        .filter(|o| o.get("type") == Some("stroke")).map(|o| o.get("id").unwrap().to_string()).collect();
    assert_eq!(strokes.len(), 2);
    let now = Some(1_800_000_000_000i128);
    let one = Some(vec![Some(strokes[1].clone())]);
    let delete = rs_ops::ink_erase::prepare_ink_delete(&deps(), &snap, Some(&page), Some(&layer), &one, now).unwrap();
    let after = xml::parse_bytes(&delete.full_xml).unwrap();
    assert_eq!(carried(&after), carried(&before));
    assert_eq!(order(&after, 0), order(&before, 0).into_iter().filter(|i| *i != strokes[1]).collect::<Vec<_>>());
    assert!(ids(&delete.upload_xml).is_empty());
    tree_intact(&delete.full_xml, &delete.upload_xml);
    // Erasing both strokes leaves the image and the others on the page.
    let both = Some(strokes.iter().cloned().map(Some).collect());
    let all = rs_ops::ink_erase::prepare_ink_delete(&deps(), &snap, Some(&page), Some(&layer), &both, now).unwrap();
    assert_eq!(carried(&xml::parse_bytes(&all.full_xml).unwrap()), carried(&before));
    let moved = rs_ops::ink_erase::prepare_ink_move(&deps(), &snap, Some(&page), Some(&layer), &one, Some(5.0), Some(7.0), now).unwrap();
    let after = xml::parse_bytes(&moved.full_xml).unwrap();
    assert_eq!(carried(&after), carried(&before));
    assert_eq!(order(&after, 0), order(&before, 0));
    assert_eq!(ids(&moved.upload_xml), vec![strokes[1].clone()]);
    tree_intact(&moved.full_xml, &moved.upload_xml);
    // A carried object is never an erase/move target.
    for target in [IMAGE, SHAPE, TEXT] {
        let ids = Some(vec![Some(target.to_string())]);
        assert!(rs_ops::ink_erase::prepare_ink_delete(&deps(), &snap, Some(&page), Some(&layer), &ids, now).is_err(), "{target}");
        assert!(rs_ops::ink_erase::prepare_ink_move(&deps(), &snap, Some(&page), Some(&layer), &ids, Some(1.0), Some(1.0), now).is_err());
    }
}

#[test]
fn first_stroke_and_highlighter_on_an_image_only_page() {
    let note = with_objects();
    let before = resource(&note);
    let snap = snapshot::from_dict(&note).unwrap();
    let raw = samples(&[(150.0, 250.0), (700.0, 900.0)]);
    for highlighter in [false, true] {
        let prepare = if highlighter {
            rs_ops::ink_highlighter::prepare_highlighter_stroke
        } else {
            rs_ops::ink_first_stroke::prepare_first_stroke
        };
        let color = if highlighter { 0x73ffeb3bu32 as i128 } else { 0xff000000u32 as i128 };
        let edit = prepare(&deps(), &snap, Some(PAGE2), Some(&raw as &dyn std::any::Any), Some(color), Some(4.0),
                           Some(1_800_000_000_000), Some(Some(NEW)))
            .unwrap_or_else(|e| panic!("highlighter={highlighter}: {e:?}"));
        let after = xml::parse_bytes(&edit.full_xml).unwrap();
        assert_eq!(carried(&after), carried(&before), "highlighter={highlighter}");
        assert_eq!(order(&after, 1), vec![IMAGE2.to_string(), NEW.to_string()]);
        // The page's drawn rectangle grows to cover the stroke; it is not replaced.
        let drawn = after.find("pageList").unwrap().children[1].findall("drawnRect");
        assert_eq!(drawn.len(), 1);
        let v = |k: &str| drawn[0].get(k).unwrap().parse::<f64>().unwrap();
        assert!(v("l") <= 100.0 && v("t") <= 200.0 && v("r") >= 700.0 && v("b") >= 900.0, "{drawn:?}");
        assert_eq!(ids(&edit.upload_xml), vec![NEW.to_string()], "only the new stroke travels");
        tree_intact(&edit.full_xml, &edit.upload_xml);
    }
    // Page 1 already has strokes: first stroke still refuses, highlighter appends.
    let page1 = before.find("pageList").unwrap().children[0].get("id").unwrap().to_string();
    let first = rs_ops::ink_first_stroke::prepare_first_stroke(&deps(), &snap, Some(&page1), Some(&raw as &dyn std::any::Any),
        Some(0xff000000u32 as i128), Some(4.0), Some(1_800_000_000_000), Some(Some(NEW)));
    assert!(first.is_err());
    let marker = rs_ops::ink_highlighter::prepare_highlighter_stroke(&deps(), &snap, Some(&page1), Some(&raw as &dyn std::any::Any),
        Some(0x73ffeb3bu32 as i128), Some(4.0), Some(1_800_000_000_000), Some(Some(NEW))).unwrap();
    assert_eq!(carried(&xml::parse_bytes(&marker.full_xml).unwrap()), carried(&before));
}
