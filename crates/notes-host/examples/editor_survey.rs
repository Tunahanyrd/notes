//! Read-only structural survey of editor capability on a PRIVATE COPY of the
//! library database (never the live file).
//!
//!     cargo run -p notes-host --example editor_survey -- <copy.sqlite3>
//!
//! Output is structural only: capability flags, the fixed reason message
//! (from our own code), sub-reason codes, element/attribute/annotation key
//! names and counts. It never prints note text, titles, ids, values or
//! geometry, and writes no files. Notes are numbered in query order only.
use std::collections::BTreeMap;

use notes_host::{areas, snapshot, Val};
use rs_doc::xml::Element;

type Counts = BTreeMap<String, usize>;

fn bump(c: &mut Counts, k: impl Into<String>) {
    *c.entry(k.into()).or_default() += 1;
}

fn ws(c: char) -> bool {
    c.is_whitespace()
}

fn b64(s: &str) -> Option<String> {
    let compact: String = s.chars().filter(|c| !ws(*c)).collect();
    String::from_utf8(rs_doc::util::b64decode_strict(compact.as_bytes())?).ok()
}

/// Annotation key names of an encoded rich stream and grammar anomalies.
fn annotation_facts(encoded: &str, keys: &mut Counts, anomalies: &mut Counts) {
    let Some(stream) = b64(encoded) else {
        bump(anomalies, "outer-base64-invalid");
        return;
    };
    let mut rest = stream.as_str();
    while let Some(i) = rest.find("<?a") {
        let after = &rest[i + 3..];
        let Some(close) = after.find("?>") else {
            bump(anomalies, "instruction-unclosed");
            return;
        };
        let inner = &after[..close];
        // Lenient scan for 'group/kind' keys and value shapes.
        let mut s = inner;
        loop {
            let t = s.trim_start_matches(ws);
            if t.is_empty() {
                break;
            }
            if t.len() == s.len() {
                bump(anomalies, "token-not-whitespace-separated");
            }
            let Some(r) = t.strip_prefix('\'') else {
                bump(anomalies, "key-not-quoted");
                break;
            };
            let Some(endq) = r.find('\'') else {
                bump(anomalies, "key-unterminated");
                break;
            };
            let key = &r[..endq];
            let shape = match key.split_once('/') {
                Some((g, k))
                    if !g.is_empty() && !k.is_empty() && g.bytes().all(|b| b.is_ascii_alphabetic())
                        && k.bytes().all(|b| b.is_ascii_alphabetic()) =>
                {
                    format!("{g}/{k}")
                }
                Some((g, _)) if g.bytes().all(|b| b.is_ascii_alphabetic()) && !g.is_empty() => {
                    format!("{g}/<non-alpha kind len {}>", key.len() - g.len() - 1)
                }
                _ => format!("<non-key len {}>", key.len()),
            };
            let mut r = &r[endq + 1..];
            let has_value = r.starts_with("='");
            if has_value {
                let v = &r[2..];
                match v.find('\'') {
                    Some(e) => {
                        let value = &v[..e];
                        let commas = value.matches(',').count();
                        bump(keys, format!("{shape} open commas={commas}"));
                        r = &v[e + 1..];
                    }
                    None => {
                        bump(anomalies, "value-unterminated");
                        bump(keys, format!("{shape} open unterminated"));
                        break;
                    }
                }
            } else if r.starts_with('=') {
                bump(anomalies, "value-not-quoted");
                break;
            } else {
                bump(keys, format!("{shape} close"));
            }
            s = r;
        }
        rest = &after[close + 2..];
    }
}

fn path_class(stack: &[String]) -> String {
    // Parent chain of an <object>, collapsed to structural names.
    let names: Vec<&str> = stack.iter().map(String::as_str).filter(|n| !matches!(*n, "noteresource")).collect();
    names.join("/")
}

