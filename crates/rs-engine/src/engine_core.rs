//! Pure-Rust pieces of `samsung_notes/engine.py`: deterministic operation IDs,
//! template discovery from note XML, PDF staging, file export naming, and the
//! small storage helpers the engine needs (`normalize_commit`, `now_ms`,
//! `private_directory`). No PyO3 types.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

// ---------------------------------------------------------------- SHA-1 / UUIDv5

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// `uuid.UUID(text)` -> 16 bytes; `Err` = "badly formed hexadecimal UUID string".
pub fn parse_uuid(text: &str) -> Result<[u8; 16], String> {
    let hex = text.replace("urn:", "").replace("uuid:", "");
    let hex: String = hex.trim_matches(|c| c == '{' || c == '}').replace('-', "");
    if hex.chars().count() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("badly formed hexadecimal UUID string".into());
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    Ok(out)
}

pub fn format_uuid(b: &[u8; 16]) -> String {
    let h: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// `str(uuid.uuid5(uuid.UUID(namespace), name))`
pub fn uuid5(namespace: &str, name: &str) -> Result<String, String> {
    let ns = parse_uuid(namespace)?;
    let mut data = ns.to_vec();
    data.extend_from_slice(name.as_bytes());
    let digest = sha1(&data);
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(format_uuid(&b))
}

// ---------------------------------------------------------------- storage helpers

/// `storage.normalize_commit` for a `str` (callers map None/falsy to "").
pub fn normalize_commit(value: &str) -> String {
    match value.find("_lc") {
        Some(at) if at > 0 => value[..at].to_string(),
        _ => value.to_string(),
    }
}

/// `storage.now_ms()`
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `storage.private_directory(path)`. `Err(None)` = the ValueError, `Err(Some)` = OSError.
pub fn private_directory(path: &Path) -> Result<PathBuf, Option<std::io::Error>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            fs::create_dir_all(parent).map_err(Some)?;
        }
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(e) => return Err(Some(e)),
    }
    let link = fs::symlink_metadata(path).map_err(Some)?;
    let meta = fs::metadata(path).map_err(Some)?;
    // SAFETY: getuid takes no arguments, has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if link.file_type().is_symlink() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(None);
    }
    Ok(path.to_path_buf())
}

pub const PRIVATE_DIRECTORY_ERROR: &str = "application directory must be private and owned by the user";

// ---------------------------------------------------------------- base64 (binascii, non-strict)

/// `base64.b64decode(s)` (validate=False): non-alphabet characters are ignored.
pub fn b64decode_lenient(s: &str) -> Result<Vec<u8>, String> {
    if !s.is_ascii() {
        return Err("string argument should contain only ASCII characters".into());
    }
    let value = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::new();
    let (mut quad, mut left, mut pads) = (0u32, 0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Ok(out);
                }
            }
            continue;
        }
        let Some(v) = value(c) else { continue };
        pads = 0;
        match quad {
            0 => {
                left = v;
                quad = 1
            }
            1 => {
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
                quad = 2
            }
            2 => {
                out.push(((left << 4) | (v >> 2)) as u8);
                left = v & 0x03;
                quad = 3
            }
            _ => {
                out.push(((left << 6) | v) as u8);
                quad = 0
            }
        }
    }
    if quad != 0 {
        return Err(if quad == 1 {
            "Invalid base64-encoded string: number of data characters cannot be 1 more than a multiple of 4".into()
        } else {
            "Incorrect padding".into()
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------- Python number helpers

/// Python `int(text)` for a `str` (whitespace, sign, single underscores).
pub fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let (neg, digits) = match t.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|&c| c != '_').collect();
    if !clean.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v: i64 = clean.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Python `str.isdigit()` (approximation: ASCII digits, other numeric chars).
pub fn py_isdigit(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit() || (!c.is_ascii() && c.is_numeric()))
}

// ---------------------------------------------------------------- templates

/// One PDF template group found in a note (`Engine.templates`).
#[derive(Debug, Clone)]
pub struct TemplateCandidate {
    pub sha: Option<String>,
    pub template_type: String,
    pub indices: Vec<i64>,
    pub file_name: Option<String>,
}

pub enum TemplateScan {
    /// ET.ParseError and friends: skip this note.
    Skip,
    /// `int(pageIndex)` failed: ValueError escapes `templates()`.
    Error(String),
    /// Groups in first-seen order, plus the ids of every contentFile whose
    /// name decoded (`frozenset(names)` in Python).
    Found(Vec<TemplateCandidate>, Vec<Option<String>>),
}

fn children<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> + 'a {
    node.children().filter(move |c| c.is_element() && c.tag_name().namespace().is_none() && c.tag_name().name() == name)
}

