//! Ports of tests/test_pdf_background.py (manager-level parts included) and
//! test_preview_performance.py snapshot authentication.
mod common;
use common::*;
use notes_preview::manager::{Entry, PreviewOptions, Source};
use notes_preview::pdf::{self, PdfBackground};
use notes_preview::{payload, Candidate, Fail, Kind, PreviewManager, Val};
use std::collections::BTreeMap;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn digest() -> String {
    sha256_hex(&pdf_fixture())
}

fn cand(index: i64, objects: i64, rect: (i64, i64, i64, i64)) -> PdfBackground {
    PdfBackground { page_index: 0, pdf_page_index: index, width: 30, height: 20, rect, annotation_objects: objects,
                    sha256: digest(), download_url: Some(URL.into()), overlay: None, local_ink: None }
}

fn write_pdf(path: &Path) -> Result<(u16, Option<String>), Fail> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(&pdf_fixture())?;
    Ok((200, None))
}

#[test]
fn straight_alpha_and_transparent_pixels_preserve_pdf_background() {
    let background = png(2, 1, &b"\0\0\xff\xff".repeat(2));
    let overlay = png(2, 1, b"\xff\0\0\x80\xff\xff\xff\0");
    let combined = pdf::compose_pdf_canvas(&background, &overlay, 2, 1).unwrap();
    assert_eq!(pixel(&combined, 0, 0), [0x80, 0, 0x7f, 0xff]);
    assert_eq!(pixel(&combined, 1, 0), [0, 0, 0xff, 0xff]);
}

#[test]
fn opaque_unproven_cache_and_wrong_dimensions_are_not_color_keyed() {
    let background = png(2, 1, &[0xff; 8]);
    let opaque = png(2, 1, &b"\0\0\0\xff".repeat(2));
    assert_eq!(pdf::compose_pdf_canvas(&background, &opaque, 2, 1).unwrap_err().kind, Kind::PdfBackground);
    assert_eq!(pdf::compose_pdf_canvas(&background, &opaque, 1, 1).unwrap_err().kind, Kind::PdfBackground);
}

#[test]
fn zero_based_page_and_hash_binding_to_known_download() {
    let found = pdf::inspect_pdf_backgrounds_raw(pdf_xml(None, "", "false").as_bytes(), "application/xml").unwrap();
    assert_eq!(found, vec![cand(0, 0, (2, 3, 22, 13))]);
    assert!(found[0].complete());
    let partial = pdf::inspect_pdf_backgrounds_raw(pdf_xml(None, "<object type=\"stroke\"/>", "false").as_bytes(), "application/xml").unwrap();
    assert!(!partial[0].complete());
    assert_eq!(partial[0].annotation_objects, 1);
}

#[test]
fn lock_dtd_ambiguous_and_out_of_bounds_placements_rejected() {
    let base = pdf_xml(None, "", "false");
    for raw in [pdf_xml(None, "", "true"), "<!DOCTYPE root><root/>".to_string(), base.replace("l=\"2\"", "l=\"-1\""),
                base.replace("r=\"22\"", "r=\"32\""), base.replace("pageIndex=\"0\"", "pageIndex=\"100001\""),
                base.replace("<pdfRect ", "<pdfRect unexpected=\"1\" ")] {
        assert_eq!(pdf::inspect_pdf_backgrounds_raw(raw.as_bytes(), "application/xml").unwrap_err().kind, Kind::PdfBackground, "{raw}");
    }
}

