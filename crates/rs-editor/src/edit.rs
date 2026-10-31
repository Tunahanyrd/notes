//! Document-level editor operations (pure Rust port of the XML half of
//! samsung_notes/editor_document.py). Calls into not-yet-ported document
//! functions go through the `Backend` trait.
use std::cmp::Ordering;

use sha2::{Digest, Sha256};

use crate::etree::{self, Element};
use crate::pyval::*;
use crate::rich::*;

/// The fields of a resolved samsung_notes.document.DocumentSnapshot used here.
pub struct Snapshot {
    pub title: Option<String>,
    pub resource_xml: Vec<u8>,
}

/// Functions still provided by samsung_notes.document (area `doc`).
pub trait Backend {
    /// object_hash(id, tick) digest bytes.
    fn object_hash(&self, id: Option<&str>, tick: i128) -> Result<Vec<u8>>;
    /// rs_doc::doc::editor_structure(ET.fromstring(resource_xml)).
    fn editor_structure(&self, resource_xml: &[u8]) -> Result<()>;
}

/// Version of the capability rules below. Cached projections computed under
/// another recipe are recomputed (rs-store drops them on open).
/// v2: text objects next to page objects (handwriting, images, shapes) and
/// binary parts; native empty text boxes without `<text>`; 32768 annotations.
/// v3: also next to free-floating objects the shared rule carries
/// (`rs_doc::objects::floating_inventory`, e.g. a picture inserted on a phone).
/// v4: body text that anchors such an object with `objectSpan` (U+FFFC);
/// the model names each anchor (`objects`), the anchors stay in place.
pub const PROJECTION_RECIPE: &str = "editor-projection-v5";

pub const ANCHOR_MISSING: &str = "Metne yerleşik bir nesne notta bulunamadı; metin güvenle düzenlenemiyor.";
pub const ANCHOR_IN_TITLE: &str = "Başlığa yerleşik nesne içeren notlar henüz düzenlenemiyor.";
pub const ANCHOR_KEPT: &str = "Metne yerleşik resim veya nesneler bu sürümde silinemez ya da yer değiştiremez.";

/// The `now_ms` argument after defaulting: an int, or a non-int object.
pub enum Now { Int(PyInt), NotInt }

pub struct EditOut {
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub requested_ids: Vec<Option<String>>,
    pub title: String,
    /// None: the snapshot's own body object.
    pub body: Option<String>,
}

/// Pure-Rust backend over the rs-doc rlib.
pub struct DocBackend {
    pub limits: rs_doc::doc::DocLimits,
    /// The snapshot's raw MIME body and content type.
    pub raw_body: Vec<u8>,
    pub content_type: String,
}

impl Backend for DocBackend {
    fn object_hash(&self, id: Option<&str>, tick: i128) -> Result<Vec<u8>> {
        Ok(rs_doc::writer::object_hash(id.unwrap_or(""), tick)?.to_vec())
    }
    fn editor_structure(&self, resource_xml: &[u8]) -> Result<()> {
        let resource = rs_doc::xml::fromstring(resource_xml)?;
        rs_doc::doc::editor_structure(&resource, &self.limits.payload, self.limits.max_text)?;
        Ok(())
    }
}

pub struct Projection {
    pub title_edit: bool,
    pub text_edit: bool,
    pub model: Option<Val>,
    pub reason: Option<String>,
}

pub fn parse_xml(data: &[u8]) -> Result<Element> {
    etree::parse(data).map_err(|e| Error::Parse(e.0))
}

/// document.one
pub fn one(parent: &Element, name: &str) -> Result<usize> {
    let found: Vec<usize> = parent.children.iter().enumerate().filter(|(_, n)| n.local() == name).map(|(i, _)| i).collect();
    if found.len() != 1 { return doc("Belgenin yapısı güvenli düzenleme için uygun değil."); }
    Ok(found[0])
}

fn sidecar(n: &Element) -> bool {
    matches!(n.local().as_str(), "spanorderdatalist" | "zerolengthspanlist")
}