fn walk_ids(e: &Element, stack: &mut Vec<String>, out: &mut Counts) {
    let name = e.local().to_lowercase();
    if e.has_attr("id") {
        let mut chain: Vec<&str> = stack.iter().map(String::as_str).filter(|n| *n != "noteresource").collect();
        chain.push(&name);
        bump(out, chain.join("/"));
    }
    stack.push(name);
    for c in &e.children {
        walk_ids(c, stack, out);
    }
    stack.pop();
}

fn walk_objects(e: &Element, stack: &mut Vec<String>, out: &mut Counts) {
    let name = e.local().to_lowercase();
    if name == "object" {
        bump(out, format!("{} type={}", path_class(stack), e.get("type").unwrap_or("<none>")));
    }
    stack.push(name);
    for c in &e.children {
        walk_objects(c, stack, out);
    }
    stack.pop();
}

fn child_names(e: &Element) -> String {
    let mut c = Counts::new();
    for ch in &e.children {
        bump(&mut c, ch.local().to_lowercase());
    }
    c.iter().map(|(k, v)| if *v == 1 { k.clone() } else { format!("{k}x{v}") }).collect::<Vec<_>>().join(",")
}

fn find_children<'a>(e: &'a Element, name: &str) -> Vec<&'a Element> {
    e.children.iter().filter(|c| c.local().to_lowercase() == name).collect()
}

struct Note {
    trashed: bool,
    title_edit: bool,
    text_edit: bool,
    reason: String,
    facts: Vec<String>,
}