#[test]
fn redirect_strips_account_headers_and_cache_is_verified_private() {
    let dir = TempDir::new();
    let calls: Mutex<Vec<(String, Vec<(String, String)>)>> = Mutex::new(vec![]);
    let headers = || Ok(vec![("Authorization".to_string(), "synthetic-private".to_string()), ("X-Account".into(), "synthetic".into())]);
    let transport = |url: &str, h: &[(String, String)], path: &Path, _: u64, guard: &dyn Fn() -> Result<(), Fail>| {
        let n = { let mut c = calls.lock().unwrap(); c.push((url.to_string(), h.to_vec())); c.len() };
        guard()?;
        if n == 1 {
            return Ok((302, Some("https://eu1rgn-down.file.samsungcloud.com/file?signature=synthetic".to_string())));
        }
        write_pdf(path)
    };
    let path = pdf::fetch_pdf_with(&cand(0, 0, (2, 3, 22, 13)), &headers, dir.path(), "synthetic", None, &transport).unwrap();
    assert_eq!(calls.lock().unwrap()[1].1, vec![("Accept".to_string(), "application/pdf".to_string())]);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(pdf::fetch_pdf_with(&cand(0, 0, (2, 3, 22, 13)), &headers, dir.path(), "synthetic", None, &transport).unwrap(), path);
    assert_eq!(calls.lock().unwrap().len(), 2);
    std::fs::write(&path, b"%PDF-corrupted").unwrap();
    let failing = |_: &str, _: &[(String, String)], _: &Path, _: u64, _: &dyn Fn() -> Result<(), Fail>| Ok((500, None));
    let error = pdf::fetch_pdf_with(&cand(0, 0, (2, 3, 22, 13)), &headers, dir.path(), "synthetic", None, &failing).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (Kind::PdfBackground, "PDF dosyasının bütünlüğü doğrulanamadı."));
    let partial = std::fs::read_dir(dir.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".partial-")).count();
    assert_eq!(partial, 0);
}

#[test]
fn untrusted_redirect_fails_before_second_get() {
    let dir = TempDir::new();
    let calls = AtomicUsize::new(0);
    let transport = |_: &str, _: &[(String, String)], _: &Path, _: u64, _: &dyn Fn() -> Result<(), Fail>| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok((302, Some("https://evil.example/file".to_string())))
    };
    let error = pdf::fetch_pdf_with(&cand(0, 0, (2, 3, 22, 13)), &|| Ok(vec![]), dir.path(), "synthetic", None, &transport).unwrap_err();
    assert_eq!(error.kind, Kind::Preview);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn actual_isolated_renderer_uses_zero_based_pdf_index_and_note_rect() {
    if !pdf::pdf_provider_available() {
        eprintln!("isolated local Poppler unavailable");
        return;
    }
    let dir = TempDir::new();
    let path = dir.path().join("synthetic.pdf");
    std::fs::write(&path, pdf_fixture()).unwrap();
    for (index, color) in [(0, [0xff, 0, 0, 0xff]), (1, [0, 0xff, 0, 0xff])] {
        let image = pdf::render_pdf_background(&path, &cand(index, 0, (2, 3, 22, 13))).unwrap();
        assert!(notes_preview::png_valid(&image, 30, 20));
        assert_eq!(pixel(&image, 10, 8), color);
        assert_eq!(pixel(&image, 0, 0), [0xff; 4]);
    }
    assert_eq!(pdf::render_pdf_background(&path, &cand(9, 0, (2, 3, 22, 13))).unwrap_err().kind, Kind::PdfBackground);
    std::fs::write(&path, b"%PDF-invalid").unwrap();
    assert_eq!(pdf::render_pdf_background(&path, &cand(0, 0, (2, 3, 22, 13))).unwrap_err().kind, Kind::PdfBackground);
}

#[test]
fn manifest_labels_missing_annotations_and_cache_keys_differ_per_pdf_page() {
    let dir = TempDir::new();
    let s = store(dir.path());
    let raw = pdf_xml(None, "<object type=\"stroke\"/>", "false");
    let id = note_with_document(&s, &snapshot(raw.as_bytes(), "application/xml"));
    let m = PreviewManager::new(s, TestEnv::new(None, true)).unwrap();
    let manifest = m.manifest(&id).unwrap();
    let Some(Val::List(pages)) = manifest.get("pages") else { panic!() };
    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    assert_eq!(page.get("available"), Some(&Val::Bool(true)));
    assert_eq!(page.get("previewKind"), Some(&Val::s("pdf-background")));
    assert_eq!(page.get("complete"), Some(&Val::Bool(false)));
    assert!(page.get("previewNotice").unwrap().truthy());
    assert_ne!(m.path(&Candidate::Pdf(cand(0, 0, (2, 3, 22, 13)))).unwrap(), m.path(&Candidate::Pdf(cand(1, 0, (2, 3, 22, 13)))).unwrap());
    m.close();
}