/// Scan `resource_xml` like the Python `templates()` loop body, in document order.
pub fn scan_templates(resource_xml: &[u8]) -> TemplateScan {
    let Ok(text) = std::str::from_utf8(resource_xml) else { return TemplateScan::Skip };
    let Ok(doc) = roxmltree::Document::parse(text) else { return TemplateScan::Skip };
    let root = doc.root_element();
    let mut names: Vec<(Option<String>, String)> = Vec::new();
    for list in children(root, "contentFileList") {
        for f in children(list, "contentFile") {
            let decoded = b64decode_lenient(f.attribute("name").unwrap_or(""))
                .ok()
                .and_then(|raw| String::from_utf8(raw).ok());
            if let Some(decoded) = decoded {
                let name = decoded.split_once('@').map(|x| x.1.to_string()).unwrap_or(decoded);
                let id = f.attribute("id").map(str::to_string);
                match names.iter_mut().find(|(k, _)| *k == id) {
                    Some(slot) => slot.1 = name,
                    None => names.push((id, name)),
                }
            }
        }
    }
    let mut groups: Vec<TemplateCandidate> = Vec::new();
    for list in children(root, "pageList") {
        for page in children(list, "page") {
            let data: Vec<_> = children(page, "pdfDataList").flat_map(|l| children(l, "pdfData")).collect();
            let Some(template_type) = page.attribute("templateType") else { continue };
            if data.len() != 1 {
                continue;
            }
            let sha = data[0].attribute("hash").map(str::to_string);
            let index_text = data[0].attribute("pageIndex").unwrap_or("0");
            let Some(index) = py_int(index_text) else {
                return TemplateScan::Error(format!("invalid literal for int() with base 10: '{}'", index_text));
            };
            match groups.iter_mut().find(|g| g.sha == sha) {
                Some(g) => g.indices.push(index),
                None => groups.push(TemplateCandidate { sha, template_type: template_type.to_string(), indices: vec![index], file_name: None }),
            }
        }
    }
    for g in &mut groups {
        g.file_name = names.iter().find(|(k, _)| *k == g.sha).map(|(_, n)| n.clone());
    }
    let ids = names.into_iter().map(|(id, _)| id).collect();
    TemplateScan::Found(groups, ids)
}

/// `names[sha].rsplit('.', 1)[0].replace('_', ' ')`
pub fn template_display_name(file_name: &str) -> String {
    file_name.rsplit_once('.').map(|x| x.0).unwrap_or(file_name).replace('_', " ")
}

// ---------------------------------------------------------------- PDF staging

/// `os.path.basename(str(file_name or '')).strip() or 'Belge.pdf'` + `.pdf` suffix.
pub fn pdf_file_name(raw: &str) -> String {
    let base = raw.rsplit('/').next().unwrap_or("");
    let base = py_strip(base);
    let mut name = if base.is_empty() { "Belge.pdf".to_string() } else { base.to_string() };
    if !name.to_lowercase().ends_with(".pdf") {
        name.push_str(".pdf");
    }
    name
}

/// Python `str.strip()` whitespace set.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\x1c'..='\x1f'))
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{:02x}", b)).collect()
}