fn records(obj: &Element, name: &str) -> Result<Vec<Record>> {
    let groups: Vec<&Element> = obj.children.iter().filter(|n| n.local() == name).collect();
    if groups.len() > 1 { return doc("Tekrarlanan biçimlendirme metadata bilgisi."); }
    let mut values = Vec::new();
    let expected = if name == "spanorderdatalist" { "spanorderdata" } else { "zerolengthspan" };
    if let Some(group) = groups.first() {
        for n in &group.children {
            if n.attrib.len() != 1 || n.local() != expected || !n.children.is_empty() {
                return doc("Biçimlendirme metadata biçimi desteklenmiyor.");
            }
            values.push(Ok(decode(&n.attrib[0].1)?));
            if values.len() > MAX_ANNOTATIONS { return doc("Biçimlendirme metadata sınırı aşıldı."); }
        }
    }
    Ok(values)
}

/// Index of the text object's `<text>` child; None for a native empty text
/// box (Samsung omits `<text>` when the box holds no text).
fn text_child(obj: &Element) -> Result<Option<usize>> {
    let found: Vec<usize> = obj.children.iter().enumerate().filter(|(_, n)| n.local() == "text").map(|(i, _)| i).collect();
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found[0])),
        _ => doc("Belgenin yapısı güvenli düzenleme için uygun değil."),
    }
}

/// editor_document._object_rich
pub fn object_rich(obj: &Element) -> Result<Rich> {
    let span_order = records(obj, "spanorderdatalist")?;
    let zero_length = records(obj, "zerolengthspanlist")?;
    let text = match text_child(obj)? {
        Some(i) => obj.children[i].text.clone().unwrap_or_default(),
        None if span_order.is_empty() && zero_length.is_empty() => String::new(),
        None => return doc("Belgenin yapısı güvenli düzenleme için uygun değil."),
    };
    read_rich_text(&text, &span_order, &zero_length)
}

/// True when a rich value has no text, formatting or page slices at all.
pub fn rich_is_empty(rich: &Rich) -> bool {
    model_text(&rich.model).is_empty() && model_spans(&rich.model).is_empty()
        && model_paragraphs(&rich.model).is_empty() && rich.sections.is_empty() && rich.objects.is_empty()
}

/// editor_document._put_rich. A missing `<text>` (native empty text box) is
/// added as the last child. With `native_empty`, an empty value is written
/// the native way: Samsung clients store an empty title/body text box without
/// a `<text>` child (seen on every handwriting-only and untitled device note).
pub fn put_rich(obj: &mut Element, rich: &Rich, native_empty: bool) -> Result<()> {
    text_child(obj)?;
    obj.children.retain(|n| !sidecar(n));
    if native_empty && rich_is_empty(rich) {
        obj.children.retain(|n| n.local() != "text");
        return Ok(());
    }
    if text_child(obj)?.is_none() {
        obj.children.push(Element::new("text"));
    }
    for (tag, child, values) in [("spanOrderDataList", "spanOrderData", &rich.span_order),
                                 ("zeroLengthSpanList", "zeroLengthSpan", &rich.zero_length)] {
        if !values.is_empty() {
            let mut group = Element::new(tag);
            for value in values.iter() {
                let mut c = Element::new(child);
                c.attrib.push(("val".into(), b64(value)));
                group.children.push(c);
            }
            obj.children.push(group);
        }
    }
    let t = one(obj, "text")?;
    obj.children[t].text = Some(rich.encoded.clone());
    Ok(())
}

/// editor_document._resource after the snapshot object itself was resolved.
pub fn resource(snap: &Snapshot) -> Result<Element> {
    let root = parse_xml(&snap.resource_xml)?;
    let truthy = |n: &Element, key: &str| n.get(key).unwrap_or("false").to_lowercase() == "true";
    if root.get("isLocked").unwrap_or("false") != "false"
        || root.iter().into_iter().any(|n| truthy(n, "encrypted") || truthy(n, "isEncrypted")) {
        return doc("Kilitli veya şifreli not düzenlenemez.");
    }
    Ok(root)
}

fn utf8_ok(data: &[u8]) -> bool {
    !(data.len() > 8 * 1024 * 1024 || data.windows(2).any(|w| w == b"<!") || data.contains(&0))
}