#[test]
fn invalid_pdf_placement_keeps_partial_notice_even_when_canvas_exists() {
    let dir = TempDir::new();
    let s = store(dir.path());
    let raw = pdf_xml(None, "", "false").replace("r=\"22\"", "r=\"32\"").replacen("<layerList>",
        "<canvasCacheDataMap><pair key=\"0\" fileId=\"7\" width=\"30\" height=\"20\" isDarkMode=\"false\"/></canvasCacheDataMap><layerList>", 1);
    let id = note_with_document(&s, &snapshot(raw.as_bytes(), "application/xml"));
    let m = PreviewManager::new(s, TestEnv::new(None, true)).unwrap();
    let manifest = m.manifest(&id).unwrap();
    let Some(Val::List(pages)) = manifest.get("pages") else { panic!() };
    assert_eq!(pages[0].get("available"), Some(&Val::Bool(true)));
    assert_eq!(pages[0].get("complete"), Some(&Val::Bool(false)));
    assert!(pages[0].get("previewNotice").unwrap().truthy());
}

#[test]
fn online_composite_keeps_annotation_layer_and_same_result_after_offline_restart() {
    if !pdf::pdf_provider_available() {
        return;
    }
    let dir = TempDir::new();
    let opaque = b"synthetic-canvas".to_vec();
    let canvas_digest = sha256_hex(&opaque);
    let raw = pdf_xml(None, "<object type=\"stroke\"/>", "false")
        .replacen("<layerList>", "<canvasCacheDataMap><pair key=\"0\" fileId=\"8\" width=\"30\" height=\"20\" isDarkMode=\"false\"/></canvasCacheDataMap><layerList>", 1)
        .replace("</contentFileList>", &format!("<contentFile id=\"{canvas_digest}\" bindNumber=\"8\"/></contentFileList>"))
        .replace("</downloadUrls>", &format!("<downloadUrl><id>{canvas_digest}</id><url>{URL}</url></downloadUrl></downloadUrls>"));
    let s = store(dir.path());
    let id = note_with_document(&s, &snapshot(raw.as_bytes(), "application/xml"));
    let session = dir.path().join("session");
    let env = TestEnv::new(Some(&session), false);
    *env.headers.lock().unwrap() = Ok(vec![]);
    *env.download.lock().unwrap() = Some(Box::new(|_, _, path, _, _| write_pdf(path)));
    let mut pixels = vec![0u8; 30 * 20 * 4];
    pixels[0..4].copy_from_slice(b"\xff\0\0\x80");
    let layer = png(30, 20, &pixels);
    let (o, l) = (opaque.clone(), layer.clone());
    let first = PreviewManager::with_options(s.clone(), env.clone(), PreviewOptions {
        fetcher: Some(Arc::new(move |_, _, _| Ok(o.clone()))),
        decoder: Some(Arc::new(move |_: &[u8], _| Ok(l.clone()))),
        ..Default::default()
    }).unwrap();
    let overlay = rs_ink_overlay(&raw);
    let layer_path = first.path(&Candidate::Canvas(overlay)).unwrap();
    if !notes_preview::codec::provider_available(&notes_preview::env::default_project_root()) {
        // Python patches previews.provider_available=True; without the
        // optional codec, seed the verified layer a previous decode kept.
        std::fs::write(&layer_path, &layer).unwrap();
    }
    let manifest = first.manifest(&id).unwrap();
    let Some(Val::List(pages)) = manifest.get("pages") else { panic!() };
    assert_eq!(pages[0].get("complete"), Some(&Val::Bool(true)));
    assert_eq!(pages[0].get("previewKind"), Some(&Val::s("pdf-with-notes")));
    let expected = first.request(&id, 0, 0, 10.0).unwrap().expect("composite");
    assert_eq!(pixel(&expected, 0, 0), [0xff, 0x7f, 0x7f, 0xff]);
    assert!(layer_path.exists(), "decoded overlay must persist for offline selection");
    first.close();
    first.join(Duration::from_secs(2));
    env.set_session(None);
    env.offline.store(true, Ordering::SeqCst);
    *env.headers.lock().unwrap() = Err(Fail::new(Kind::Other, "offline restart must not access credentials"));
    let restarted = PreviewManager::new(s, env).unwrap();
    let manifest = restarted.manifest(&id).unwrap();
    let Some(Val::List(pages)) = manifest.get("pages") else { panic!() };
    assert_eq!(pages[0].get("complete"), Some(&Val::Bool(true)));
    assert_eq!(restarted.request(&id, 0, 0, 0.0).unwrap(), Some(expected));
    restarted.close();
}

