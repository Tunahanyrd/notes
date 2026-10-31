//! Synthetic multipart snapshots for authenticated page stroke extraction.
use _store::note_container::{self, BuiltContainer};
use _store::note_container_page::{capture_page_index, read_page_strokes, read_page_strokes_indexed, PageReadError, PageReadExpectation};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::io::Cursor;

const CT: &str = "multipart/form-data; boundary=synthetic";
const OLD: &str = "00000000-0000-4000-8000-000000000010";
const NEW: &str = "00000000-0000-4000-8000-000000000011";
const TIME: &str = "1700000000000000";

fn part(out: &mut Vec<u8>, name: &str, filename: Option<&str>, body: &[u8]) {
    let file = filename
        .map(|s| format!("; filename=\"{s}\""))
        .unwrap_or_default();
    out.extend_from_slice(format!("--synthetic\r\nContent-Disposition: form-data; name=\"{name}\"{file}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n", if filename.is_some() { "application/octet-stream" } else { "application/xml" }, body.len()).as_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(b"\r\n");
}

fn fixture(second_id: &str) -> (BuiltContainer, String, String, String, Vec<u8>, Vec<u8>) {
    fixture_with_suffix(second_id, "exception_2")
}

fn fixture_with_suffix(
    second_id: &str,
    suffix: &str,
) -> (BuiltContainer, String, String, String, Vec<u8>, Vec<u8>) {
    let snapshot = _store::val::loads(include_str!(
        "../../notes-host/tests/fixtures/ink_objects_base.json"
    ))
    .unwrap();
    let source = STANDARD
        .decode(snapshot.get("raw_body").unwrap().as_str().unwrap())
        .unwrap();
    let content_type = snapshot.get("content_type").unwrap().as_str().unwrap();
    let limits = rs_doc::mime::Limits::default();
    let parts = rs_doc::mime::parts(
        &source,
        content_type,
        limits.max_readonly_parts,
        &limits,
        &mut rs_doc::mime::NoHooks,
    )
    .unwrap();
    let xml = std::str::from_utf8(&source[parts[0].body.clone()]).unwrap();
    let document = roxmltree::Document::parse(xml).unwrap();
    let targets: Vec<_> = document
        .descendants()
        .filter(|n| {
            n.is_element() && n.tag_name().name() == "object" && n.attribute("id") == Some(OLD)
        })
        .map(|n| n.range())
        .collect();
    assert_eq!(targets.len(), 2);
    let mut changed = xml.to_string();
    for range in targets.into_iter().rev() {
        let copy = xml[range.clone()].replace(OLD, second_id);
        changed.insert_str(range.end, &copy);
    }
    let original = &source[parts[1].body.clone()];
    let mut v0 = original.to_vec();
    v0[8..12].copy_from_slice(&0u32.to_le_bytes());
    let mut v4000 = original.to_vec();
    v4000[8..12].copy_from_slice(&4000u32.to_le_bytes());
    let mut raw = Vec::new();
    part(&mut raw, "noteResource", None, changed.as_bytes());
    part(
        &mut raw,
        "strokeResource",
        Some(&format!("{second_id}_{suffix}")),
        &v0,
    );
    part(
        &mut raw,
        "strokeResource",
        Some(&format!("{OLD}_{TIME}")),
        &v4000,
    );
    raw.extend_from_slice(b"--synthetic--\r\n");
    let parsed = rs_doc::doc::parse_document(&raw, CT, None).unwrap();
    let local = snapshot
        .get("local_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let commit = snapshot
        .get("commit_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let built = note_container::build_with_snapshot(
        &raw,
        CT,
        None,
        &parsed.resource_xml,
        Some(&local),
        Some(&commit),
    )
    .unwrap();
    let page_id = built.pages[0].page_id.clone();
    (built, local, commit, page_id, v0, v4000)
}

fn expect<'a>(
    built: &'a BuiltContainer,
    local: &'a str,
    commit: &'a str,
    page: &'a str,
) -> PageReadExpectation<'a> {
    let info = built.pages.iter().find(|p| p.page_id == page).unwrap();
    PageReadExpectation {
        page_id: page,
        page_sha256: &info.page_sha256,
        xml_sha256: &info.xml_sha256,
        context_sha256: built.context_sha256.as_deref().unwrap(),
        raw_sha256: &built.raw_sha256,
        manifest_sha256: &built.manifest_sha256,
        local_id: local,
        commit_id: commit,
    }
}

#[test]
fn strokes_preserve_multipart_order_wrappers_and_suffixes() {
    let (built, local, commit, page, v0, v4000) = fixture(NEW);
    let result = read_page_strokes(
        Cursor::new(&built.bytes),
        &expect(&built, &local, &commit, &page),
    )
    .unwrap();
    assert_eq!(result.strokes.len(), 2);
    assert_eq!(result.strokes[0].id, NEW);
    assert_eq!(result.strokes[0].filename, format!("{NEW}_exception_2"));
    assert_eq!(result.strokes[0].body, v0);
    assert_eq!(result.strokes[1].id, OLD);
    assert_eq!(result.strokes[1].filename, format!("{OLD}_{TIME}"));
    assert_eq!(result.strokes[1].body, v4000);
}