fn survey(document: &Val, trashed: bool) -> Note {
    let mut facts = Vec::new();
    let snap = match snapshot::from_dict(document) {
        Ok(s) => s,
        Err(_) => {
            return Note { trashed, title_edit: false, text_edit: false, reason: "<snapshot unreadable>".into(), facts };
        }
    };
    let (title_edit, text_edit, reason) = match areas::editor_projection(&snap) {
        Ok(p) => {
            let caps = p.get("capabilities").cloned().unwrap_or(Val::Null);
            let flag = |k: &str| caps.get(k).map(Val::truthy).unwrap_or(false);
            (flag("titleEdit"), flag("textEdit"), p.get("reason").and_then(Val::as_str).unwrap_or("").to_string())
        }
        Err(e) => (false, false, format!("<projection error kind {:?}>", e.kind)),
    };
    let parts = rs_doc::mime::parts(&snap.raw_body, &snap.content_type, rs_doc::doc::DocLimits::default().max_readonly_parts, &rs_doc::mime::Limits::default(), &mut rs_doc::mime::NoHooks);
    match parts {
        Ok(p) => {
            let mut c = Counts::new();
            for part in &p {
                bump(&mut c, part.content_type.clone());
            }
            facts.push(format!("mime: {c:?}"));
        }
        Err(_) => facts.push("mime: unreadable".into()),
    }
    let Ok(root) = rs_doc::xml::fromstring(&snap.resource_xml) else {
        facts.push("xml: unreadable".into());
        return Note { trashed, title_edit, text_edit, reason, facts };
    };
    facts.push(format!("root children: {}", child_names(&root)));
    if let Some(a) = find_children(&root, "attributes").first() {
        facts.push(format!("formatVersion={}", a.get("formatVersion").unwrap_or("<none>")));
        let titles = find_children(a, "title");
        facts.push(format!("attributes/title count={}", titles.len()));
        if let Some(t) = titles.first() {
            for o in find_children(t, "object") {
                facts.push(format!("title object children: {}", child_names(o)));
            }
        }
    }
    let bodies = find_children(&root, "bodytext");
    facts.push(format!("bodytext count={}", bodies.len()));
    for b in &bodies {
        facts.push(format!("bodytext children: {}", child_names(b)));
        for o in find_children(b, "object") {
            facts.push(format!("body object type={} children: {}", o.get("type").unwrap_or("<none>"), child_names(o)));
            facts.push(format!("body object child order: {}", o.children.iter().map(|c| c.tag.clone()).collect::<Vec<_>>().join(",")));
            let flags: Vec<String> = ["isTextEditable", "isTextReadOnly", "textAutoFit", "textGravity", "shapeType", "ellipsisType", "isHintTextVisible", "formatVersion"]
                .iter()
                .map(|k| format!("{k}={}", o.get(k).unwrap_or("<none>")))
                .collect();
            facts.push(format!("body object flags: {}", flags.join(",")));
            let attrs: Vec<&str> = o.attrib_names();
            facts.push(format!("body object attrs: {}", attrs.join(",")));
            for t in find_children(o, "text") {
                let mut keys = Counts::new();
                let mut anomalies = Counts::new();
                annotation_facts(t.text.as_deref().unwrap_or(""), &mut keys, &mut anomalies);
                if !keys.is_empty() {
                    facts.push(format!("body annotation keys: {keys:?}"));
                }
                if !anomalies.is_empty() {
                    facts.push(format!("body annotation anomalies: {anomalies:?}"));
                }
            }
            for name in ["spanorderdatalist", "zerolengthspanlist"] {
                for g in find_children(o, name) {
                    let mut kinds = Counts::new();
                    for rec in &g.children {
                        let first = rec
                            .get("val")
                            .and_then(b64)
                            .map(|v| v.split(',').next().unwrap_or("").to_string())
                            .unwrap_or_else(|| "<bad>".into());
                        bump(&mut kinds, format!("type{first}"));
                    }
                    facts.push(format!("{name}: {kinds:?}"));
                }
            }
        }
    }
    let mut objects = Counts::new();
    walk_objects(&root, &mut Vec::new(), &mut objects);
    facts.push(format!("objects: {objects:?}"));
    let mut ids = Counts::new();
    walk_ids(&root, &mut Vec::new(), &mut ids);
    ids.retain(|k, _| !k.starts_with("pagelist/") && !k.starts_with("notetree/pagelist/") && !k.starts_with("contentfilelist/"));
    facts.push(format!("id-bearing nodes outside pages: {ids:?}"));
    let mut misc = Counts::new();
    for n in root.iter() {
        let name = n.local().to_lowercase();
        if matches!(
            name.as_str(),
            "contentfile" | "downloadurl" | "stroke" | "attachment" | "voice" | "extrabundle" | "page" | "layer"
        ) {
            bump(&mut misc, name);
        }
    }
    facts.push(format!("nodes: {misc:?}"));
    Note { trashed, title_edit, text_edit, reason, facts }
}

// ------------------------------------------------------------ round trip

use rs_editor::edit::{self as ed, DocBackend, Now};
use rs_editor::etree::{self, Element as EtEl};
use rs_editor::pyval::{dget, PyInt, Val as E};
use rs_editor::rich;

fn snap_of(xml: &[u8], title: Option<&str>) -> ed::Snapshot {
    ed::Snapshot { title: title.map(str::to_string), resource_xml: xml.to_vec() }
}

fn backend(snap: &rs_doc::doc::DocumentSnapshot) -> DocBackend {
    DocBackend { limits: rs_doc::doc::DocLimits::default(), raw_body: snap.raw_body.clone(), content_type: snap.content_type.clone() }
}

fn child<'a>(e: &'a EtEl, name: &str) -> &'a EtEl {
    e.children.iter().find(|c| c.local() == name).expect("child")
}

fn body_object(root: &EtEl) -> &EtEl {
    child(child(root, "bodytext"), "object")
}

fn title_object(root: &EtEl) -> &EtEl {
    child(child(child(root, "attributes"), "title"), "object")
}

