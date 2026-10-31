//! Lossless Cloud snapshots in immutable, page-addressable ZIP containers.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::borrow::Cow;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const FORMAT: &str = "notes-linux/sdocx-cloud/1";
const WRAPPER: [u8; 12] = [255, 255, 255, 255, 4, 0, 0, 0, 160, 15, 0, 0];
const MAX_ENTRY: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 32768;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;
pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Hole {
    offset: usize,
    page: String,
    span: usize,
    after_download: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Part {
    pub(crate) id: String,
    pub(crate) prefix: Vec<u8>,
    pub(crate) entry: Option<String>,
    pub(crate) xml: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    format: String,
    pub(crate) content_type: String,
    pub(crate) boundary: Option<String>,
    pub(crate) raw_sha256: String,
    holes: Vec<Hole>,
    download_offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) part_order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) prefix_exceptions: BTreeMap<String, Vec<u8>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) suffix_exceptions: BTreeMap<String, String>,
    pub(crate) entry_sha256: BTreeMap<String, String>,
}
#[derive(Clone, Debug)]
pub struct PageInfo {
    pub page_id: String,
    pub page_sha256: String,
    pub xml_sha256: String,
    pub has_ink: bool,
}
#[derive(Clone, Debug)]
pub struct BuiltContainer {
    pub bytes: Vec<u8>,
    pub raw_sha256: String,
    pub manifest_sha256: String,
    pub pages: Vec<PageInfo>,
    pub context_sha256: Option<String>,
    pub copied_entries: Vec<String>,
}
/// A previous container can be read directly from its immutable file.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

pub struct BuiltContainerInfo {
    pub raw_sha256: String,
    pub manifest_sha256: String,
    pub pages: Vec<PageInfo>,
    pub context_sha256: Option<String>,
    pub copied_entries: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Reconstructed {
    pub raw_body: Vec<u8>,
    pub content_type: String,
    pub resource_xml: String,
}
#[derive(Clone, Debug)]
pub struct PageData {
    pub page: Vec<u8>,
    pub xml: Vec<u8>,
}
fn standard_part(
    out: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    filename: Option<&str>,
    body: &[u8],
) {
    out.extend_from_slice(standard_header(boundary, name, filename, body.len()).as_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(b"\r\n");
}
fn standard_header(boundary: &str, name: &str, filename: Option<&str>, body_len: usize) -> String {
    let file = filename
        .map(|s| format!("; filename=\"{s}\""))
        .unwrap_or_default();
    let content_type = if filename.is_some() {
        "application/octet-stream"
    } else {
        "application/xml"
    };
    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"{file}\r\nContent-Type: {content_type}\r\nContent-Length: {body_len}\r\n\r\n")
}
fn standard_end(out: &mut Vec<u8>, boundary: &str) {
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
}
fn hash(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn attr<'a, 'i>(n: roxmltree::Node<'a, 'i>, key: &str) -> Result<&'a str> {
    n.attribute(key).ok_or_else(|| format!("missing {key}"))
}
fn number(n: roxmltree::Node<'_, '_>, key: &str) -> Result<i64> {
    attr(n, key)?.parse().map_err(err)
}
fn opt_number(n: roxmltree::Node<'_, '_>, key: &str) -> Result<u32> {
    n.attribute(key).unwrap_or("0").parse().map_err(err)
}
fn u16b(o: &mut Vec<u8>, n: usize) -> Result<()> {
    o.extend_from_slice(&u16::try_from(n).map_err(err)?.to_le_bytes());
    Ok(())
}
fn u32b(o: &mut Vec<u8>, n: usize) -> Result<()> {
    o.extend_from_slice(&u32::try_from(n).map_err(err)?.to_le_bytes());
    Ok(())
}
fn utf16(o: &mut Vec<u8>, s: &str) -> Result<()> {
    let v: Vec<u16> = s.encode_utf16().collect();
    u16b(o, v.len())?;
    for c in v {
        o.extend_from_slice(&c.to_le_bytes());
    }
    Ok(())
}
fn ident(id: &str, t: i64) -> Vec<u8> {
    Sha256::digest(format!("{id}{t}").as_bytes()).to_vec()
}
fn tag(n: roxmltree::Node<'_, '_>, name: &str) -> bool {
    n.is_element() && n.tag_name().name() == name
}
fn tree_page(n: roxmltree::Node<'_, '_>) -> bool {
    n.ancestors().any(|a| tag(a, "noteTree"))
}
fn page_binary(
    pg: roxmltree::Node<'_, '_>,
    payload: &HashMap<String, &[u8]>,
) -> Result<(Vec<u8>, bool)> {
    let id = attr(pg, "id")?;
    let t = number(pg, "modifiedTime")?;
    let mut hdr = vec![4, 0, 0, 0, 0, 4, 0, 0, 0, 0];
    for k in ["orientation", "width", "height"] {
        hdr.extend_from_slice(&opt_number(pg, k)?.to_le_bytes());
    }
    for k in ["offsetX", "offsetY"] {
        let offset: i64 = pg.attribute(k).unwrap_or("0").parse().map_err(err)?;
        if offset < i32::MIN as i64 || offset > u32::MAX as i64 {
            return Err("invalid page offset".into());
        }
        hdr.extend_from_slice(&(offset as u32).to_le_bytes());
    }
    utf16(&mut hdr, id)?;
    hdr.extend_from_slice(&t.to_le_bytes());
    for k in ["formatVersion", "minFormatVersion"] {
        hdr.extend_from_slice(&opt_number(pg, k)?.to_le_bytes());
    }
    let layers: Vec<_> = pg.descendants().filter(|n| tag(*n, "layer")).collect();
    let mut body = Vec::new();
    u16b(&mut body, layers.len())?;
    body.extend_from_slice(&[0; 2]);
    let mut layer_hashes = Vec::new();
    let mut has_ink = false;
    for ly in layers {
        let lid = attr(ly, "id")?;
        let lt = number(ly, "modifiedTime")?;
        let mut ext = Vec::new();
        utf16(&mut ext, lid)?;
        ext.extend_from_slice(&lt.to_le_bytes());
        u32b(&mut body, 16 + ext.len())?;
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&[1, 2, 1, 24, 0, 0, 0, 0]);
        body.extend_from_slice(&ext);
        let objs: Vec<_> = ly
            .descendants()
            .filter(|n| {
                tag(*n, "object") && n.attribute("id").is_some_and(|id| payload.contains_key(id))
            })
            .collect();
        u32b(&mut body, objs.len())?;
        let mut object_hashes = Vec::new();
        for o in objs {
            let oid = attr(o, "id")?;
            let ot = number(o, "modifiedTime")?;
            let p = &payload[oid];
            let h = ident(oid, ot);
            body.push(1);
            body.extend_from_slice(&[0; 2]);
            u32b(&mut body, p.len() + 32)?;
            body.extend_from_slice(p);
            body.extend_from_slice(&h);
            object_hashes.extend_from_slice(&h);
            has_ink = true;
        }
        object_hashes.extend_from_slice(&ident(lid, lt));
        let lh = Sha256::digest(&object_hashes);
        body.extend_from_slice(&lh);
        layer_hashes.extend_from_slice(&lh);
    }
    layer_hashes.extend_from_slice(&ident(id, t));
    body.extend_from_slice(&Sha256::digest(&layer_hashes));
    body.extend_from_slice(b"Page for SAMSUNG S-Pen SDK");
    let mut out = Vec::new();
    u32b(&mut out, 8 + hdr.len())?;
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&hdr);
    out.extend_from_slice(&body);
    Ok((out, has_ink))
}
fn read_u32(b: &[u8], p: &mut usize) -> Result<usize> {
    let s = b.get(*p..*p + 4).ok_or("truncated page")?;
    *p += 4;
    Ok(u32::from_le_bytes(s.try_into().unwrap()) as usize)
}
fn read_u16(b: &[u8], p: &mut usize) -> Result<usize> {
    let s = b.get(*p..*p + 2).ok_or("truncated page")?;
    *p += 2;
    Ok(u16::from_le_bytes(s.try_into().unwrap()) as usize)
}
pub(crate) fn page_strokes(b: &[u8]) -> Result<Vec<Vec<u8>>> {
    Ok(page_stroke_ranges(b)?.into_iter().map(|range| b[range].to_vec()).collect())
}
fn page_stroke_ranges(b: &[u8]) -> Result<Vec<std::ops::Range<usize>>> {
    let mut p = 0;
    let header = read_u32(b, &mut p)?;
    if header > b.len() {
        return Err("invalid page header".into());
    }
    p = header;
    let layers = read_u16(b, &mut p)?;
    read_u16(b, &mut p)?;
    let mut out = Vec::new();
    for _ in 0..layers {
        let start = p;
        let size = read_u32(b, &mut p)?;
        p = start
            .checked_add(size)
            .filter(|p| *p <= b.len())
            .ok_or("invalid layer")?;
        let count = read_u32(b, &mut p)?;
        for _ in 0..count {
            let kind = *b.get(p).ok_or("truncated object")?;
            p += 1;
            let children = read_u16(b, &mut p)?;
            if children != 0 || kind != 1 {
                return Err("unsupported page object".into());
            }
            let size = read_u32(b, &mut p)?;
            if size < 32 {
                return Err("invalid object".into());
            }
            let end = p
                .checked_add(size)
                .filter(|x| *x <= b.len())
                .ok_or("truncated object")?;
            out.push(p..end - 32);
            p = end;
        }
        p = p
            .checked_add(32)
            .filter(|x| *x <= b.len())
            .ok_or("truncated layer")?;
    }
    Ok(out)
}
pub(crate) fn read_entry<R: Read + Seek>(
    z: &mut ZipArchive<R>,
    name: &str,
    hashes: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    #[cfg(feature = "read-tracing")]
    crate::read_trace::entry(name);
    let expected = hashes.get(name).ok_or("entry is absent from manifest")?;
    let mut f = z.by_name(name).map_err(err)?;
    if f.size() > MAX_ENTRY {
        return Err("oversized entry".into());
    }
    let declared = f.size();
    let mut b = Vec::with_capacity(declared as usize);
    (&mut f).take(MAX_ENTRY + 1).read_to_end(&mut b).map_err(err)?;
    if b.len() as u64 != declared || b.len() as u64 > MAX_ENTRY { return Err("invalid entry size".into()); }
    if hash(&b) != *expected {
        return Err("entry hash mismatch".into());
    }
    Ok(b)
}
// zip-rs scans backwards in 2 KiB blocks for its central-directory footer.
// During that scan hide bytes before the known directory so opening a small
// container does not read unrelated page entries.
pub(crate) struct DirectoryReader<R> {
    inner: R,
    directory_start: u64,
    directory_only: std::rc::Rc<std::cell::Cell<bool>>,
}
impl<R: Read + Seek> Read for DirectoryReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pos = self.inner.stream_position()?;
        if self.directory_only.get() && pos < self.directory_start {
            let n = (self.directory_start - pos).min(buf.len() as u64) as usize;
            buf[..n].fill(0);
            self.inner.seek(std::io::SeekFrom::Current(n as i64))?;
            if n == buf.len() {
                return Ok(n);
            }
            return Ok(n + self.inner.read(&mut buf[n..])?);
        }
        self.inner.read(buf)
    }
}
impl<R: Read + Seek> Seek for DirectoryReader<R> {
    fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(p)
    }
}
pub(crate) fn archive<R: Read + Seek>(mut reader: R) -> Result<ZipArchive<DirectoryReader<R>>> {
    use std::io::SeekFrom;
    // zip's name map collapses duplicate names, so compare its count with the
    // central-directory count before accepting the archive.
    if reader.seek(SeekFrom::End(0)).map_err(err)? < 22 {
        return Err("missing ZIP end record".into());
    }
    reader.seek(SeekFrom::End(-22)).map_err(err)?;
    let mut tail = [0u8; 22];
    reader.read_exact(&mut tail).map_err(err)?;
    if &tail[..4] != b"PK\x05\x06" || tail[20..22] != [0, 0] {
        return Err("unsupported ZIP end record".into());
    }
    let end = 0usize;
    let u16_at = |i| u16::from_le_bytes([tail[end + i], tail[end + i + 1]]);
    let disk = u16_at(4);
    let cd_disk = u16_at(6);
    let disk_count = u16_at(8);
    let count = u16_at(10);
    if disk != 0
        || cd_disk != 0
        || disk_count != count
        || count == u16::MAX
        || count as usize > MAX_ENTRIES
    {
        return Err("unsupported ZIP directory".into());
    }
    let directory_start = u32::from_le_bytes(tail[16..20].try_into().unwrap()) as u64;
    let directory_only = std::rc::Rc::new(std::cell::Cell::new(true));
    let z = ZipArchive::new(DirectoryReader {
        inner: reader,
        directory_start,
        directory_only: directory_only.clone(),
    })
    .map_err(err)?;
    directory_only.set(false);
    if z.len() != count as usize {
        return Err("duplicate ZIP name".into());
    }
    if z.decompressed_size().is_none_or(|n| n > MAX_TOTAL as u128) {
        return Err("oversized or unsupported ZIP".into());
    }
    for name in z.file_names() {
        if name.starts_with('/')
            || name
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return Err("unsafe ZIP entry".into());
        }
    }
    Ok(z)
}
pub(crate) fn manifest<R: Read + Seek>(z: &mut ZipArchive<R>) -> Result<Manifest> {
    #[cfg(feature = "read-tracing")]
    crate::read_trace::entry("cloud/manifest.json");
    let mut f = z.by_name("cloud/manifest.json").map_err(err)?;
    if f.size() > MAX_ENTRY {
        return Err("oversized manifest".into());
    }
    let mut b = Vec::new();
    f.read_to_end(&mut b).map_err(err)?;
    let m: Manifest = serde_json::from_slice(&b).map_err(err)?;
    if m.format != FORMAT {
        return Err("unsupported container format".into());
    }
    Ok(m)
}
/// Independently authenticate manifest metadata against the SQLite reference.
pub(crate) fn verified_manifest<R: Read + Seek>(
    z: &mut ZipArchive<R>,
    expected_sha256: &str,
) -> Result<Manifest> {
    #[cfg(feature = "read-tracing")]
    crate::read_trace::entry("cloud/manifest.json");
    let mut f = z.by_name("cloud/manifest.json").map_err(err)?;
    if f.size() > MAX_ENTRY {
        return Err("oversized manifest".into());
    }
    let mut b = Vec::new();
    f.read_to_end(&mut b).map_err(err)?;
    if hash(&b) != expected_sha256 {
        return Err("manifest hash differs from index".into());
    }
    let m: Manifest = serde_json::from_slice(&b).map_err(err)?;
    if m.format != FORMAT {
        return Err("unsupported container format".into());
    }
    Ok(m)
}
/// Build and verify a container before publishing it. Equal entries in the
/// previous revision are copied in their compressed form.
pub fn build(raw: &[u8], content_type: &str, previous: Option<&[u8]>) -> Result<BuiltContainer> {
    let mut previous = previous.map(Cursor::new);
    let mut output = Cursor::new(Vec::new());
    let info = build_inner(raw, content_type, previous.as_mut().map(|p| p as &mut dyn ReadSeek), None, &mut output)?;
    Ok(BuiltContainer { bytes: output.into_inner(), raw_sha256: info.raw_sha256, manifest_sha256: info.manifest_sha256, pages: info.pages, context_sha256: info.context_sha256, copied_entries: info.copied_entries })
}