pub fn title_content_fingerprint(resource_xml: &[u8]) -> Result<String> {
    if !utf8_ok(resource_xml) { return doc("Belge parmak izi girdisi geçersiz."); }
    let mut root = parse_xml(resource_xml)?;
    if root.local() != "noteresource" { return doc("Tam not kaynağı gerekli."); }
    root.pop_attr("syncModifiedTime");
    let namespace = match root.tag.rfind('}') { Some(i) => root.tag[..=i].to_string(), None => String::new() };
    root.tag = format!("{namespace}noteResource");
    root.iter_mut_apply(&mut |node: &mut Element| {
        if !node.children.is_empty() && node.text.as_deref().map_or(true, |t| py_strip(t).is_empty()) {
            node.text = None;
            for child in &mut node.children {
                if child.tail.as_deref().is_some_and(|t| py_strip(t).is_empty()) { child.tail = None; }
            }
        }
    });
    Ok(hex(&Sha256::digest(etree::canonicalize(&root).as_bytes())))
}

fn drop_empty_single(parent: &mut Element, tag: &str) {
    let matches: Vec<usize> = parent.children.iter().enumerate().filter(|(_, c)| c.tag == tag).map(|(i, _)| i).collect();
    if matches.len() != 1 { return; }
    let node = &parent.children[matches[0]];
    if node.attrib.is_empty() && node.children.is_empty() && py_strip(node.text.as_deref().unwrap_or("")).is_empty() {
        parent.children.remove(matches[0]);
    }
}

pub fn editor_content_fingerprint(resource_xml: &[u8]) -> Result<String> {
    title_content_fingerprint(resource_xml)?;
    let mut root = parse_xml(resource_xml)?;
    // Independent decisions: layer objectLists first, then the root lists.
    for pages in root.children.iter_mut().filter(|c| c.tag == "pageList") {
        for page in pages.children.iter_mut().filter(|c| c.tag == "page") {
            for layers in page.children.iter_mut().filter(|c| c.tag == "layerList") {
                for layer in layers.children.iter_mut().filter(|c| c.tag == "layer") {
                    drop_empty_single(layer, "objectList");
                }
            }
        }
    }
    drop_empty_single(&mut root, "contentFileList");
    drop_empty_single(&mut root, "floatingObjectList");
    title_content_fingerprint(&etree::xml_bytes(&root))
}

fn check_now_after(now: &Now, root: &Element, strict_upper: bool) -> Result<PyInt> {
    let bad = || Error::Document("Saat bilgisi son Cloud sürümünden yeni değil.".into());
    let Now::Int(n) = now else { return Err(bad()) };
    let sync = integer(root.get("syncModifiedTime"))?;
    if n.cmp_i128(sync) != Ordering::Greater { return Err(bad()); }
    if strict_upper && n.cmp_i128(i64::MAX as i128) == Ordering::Greater { return Err(bad()); }
    Ok(n.clone())
}