/// Canonical form of every root section except the edited text objects and
/// the bookkeeping attributes an edit advances.
fn carried(root: &EtEl, body_too: bool) -> Vec<String> {
    let mut out = Vec::new();
    for c in &root.children {
        let mut c = c.clone();
        match c.local().as_str() {
            "bodytext" if body_too => continue,
            "attributes" => {
                for k in ["modifiedTime", "fileRevision", "displayModifiedTime"] {
                    c.pop_attr(k);
                }
                c.children.retain(|n| n.local() != "title");
            }
            "notetree" => {
                for n in c.children.iter_mut().filter(|n| n.local() == "bodytext") {
                    n.pop_attr("t");
                    n.pop_attr("h");
                }
            }
            _ => {}
        }
        out.push(etree::canonicalize(&c));
    }
    out
}

/// The edited model; text-anchored objects follow their U+FFFC (the k-th
/// U+FFFC of the new text gets the k-th anchor) and are dropped when the
/// count no longer matches (the edit then must be refused).
fn with_text(model: &E, text: String, spans: Vec<E>, paragraphs: Vec<E>) -> E {
    let anchors = rich::model_objects(model);
    let at: Vec<i128> = text.encode_utf16().enumerate().filter(|(_, u)| *u == 0xfffc).map(|(i, _)| i as i128).collect();
    let mut fields = vec![
        (E::str("schemaVersion"), E::int(1)),
        (E::str("text"), E::Str(text)),
        (E::str("spans"), E::List(spans)),
        (E::str("paragraphs"), E::List(paragraphs)),
    ];
    if !anchors.is_empty() && at.len() == anchors.len() {
        let moved = anchors.iter().zip(at).map(|(o, start)| {
            let E::Dict(d) = o else { unreachable!() };
            E::Dict(d.iter().map(|(k, v)| if k.as_str() == Some("start") { (k.clone(), E::int(start)) } else { (k.clone(), v.clone()) }).collect())
        });
        fields.push((E::str("objects"), E::List(moved.collect())));
    }
    E::Dict(fields)
}