/// Stream an admission-free container to a seekable output.
pub fn build_to<W: Read + Write + Seek>(raw: &[u8], content_type: &str, previous: Option<&mut dyn ReadSeek>, output: &mut W) -> Result<BuiltContainerInfo> {
    build_inner(raw, content_type, previous, None, output)
}

/// Build a container with a versioned admission proof only when the shared
/// full-snapshot verifier accepts the provided snapshot identity and content.
pub fn build_with_snapshot(
    raw: &[u8],
    content_type: &str,
    previous: Option<&[u8]>,
    resource_xml: &[u8],
    local_id: Option<&str>,
    commit_id: Option<&str>,
) -> Result<BuiltContainer> {
    let mut previous = previous.map(Cursor::new);
    let mut output = Cursor::new(Vec::new());
    let info = build_with_snapshot_to(raw, content_type, previous.as_mut().map(|p| p as &mut dyn ReadSeek), resource_xml, local_id, commit_id, &mut output)?;
    Ok(BuiltContainer { bytes: output.into_inner(), raw_sha256: info.raw_sha256, manifest_sha256: info.manifest_sha256, pages: info.pages, context_sha256: info.context_sha256, copied_entries: info.copied_entries })
}

/// Write a container to a seekable file without retaining the ZIP in memory.
pub fn build_with_snapshot_to<W: Read + Write + Seek>(
    raw: &[u8], content_type: &str, previous: Option<&mut dyn ReadSeek>,
    resource_xml: &[u8], local_id: Option<&str>, commit_id: Option<&str>, output: &mut W,
) -> Result<BuiltContainerInfo> {
    let started = std::time::Instant::now();
    let verified = _ops::ink_projection::verify_raw_snapshot_admission(
        raw,
        content_type,
        resource_xml,
        local_id,
        commit_id,
    );
    let admission_ms = started.elapsed().as_millis();
    let admission = match (verified, local_id.zip(commit_id)) {
        (Ok(true), Some((local, commit))) => {
            let limits = rs_doc::mime::Limits::default();
            let parts = rs_doc::mime::parts(raw, content_type, limits.max_readonly_parts, &limits, &mut rs_doc::mime::NoHooks).map_err(err)?;
            let bodies: Vec<&[u8]> = parts
                .iter()
                .filter(|p| p.name.as_deref() == Some("strokeResource"))
                .map(|p| &raw[p.body.clone()])
                .collect();
            let strokes_verified =
                rs_ink::core::records::validate_records(resource_xml, &bodies).is_ok();
            Some((local, commit, resource_xml, strokes_verified))
        }
        _ => None,
    };
    let bindings_ms = started.elapsed().as_millis() - admission_ms;
    let result = build_inner(raw, content_type, previous, admission, output);
    if std::env::var_os("NOTES_MIGRATION_PROFILE").is_some() {
        eprintln!("container-build input_bytes={} admission_ms={} binding_ms={} zip_ms={}", raw.len(), admission_ms, bindings_ms, started.elapsed().as_millis() - admission_ms - bindings_ms);
    }
    result
}