fn rs_ink_overlay(raw: &str) -> notes_preview::CanvasPreviewCandidate {
    let parsed = payload::parse_payload(raw.as_bytes().to_vec(), "application/xml").unwrap();
    payload::inspect_ink_payload(&parsed).unwrap().canvas_candidates.into_iter().next().unwrap()
}

#[test]
fn cached_pdf_can_render_another_page_offline_without_any_api_access() {
    if !pdf::pdf_provider_available() {
        return;
    }
    let dir = TempDir::new();
    let env = TestEnv::new(None, true);
    *env.headers.lock().unwrap() = Err(Fail::new(Kind::Other, "offline PDF must never access API credentials"));
    let entry = Entry { source: Source(vec![Val::s("synthetic-id"), Val::s("commit")]), pages: vec![],
                        candidates: BTreeMap::from([(0, Candidate::Pdf(cand(1, 0, (2, 3, 22, 13))))]) };
    let (e1, e2) = (entry.clone(), entry.clone());
    let m = PreviewManager::with_options(store(dir.path()), env, PreviewOptions {
        entry_hook: Some(Arc::new(move |_| Ok(e1.clone()))),
        source_hook: Some(Arc::new(move |_| Ok(e2.source.clone()))),
        ..Default::default()
    }).unwrap();
    pdf::fetch_pdf_with(&cand(0, 0, (2, 3, 22, 13)), &|| Ok(vec![]), &m.directory().join("pdf-files"), "offline", None,
                        &|_, _, path, _, _| write_pdf(path)).unwrap();
    assert_eq!(m.request("synthetic-id", 0, 0, 0.0).unwrap(), None);
    assert!(m.wait_idle(Some(Duration::from_secs(30))));
    let image = m.request("synthetic-id", 0, 0, 0.0).unwrap().unwrap();
    assert_eq!(pixel(&image, 10, 8), [0, 0xff, 0, 0xff]);
    let path = m.path(&entry.candidates[&0]).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    m.close();
    m.join(Duration::from_secs(2));
}

#[test]
fn cached_snapshot_tampering_and_schema_rejected_before_parsing() {
    let raw = pdf_xml(None, "", "false").into_bytes();
    let good = snapshot(&raw, "application/xml");
    assert!(payload::payload_from_snapshot(&good).is_ok());
    for (key, value) in [("schemaVersion", Val::Int(2)), ("raw_sha256", Val::s("00".repeat(32))),
                         ("raw_body", Val::s("%invalid-base64")), ("raw_body", Val::Int(123))] {
        let mut bad = good.clone();
        bad.set(key, value);
        let error = payload::payload_from_snapshot(&bad).err().expect("must fail");
        assert!(error.is_document() || error.is_value(), "{key}: {error:?}");
    }
    let locked = snapshot(pdf_xml(None, "", "true").as_bytes(), "application/xml");
    assert!(payload::inventory_from_snapshot(&locked).is_err());
    let dtd = snapshot(b"<!DOCTYPE root [<!ENTITY test \"x\">]><root>&test;</root>", "application/xml");
    assert!(payload::inventory_from_snapshot(&dtd).is_err());
}