fn shifted(items: &[E], key_pairs: &[(&str, i128)]) -> Vec<E> {
    items
        .iter()
        .map(|item| {
            let E::Dict(d) = item else { unreachable!() };
            E::Dict(
                d.iter()
                    .map(|(k, v)| {
                        let shift = key_pairs.iter().find(|(name, _)| k.as_str() == Some(*name)).map(|(_, s)| *s);
                        match shift {
                            Some(s) => (k.clone(), E::int(v.exact_i64().unwrap() as i128 + s)),
                            None => (k.clone(), v.clone()),
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}

#[derive(Default)]
struct Tally(BTreeMap<String, usize>);
impl Tally {
    fn add(&mut self, k: &str, ok: bool) {
        *self.0.entry(format!("{k}: {}", if ok { "ok" } else { "FAIL" })).or_default() += 1;
    }
}

/// One edit through prepare_editor_edit and its invariants.
fn check_edit(t: &mut Tally, label: &str, snap: &rs_doc::doc::DocumentSnapshot, root: &EtEl, edited: &E) {
    let be = backend(snap);
    let s = snap_of(&snap.resource_xml, Some(&snap.title));
    let now = rs_editor::rich::integer(root.get("syncModifiedTime")).unwrap() + 1000;
    let anchored = !rich::model_objects(&ed::editor_projection(Ok(&s), &be).unwrap().model.unwrap()).is_empty();
    let out = match ed::prepare_editor_edit(&s, &E::Str(snap.title.clone()), edited, &Now::Int(PyInt::from_i128(now)), &be) {
        Ok(o) => o,
        Err(e) if anchored && rich::model_objects(edited).is_empty() => {
            t.add(&format!("{label} refused: the edit drops a text-anchored object"), e.message() == ed::ANCHOR_KEPT);
            return;
        }
        Err(e) => {
            t.add(&format!("{label} prepare ({})", e.message()), false);
            return;
        }
    };
    let new_root = etree::parse(&out.full_xml).expect("draft xml");
    t.add(&format!("{label} pages/content/tree carried byte-equal (canonical)"), carried(root, true) == carried(&new_root, true));
    t.add(&format!("{label} title object unchanged"), etree::canonicalize(title_object(root)) == etree::canonicalize(title_object(&new_root)));
    let reread = ed::object_rich(body_object(&new_root));
    let expected = rich::normalize_model(edited).unwrap();
    t.add(&format!("{label} body re-read equals edit"), reread.as_ref().map(|r| rs_editor::pyval::py_eq(&r.model, &rich::without_object_types(&expected))).unwrap_or(false));
    // The draft must itself be editable again (same gate as a Cloud readback).
    let p = ed::editor_projection(Ok(&snap_of(&out.full_xml, Some(&snap.title))), &be).unwrap();
    t.add(&format!("{label} draft projection textEdit+model"), p.text_edit && p.model.as_ref().map(|m| rs_editor::pyval::py_eq(m, &expected)).unwrap_or(false));
    let up = etree::parse(&out.upload_xml).unwrap();
    let pages_empty = up.children.iter().filter(|c| matches!(c.local().as_str(), "pagelist" | "contentfilelist")).all(|c| c.children.is_empty() && c.attrib.is_empty());
    t.add(&format!("{label} upload carries no page/content data"), pages_empty && out.requested_ids.len() == 1);
    if rich::model_text(&expected).is_empty() {
        let has_text = body_object(&new_root).children.iter().any(|c| c.local() == "text");
        let slices = reread.map(|r| r.sections.len()).unwrap_or(0);
        t.add(&format!("{label} empty body: <text> absent iff no page slices remain"), has_text == (slices > 0));
    }
}

/// Positions (UTF-16) with the instruction tokens there, values masked.
fn events(encoded: &str) -> (Vec<(usize, Vec<String>)>, Vec<usize>) {
    let stream = b64(encoded).unwrap_or_default();
    let mut out = Vec::new();
    let mut chunks = Vec::new();
    let mut pos = 0usize;
    let mut rest = stream.as_str();
    loop {
        let (chunk, next) = match rest.find("<?a") {
            Some(i) => (&rest[..i], Some(i)),
            None => (rest, None),
        };
        let text = b64(chunk).unwrap_or_default();
        chunks.push(chunk.len());
        pos += text.encode_utf16().count();
        let Some(i) = next else { break };
        let after = &rest[i + 3..];
        let close = after.find("?>").unwrap_or(after.len());
        let inner = &after[..close];
        let mut toks = Vec::new();
        for part in inner.split(" '").skip(1) {
            let key = part.split('\'').next().unwrap_or("");
            toks.push(format!("{key}{}", if part.contains("='") { "=" } else { "" }));
        }
        out.push((pos, toks));
        rest = &after[(close + 2).min(after.len())..];
    }
    (out, chunks)
}

fn byte_diff_classes(a: &rich::Rich, b: &rich::Rich) -> Vec<String> {
    let mut classes = Vec::new();
    if a.span_order != b.span_order {
        classes.push(if a.span_order.len() == b.span_order.len() { "spanOrderData records differ (same count)" } else { "spanOrderData count differs" }.to_string());
    }
    if a.zero_length != b.zero_length {
        classes.push("zeroLengthSpan records differ".into());
    }
    let (ea, ca) = events(&a.encoded);
    let (eb, cb) = events(&b.encoded);
    if ea != eb {
        let flat = |e: &[(usize, Vec<String>)]| {
            let mut v: Vec<(usize, String)> = e.iter().flat_map(|(p, t)| t.iter().map(move |x| (*p, x.clone()))).collect();
            v.sort();
            v
        };
        let merged = |e: &[(usize, Vec<String>)]| {
            let mut m: BTreeMap<usize, Vec<String>> = BTreeMap::new();
            for (p, t) in e {
                m.entry(*p).or_default().extend(t.iter().cloned());
            }
            m
        };
        if flat(&ea) != flat(&eb) {
            let (fa, fb) = (flat(&ea), flat(&eb));
            let mut kinds: BTreeMap<String, (usize, usize)> = BTreeMap::new();
            for x in fa.iter().filter(|x| !fb.contains(x)) {
                kinds.entry(x.1.clone()).or_default().0 += 1;
            }
            for x in fb.iter().filter(|x| !fa.contains(x)) {
                kinds.entry(x.1.clone()).or_default().1 += 1;
            }
            for (k, (only_a, only_b)) in kinds {
                classes.push(format!("moved token {k} (native-only {}, ours-only {})", only_a.min(1), only_b.min(1)));
            }
        } else if merged(&ea) != merged(&eb) {
            classes.push("same tokens per position, different order".into());
        } else {
            classes.push("same tokens, split into different instructions".into());
        }
    } else if a.encoded != b.encoded {
        if ca != cb {
            classes.push("text chunk base64 split differs".into());
        } else {
            classes.push("token values or spacing differ".into());
        }
    }
    if classes.is_empty() {
        classes.push("unclassified".into());
    }
    classes
}

fn roundtrip(t: &mut Tally, document: &Val) {
    let Ok(snap) = snapshot::from_dict(document) else { return t.add("snapshot", false) };
    let root = etree::parse(&snap.resource_xml).unwrap();
    let be = backend(&snap);
    let p = ed::editor_projection(Ok(&snap_of(&snap.resource_xml, Some(&snap.title))), &be).unwrap();
    t.add("projection textEdit", p.text_edit);
    t.add("projection titleEdit", p.title_edit);
    if !p.text_edit {
        return;
    }
    let model = p.model.clone().unwrap();
    let body = ed::object_rich(body_object(&root)).unwrap();
    // 1. codec: parse -> serialize with no edit.
    let values: Vec<String> = body.objects.iter().map(|(_, raw)| raw.clone()).collect();
    match rich::encode_with_objects(body.model.clone(), body.sections.clone(), &values) {
        Ok(again) => {
            let byte = again.encoded == body.encoded && again.span_order == body.span_order && again.zero_length == body.zero_length;
            t.add("no-edit re-encode byte-identical", byte);
            if !byte {
                for class in byte_diff_classes(&body, &again) {
                    t.add(&format!("  byte difference class: {class}"), true);
                }
            }
            let records = |v: &[String]| -> Vec<rich::Record> { v.iter().cloned().map(Ok).collect() };
            let back = rich::read_rich_text(&again.encoded, &records(&again.span_order), &records(&again.zero_length)).unwrap();
            t.add("no-edit re-encode document-equivalent (model+slices+anchors)",
                rs_editor::pyval::py_eq(&back.model, &body.model) && back.sections == body.sections && back.objects == body.objects);
        }
        Err(e) => t.add(&format!("no-edit re-encode ({})", e.message()), false),
    }
    // 2. an unchanged save is refused (never re-encoded).
    let now = rich::integer(root.get("syncModifiedTime")).unwrap() + 1000;
    let same = ed::prepare_editor_edit(&snap_of(&snap.resource_xml, Some(&snap.title)), &E::Str(snap.title.clone()), &model, &Now::Int(PyInt::from_i128(now)), &be);
    t.add("unchanged save refused", matches!(&same, Err(e) if e.message() == "Kaydedilecek bir değişiklik yok."));
    // 3. edits.
    let d = model.dict().unwrap();
    let text = dget(d, "text").unwrap().as_str().unwrap().to_string();
    let spans = dget(d, "spans").unwrap().list().unwrap().clone();
    let paragraphs = dget(d, "paragraphs").unwrap().list().unwrap().clone();
    if text.is_empty() {
        check_edit(t, "empty body -> text", &snap, &root, &with_text(&model, "Linux".into(), vec![], vec![]));
    } else {
        check_edit(t, "append at end", &snap, &root, &with_text(&model, format!("{text} X"), spans.clone(), paragraphs.clone()));
        let pre_spans = shifted(&spans, &[("start", 2), ("end", 2)]);
        let pre_paras = shifted(&paragraphs, &[("index", 1)]);
        check_edit(t, "insert paragraph at start", &snap, &root, &with_text(&model, format!("Y\n{text}"), pre_spans, pre_paras));
        let units = rich::utf16_length(&text) as i128;
        let bold_free = !spans.iter().any(|s| s.dict().and_then(|d| dget(d, "kind")).and_then(E::as_str) == Some("bold"));
        if bold_free && text.chars().next().map(|c| c.len_utf16() == 1).unwrap_or(false) {
            let mut with_bold = spans.clone();
            with_bold.push(E::Dict(vec![
                (E::str("start"), E::int(0)),
                (E::str("end"), E::int(1.min(units))),
                (E::str("kind"), E::str("bold")),
                (E::str("value"), E::Bool(true)),
                (E::str("interval"), E::int(3)),
            ]));
            check_edit(t, "bold first character", &snap, &root, &with_text(&model, text.clone(), with_bold, paragraphs.clone()));
        }
        check_edit(t, "clear body", &snap, &root, &with_text(&model, String::new(), vec![], vec![]));
    }
    // 4. title edit keeps the body and pages.
    let s = snap_of(&snap.resource_xml, Some(&snap.title));
    match ed::prepare_title_edit(&s, &E::str("Survey title"), &Now::Int(PyInt::from_i128(now)), &be) {
        Ok(out) => {
            let new_root = etree::parse(&out.full_xml).unwrap();
            t.add("title edit carries body/pages", carried(&root, false).len() == carried(&new_root, false).len()
                && etree::canonicalize(body_object(&root)) == etree::canonicalize(body_object(&new_root))
                && carried(&root, true) == carried(&new_root, true));
            t.add("title edit re-read", ed::object_rich(title_object(&new_root)).map(|r| rich::model_text(&r.model) == "Survey title").unwrap_or(false));
        }
        Err(e) => t.add(&format!("title edit ({})", e.message()), false),
    }
}

trait AttribNames {
    fn attrib_names(&self) -> Vec<&str>;
}
impl AttribNames for Element {
    fn attrib_names(&self) -> Vec<&str> {
        self.attrs.iter().map(|(k, _)| rs_doc::xml::local_name(k)).collect()
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: editor_survey <private copy.sqlite3>");
    assert!(
        !path.contains("/.local/state/"),
        "refusing to open the live library; make an online backup copy first"
    );
    let db = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open copy");
    let mut stmt = db
        .prepare("SELECT base_document, trashed FROM notes WHERE base_document IS NOT NULL ORDER BY rowid")
        .expect("query");
    let rows: Vec<(String, bool)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)))
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    let verbose = std::env::args().any(|a| a == "--verbose");
    if std::env::args().any(|a| a == "--roundtrip") {
        let mut t = Tally::default();
        for (raw, _) in &rows {
            roundtrip(&mut t, &rs_store::val::loads(raw).expect("json"));
        }
        println!("== round trip over {} notes (counts of checks) ==", rows.len());
        for (k, v) in &t.0 {
            println!("{v:4}  {k}");
        }
        return;
    }
    let mut by_reason: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (i, (raw, trashed)) in rows.iter().enumerate() {
        let document = rs_store::val::loads(raw).expect("json");
        let note = survey(&document, *trashed);
        let caps = format!(
            "{}{}",
            if note.title_edit { "T" } else { "-" },
            if note.text_edit { "B" } else { "-" }
        );
        *by_reason.entry((caps.clone(), note.reason.clone())).or_default() += 1;
        if verbose || !note.text_edit {
            println!("#{i:02} trashed={} caps={caps} reason={:?}", note.trashed, note.reason);
            for f in &note.facts {
                println!("    {f}");
            }
        }
    }
    println!("\n== aggregate (caps T=title B=body) ==");
    for ((caps, reason), n) in &by_reason {
        println!("{n:3}  {caps}  {reason}");
    }
}