#[test]
fn noncanonical_headers_still_allow_verified_page_reads() {
    let (original, local, commit, page, v0, v4000) = fixture(NEW);
    let reconstructed = note_container::reconstruct(Cursor::new(&original.bytes)).unwrap();
    let limits = rs_doc::mime::Limits::default();
    let parts = rs_doc::mime::parts(&reconstructed.raw_body, CT, limits.max_readonly_parts, &limits, &mut rs_doc::mime::NoHooks).unwrap();
    let mut raw = Vec::new();
    for p in &parts {
        let file = p.filename.as_ref().map(|f| format!("; filename=\"{f}\"")).unwrap_or_default();
        raw.extend_from_slice(format!("--synthetic\r\nContent-Disposition: form-data; name=\"{}\"{file}\r\nContent-Type: {}\r\n\r\n", p.name.as_deref().unwrap(), p.content_type).as_bytes());
        raw.extend_from_slice(&reconstructed.raw_body[p.body.clone()]);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"--synthetic--\r\n");
    let parsed = rs_doc::doc::parse_document(&raw, CT, None).unwrap();
    let built = note_container::build_with_snapshot(&raw, CT, None, &parsed.resource_xml, Some(&local), Some(&commit)).unwrap();
    assert_eq!(note_container::reconstruct(Cursor::new(&built.bytes)).unwrap().raw_body, raw);
    let expected = expect(&built, &local, &commit, &page);
    let direct = read_page_strokes(Cursor::new(&built.bytes), &expected).unwrap();
    let index = capture_page_index(Cursor::new(&built.bytes), &built.manifest_sha256).unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(&built.bytes)).unwrap();
    let mut context = Vec::new();
    use std::io::Read;
    zip.by_name("cloud/context.json").unwrap().read_to_end(&mut context).unwrap();
    let indexed = read_page_strokes_indexed(Cursor::new(&built.bytes), &expected, &index, &context).unwrap();
    for result in [direct, indexed] {
        assert_eq!(result.strokes.len(), 2);
        assert_eq!(result.strokes[0].filename, format!("{NEW}_exception_2"));
        assert_eq!(result.strokes[0].body, v0);
        assert_eq!(result.strokes[1].filename, format!("{OLD}_{TIME}"));
        assert_eq!(result.strokes[1].body, v4000);
    }
}

#[test]
fn independent_hashes_and_malformed_ids_fail_closed() {
    let (built, local, commit, page, _, _) = fixture(NEW);
    let zero = "0".repeat(64);
    let mut expected = expect(&built, &local, &commit, &page);
    expected.manifest_sha256 = &zero;
    assert!(matches!(
        read_page_strokes(Cursor::new(&built.bytes), &expected),
        Err(PageReadError::Corrupt(_))
    ));
    let mut expected = expect(&built, &local, &commit, &page);
    expected.page_sha256 = &zero;
    assert!(matches!(
        read_page_strokes(Cursor::new(&built.bytes), &expected),
        Err(PageReadError::Corrupt(_))
    ));
    let mut expected = expect(&built, &local, &commit, &page);
    expected.context_sha256 = &zero;
    assert!(matches!(
        read_page_strokes(Cursor::new(&built.bytes), &expected),
        Err(PageReadError::Corrupt(_))
    ));
    let mut expected = expect(&built, &local, &commit, &page);
    expected.page_id = "../bad";
    assert!(matches!(
        read_page_strokes(Cursor::new(&built.bytes), &expected),
        Err(PageReadError::Corrupt(_))
    ));
    let (bad, local, commit, page, _, _) = fixture("not-a-uuid");
    assert!(matches!(
        read_page_strokes(
            Cursor::new(&bad.bytes),
            &expect(&bad, &local, &commit, &page)
        ),
        Err(PageReadError::Corrupt(_))
    ));
}

#[test]
fn modified_manifest_is_detected_even_when_page_and_context_are_unchanged() {
    use std::io::{Read, Write};
    let (built, local, commit, page, _, _) = fixture(NEW);
    let mut archive = zip::ZipArchive::new(Cursor::new(&built.bytes)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        if entry.name() == "cloud/manifest.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            value["suffix_exceptions"][NEW] = serde_json::json!("forged");
            writer
                .start_file(
                    "cloud/manifest.json",
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            writer
                .write_all(&serde_json::to_vec(&value).unwrap())
                .unwrap();
        } else {
            writer.raw_copy_file(entry).unwrap();
        }
    }
    let changed = writer.finish().unwrap().into_inner();
    assert!(matches!(
        read_page_strokes(
            Cursor::new(changed),
            &expect(&built, &local, &commit, &page)
        ),
        Err(PageReadError::Corrupt(_))
    ));
}