fn write_entry<W: Write + Seek, R: Read + Seek>(
    writer: &mut ZipWriter<&mut W>, old: &mut Option<ZipArchive<R>>,
    old_manifest: &Option<Manifest>, hashes: &mut BTreeMap<String, String>,
    copied: &mut Vec<String>, name: String, data: &[u8],
) -> Result<()> {
    let digest = hash(data);
    let same = old_manifest.as_ref().and_then(|m| m.entry_sha256.get(&name)).is_some_and(|h| *h == digest);
    if same {
        // A previous manifest is not authoritative: verify the uncompressed
        // bytes and ZIP CRC before carrying an entry forward.
        {
            let mut file = old.as_mut().unwrap().by_name(&name).map_err(err)?;
            let declared = file.size();
            if declared > MAX_ENTRY { return Err("oversized entry".into()); }
            let mut actual = Sha256::new();
            let mut total = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let n = file.read(&mut buffer).map_err(err)?;
                if n == 0 { break; }
                total = total.checked_add(n as u64).ok_or("invalid entry size")?;
                if total > MAX_ENTRY { return Err("oversized entry".into()); }
                actual.update(&buffer[..n]);
            }
            if total != declared || format!("{:x}", actual.finalize()) != digest {
                return Err("previous entry hash mismatch".into());
            }
        }
        let f = old.as_mut().unwrap().by_name(&name).map_err(err)?;
        writer.raw_copy_file(f).map_err(err)?;
        copied.push(name.clone());
    } else {
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).compression_level(Some(6));
        writer.start_file(&name, opts).map_err(err)?;
        writer.write_all(data).map_err(err)?;
    }
    hashes.insert(name, digest);
    Ok(())
}
fn build_inner<W: Read + Write + Seek>(
    raw: &[u8],
    content_type: &str,
    previous: Option<&mut dyn ReadSeek>,
    admission: Option<(&str, &str, &[u8], bool)>,
    output: &mut W,
) -> Result<BuiltContainerInfo> {
    if raw.len() as u64 > MAX_TOTAL {
        return Err("oversized snapshot".into());
    }
    let limits = rs_doc::mime::Limits::default();
    let parts = rs_doc::mime::parts(
        raw,
        content_type,
        limits.max_readonly_parts,
        &limits,
        &mut rs_doc::mime::NoHooks,
    )
    .map_err(err)?;
    let xml_idx = parts
        .iter()
        .position(|p| p.name.as_deref() == Some("noteResource"))
        .or_else(|| if parts.len() == 1 { Some(0) } else { None })
        .ok_or("missing noteResource")?;
    let xml = std::str::from_utf8(&raw[parts[xml_idx].body.clone()]).map_err(err)?;
    let doc = roxmltree::Document::parse(xml).map_err(err)?;
    let pgs: Vec<_> = doc
        .descendants()
        .filter(|n| {
            tag(*n, "page") && !tree_page(*n) && n.parent().is_some_and(|p| tag(p, "pageList"))
        })
        .collect();
    let tree: Vec<_> = doc
        .descendants()
        .filter(|n| tag(*n, "page") && tree_page(*n))
        .collect();
    let mut ids = HashSet::new();
    for pg in &pgs {
        let id = attr(*pg, "id")?;
        if !safe_id(id) || !ids.insert(id.to_string()) {
            return Err("invalid or duplicate page ID".into());
        }
    }
    let object_ids: HashSet<&str> = pgs
        .iter()
        .flat_map(|pg| pg.descendants())
        .filter(|n| tag(*n, "object"))
        .filter_map(|n| n.attribute("id"))
        .collect();
    let mut parts_meta = Vec::new();
    let mut payload = HashMap::new();
    let mut extra = BTreeMap::new();
    for (i, p) in parts.iter().enumerate() {
        if i == xml_idx {
            parts_meta.push(Part {
                id: String::new(),
                prefix: Vec::new(),
                entry: None,
                xml: true,
            });
            continue;
        }
        let body = &raw[p.body.clone()];
        let oid = p
            .filename
            .as_deref()
            .and_then(|s| s.split_once('_'))
            .map(|x| x.0);
        let known = oid.is_some_and(|id| {
            p.name.as_deref() == Some("strokeResource")
                && body.len() >= 12
                && object_ids.contains(id)
        });
        if known {
            let id = oid.unwrap().to_string();
            if payload.insert(id.clone(), &body[12..]).is_some() {
                return Err("duplicate stroke ID".into());
            }
            parts_meta.push(Part {
                id,
                prefix: body[..12].to_vec(),
                entry: None,
                xml: false,
            });
        } else {
            let name = format!("cloud/parts/{i}");
            extra.insert(name.clone(), body);
            parts_meta.push(Part {
                id: String::new(),
                prefix: Vec::new(),
                entry: Some(name),
                xml: false,
            });
        }
    }
    let mut old = previous.map(archive).transpose()?;
    let old_manifest = match old.as_mut() { Some(z) => Some(manifest(z)?), None => None };
    if output.seek(SeekFrom::End(0)).map_err(err)? != 0 { return Err("container output must be empty".into()); }
    output.seek(SeekFrom::Start(0)).map_err(err)?;
    let mut writer = ZipWriter::new(&mut *output);
    let mut hashes = BTreeMap::new();
    let mut copied = Vec::new();
    let mut entries: BTreeMap<String, Cow<'_, [u8]>> = BTreeMap::new();
    let mut pages = Vec::new();
    let mut pinfo = vec![0; 32];
    u16b(&mut pinfo, pgs.len())?;
    for pg in &pgs {
        let id = attr(*pg, "id")?;
        utf16(&mut pinfo, id)?;
        let h = pg.attribute("hash").unwrap_or("");
        if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
            for i in (0..64).step_by(2) {
                pinfo.push(u8::from_str_radix(&h[i..i + 2], 16).map_err(err)?);
            }
        } else {
            pinfo.extend_from_slice(&[0; 32]);
        }
    }
    write_entry(&mut writer, &mut old, &old_manifest, &mut hashes, &mut copied, "pageIdInfo.dat".into(), &pinfo)?;
    for pg in &pgs {
        let id = attr(*pg, "id")?;
        let (binary, has_ink) = page_binary(*pg, &payload)?;
        let page_sha256 = hash(&binary);
        write_entry(&mut writer, &mut old, &old_manifest, &mut hashes, &mut copied, format!("{id}.page"), &binary)?;
        pages.push(PageInfo {
            page_id: id.into(),
            page_sha256,
            xml_sha256: String::new(),
            has_ink,
        });
    }
    let split = !xml.contains("<![CDATA[")
        && !xml.contains("<!--")
        && tree.len() == pgs.len()
        && tree
            .iter()
            .filter_map(|p| p.attribute("id"))
            .collect::<HashSet<_>>()
            .len()
            == pgs.len()
        && tree
            .iter()
            .all(|p| p.attribute("id").is_some_and(|id| ids.contains(id)))
        && !pgs
            .iter()
            .any(|p| p.descendants().skip(1).any(|n| tag(n, "page")));
    let mut spans: Vec<_> = if split {
        tree.iter()
            .chain(pgs.iter())
            .map(|p| (p.range(), p.attribute("id").unwrap().to_string()))
            .collect()
    } else {
        Vec::new()
    };
    spans.sort_by_key(|x| x.0.start);
    let mut env = Vec::new();
    let mut holes = Vec::new();
    let mut by_page: HashMap<String, Vec<std::ops::Range<usize>>> = HashMap::new();
    let mut last = 0;
    for (range, id) in spans {
        if range.start < last || range.end > xml.len() {
            return Err("overlapping page spans".into());
        }
        env.extend_from_slice(&xml.as_bytes()[last..range.start]);
        let v = by_page.entry(id.clone()).or_default();
        holes.push(Hole {
            offset: env.len(),
            page: id,
            span: v.len(),
            after_download: false,
        });
        v.push(range.clone());
        last = range.end;
    }
    env.extend_from_slice(&xml.as_bytes()[last..]);
    let mut page_xml_entries = BTreeMap::new();
    for p in &mut pages {
        if let Some(v) = by_page.get(&p.page_id) {
            if v.len() != 2 {
                return Err("unpaired page XML".into());
            }
            let len = u32::try_from(v[0].len()).map_err(err)?;
            let mut digest = Sha256::new();
            digest.update(len.to_le_bytes());
            digest.update(&xml.as_bytes()[v[0].clone()]);
            digest.update(&xml.as_bytes()[v[1].clone()]);
            p.xml_sha256 = format!("{:x}", digest.finalize());
            let name = format!("cloud/pages/{}.xml", p.page_id);
            hashes.insert(name.clone(), p.xml_sha256.clone());
            page_xml_entries.insert(name, (v[0].clone(), v[1].clone()));
        }
    }
    let dl = doc
        .descendants()
        .find(|n| tag(*n, "downloadUrls"))
        .map(|n| n.range());
    let mut dl_off = None;
    let mut dl_bytes = Vec::new();
    if let Some(r) = dl {
        if !holes.is_empty()
            && tree
                .iter()
                .chain(pgs.iter())
                .any(|p| p.range().start <= r.start && p.range().end >= r.end)
        {
            return Err("downloadUrls within page".into());
        }
        let removed: usize = doc
            .descendants()
            .filter(|n| {
                tag(*n, "page") && (tree_page(*n) || n.parent().is_some_and(|p| tag(p, "pageList")))
            })
            .filter(|n| n.range().end <= r.start && split)
            .map(|n| n.range().len())
            .sum();
        let start = r.start.checked_sub(removed).ok_or("invalid URL offset")?;
        let end = start
            .checked_add(r.len())
            .filter(|e| *e <= env.len())
            .ok_or("invalid URL span")?;
        dl_bytes = env[start..end].to_vec();
        env.drain(start..end);
        for h in &mut holes {
            if h.offset >= end {
                h.offset -= r.len();
                h.after_download = true;
            }
        }
        dl_off = Some(start);
    }
    // The index is advisory until it is independently bound by SQLite. An
    // unsplit XML has no page-scoped XML entries, so do not advertise it.
    let context = if split {
        crate::page_context::build(raw, content_type)?
    } else {
        None
    };
    let context = context
        .map(|c| {
            let admitted = admission.and_then(|(local, commit, resource, strokes_verified)| {
                crate::page_context::bind_admission(&c, local, commit, resource, strokes_verified)
                    .ok()
            });
            crate::page_context::bind_globals(admitted.as_deref().unwrap_or(&c), &env, &dl_bytes)
        })
        .transpose()?;
    let context_sha256 = context.as_deref().map(hash);
    if let Some(context) = context {
        entries.insert("cloud/context.json".into(), context.into());
    }
    entries.insert("cloud/note.xml".into(), env.into());
    entries.insert("cloud/downloadUrls.xml".into(), dl_bytes.into());
    entries.extend(extra.into_iter().map(|(name, body)| (name, Cow::Borrowed(body))));
    // Most Cloud snapshots have deterministic MIME headers, part order and
    // wrapper. Store only deviations; retain arbitrary framing as a fallback.
    let mut boundary = None;
    let mut part_order = None;
    let mut prefix_exceptions = BTreeMap::new();
    let mut suffix_exceptions = BTreeMap::new();
    let mut tree_times = HashMap::new();
    let mut implied = Vec::new();
    let stroke_ids: HashSet<_> = pgs
        .iter()
        .flat_map(|p| p.descendants())
        .filter(|n| {
            tag(*n, "object")
                && n.attribute("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("stroke"))
        })
        .filter_map(|n| n.attribute("id"))
        .collect();
    for pg in &tree {
        for o in pg.descendants().filter(|n| tag(*n, "object")) {
            if let (Some(id), Some(t)) = (o.attribute("id"), o.attribute("t")) {
                if stroke_ids.contains(id) {
                    implied.push(id.to_string());
                    tree_times.insert(id.to_string(), t.to_string());
                }
            }
        }
    }
    let parsed_boundary =
        rs_doc::mime::content_type_headers(content_type, &limits, &mut rs_doc::mime::NoHooks)
            .ok()
            .and_then(|h| h.boundary().ok().flatten());
    if let Some(candidate) = parsed_boundary {
        let known = xml_idx == 0
            && parts_meta
                .iter()
                .skip(1)
                .all(|p| !p.xml && p.entry.is_none())
            && payload.len() == stroke_ids.len()
            && stroke_ids.iter().all(|id| payload.contains_key(*id))
            && implied.len() == payload.len();
        if known {
            let mut canonical = true;
            let mut pos = 0usize;
            let mut actual_order = Vec::new();
            for (i, p) in parts.iter().enumerate() {
                let body = &raw[p.body.clone()];
                if i == 0 {
                    let header = standard_header(&candidate, "noteResource", None, body.len());
                    canonical &= raw.get(pos..p.body.start) == Some(header.as_bytes());
                    canonical &= raw.get(p.body.end..p.body.end.saturating_add(2)) == Some(b"\r\n");
                    pos = p.body.end.saturating_add(2);
                    continue;
                }
                let id = &parts_meta[i].id;
                let Some((_, suffix)) = p.filename.as_deref().and_then(|f| f.split_once('_'))
                else {
                    canonical = false;
                    break;
                };
                let Some(t) = tree_times.get(id) else {
                    canonical = false;
                    break;
                };
                if suffix != t {
                    suffix_exceptions.insert(id.clone(), suffix.to_string());
                }
                if body[..12] != WRAPPER {
                    prefix_exceptions.insert(id.clone(), body[..12].to_vec());
                }
                actual_order.push(id.clone());
                let header = standard_header(&candidate, "strokeResource", p.filename.as_deref(), body.len());
                canonical &= raw.get(pos..p.body.start) == Some(header.as_bytes());
                canonical &= raw.get(p.body.end..p.body.end.saturating_add(2)) == Some(b"\r\n");
                pos = p.body.end.saturating_add(2);
            }
            let end = format!("--{candidate}--\r\n");
            canonical &= raw.get(pos..) == Some(end.as_bytes());
            if canonical {
                boundary = Some(candidate);
                if actual_order != implied {
                    part_order = Some(actual_order);
                }
                parts_meta.clear();
            } else {
                prefix_exceptions.clear();
                suffix_exceptions.clear();
            }
        }
    }
    if boundary.is_none() {
        let mut framing = Vec::new();
        let mut last = 0;
        for p in &parts {
            u32b(&mut framing, p.body.start - last)?;
            framing.extend_from_slice(&raw[last..p.body.start]);
            last = p.body.end;
        }
        u32b(&mut framing, raw.len() - last)?;
        framing.extend_from_slice(&raw[last..]);
        entries.insert("cloud/framing".into(), framing.into());
    }
    hashes.extend(entries.iter().map(|(k, v)| (k.clone(), hash(v))));
    let man = Manifest {
        format: FORMAT.into(),
        content_type: content_type.into(),
        boundary,
        raw_sha256: hash(raw),
        holes,
        download_offset: dl_off,
        parts: parts_meta,
        part_order,
        prefix_exceptions,
        suffix_exceptions,
        entry_sha256: hashes.clone(),
    };
    let manifest_bytes = serde_json::to_vec(&man).map_err(err)?;
    let manifest_sha256 = hash(&manifest_bytes);
    entries.insert("cloud/manifest.json".into(), manifest_bytes.into());
    let mut names: Vec<_> = entries.keys().chain(page_xml_entries.keys()).cloned().collect();
    names.sort();
    for name in names {
        if let Some(data) = entries.remove(&name) {
            write_entry(&mut writer, &mut old, &old_manifest, &mut hashes, &mut copied, name, &data)?;
        } else {
            let (first, second) = page_xml_entries.remove(&name).ok_or("missing page XML")?;
            let mut data = Vec::with_capacity(4 + first.len() + second.len());
            u32b(&mut data, first.len())?;
            data.extend_from_slice(&xml.as_bytes()[first]);
            data.extend_from_slice(&xml.as_bytes()[second]);
            write_entry(&mut writer, &mut old, &old_manifest, &mut hashes, &mut copied, name, &data)?;
        }
    }
    writer.finish().map_err(err)?;
    output.seek(SeekFrom::Start(0)).map_err(err)?;
    let rebuilt = reconstruct(&mut *output)?;
    if rebuilt.raw_body != raw {
        return Err("snapshot round-trip mismatch".into());
    }
    Ok(BuiltContainerInfo {
        raw_sha256: hash(raw),
        manifest_sha256,
        pages,
        context_sha256,
        copied_entries: copied,
    })
}
/// Read the page index and verify it against the independent SQLite digest.
pub fn read_context<R: Read + Seek>(
    reader: R,
    expected_context_sha256: &str,
    expected_raw_sha256: &str,
) -> Result<crate::page_context::PageContext> {
    let mut z = archive(reader)?;
    let m = manifest(&mut z)?;
    let bytes = read_entry(&mut z, "cloud/context.json", &m.entry_sha256)?;
    if hash(&bytes) != expected_context_sha256 {
        return Err("page context hash differs from index".into());
    }
    crate::page_context::parse(&bytes, expected_raw_sha256)
}
/// A global index and the exact split XML entries it authenticates.
#[derive(Clone, Debug)]
pub struct GlobalPageContext {
    pub index: crate::page_context::PageContext,
    pub note_xml: Vec<u8>,
    pub download_urls: Vec<u8>,
}
/// Verify global data against the index independently bound to SQLite.
pub fn read_global_context<R: Read + Seek>(
    reader: R,
    expected_context_sha256: &str,
    expected_raw_sha256: &str,
) -> Result<GlobalPageContext> {
    let mut z = archive(reader)?;
    let m = manifest(&mut z)?;
    let bytes = read_entry(&mut z, "cloud/context.json", &m.entry_sha256)?;
    if hash(&bytes) != expected_context_sha256 {
        return Err("page context hash differs from index".into());
    }
    let index = crate::page_context::parse(&bytes, expected_raw_sha256)?;
    let note_xml = read_entry(&mut z, "cloud/note.xml", &m.entry_sha256)?;
    let download_urls = read_entry(&mut z, "cloud/downloadUrls.xml", &m.entry_sha256)?;
    if hash(&note_xml) != index.note_xml_sha256
        || hash(&download_urls) != index.download_urls_sha256
    {
        return Err("global context hash differs from index".into());
    }
    Ok(GlobalPageContext {
        index,
        note_xml,
        download_urls,
    })
}
/// Read only the two entries belonging to a page, verifying their hashes.
pub fn read_page<R: Read + Seek>(reader: R, page_id: &str) -> Result<PageData> {
    if !safe_id(page_id) {
        return Err("invalid page ID".into());
    }
    let mut z = archive(reader)?;
    let m = manifest(&mut z)?;
    Ok(PageData {
        page: read_entry(&mut z, &format!("{page_id}.page"), &m.entry_sha256)?,
        xml: read_entry(
            &mut z,
            &format!("cloud/pages/{page_id}.xml"),
            &m.entry_sha256,
        )?,
    })
}
/// Verify a partial read against SQLite's independently stored entry hashes.
pub fn read_page_verified<R: Read + Seek>(
    reader: R,
    page_id: &str,
    page_sha256: &str,
    xml_sha256: &str,
) -> Result<PageData> {
    let page = read_page(reader, page_id)?;
    if hash(&page.page) != page_sha256 || hash(&page.xml) != xml_sha256 {
        return Err("page hash differs from index".into());
    }
    Ok(page)
}
struct StrokePageCache {
    page: String,
    bytes: Vec<u8>,
    ranges: Vec<std::ops::Range<usize>>,
}
#[cfg(test)]
thread_local! { static MAX_CACHED_STROKE_PAGE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
impl StrokePageCache {
    fn get<'a, R: Read + Seek>(cache: &'a mut Option<Self>, zip: &mut ZipArchive<R>, manifest: &Manifest, page: &str, index: usize, count: usize) -> Result<&'a [u8]> {
        if cache.as_ref().is_none_or(|c| c.page != page) {
            // Release the previous page before decompressing the next one.
            *cache = None;
            let bytes = read_entry(zip, &format!("{page}.page"), &manifest.entry_sha256)?;
            let ranges = page_stroke_ranges(&bytes)?;
            if ranges.len() != count { return Err("stroke count mismatch".into()); }
            #[cfg(test)]
            MAX_CACHED_STROKE_PAGE.with(|max| max.set(max.get().max(bytes.len())));
            *cache = Some(Self { page: page.to_owned(), bytes, ranges });
        }
        let cached = cache.as_ref().unwrap();
        let range = cached.ranges.get(index).ok_or("missing stroke")?;
        Ok(&cached.bytes[range.clone()])
    }
}