pub fn prepare_title_edit(snap: &Snapshot, title: &Val, now: &Now, be: &dyn Backend) -> Result<EditOut> {
    let mut root = resource(snap)?;
    let title = text_value(title)?;
    if title.contains('\n') || title.contains('\r') { return doc("Başlık tek satır olmalı."); }
    if snap.title.as_deref() == Some(title.as_str()) { return doc("Kaydedilecek başlık değişikliği yok."); }
    let ai = one(&root, "attributes")?;
    let ti = one(&root, "notetree")?;
    let tti = one(&root.children[ai], "title")?;
    let oi = one(&root.children[ai].children[tti], "object")?;
    let obj = &root.children[ai].children[tti].children[oi];
    if obj.get("type") != Some("textBox") { return doc("Başlık nesnesinin biçimi desteklenmiyor."); }
    let old = object_rich(obj)?;
    let old_text = model_text(&old.model).to_string();
    let length = utf16_length(&old_text) as i128;
    let new_length = utf16_length(&title) as i128;
    if old_text.contains('\n') || old_text.contains('\r') { return doc("Çok satırlı biçimlendirilmiş başlık desteklenmiyor."); }
    if !old.sections.is_empty() { return doc("Sayfa bölümü içeren başlık desteklenmiyor."); }
    if !old.objects.is_empty() { return doc(ANCHOR_IN_TITLE); }
    let mut model = old.model.clone();
    let Val::Dict(fields) = &mut model else { unreachable!() };
    *dget_mut(fields, "text").unwrap() = Val::Str(title.clone());
    if let Some(Val::List(spans)) = dget_mut(fields, "spans") {
        for span in spans.iter_mut() {
            let Val::Dict(s) = span else { unreachable!() };
            let start = dget(s, "start").unwrap().exact_i64().unwrap() as i128;
            let end = dget(s, "end").unwrap().exact_i64().unwrap() as i128;
            if start == 0 && end == length {
                *dget_mut(s, "end").unwrap() = Val::int(new_length);
            } else if start == end && end == length {
                *dget_mut(s, "start").unwrap() = Val::int(new_length);
                *dget_mut(s, "end").unwrap() = Val::int(new_length);
            } else {
                return doc("Başlığın kısmi biçimlendirmesini güvenle yeniden eşleyemiyoruz.");
            }
        }
    }
    if title.is_empty() { *dget_mut(fields, "paragraphs").unwrap() = Val::List(vec![]); }
    let rich = encode_rich_text(&model, &[])?;
    put_rich(&mut root.children[ai].children[tti].children[oi], &rich, true)?;
    let now = check_now_after(now, &root, false)?;
    let attrs = &root.children[ai];
    let obj = &attrs.children[tti].children[oi];
    let tick = integer(attrs.get("modifiedTime"))?.max(integer(obj.get("modifiedTime"))?) + 1;
    let revision = integer(attrs.get("fileRevision"))? + 1;
    if tick > i64::MAX as i128 || revision > i32::MAX as i128 { return doc("Belge sürüm sınırı aşıldı."); }
    let hash = hex(&be.object_hash(obj.get("id"), tick)?);
    {
        let obj = &mut root.children[ai].children[tti].children[oi];
        obj.set("modifiedTime", tick.to_string());
        obj.set("hash", hash);
    }
    {
        let attrs = &mut root.children[ai];
        attrs.set("modifiedTime", tick.to_string());
        attrs.set("fileRevision", revision.to_string());
        attrs.set("displayModifiedTime", now.0.clone());
    }
    root.set("syncModifiedTime", now.0.clone());
    let mut upload = root.clone();
    upload.tag = "NoteResource".into();
    let mut kept = Vec::new();
    for mut n in std::mem::take(&mut upload.children) {
        match n.local().as_str() {
            "bodytext" | "floatingobjectlist" => {}
            "pagelist" | "contentfilelist" => { n.clear(); kept.push(n); }
            "notetree" | "attributes" => kept.push(n),
            _ => return doc("Bilinmeyen belge bölümü düzenlenemez."),
        }
    }
    upload.children = kept;
    Ok(EditOut { full_xml: etree::xml_bytes(&root), tree_xml: etree::xml_bytes(&root.children[ti]),
        upload_xml: etree::xml_bytes(&upload), requested_ids: vec![], title, body: None })
}

/// The model with each text anchor's object `type`, looked up in the note's
/// `floatingObjectList` (the only objects the editor carries for an anchor;
/// the native writer refuses an anchor whose object it cannot find,
/// `n1.h.f` "setBodyObjectToObjectSpan"). Err: an anchor names no such object.
fn typed_anchors(model: &Val, root: &Element) -> Result<Val> {
    if model_objects(model).is_empty() { return Ok(model.clone()); }
    let floating: Vec<&Element> = root.children.iter().filter(|n| n.local() == "floatingobjectlist")
        .flat_map(|list| list.children.iter()).collect();
    let mut model = model.clone();
    let Val::Dict(fields) = &mut model else { unreachable!() };
    let Some(Val::List(objects)) = dget_mut(fields, "objects") else { unreachable!() };
    for object in objects.iter_mut() {
        let Val::Dict(o) = object else { unreachable!() };
        let id = dget(o, "objectId").and_then(Val::as_str).unwrap_or("").to_string();
        let found: Vec<&&Element> = floating.iter().filter(|n| n.local() == "object" && n.get("id") == Some(id.as_str())).collect();
        let kind = match found.as_slice() {
            [one] => one.get("type").filter(|t| rs_doc::objects::PASSTHROUGH_TYPES.contains(t)),
            _ => None,
        };
        let Some(kind) = kind else { return doc(ANCHOR_MISSING) };
        o.retain(|(k, _)| k.as_str() != Some("type"));
        o.push((Val::str("type"), Val::str(kind)));
    }
    Ok(model)
}

