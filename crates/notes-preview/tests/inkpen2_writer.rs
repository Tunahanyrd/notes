//! A stroke our writer appends (rs-ink recipe 2/3, the PUT /api/notes/:id/ink
//! job) to an InkPen2 page must stay renderable by the local page renderer.
//! The writer copies the template stroke's pen, tool and brush bytes but has no
//! native tilt/orientation, so it clears the stylus-axis bit. Synthetic data only.
use notes_preview::ink::{prepare, InkBackend};
use notes_preview::Val;
use rs_ink::core::binary::{decode_stroke_resource, RawSamples};
use rs_ink::core::prepare::{payload_parts, prepare_append, AppendRecipe, AppendSnapshot};

const FOUNTAIN: &str = "com.samsung.android.sdk.pen.pen.preload.FountainPen";
const INKPEN2: &str = "com.samsung.android.sdk.pen.pen.preload.InkPen2";
const NEW: &str = "b0000000-0000-4000-8000-000000000002";
const BOUNDARY: &str = "InkPen2WriterTest";

fn fountain18() -> Val {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ink_cases.json")).unwrap();
    let Val::Dict(items) = rs_store::val::loads(&text).unwrap() else { panic!() };
    items.into_iter().find(|(n, _)| n == "fountain18").unwrap().1.get("snapshot").unwrap().clone()
}