fn random_suffix() -> String {
    let mut b = [0u8; 8];
    // Temporary names are also opened with O_EXCL (create_new), so a failed
    // random source only risks a collision error, never a shared file.
    getrandom::fill(&mut b).expect("operating system random source unavailable");
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// Write `data` to `target` atomically with mode 0600 unless it already exists.
pub fn stage_file(target: &Path, data: &[u8]) -> std::io::Result<()> {
    if target.exists() {
        return Ok(());
    }
    let dir = target.parent().unwrap_or(Path::new("."));
    let temporary = dir.join(format!("tmp{}", random_suffix()));
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
    let result = f.write_all(data).and_then(|_| f.flush());
    drop(f);
    if let Err(e) = result.and_then(|_| fs::rename(&temporary, target)) {
        let _ = fs::remove_file(&temporary);
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------- export

/// Run a command with a timeout; `None` = OSError or timeout.
pub fn run_capture(argv: &[&str], timeout: Duration) -> Option<(i32, Vec<u8>)> {
    let mut child = Command::new(argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    Some((out.status.code().unwrap_or(-1), out.stdout))
}

/// `shutil.which(name)`
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| {
        fs::metadata(p).map(|m| m.is_file() && m.mode() & 0o111 != 0).unwrap_or(false)
    })
}

/// The Downloads folder (Linux: `xdg-user-dir DOWNLOAD`; macOS:
/// `~/Downloads`) or `home` (`notes_platform::dirs::downloads_dir`).
pub fn download_folder(home: &Path) -> PathBuf {
    notes_platform::dirs::downloads_dir(home)
}

/// First free `base.ext`, `base (2).ext`, ... in `folder`.
pub fn free_target(folder: &Path, base: &str, ext: &str) -> PathBuf {
    let mut target = folder.join(format!("{}.{}", base, ext));
    let mut counter = 2;
    while target.exists() {
        target = folder.join(format!("{} ({}).{}", base, counter, ext));
        counter += 1;
    }
    target
}

/// Create `base.ext`, `base (2).ext`, ... in `folder` exclusively and return
/// the path with its open file. `O_CREAT|O_EXCL|O_NOFOLLOW`: an existing entry
/// — including a dangling symlink planted in the (user-writable) Downloads
/// folder, e.g. by an extracted archive — is never followed or overwritten,
/// and there is no window between choosing the name and creating the file.
pub fn create_free_target(folder: &Path, base: &str, ext: &str) -> std::io::Result<(PathBuf, fs::File)> {
    for counter in 1..=10_000u32 {
        let name = if counter == 1 { format!("{}.{}", base, ext) } else { format!("{} ({}).{}", base, counter, ext) };
        let target = folder.join(name);
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o666).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&target) {
            Ok(file) => return Ok((target, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free export file name"))
}

/// Write `data` to a new file chosen by [`create_free_target`]; a partially
/// written file is removed on error.
pub fn write_free_target(folder: &Path, base: &str, ext: &str, data: &mut dyn std::io::Read) -> std::io::Result<PathBuf> {
    let (target, mut file) = create_free_target(folder, base, ext)?;
    let result = std::io::copy(data, &mut file).and_then(|_| file.flush()).and_then(|_| file.sync_all());
    drop(file);
    if let Err(e) = result {
        let _ = fs::remove_file(&target);
        return Err(e);
    }
    Ok(target)
}

/// File name of the magick output for the export `target_name`.
/// ImageMagick writes the output basename (minus its last extension) into the
/// PDF `/Title`, so even a private work file keeps the Python reference's
/// `.<name>.part` partial name; otherwise the exported bytes differ.
pub fn pdf_work_name(target_name: &str) -> String {
    format!(".{}.part", target_name)
}

/// `magick ... pdf:<output>` into a private work directory (never into the
/// shared Downloads folder): whether a PDF was produced at `output`.
pub fn images_to_pdf_at(files: &[PathBuf], output: &Path) -> bool {
    let mut args: Vec<String> = ["-units", "PixelsPerInch", "-density", "130"].iter().map(|s| s.to_string()).collect();
    args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
    for a in ["-background", "white", "-alpha", "remove", "-compress", "jpeg", "-quality", "92"] {
        args.push(a.into());
    }
    args.push(format!("pdf:{}", output.to_string_lossy()));
    let magick = notes_platform::tools::program(notes_platform::tools::Tool::Magick).to_string_lossy().into_owned();
    let mut argv: Vec<&str> = vec![magick.as_str()];
    argv.extend(args.iter().map(String::as_str));
    matches!(run_capture(&argv, Duration::from_secs(600)), Some((0, _))) && output.is_file()
}

/// A private temporary directory removed on drop (`tempfile.TemporaryDirectory`).
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> std::io::Result<TempDir> {
        let path = std::env::temp_dir().join(format!("{}{}", prefix, random_suffix()));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(TempDir(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `magick ... pdf:<partial>`: returns whether a PDF was produced at `target`.
pub fn images_to_pdf(files: &[PathBuf], target: &Path) -> bool {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let partial = target.with_file_name(pdf_work_name(&name));
    let mut args: Vec<String> = ["-units", "PixelsPerInch", "-density", "130"].iter().map(|s| s.to_string()).collect();
    args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
    for a in ["-background", "white", "-alpha", "remove", "-compress", "jpeg", "-quality", "92"] {
        args.push(a.into());
    }
    args.push(format!("pdf:{}", partial.to_string_lossy()));
    // Linux: "magick" via PATH (unchanged); macOS: the Homebrew/MacPorts copy.
    let magick = notes_platform::tools::program(notes_platform::tools::Tool::Magick).to_string_lossy().into_owned();
    let mut argv: Vec<&str> = vec![magick.as_str()];
    argv.extend(args.iter().map(String::as_str));
    let ok = matches!(run_capture(&argv, Duration::from_secs(600)), Some((0, _))) && partial.is_file();
    if !ok {
        let _ = fs::remove_file(&partial);
        return false;
    }
    fs::rename(&partial, target).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uuid5_matches_python() {
        // python3 -c "import uuid;print(uuid.uuid5(uuid.UUID('6ba7b810-9dad-11d1-80b4-00c04fd430c8'),'0'))"
        assert_eq!(uuid5("6ba7b810-9dad-11d1-80b4-00c04fd430c8", "erase-0").unwrap(), "5902abb6-8d22-574a-a0f9-ae0959e15876");
        assert_eq!(normalize_commit("abc_lc123"), "abc");
        assert_eq!(normalize_commit("_lcx"), "_lcx");
        assert_eq!(b64decode_lenient("YWJj").unwrap(), b"abc");
        assert_eq!(b64decode_lenient("YW\nJjZA==").unwrap(), b"abcd");
        assert!(b64decode_lenient("YWJjZ").is_err());
        assert_eq!(pdf_file_name("../x/My File "), "My File.pdf");
        assert_eq!(pdf_file_name(""), "Belge.pdf");
        assert_eq!(py_int(" +1_0 "), Some(10));
    }

    /// The PDF /Title magick derives from the work file name equals the
    /// reference's (`.<target>.part` -> ".<target>"), not the work name.
    #[test]
    fn private_pdf_work_file_keeps_reference_title() {
        if which("magick").is_none() {
            eprintln!("magick missing; skipping");
            return;
        }
        // 1x1 opaque white RGB PNG.
        const PNG: [u8; 69] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xde, 0, 0, 0, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xff, 0xff, 0x3f, 0, 0x05,
            0xfe, 0x02, 0xfe, 0x0d, 0xef, 0x46, 0xb8, 0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82];
        let dir = TempDir::new("notes-export-test-").unwrap();
        let page = dir.0.join("00000.png");
        fs::write(&page, PNG).unwrap();
        assert_eq!(pdf_work_name("Synthetic ink.pdf"), ".Synthetic ink.pdf.part");
        let output = dir.0.join(pdf_work_name("Synthetic ink.pdf"));
        assert!(images_to_pdf_at(&[page], &output));
        let pdf = fs::read(&output).unwrap();
        let title: String = ".Synthetic ink.pdf".encode_utf16().map(|u| format!("{:04X}", u)).collect();
        let wanted = format!("/Title <FEFF{}>", title);
        assert!(pdf.windows(wanted.len()).any(|w| w == wanted.as_bytes()), "PDF title differs from the reference");
    }

    /// Exports never follow a (dangling) symlink or overwrite an existing
    /// file in the Downloads folder.
    #[test]
    fn export_target_is_exclusive_and_nofollow() {
        let dir = TempDir::new("notes-export-test-").unwrap();
        let outside = dir.0.join("outside");
        fs::create_dir(&outside).unwrap();
        let victim = outside.join("victim.txt");
        std::os::unix::fs::symlink(&victim, dir.0.join("Not.txt")).unwrap();
        fs::write(dir.0.join("Not (2).txt"), b"keep").unwrap();
        let target = write_free_target(&dir.0, "Not", "txt", &mut &b"note"[..]).unwrap();
        assert_eq!(target, dir.0.join("Not (3).txt"));
        assert_eq!(fs::read(&target).unwrap(), b"note");
        assert!(!victim.exists(), "dangling symlink was followed");
        assert_eq!(fs::read(dir.0.join("Not (2).txt")).unwrap(), b"keep");
    }
}