/// Reconstruct and hash-check the original Cloud bytes. Only one page's
/// stroke data and one page's XML are retained at a time.
pub fn reconstruct<R: Read + Seek>(reader: R) -> Result<Reconstructed> {
    let mut z = archive(reader)?;
    let m = manifest(&mut z)?;
    let mut env = read_entry(&mut z, "cloud/note.xml", &m.entry_sha256)?;
    let dl = read_entry(&mut z, "cloud/downloadUrls.xml", &m.entry_sha256)?;
    if let Some(offset) = m.download_offset {
        if offset > env.len() {
            return Err("invalid URL offset".into());
        }
        env.splice(offset..offset, dl.iter().copied());
    } else if !dl.is_empty() {
        return Err("unexpected URL data".into());
    }
    let mut xml = Vec::new();
    let mut last = 0;
    for h in &m.holes {
        let offset = h
            .offset
            .checked_add(if h.after_download { dl.len() } else { 0 })
            .ok_or("invalid hole")?;
        if offset < last || offset > env.len() {
            return Err("invalid hole offset".into());
        }
        xml.extend_from_slice(&env[last..offset]);
        if !safe_id(&h.page) { return Err("invalid page ID".into()); }
        let b = read_entry(&mut z, &format!("cloud/pages/{}.xml", h.page), &m.entry_sha256)?;
        let mut p = 0;
        let n = read_u32(&b, &mut p)?;
        let end = p.checked_add(n).filter(|x| *x <= b.len()).ok_or("invalid XML page")?;
        let span = match h.span { 0 => &b[p..end], 1 => &b[end..], _ => return Err("invalid page span".into()) };
        if xml
            .len()
            .checked_add(span.len())
            .is_none_or(|n| n as u64 > MAX_TOTAL)
        {
            return Err("oversized reconstructed XML".into());
        }
        xml.extend_from_slice(span);
        last = offset;
    }
    xml.extend_from_slice(&env[last..]);
    let resource_xml = String::from_utf8(xml).map_err(err)?;
    let doc = roxmltree::Document::parse(&resource_xml).map_err(err)?;
    let ids: HashSet<&str> = if m.boundary.is_some() {
        doc.descendants()
            .filter(|n| {
                tag(*n, "page") && !tree_page(*n) && n.parent().is_some_and(|p| tag(p, "pageList"))
            })
            .flat_map(|p| p.descendants())
            .filter(|n| {
                tag(*n, "object")
                    && n.attribute("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("stroke"))
            })
            .filter_map(|n| n.attribute("id"))
            .collect()
    } else {
        m.parts
            .iter()
            .filter(|p| !p.xml && p.entry.is_none())
            .map(|p| p.id.as_str())
            .collect()
    };
    let mut strokes = HashMap::new();
    let mut page_counts = HashMap::new();
    for pg in doc.descendants().filter(|n| {
        tag(*n, "page") && !tree_page(*n) && n.parent().is_some_and(|p| tag(p, "pageList"))
    }) {
        let id = attr(pg, "id")?;
        if !safe_id(id) {
            return Err("invalid page ID".into());
        }
        let objects: Vec<_> = pg
            .descendants()
            .filter(|n| tag(*n, "object") && n.attribute("id").is_some_and(|id| ids.contains(id)))
            .collect();
        if page_counts.insert(id, objects.len()).is_some() { return Err("duplicate page ID".into()); }
        for (index, o) in objects.into_iter().enumerate() {
            if strokes
                .insert(attr(o, "id")?, (id, index))
                .is_some()
            {
                return Err("duplicate object ID".into());
            }
        }
    }
    let mut raw = Vec::new();
    let mut cache = None;
    let mut visited = HashSet::new();
    if let Some(boundary) = &m.boundary {
        standard_part(
            &mut raw,
            boundary,
            "noteResource",
            None,
            resource_xml.as_bytes(),
        );
        let mut times = HashMap::new();
        let mut implied = Vec::new();
        for pg in doc
            .descendants()
            .filter(|n| tag(*n, "page") && tree_page(*n))
        {
            for o in pg.descendants().filter(|n| tag(*n, "object")) {
                if let (Some(id), Some(t)) = (o.attribute("id"), o.attribute("t")) {
                    if ids.contains(id) {
                        if times.insert(id, t).is_some() {
                            return Err("duplicate tree stroke".into());
                        }
                        implied.push(id.to_string());
                    }
                }
            }
        }
        let order = m.part_order.as_ref().unwrap_or(&implied);
        let mut seen = HashSet::new();
        for id in order {
            if raw.len() as u64 > MAX_TOTAL {
                return Err("oversized reconstructed snapshot".into());
            }
            if !seen.insert(id) {
                return Err("duplicate stroke part".into());
            }
            let t = times.get(id.as_str()).ok_or("missing tree stroke")?;
            let suffix = m.suffix_exceptions.get(id).map(String::as_str).unwrap_or(t);
            let prefix = m
                .prefix_exceptions
                .get(id)
                .map(Vec::as_slice)
                .unwrap_or(&WRAPPER);
            let &(page, index) = strokes.get(id.as_str()).ok_or("missing stroke")?;
            let payload = StrokePageCache::get(&mut cache, &mut z, &m, page, index, page_counts[page])?;
            visited.insert(page);
            let filename = format!("{id}_{suffix}");
            raw.extend_from_slice(standard_header(boundary, "strokeResource", Some(&filename), prefix.len() + payload.len()).as_bytes());
            raw.extend_from_slice(prefix);
            raw.extend_from_slice(payload);
            raw.extend_from_slice(b"\r\n");
        }
        if seen.len() != strokes.len() {
            return Err("incomplete stroke order".into());
        }
        standard_end(&mut raw, boundary);
    } else {
        let framing = read_entry(&mut z, "cloud/framing", &m.entry_sha256)?;
        let mut pos = 0;
        for part in &m.parts {
            if raw.len() as u64 > MAX_TOTAL {
                return Err("oversized reconstructed snapshot".into());
            }
            let len = read_u32(&framing, &mut pos)?;
            let end = pos
                .checked_add(len)
                .filter(|x| *x <= framing.len())
                .ok_or("invalid framing")?;
            raw.extend_from_slice(&framing[pos..end]);
            pos = end;
            if part.xml {
                raw.extend_from_slice(resource_xml.as_bytes());
            } else if let Some(name) = &part.entry {
                raw.extend_from_slice(&read_entry(&mut z, name, &m.entry_sha256)?);
            } else {
                raw.extend_from_slice(&part.prefix);
                let &(page, index) = strokes.get(part.id.as_str()).ok_or("missing stroke")?;
                let payload = StrokePageCache::get(&mut cache, &mut z, &m, page, index, page_counts[page])?;
                visited.insert(page);
                raw.extend_from_slice(payload);
            }
        }
        let len = read_u32(&framing, &mut pos)?;
        let end = pos
            .checked_add(len)
            .filter(|x| *x == framing.len())
            .ok_or("invalid trailing framing")?;
        raw.extend_from_slice(&framing[pos..end]);
    }
    for (page, count) in page_counts {
        if !visited.contains(page) {
            let bytes = read_entry(&mut z, &format!("{page}.page"), &m.entry_sha256)?;
            if page_stroke_ranges(&bytes)?.len() != count { return Err("stroke count mismatch".into()); }
        }
    }
    if raw.len() as u64 > MAX_TOTAL {
        return Err("oversized reconstructed snapshot".into());
    }
    if hash(&raw) != m.raw_sha256 {
        return Err("snapshot hash mismatch".into());
    }
    Ok(Reconstructed {
        raw_body: raw,
        content_type: m.content_type,
        resource_xml,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(extra: bool) -> Vec<u8> {
        let xml=format!("<CompareNoteView><noteTree><pageList><page id=\"p1\"><layer id=\"l1\"><object id=\"s1\" t=\"7\"/>{}</layer></page><page id=\"p2\"><layer id=\"l2\"><object id=\"s2\" t=\"8\"/></layer></page></pageList></noteTree><pageList><page id=\"p1\" modifiedTime=\"2\" width=\"100\" height=\"200\"><layer id=\"l1\" modifiedTime=\"3\"><object id=\"s1\" modifiedTime=\"7\" type=\"stroke\"/><object id=\"image\" type=\"image\"/>{}</layer><pdfDataList/></page><page id=\"p2\" modifiedTime=\"4\" width=\"100\" height=\"200\"><layer id=\"l2\" modifiedTime=\"5\"><object id=\"s2\" modifiedTime=\"8\" type=\"stroke\"/><object id=\"shape\" type=\"shape\"/></layer></page></pageList><floatingObjectList/><downloadUrls><url>synthetic</url></downloadUrls></CompareNoteView>",if extra {"<object id=\"s3\" t=\"9\"/>"} else {""},if extra {"<object id=\"s3\" modifiedTime=\"9\" type=\"stroke\"/>"} else {""});
        let mut bodies = vec![
            ("noteResource", None, xml.into_bytes()),
            (
                "strokeResource",
                Some("s1_7"),
                [WRAPPER.as_slice(), b"stroke-1"].concat(),
            ),
            (
                "strokeResource",
                Some("s2_8"),
                [
                    [255, 255, 255, 255, 4, 0, 0, 0, 0, 0, 0, 0].as_slice(),
                    b"stroke-2",
                ]
                .concat(),
            ),
        ];
        if extra {
            bodies.push((
                "strokeResource",
                Some("s3_9"),
                [WRAPPER.as_slice(), b"stroke-3"].concat(),
            ));
        }
        let mut raw = Vec::new();
        for (name, filename, body) in bodies {
            raw.extend_from_slice(format!("--test-boundary\r\nContent-Disposition: form-data; name=\"{name}\"{}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",filename.map(|f|format!("; filename=\"{f}\"")).unwrap_or_default(),if filename.is_some(){"application/octet-stream"}else{"application/xml"},body.len()).as_bytes());
            raw.extend_from_slice(&body);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"--test-boundary--\r\n");
        raw
    }
    const CT: &str = "multipart/form-data; boundary=test-boundary";
    #[cfg(target_os = "linux")]
    #[test]
    fn subprocess_large_reconstruction_helper() {
        let Some(path) = std::env::var_os("NOTES_TEST_CONTAINER_MEMORY_FILE") else { return };
        let file = std::fs::File::open(path).unwrap();
        // getrusage peak can be inherited from the test runner at fork, so
        // reset this process's own Linux high-water mark before measurement.
        std::fs::write("/proc/self/clear_refs", b"5").unwrap();
        let result = reconstruct(file).unwrap();
        MAX_CACHED_STROKE_PAGE.with(|max| assert!(max.get() > 32 * 1024 && max.get() < 40 * 1024, "single cached page size {}", max.get()));
        let expected = std::env::var("NOTES_TEST_CONTAINER_MEMORY_HASH").unwrap();
        assert_eq!(hash(&result.raw_body), expected);
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let peak = status.lines().find_map(|line| line.strip_prefix("VmHWM:")).unwrap().split_whitespace().next().unwrap();
        println!("container-max-rss-kib={peak}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn large_reconstruction_is_bounded_and_byte_exact() {
        let mut tree = String::from("<noteTree><pageList>");
        let mut pages = String::from("<pageList>");
        for i in 0..520 {
            tree.push_str(&format!("<page id=\"p{i}\"><layer id=\"l{i}\"><object id=\"s{i}\" t=\"1\"/></layer></page>"));
            pages.push_str(&format!("<page id=\"p{i}\" modifiedTime=\"1\"><layer id=\"l{i}\" modifiedTime=\"1\"><object id=\"s{i}\" type=\"stroke\" modifiedTime=\"1\"/></layer></page>"));
        }
        tree.push_str("</pageList></noteTree>");
        pages.push_str("</pageList>");
        let xml = format!("<CompareNoteView>{tree}{pages}</CompareNoteView>");
        let mut raw = Vec::new();
        standard_part(&mut raw, "test-boundary", "noteResource", None, xml.as_bytes());
        let body = [WRAPPER.as_slice(), &vec![42u8; 32 * 1024]].concat();
        for i in 0..520 {
            standard_part(&mut raw, "test-boundary", "strokeResource", Some(&format!("s{i}_1")), &body);
        }
        standard_end(&mut raw, "test-boundary");
        let expected_hash = hash(&raw);
        let built = build(&raw, CT, None).unwrap();
        let dir = std::env::temp_dir().join(format!("container-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("synthetic.sdocx");
        std::fs::write(&path, &built.bytes).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "note_container::tests::subprocess_large_reconstruction_helper", "--nocapture"])
            .env("NOTES_TEST_CONTAINER_MEMORY_FILE", &path)
            .env("NOTES_TEST_CONTAINER_MEMORY_HASH", expected_hash)
            .output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let out = String::from_utf8(result.stdout).unwrap();
        let rss: usize = out.lines().find_map(|line| line.strip_prefix("container-max-rss-kib=")).unwrap().parse().unwrap();
        println!("520-page synthetic snapshot: {} bytes, reconstruction peak RSS {rss} KiB", raw.len());
        assert!(rss < raw.len() * 3 / 1024 + 32 * 1024, "peak RSS {rss} KiB for {} input bytes", raw.len());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn roundtrip_and_copy_unchanged_page() {
        let raw = fixture(false);
        let a = build(&raw, CT, None).unwrap();
        assert_eq!(reconstruct(Cursor::new(&a.bytes)).unwrap().raw_body, raw);
        let mut z = archive(Cursor::new(&a.bytes)).unwrap();
        let m = manifest(&mut z).unwrap();
        assert_eq!(m.boundary.as_deref(), Some("test-boundary"));
        assert!(m.parts.is_empty() && m.part_order.is_none());
        assert_eq!(m.prefix_exceptions.len(), 1);
        assert!(z.by_name("cloud/framing").is_err());
        let p = read_page(Cursor::new(&a.bytes), "p2").unwrap();
        assert!(p.xml.windows(5).any(|w| w == b"shape"));
        assert_eq!(page_strokes(&p.page).unwrap(), vec![b"stroke-2".to_vec()]);
        let next = fixture(true);
        let b = build(&next, CT, Some(&a.bytes)).unwrap();
        assert_eq!(reconstruct(Cursor::new(&b.bytes)).unwrap().raw_body, next);
        let mut z = archive(Cursor::new(&b.bytes)).unwrap();
        assert!(manifest(&mut z).unwrap().part_order.is_some());
        assert!(b.copied_entries.contains(&"p2.page".to_string()));
        assert!(b.copied_entries.contains(&"cloud/pages/p2.xml".to_string()));
        assert!(!b.copied_entries.contains(&"p1.page".to_string()));
    }
    #[test]
    fn file_stream_matches_memory_and_reuses_unchanged_entries() {
        let original = fixture(false);
        let old = build(&original, CT, None).unwrap();
        let temp = std::env::temp_dir().join(format!("container-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp).unwrap();
        let old_path = temp.join("old.sdocx");
        std::fs::write(&old_path, &old.bytes).unwrap();
        let mut previous = std::fs::File::open(&old_path).unwrap();
        let new_path = temp.join("new.sdocx");
        let mut output = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&new_path).unwrap();
        let raw = fixture(true);
        let info = build_with_snapshot_to(&raw, CT, Some(&mut previous), b"unsupported synthetic admission", None, None, &mut output).unwrap();
        assert!(info.copied_entries.contains(&"p2.page".to_string()));
        assert!(info.copied_entries.contains(&"cloud/pages/p2.xml".to_string()));
        output.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(reconstruct(&mut output).unwrap().raw_body, raw);
        assert_eq!(std::fs::read(&old_path).unwrap(), old.bytes);
        let expected = build(&raw, CT, Some(&old.bytes)).unwrap();
        assert_eq!(std::fs::read(&new_path).unwrap(), expected.bytes);
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn rejects_corrupt_previous_entry_before_raw_copy() {
        let raw = fixture(false);
        let original = build(&raw, CT, None).unwrap();
        let mut old = archive(Cursor::new(&original.bytes)).unwrap();
        let mut output = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for i in 0..old.len() {
            let file = old.by_index(i).unwrap();
            if file.name() == "pageIdInfo.dat" {
                drop(file);
                output.start_file("pageIdInfo.dat", opts).unwrap();
                output.write_all(b"corrupt old page index").unwrap();
            } else {
                output.raw_copy_file(file).unwrap();
            }
        }
        let corrupted = output.finish().unwrap().into_inner();
        assert!(build(&raw, CT, Some(&corrupted)).is_err());
    }
    #[test]
    fn simple_xml_and_unsplit_fallback() {
        let xml = b"<root><!-- x --></root>";
        let c = build(xml, "application/xml", None).unwrap();
        assert_eq!(reconstruct(Cursor::new(c.bytes)).unwrap().raw_body, xml);
    }
    #[test]
    fn page_read_uses_index_hashes_and_skips_other_page_data() {
        use std::io::{Read, Seek, SeekFrom};
        use std::sync::{Arc, Mutex};
        struct Tracked<'a> {
            inner: Cursor<&'a [u8]>,
            reads: Arc<Mutex<Vec<(u64, u64)>>>,
        }
        impl Read for Tracked<'_> {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                let start = self.inner.position();
                let n = self.inner.read(b)?;
                self.reads.lock().unwrap().push((start, start + n as u64));
                Ok(n)
            }
        }
        impl Seek for Tracked<'_> {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                self.inner.seek(from)
            }
        }
        let c = build(&fixture(false), CT, None).unwrap();
        let mut z = ZipArchive::new(Cursor::new(&c.bytes)).unwrap();
        let f = z.by_name("p1.page").unwrap();
        let start = f.data_start().unwrap();
        let untouched = (start, start + f.compressed_size());
        drop(f);
        let reads = Arc::new(Mutex::new(Vec::new()));
        let r = Tracked {
            inner: Cursor::new(&c.bytes),
            reads: reads.clone(),
        };
        let index = &c.pages[1];
        read_page_verified(r, "p2", &index.page_sha256, &index.xml_sha256).unwrap();
        assert!(reads
            .lock()
            .unwrap()
            .iter()
            .all(|&(start, end)| end <= untouched.0 || start >= untouched.1));
        let mut modified = read_page(Cursor::new(&c.bytes), "p2").unwrap().page;
        modified.push(42);
        let mut m = manifest(&mut z).unwrap();
        m.entry_sha256.insert("p2.page".into(), hash(&modified));
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for i in 0..z.len() {
            let f = z.by_index(i).unwrap();
            let name = f.name().to_string();
            if name == "p2.page" || name == "cloud/manifest.json" {
                drop(f);
                writer.start_file(&name, options).unwrap();
                if name == "p2.page" {
                    writer.write_all(&modified).unwrap();
                } else {
                    writer.write_all(&serde_json::to_vec(&m).unwrap()).unwrap();
                }
            } else {
                writer.raw_copy_file(f).unwrap();
            }
        }
        let changed = writer.finish().unwrap().into_inner();
        assert!(read_page(Cursor::new(&changed), "p2").is_ok());
        assert!(read_page_verified(
            Cursor::new(changed),
            "p2",
            &index.page_sha256,
            &index.xml_sha256
        )
        .is_err());
    }
    #[test]
    fn unsplit_page_and_nonstandard_headers_roundtrip() {
        let mut raw = fixture(false);
        let at = raw
            .windows(b"<noteTree>".len())
            .position(|x| x == b"<noteTree>")
            .unwrap();
        raw.splice(at..at, b"<!-- synthetic -->".iter().copied());
        let c = build(&raw, CT, None).unwrap();
        assert_eq!(reconstruct(Cursor::new(&c.bytes)).unwrap().raw_body, raw);
        assert!(read_page(Cursor::new(&c.bytes), "p1").is_err());
        let mut z = archive(Cursor::new(&c.bytes)).unwrap();
        assert!(manifest(&mut z).unwrap().boundary.is_none());
        assert!(z.by_name("cloud/framing").is_ok());
    }
    #[test]
    fn suffix_exception_is_preserved_without_explicit_part_list() {
        let mut raw = fixture(false);
        let needle = b"filename=\"s1_7\"";
        let at = raw.windows(needle.len()).position(|x| x == needle).unwrap();
        raw[at + needle.len() - 2] = b'9';
        let c = build(&raw, CT, None).unwrap();
        let mut z = archive(Cursor::new(&c.bytes)).unwrap();
        let m = manifest(&mut z).unwrap();
        assert_eq!(m.suffix_exceptions.get("s1").map(String::as_str), Some("9"));
        assert!(m.parts.is_empty() && m.part_order.is_none());
        assert_eq!(reconstruct(Cursor::new(c.bytes)).unwrap().raw_body, raw);
    }
    #[test]
    fn page_context_requires_independent_digest() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let raw = STANDARD
            .decode(fixture["raw_body"].as_str().unwrap())
            .unwrap();
        let ct = fixture["content_type"].as_str().unwrap();
        let container = build(&raw, ct, None).unwrap();
        let context_sha = container.context_sha256.as_deref().unwrap();
        let context = read_context(
            Cursor::new(&container.bytes),
            context_sha,
            &container.raw_sha256,
        )
        .unwrap();
        assert_eq!(context.pages.len(), 1);
        let global = read_global_context(
            Cursor::new(&container.bytes),
            context_sha,
            &container.raw_sha256,
        )
        .unwrap();
        assert_eq!(hash(&global.note_xml), global.index.note_xml_sha256);
        assert_eq!(
            hash(&global.download_urls),
            global.index.download_urls_sha256
        );
        // A modified manifest cannot vouch for a changed global entry.
        let mut z = archive(Cursor::new(&container.bytes)).unwrap();
        let mut m = manifest(&mut z).unwrap();
        let mut changed = global.note_xml.clone();
        changed.extend_from_slice(b" ");
        m.entry_sha256
            .insert("cloud/note.xml".into(), hash(&changed));
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for i in 0..z.len() {
            let file = z.by_index(i).unwrap();
            let name = file.name().to_string();
            if name == "cloud/note.xml" || name == "cloud/manifest.json" {
                drop(file);
                w.start_file(&name, opts).unwrap();
                if name == "cloud/note.xml" {
                    w.write_all(&changed).unwrap();
                } else {
                    w.write_all(&serde_json::to_vec(&m).unwrap()).unwrap();
                }
            } else {
                w.raw_copy_file(file).unwrap();
            }
        }
        let tampered = w.finish().unwrap().into_inner();
        assert!(read_context(Cursor::new(&tampered), context_sha, &container.raw_sha256).is_ok());
        assert!(
            read_global_context(Cursor::new(&tampered), context_sha, &container.raw_sha256)
                .is_err()
        );
        assert_eq!(context.pages[0].id, container.pages[0].page_id);
        assert!(read_context(
            Cursor::new(&container.bytes),
            &"0".repeat(64),
            &container.raw_sha256
        )
        .is_err());
        assert!(read_context(Cursor::new(&container.bytes), context_sha, &"0".repeat(64)).is_err());
    }
    #[test]
    fn admission_proof_tracks_shared_verifier_and_preserves_v1() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../notes-host/tests/fixtures/ink_objects_base.json"
        ))
        .unwrap();
        let raw = STANDARD
            .decode(value["raw_body"].as_str().unwrap())
            .unwrap();
        let resource = STANDARD
            .decode(value["resource_xml"].as_str().unwrap())
            .unwrap();
        let ct = value["content_type"].as_str().unwrap();
        let local = value["local_id"].as_str().unwrap();
        let commit = value["commit_id"].as_str().unwrap();
        let context = |c: &BuiltContainer| {
            read_context(
                Cursor::new(&c.bytes),
                c.context_sha256.as_deref().unwrap(),
                &c.raw_sha256,
            )
            .unwrap()
        };
        let old = build(&raw, ct, None).unwrap();
        assert_eq!(context(&old).format, "notes-linux/page-context/1");
        assert!(context(&old).admission.is_none());
        let good =
            build_with_snapshot(&raw, ct, None, &resource, Some(local), Some(commit)).unwrap();
        let c = context(&good);
        assert_eq!(c.format, "notes-linux/page-context/3");
        assert!(c.verified_resource_matches(Some(local), Some(commit)));
        assert!(c.stroke_bindings_match(Some(local), Some(commit)));
        assert!(!c.verified_resource_matches(Some(local), Some("wrong-commit")));
        assert_eq!(reconstruct(Cursor::new(&good.bytes)).unwrap().raw_body, raw);
        for (r, l, k) in [
            (
                &resource[..],
                Some("00000000-0000-4000-8000-000000000001"),
                Some(commit),
            ),
            (&resource[..], Some(local), Some("wrong-commit")),
            (b"wrong-resource".as_slice(), Some(local), Some(commit)),
            (&resource[..], None, Some(commit)),
        ] {
            let built = build_with_snapshot(&raw, ct, None, r, l, k).unwrap();
            assert!(context(&built).admission.is_none());
        }
        fn mutate(raw: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
            let at = raw.windows(old.len()).position(|w| w == old).unwrap();
            let mut out = raw.to_vec();
            out.splice(at..at + old.len(), new.iter().copied());
            out
        }
        let bad_wrapper = mutate(
            &raw,
            &[255, 255, 255, 255, 4, 0, 0, 0, 0, 0, 0, 0],
            &[255, 255, 255, 255, 4, 0, 0, 0, 1, 0, 0, 0],
        );
        let degraded =
            build_with_snapshot(&bad_wrapper, ct, None, &resource, Some(local), Some(commit))
                .unwrap();
        let c2 = context(&degraded);
        assert_eq!(c2.format, "notes-linux/page-context/2");
        assert!(c2.verified_resource_matches(Some(local), Some(commit)));
        assert!(!c2.stroke_bindings_match(Some(local), Some(commit)));
        assert_eq!(
            reconstruct(Cursor::new(&degraded.bytes)).unwrap().raw_body,
            bad_wrapper
        );
        for changed in [
            mutate(
                &raw,
                b"<object type=\"stroke\"",
                b"<object isLocked=\"true\" type=\"stroke\"",
            ),
            mutate(
                &raw,
                b"</objectList>",
                b"<x:extra xmlns:x=\"urn:test\"/></objectList>",
            ),
        ] {
            let snap = rs_doc::doc::parse_document(&changed, ct, None).unwrap();
            let built = build_with_snapshot(
                &changed,
                ct,
                None,
                &snap.resource_xml,
                snap.local_id.as_deref(),
                snap.commit_id.as_deref(),
            )
            .unwrap();
            assert_eq!(context(&built).format, "notes-linux/page-context/1");
            assert!(!context(&built)
                .verified_resource_matches(snap.local_id.as_deref(), snap.commit_id.as_deref()));
            assert_eq!(
                reconstruct(Cursor::new(&built.bytes)).unwrap().raw_body,
                changed
            );
        }
    }

    #[test]
    fn streaming_stroke_admission_matches_renderer_on_valid_and_bad_records() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let value: serde_json::Value = serde_json::from_str(include_str!("../../notes-host/tests/fixtures/ink_objects_base.json")).unwrap();
        let raw = STANDARD.decode(value["raw_body"].as_str().unwrap()).unwrap();
        let resource = STANDARD.decode(value["resource_xml"].as_str().unwrap()).unwrap();
        let ct = value["content_type"].as_str().unwrap();
        let limits = rs_doc::mime::Limits::default();
        let parts = rs_doc::mime::parts(&raw, ct, limits.max_readonly_parts, &limits, &mut rs_doc::mime::NoHooks).unwrap();
        let bodies: Vec<Vec<u8>> = parts.iter().filter(|p| p.name.as_deref() == Some("strokeResource"))
            .map(|p| raw[p.body.clone()].to_vec()).collect();
        assert!(!bodies.is_empty());
        assert!(rs_ink::core::records::decode_records(&resource, &bodies).is_ok());
        let mut duplicate = bodies.clone(); duplicate.push(bodies[0].clone());
        let mut malformed = bodies.clone(); malformed[0][0] ^= 1;
        let mut truncated = bodies.clone(); truncated[0].truncate(15);
        let mut wrong_id = bodies.clone();
        let record = rs_ink::core::binary::decode_stroke_resource(&wrong_id[0]).unwrap();
        let id = rs_ink::core::binary::py_slice(&record.frames[0].raw, record.frames[0].fixed_start + 6, Some(record.frames[0].fixed_start + 42));
        if let Some(pos) = wrong_id[0].windows(id.len()).position(|window| window == id) { wrong_id[0][pos] = b'Z'; }
        for (xml, input) in [(&resource[..], &bodies), (&resource[..], &duplicate), (&resource[..], &malformed), (&resource[..], &truncated), (&resource[..], &wrong_id), (&b"<invalid"[..], &bodies)] {
            let decoded = rs_ink::core::records::decode_records(xml, input).map(|_| ());
            let borrowed: Vec<&[u8]> = input.iter().map(Vec::as_slice).collect();
            assert_eq!(decoded, rs_ink::core::records::validate_records(xml, &borrowed));
        }
        assert!(rs_ink::core::records::validate_records(&resource, &duplicate).is_err());
        assert!(rs_ink::core::records::validate_records(&resource, &malformed).is_err());
        assert!(rs_ink::core::records::validate_records(&resource, &truncated).is_err());
        assert!(rs_ink::core::records::validate_records(&resource, &wrong_id).is_err());
    }
}