fn projection_inner(snap: Result<&Snapshot>, be: &dyn Backend, p: &mut Projection) -> Result<()> {
    let snap = snap?;
    let root = resource(snap)?;
    let inner = (|| -> Result<EditOut> {
        let now = integer(root.get("syncModifiedTime"))? + 1;
        let synthetic = if snap.title.as_deref() != Some("Editör yetenek denetimi") { "Editör yetenek denetimi" } else { "Başlık denetimi" };
        prepare_title_edit(snap, &Val::str(synthetic), &Now::Int(PyInt::from_i128(now)), be)
    })();
    match inner {
        Ok(_) => p.title_edit = true,
        Err(e) if e.is_value_error() => {}
        Err(e) => return Err(e),
    }
    let bi = one(&root, "bodytext")?;
    let boi = one(&root.children[bi], "object")?;
    let rich = object_rich(&root.children[bi].children[boi])?;
    p.model = Some(rich.model.clone());
    p.model = Some(typed_anchors(&rich.model, &root)?);
    let mut temporary = root.clone();
    {
        let body = &mut temporary.children[bi].children[boi];
        body.children.retain(|n| !sidecar(n));
        if text_child(body)?.is_none() { body.children.push(Element::new("text")); }
        let t = one(body, "text")?;
        body.children[t].text = Some(encode_text(model_text(&rich.model))?);
    }
    let ai = one(&temporary, "attributes")?;
    let tti = one(&temporary.children[ai], "title")?;
    let oi = one(&temporary.children[ai].children[tti], "object")?;
    {
        let temp_title = &mut temporary.children[ai].children[tti].children[oi];
        if !temp_title.children.iter().any(|n| n.local() == "text") { temp_title.children.push(Element::new("text")); }
        let title_rich = object_rich(temp_title)?;
        if !title_rich.objects.is_empty() { return doc(ANCHOR_IN_TITLE); }
        temp_title.children.retain(|n| !sidecar(n));
        let t = one(temp_title, "text")?;
        temp_title.children[t].text = Some(encode_text(model_text(&title_rich.model))?);
    }
    be.editor_structure(&etree::xml_bytes(&temporary))?;
    p.text_edit = true;
    Ok(())
}

pub fn editor_projection(snap: Result<&Snapshot>, be: &dyn Backend) -> Result<Projection> {
    let mut p = Projection { title_edit: false, text_edit: false, model: None, reason: None };
    if let Err(e) = projection_inner(snap, be, &mut p) {
        if !e.is_format_error() { return Err(e); }
        p.reason = Some(if e.is_document_error() { e.message() } else { "Belge biçimi henüz düzenlenemiyor.".into() });
    }
    Ok(p)
}

pub fn new_editor_document<T>(model: &Val, make: impl FnOnce(&str) -> Result<(T, Vec<u8>)>) -> Result<(T, Vec<u8>)> {
    let rich = encode_rich_text(model, &[])?;
    let (identity, xml) = make(model_text(&rich.model))?;
    let mut root = parse_xml(&xml)?;
    let bi = one(&root, "bodytext")?;
    let oi = one(&root.children[bi], "object")?;
    put_rich(&mut root.children[bi].children[oi], &rich, false)?;
    Ok((identity, etree::xml_bytes(&root)))
}

/// The edited model's text anchors against the projected ones: the same
/// objects in the same order (none added, removed or swapped — deleting an
/// anchor would also need the floating object, its note-tree entry and its
/// content file's `refCount` to change, which is not established). Each keeps
/// its projected `type`; only its position follows the text.
fn carry_anchors(normalized: Val, projected: Option<&Val>, baseline: &Rich) -> Result<Val> {
    let expected = projected.map(model_objects).unwrap_or(&[]);
    let edited = model_objects(&normalized);
    let field = |o: &Val, key: &str| dget(o.dict().unwrap(), key).and_then(Val::as_str).map(str::to_string);
    if expected.len() != edited.len() || expected.len() != baseline.objects.len()
        || expected.iter().zip(edited).any(|(a, b)| {
            field(a, "objectId") != field(b, "objectId")
                || field(b, "type").is_some_and(|t| Some(t) != field(a, "type"))
        }) {
        return doc(ANCHOR_KEPT);
    }
    if edited.is_empty() { return Ok(normalized); }
    let mut model = normalized;
    let Val::Dict(fields) = &mut model else { unreachable!() };
    let Some(Val::List(objects)) = dget_mut(fields, "objects") else { unreachable!() };
    for (object, projected) in objects.iter_mut().zip(expected) {
        let Val::Dict(o) = object else { unreachable!() };
        o.retain(|(k, _)| k.as_str() != Some("type"));
        if let Some(t) = dget(projected.dict().unwrap(), "type") { o.push((Val::str("type"), t.clone())); }
    }
    Ok(model)
}