#[test]
fn refused_signed_link_is_pdf_link_expired() {
    let dir = TempDir::new();
    for status in [401u16, 403] {
        let transport = move |_: &str, _: &[(String, String)], _: &Path, _: u64, _: &dyn Fn() -> Result<(), Fail>| Ok((status, None));
        let error = pdf::fetch_pdf_with(&cand(0, 0, (2, 3, 22, 13)), &|| Ok(vec![]), dir.path(), "synthetic", None, &transport).unwrap_err();
        assert!(pdf::is_link_expired(&error));
        assert_eq!((error.kind, error.message.as_str()), (Kind::PdfBackground, "PDF indirme bağlantısının süresi dolmuş."));
    }
}

/// previews._fetch_pdf_refreshing: a refused link triggers one note refresh
/// and one retry with the renewed candidate; a second refusal is not retried.
#[test]
fn expired_pdf_link_refreshes_note_once_and_retries() {
    if !pdf::pdf_provider_available() {
        return;
    }
    let dir = TempDir::new();
    let raw = pdf_xml(None, "", "false");
    let s = store(dir.path());
    let document = snapshot(raw.as_bytes(), "application/xml");
    let id = note_with_document(&s, &document);
    let session = dir.path().join("session");
    let env = TestEnv::new(Some(&session), false);
    *env.headers.lock().unwrap() = Ok(vec![]);
    let downloads = Arc::new(AtomicUsize::new(0));
    let d = downloads.clone();
    *env.download.lock().unwrap() = Some(Box::new(move |_, _, path, _, _| {
        if d.fetch_add(1, Ordering::SeqCst) == 0 { Ok((403, None)) } else { write_pdf(path) }
    }));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let (r, doc) = (refreshes.clone(), document.clone());
    *env.refresh.lock().unwrap() = Some(Box::new(move |_| { r.fetch_add(1, Ordering::SeqCst); Ok(doc.clone()) }));
    let manager = PreviewManager::new(s.clone(), env.clone()).unwrap();
    assert!(manager.request(&id, 0, 0, 10.0).unwrap().is_some());
    assert_eq!((downloads.load(Ordering::SeqCst), refreshes.load(Ordering::SeqCst)), (2, 1));
    manager.close();
    // Always refused: exactly one refresh and one retry, then the error.
    let dir2 = TempDir::new();
    let s2 = store(dir2.path());
    let id2 = note_with_document(&s2, &document);
    let env2 = TestEnv::new(Some(&session), false);
    *env2.headers.lock().unwrap() = Ok(vec![]);
    let downloads2 = Arc::new(AtomicUsize::new(0));
    let d2 = downloads2.clone();
    *env2.download.lock().unwrap() = Some(Box::new(move |_, _, _, _, _| { d2.fetch_add(1, Ordering::SeqCst); Ok((403, None)) }));
    let refreshes2 = Arc::new(AtomicUsize::new(0));
    let (r2, doc2) = (refreshes2.clone(), document.clone());
    *env2.refresh.lock().unwrap() = Some(Box::new(move |_| { r2.fetch_add(1, Ordering::SeqCst); Ok(doc2.clone()) }));
    let manager2 = PreviewManager::new(s2, env2).unwrap();
    // request(wait=10) sees the failed job and raises preview_unavailable.
    let error = manager2.request(&id2, 0, 0, 10.0).unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("preview_unavailable", "PDF indirme bağlantısının süresi dolmuş."));
    assert_eq!((downloads2.load(Ordering::SeqCst), refreshes2.load(Ordering::SeqCst)), (2, 1));
    manager2.close();
}