#[test]
fn unadmitted_context_requires_the_full_read_path() {
    let (built, local, commit, page, _, _) = fixture(NEW);
    let rebuilt = note_container::reconstruct(Cursor::new(&built.bytes)).unwrap();
    let no_proof = note_container::build(&rebuilt.raw_body, &rebuilt.content_type, None).unwrap();
    assert!(matches!(
        read_page_strokes(
            Cursor::new(&no_proof.bytes),
            &expect(&no_proof, &local, &commit, &page)
        ),
        Err(PageReadError::Unsupported(_))
    ));
}

#[test]
fn reading_one_page_does_not_read_other_page_entries() {
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::{Arc, Mutex};
    struct Tracked<'a> {
        inner: Cursor<&'a [u8]>,
        reads: Arc<Mutex<Vec<(u64, u64)>>>,
    }
    impl Read for Tracked<'_> {
        fn read(&mut self, data: &mut [u8]) -> std::io::Result<usize> {
            let start = self.inner.position();
            let n = self.inner.read(data)?;
            self.reads.lock().unwrap().push((start, start + n as u64));
            Ok(n)
        }
    }
    impl Seek for Tracked<'_> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }
    let (built, local, commit, page, _, _) = fixture(NEW);
    let old = note_container::reconstruct(Cursor::new(&built.bytes)).unwrap();
    let limits = rs_doc::mime::Limits::default();
    let parts = rs_doc::mime::parts(
        &old.raw_body,
        CT,
        limits.max_readonly_parts,
        &limits,
        &mut rs_doc::mime::NoHooks,
    )
    .unwrap();
    let xml = std::str::from_utf8(&old.raw_body[parts[0].body.clone()]).unwrap();
    let parsed = roxmltree::Document::parse(xml).unwrap();
    let pages: Vec<_> = parsed
        .descendants()
        .filter(|n| {
            n.is_element()
                && n.tag_name().name() == "page"
                && n.attribute("id") == Some(page.as_str())
        })
        .map(|n| n.range())
        .collect();
    assert_eq!(pages.len(), 2);
    let second_page = "00000000-0000-4000-8000-000000000099";
    let third = "00000000-0000-4000-8000-000000000012";
    let fourth = "00000000-0000-4000-8000-000000000013";
    let mut expanded = xml.to_owned();
    for range in pages.into_iter().rev() {
        let copy = xml[range.clone()]
            .replace(&page, second_page)
            .replace(OLD, third)
            .replace(NEW, fourth);
        expanded.insert_str(range.end, &copy);
    }
    let mut raw = Vec::new();
    part(&mut raw, "noteResource", None, expanded.as_bytes());
    for index in [1usize, 2] {
        part(
            &mut raw,
            "strokeResource",
            parts[index].filename.as_deref(),
            &old.raw_body[parts[index].body.clone()],
        );
    }
    part(
        &mut raw,
        "strokeResource",
        Some(&format!("{third}_{TIME}")),
        &old.raw_body[parts[2].body.clone()],
    );
    part(
        &mut raw,
        "strokeResource",
        Some(&format!("{fourth}_{TIME}")),
        &old.raw_body[parts[1].body.clone()],
    );
    raw.extend_from_slice(b"--synthetic--\r\n");
    let resource = rs_doc::doc::parse_document(&raw, CT, None).unwrap();
    let built = note_container::build_with_snapshot(
        &raw,
        CT,
        None,
        &resource.resource_xml,
        Some(&local),
        Some(&commit),
    )
    .unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(&built.bytes)).unwrap();
    let ranges: Vec<_> = [
        format!("{second_page}.page"),
        format!("cloud/pages/{second_page}.xml"),
    ]
    .iter()
    .map(|name| {
        let entry = zip.by_name(name).unwrap();
        let start = entry.data_start().unwrap();
        (start, start + entry.compressed_size())
    })
    .collect();
    let reads = Arc::new(Mutex::new(Vec::new()));
    let tracked = Tracked {
        inner: Cursor::new(&built.bytes),
        reads: reads.clone(),
    };
    let result = read_page_strokes(tracked, &expect(&built, &local, &commit, &page)).unwrap();
    assert_eq!(result.strokes.len(), 2);
    for (start, end) in ranges {
        assert!(
            !reads
                .lock()
                .unwrap()
                .iter()
                .any(|(a, b)| *a < end && *b > start),
            "unrelated page read"
        );
    }
}

#[test]
fn unusual_valid_suffix_uses_full_snapshot_fallback() {
    let (built, local, commit, page, _, _) = fixture_with_suffix(NEW, "exception:valid");
    let rebuilt = note_container::reconstruct(Cursor::new(&built.bytes)).unwrap();
    assert!(rebuilt
        .raw_body
        .windows("exception:valid".len())
        .any(|w| w == b"exception:valid"));
    assert!(matches!(
        read_page_strokes(
            Cursor::new(&built.bytes),
            &expect(&built, &local, &commit, &page)
        ),
        Err(PageReadError::Unsupported(_))
    ));
}