pub fn prepare_editor_edit(snap: &Snapshot, title: &Val, model: &Val, now: &Now, be: &dyn Backend) -> Result<EditOut> {
    let root = resource(snap)?;
    let title = text_value(title)?;
    let capability = editor_projection(Ok(snap), be)?;
    if !capability.text_edit {
        let reason = capability.reason.filter(|r| !r.is_empty()).unwrap_or_else(|| "Bu gövde biçimi henüz düzenlenemiyor.".into());
        return Err(Error::Document(reason));
    }
    let bi = one(&root, "bodytext")?;
    let boi = one(&root.children[bi], "object")?;
    let baseline = object_rich(&root.children[bi].children[boi])?;
    let normalized = carry_anchors(normalize_model(model)?, capability.model.as_ref(), &baseline)?;
    let sections = remap_sections(model_text(&baseline.model), model_text(&normalized), &baseline.sections)?;
    let sections = check_sections(model_text(&normalized), &sections_to_vals(&sections))?;
    let values: Vec<String> = baseline.objects.iter().map(|(_, raw)| raw.clone()).collect();
    let rich = encode_with_objects(normalized, sections, &values)?;
    let body_changed = !py_eq(&rich.model, capability.model.as_ref().unwrap_or(&Val::None));
    let title_changed = snap.title.as_deref() != Some(title.as_str());
    if !body_changed && !title_changed { return doc("Kaydedilecek bir değişiklik yok."); }
    let now_value = check_now_after(now, &root, true)?;
    let mut root = if title_changed {
        parse_xml(&prepare_title_edit(snap, &Val::Str(title.clone()), now, be)?.full_xml)?
    } else { root };
    let ai = one(&root, "attributes")?;
    let ti = one(&root, "notetree")?;
    let bi = one(&root, "bodytext")?;
    let boi = one(&root.children[bi], "object")?;
    let tbi = one(&root.children[ti], "bodytext")?;
    let tick = integer(root.children[ai].get("modifiedTime"))?
        .max(integer(root.children[bi].children[boi].get("modifiedTime"))?)
        .max(integer(root.children[ti].children[tbi].get("t"))?) + 1;
    let revision = integer(root.children[ai].get("fileRevision"))? + if title_changed { 0 } else { 1 };
    if tick > i64::MAX as i128 || revision > i32::MAX as i128 { return doc("Belge sürüm sınırı aşıldı."); }
    let mut requested = Vec::new();
    if body_changed {
        let body = &mut root.children[bi].children[boi];
        put_rich(body, &rich, true)?;
        body.set("modifiedTime", tick.to_string());
        let hash = hex(&be.object_hash(body.get("id"), tick)?);
        body.set("hash", hash.clone());
        let id = body.get("id").map(str::to_string);
        let tree_body = &mut root.children[ti].children[tbi];
        tree_body.set("t", tick.to_string());
        tree_body.set("h", hash);
        requested.push(id);
    }
    {
        let attrs = &mut root.children[ai];
        attrs.set("modifiedTime", tick.to_string());
        attrs.set("fileRevision", revision.to_string());
        attrs.set("displayModifiedTime", now_value.0.clone());
    }
    root.set("syncModifiedTime", now_value.0.clone());
    let mut upload = root.clone();
    upload.tag = "NoteResource".into();
    let mut kept = Vec::new();
    for mut n in std::mem::take(&mut upload.children) {
        let name = n.local();
        if name == "pagelist" || name == "contentfilelist" { n.clear(); kept.push(n); }
        else if name == "floatingobjectlist" || (name == "bodytext" && !body_changed) {}
        else if matches!(name.as_str(), "notetree" | "attributes" | "bodytext") { kept.push(n); }
        else { return doc("Bilinmeyen belge bölümü düzenlenemez."); }
    }
    upload.children = kept;
    Ok(EditOut { full_xml: etree::xml_bytes(&root), tree_xml: etree::xml_bytes(&root.children[ti]),
        upload_xml: etree::xml_bytes(&upload), requested_ids: requested, title,
        body: Some(model_text(&rich.model).to_string()) })
}