fn multipart(parts: &[(String, Option<String>, String, Vec<u8>)]) -> Vec<u8> {
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

fn reparse(raw: Vec<u8>) -> Val {
    let ct = format!("multipart/form-data;boundary={BOUNDARY}");
    let doc = notes_host::snapshot::parse_document(&raw, &ct, Some("synthetic-server")).unwrap();
    notes_host::snapshot::to_dict(&doc)
}

fn replace(haystack: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let at = haystack.windows(from.len()).position(|w| w == from).expect("pattern present");
    assert!(haystack[at + 1..].windows(from.len()).all(|w| w != from), "pattern unique");
    [&haystack[..at], to, &haystack[at + from.len()..]].concat()
}

/// The synthetic Fountain note turned into what a Samsung S Pen InkPen2 page
/// looks like: InkPen2 pen name, `settings`, tool 2, stylus channels kept.
fn inkpen2_note(settings: &str) -> Val {
    let snap = notes_host::snapshot::from_dict(&fountain18()).unwrap();
    let parts = payload_parts(&snap.raw_body, &snap.content_type, &Default::default()).unwrap();
    let b64 = |s: &str| rs_doc::util::b64encode(s.as_bytes()).into_bytes();
    let mut out = Vec::new();
    for p in parts {
        let mut body = p.body.clone();
        match p.name.as_deref() {
            Some("noteResource") => {
                body = replace(&body, &b64(FOUNTAIN), &b64(INKPEN2));
                body = replace(&body, &b64("18;0;100;"), &b64(settings));
            }
            Some("strokeResource") => {
                let record = decode_stroke_resource(&body).unwrap();
                assert!(record.stylus, "the Samsung-like source keeps its stylus channels");
                let tool = 12 + record.frames[0].raw.len() + record.frames[1].flexible_start as usize - 2;
                body[tool..tool + 2].copy_from_slice(&2u16.to_le_bytes());
                assert_eq!(decode_stroke_resource(&body).unwrap().tool_type, 2);
            }
            _ => {}
        }
        out.push((p.name.unwrap(), p.filename, p.content_type, body));
    }
    reparse(multipart(&out))
}

/// The document after our recipe 2/3 append, as the Cloud readback returns it:
/// the writer's full resource plus every stroke part (old and new).
fn appended(note: &Val, recipe: AppendRecipe) -> (Val, rs_ink::core::binary::Record) {
    let snap = notes_host::snapshot::from_dict(note).unwrap();
    let source_id = first_stroke_id(&snap.resource_xml);
    let samples = RawSamples::from_parts(&[(40.0, 40.0), (60.0, 48.0), (80.0, 70.0), (100.0, 80.0)], &[0.3, 0.5, 0.7, 0.6], &[0, 8, 16, 24]);
    let edit = prepare_append(&AppendSnapshot::from_doc(&snap), &source_id, NEW, &samples, 1_800_000_000_000, recipe, None).unwrap();
    let parts = payload_parts(&snap.raw_body, &snap.content_type, &Default::default()).unwrap();
    let mut out = Vec::new();
    let mut view = b"<CompareNoteView>".to_vec();
    let start = edit.full_xml.iter().position(|b| *b == b'<').unwrap();
    let mut xml = edit.full_xml[start..].to_vec();
    if xml.starts_with(b"<?xml") {
        let end = xml.windows(2).position(|w| w == b"?>").unwrap() + 2;
        xml = xml[end..].to_vec();
    }
    view.extend_from_slice(&xml);
    view.extend_from_slice(b"<commitId>synthetic-commit-2</commitId></CompareNoteView>");
    out.push(("noteResource".to_string(), None, "application/xml".to_string(), view));
    for p in parts.into_iter().filter(|p| p.name.as_deref() == Some("strokeResource")) {
        out.push((p.name.unwrap(), p.filename, p.content_type, p.body));
    }
    let (name, filename, body) = edit.binary_parts[0].clone();
    let record = decode_stroke_resource(&body).unwrap();
    out.push((name, Some(filename), "application/octet-stream".to_string(), body));
    (reparse(multipart(&out)), record)
}

fn first_stroke_id(xml: &[u8]) -> String {
    let text = String::from_utf8_lossy(xml);
    let at = text.find("type=\"stroke\"").unwrap();
    let rest = &text[at..];
    let id = &rest[rest.find("id=\"").unwrap() + 4..];
    id[..36].to_string()
}

fn native() -> notes_preview::NativeInk {
    notes_preview::NativeInk::new(notes_preview::env::default_project_root(), None)
}

fn body_strokes(body: &[u8]) -> Vec<Val> {
    let Val::Dict(doc) = rs_store::val::loads(std::str::from_utf8(body).unwrap()).unwrap() else { panic!() };
    match doc.into_iter().find(|(k, _)| k == "strokes").unwrap().1 {
        Val::List(items) => items,
        _ => panic!(),
    }
}

fn rendering_text(body: &[u8], index: usize) -> String {
    let text = std::str::from_utf8(body).unwrap();
    let mut at = 0;
    for _ in 0..=index {
        at += text[at..].find("{\"rendering\":").unwrap() + 1;
    }
    let start = at - 1 + "{\"rendering\":".len();
    let end = start + text[start..].find(",\"bbox\":").unwrap();
    text[start..end].to_string()
}

/// RGBA of a rendered page and whether any pixel of the box differs from the
/// page background (the corner pixel).
fn inked(png: &[u8], w: i64, h: i64, area: (i64, i64, i64, i64)) -> (bool, Vec<[u8; 4]>) {
    let px = rs_render::pixels::unfilter_png(png, w, h, notes_preview::MAX_PNG).ok().expect("decodable page PNG");
    let at = |x: i64, y: i64| -> [u8; 4] { let i = ((y * w + x) * 4) as usize; [px[i], px[i + 1], px[i + 2], px[i + 3]] };
    let background = at(w - 1, h - 1);
    let mut changed = Vec::new();
    for y in area.1..area.3 {
        for x in area.0..area.2 {
            if at(x, y) != background {
                changed.push(at(x, y));
            }
        }
    }
    (!changed.is_empty(), changed)
}

#[test]
fn samsung_inkpen2_page_is_admitted_with_its_stylus_channels() {
    for settings in ["9;", "8;"] {
        let note = inkpen2_note(settings);
        let pages = prepare::prepare_pages(&note, None, true).unwrap();
        assert_eq!(pages.len(), 1, "{settings}");
        assert!(rendering_text(&pages[0].body, 0).contains("\"tool_type_raw\":2,\"properties\":{\"compressed\":true,\"replay_only\":false,\"stylus_channels\":true"));
    }
}

#[test]
fn page_with_our_appended_inkpen2_stroke_stays_renderable() {
    // Before the fix the stroke (stylus bit cleared by the writer) failed the
    // InkPen2 admission and the whole page left the inventory: the page whose
    // Samsung canvas cache the write released showed nothing on Linux.
    for settings in ["9;", "8;"] {
        let note = inkpen2_note(settings);
        for (recipe, red) in [(AppendRecipe::V2, false), (AppendRecipe::V3 { color_argb: 0xffff0000, pen_size: 3.5 }, true)] {
            let label = format!("{settings} {recipe:?}");
            let (after, record) = appended(&note, recipe);
            assert!(!record.stylus && record.tool_type == 2, "{label}: writer profile");
            let pages = prepare::prepare_pages(&after, None, true).unwrap_or_else(|e| panic!("{label}: {e:?}"));
            assert_eq!(pages.len(), 1, "{label}: page refused");
            assert_eq!(native().inspect_render_pages(&after).len(), 1, "{label}");
            let body = &pages[0].body;
            assert_eq!(body_strokes(body).len(), 2, "{label}");
            let new = rendering_text(body, 1);
            assert!(new.contains("\"stylus_channels\":false"), "{label}");
            if !red {
                // Same pen as the template the live preview copies
                // (pen_templates), with the stylus bit dropped as it does.
                let pens = prepare::pen_templates(&note).unwrap();
                let template = &pens[pens.find("\"rendering\":").unwrap() + 12..pens.rfind("}}").unwrap()];
                assert_eq!(new, template.replace("\"stylus_channels\":true", "\"stylus_channels\":false"), "{label}");
            }
            let n = native();
            if !n.provider_available() {
                eprintln!("ink renderer not built; skipping render");
                continue;
            }
            let (w, h) = (pages[0].width, pages[0].height);
            let before_png = n.render_ink_page(&note, 0).unwrap();
            let png = n.render_ink_page(&after, 0).unwrap_or_else(|e| panic!("{label}: render {e:?}"));
            assert!(notes_preview::png_valid(&png, w, h));
            // The new stroke (page units 40..100 x 40..80) is drawn; the
            // source stroke region keeps its ink.
            let area = (36, 36, 106, 86);
            assert!(!inked(&before_png, w, h, area).0, "{label}");
            let (drawn, pixels) = inked(&png, w, h, area);
            assert!(drawn, "{label}: new stroke missing");
            if red {
                assert!(pixels.iter().any(|p| p[0] > 200 && p[1] < 80 && p[2] < 80), "{label}: recipe 3 colour");
            }
            assert!(inked(&png, w, h, (8, 17, 15, 24)).0, "{label}: source stroke");
        }
    }
}

#[test]
fn stylusless_inkpen2_keeps_the_other_admission_gates() {
    // Only the axis requirement is relaxed: another tool, or a property the
    // renderer has no route for, still refuses the page.
    let (after, _) = appended(&inkpen2_note("9;"), AppendRecipe::V2);
    let snap = notes_host::snapshot::from_dict(&after).unwrap();
    let parts = payload_parts(&snap.raw_body, &snap.content_type, &Default::default()).unwrap();
    let patch = |edit: &dyn Fn(&mut Vec<u8>, &rs_ink::core::binary::Record)| -> Val {
        let out: Vec<_> = parts.iter().map(|p| {
            let mut body = p.body.clone();
            if p.filename.as_deref().is_some_and(|f| f.starts_with(NEW)) {
                let record = decode_stroke_resource(&body).unwrap();
                edit(&mut body, &record);
            }
            (p.name.clone().unwrap(), p.filename.clone(), p.content_type.clone(), body)
        }).collect();
        reparse(multipart(&out))
    };
    assert_eq!(prepare::prepare_pages(&patch(&|_, _| {}), None, false).unwrap().len(), 1);
    let tool1 = patch(&|body, r| {
        let at = 12 + r.frames[0].raw.len() + r.frames[1].flexible_start as usize - 2;
        body[at..at + 2].copy_from_slice(&1u16.to_le_bytes());
    });
    assert_eq!(prepare::prepare_pages(&tool1, None, false).unwrap().len(), 0, "tool 1");
    let eraser = patch(&|body, r| {
        // PROPS bit 3 (eraser): first property-mask byte of the stroke frame.
        assert!(!r.frames[1].properties.is_empty());
        body[12 + r.frames[0].raw.len() + 11] |= 1 << 3;
    });
    assert_eq!(prepare::prepare_pages(&eraser, None, false).unwrap().len(), 0, "eraser");
}
