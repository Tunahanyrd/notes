//! Shared synthetic fixtures for the notes-preview oracle ports.
#![allow(dead_code)]
use notes_preview::{CanvasPreviewCandidate, Fail, PreviewEnv, Val};
use rs_store::storage::StoreCore;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::var_os("NOTES_PREVIEW_TEST_TMP").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!("notes-preview-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        TempDir(path)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub type Http = Box<dyn Fn(&str, &[(String, String)]) -> Result<(u16, Vec<u8>, Option<String>), Fail> + Send + Sync>;
pub type Download = Box<dyn Fn(&str, &[(String, String)], &Path, u64, &dyn Fn() -> Result<(), Fail>) -> Result<(u16, Option<String>), Fail> + Send + Sync>;

/// `SimpleNamespace(sessions=..., _api=...)`
pub struct TestEnv {
    pub session: Mutex<Option<PathBuf>>,
    pub offline: AtomicBool,
    pub headers: Mutex<Result<Vec<(String, String)>, Fail>>,
    pub http: Mutex<Option<Http>>,
    pub download: Mutex<Option<Download>>,
    /// `_fetch_pdf_refreshing`'s note refresh (fresh `to_dict()`).
    pub refresh: Mutex<Option<Box<dyn Fn(&str) -> Result<Val, Fail> + Send + Sync>>>,
}

impl TestEnv {
    pub fn new(session: Option<&Path>, offline: bool) -> Arc<TestEnv> {
        Arc::new(TestEnv {
            session: Mutex::new(session.map(Path::to_path_buf)),
            offline: AtomicBool::new(offline),
            headers: Mutex::new(Ok(vec![("Authorization".into(), "Bearer synthetic".into())])),
            http: Mutex::new(None),
            download: Mutex::new(None),
            refresh: Mutex::new(None),
        })
    }
    pub fn set_session(&self, session: Option<&Path>) {
        *self.session.lock().unwrap() = session.map(Path::to_path_buf);
    }
}

impl PreviewEnv for TestEnv {
    fn session_path(&self) -> Option<PathBuf> {
        self.session.lock().unwrap().clone()
    }
    fn offline(&self) -> bool {
        self.offline.load(Ordering::SeqCst)
    }
    fn headers(&self) -> Result<Vec<(String, String)>, Fail> {
        self.headers.lock().unwrap().clone()
    }
    fn http(&self, url: &str, headers: &[(String, String)]) -> Result<(u16, Vec<u8>, Option<String>), Fail> {
        match &*self.http.lock().unwrap() {
            Some(f) => f(url, headers),
            None => panic!("unexpected network"),
        }
    }
    fn refresh_note(&self, identity: &str) -> Result<Val, Fail> {
        match &*self.refresh.lock().unwrap() {
            Some(f) => f(identity),
            None => panic!("unexpected note refresh"),
        }
    }
    fn download(&self, url: &str, headers: &[(String, String)], dest: &Path, max: u64,
                check: &dyn Fn() -> Result<(), Fail>) -> Result<(u16, Option<String>), Fail> {
        match &*self.download.lock().unwrap() {
            Some(f) => f(url, headers, dest, max, check),
            None => panic!("unexpected network"),
        }
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// `tests.test_previews.candidate(raw)`
pub fn candidate(raw: &[u8]) -> CanvasPreviewCandidate {
    CanvasPreviewCandidate {
        page_index: 0, key: 0, width: 1, height: 1, is_dark_mode: false, bind_number: "7".into(),
        sha256: sha256_hex(raw), download_url: Some("https://ew1a-api.samsungcloud.com/file?signature=synthetic".into()),
    }
}

pub fn png(width: i64, height: i64, pixels: &[u8]) -> Vec<u8> {
    rs_render::pixels::rgba_png(width, height, pixels).unwrap()
}

pub fn store(dir: &Path) -> Arc<StoreCore> {
    Arc::new(StoreCore::open(dir).unwrap())
}

/// A loaded note whose base_document is `snapshot` (a stored snapshot dict).
pub fn note_with_document(store: &StoreCore, snapshot: &Val) -> String {
    let note = store.create(&Val::s("Synthetic"), &Val::s("")).unwrap();
    let id = note.get("id").unwrap().as_str().unwrap().to_string();
    let text = rs_store::val::dumps(snapshot, true, true).unwrap();
    store.with_db(|db| {
        use rs_store::db::Sql;
        db.exec("UPDATE notes SET base_document=?, loaded=1, deleted_remote=0 WHERE id=?", &rs_store::p![text, id.as_str()])?;
        Ok(())
    }).unwrap();
    id
}

pub fn b64(data: &[u8]) -> String {
    rs_editor::pyval::b64encode(data)
}

/// `{'schemaVersion':1,'raw_body':...,'raw_sha256':...,'content_type':...}`
pub fn snapshot(raw: &[u8], content_type: &str) -> Val {
    Val::dict(vec![("schemaVersion", Val::Int(1)), ("raw_body", Val::s(b64(raw))), ("raw_sha256", Val::s(sha256_hex(raw))),
                   ("content_type", Val::s(content_type))])
}

/// `tests.test_pdf_background.pdf_fixture()`
pub fn pdf_fixture() -> Vec<u8> {
    let red = b"1 0 0 rg 0 0 20 10 re f\n".to_vec();
    let green = b"0 1 0 rg 0 0 20 10 re f\n".to_vec();
    let stream = |c: &[u8]| { let mut v = format!("<< /Length {} >>\nstream\n", c.len()).into_bytes(); v.extend_from_slice(c); v.extend_from_slice(b"endstream"); v };
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 10] /Resources << >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 10] /Resources << >> /Contents 6 0 R >>".to_vec(),
        stream(&red), stream(&green)];
    let mut data = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(data.len());
        data.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        data.extend_from_slice(obj);
        data.extend_from_slice(b"\nendobj\n");
    }
    let xref = data.len();
    data.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for o in offsets {
        data.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    data
}

pub const URL: &str = "https://ew1a-api.samsungcloud.com/file?signature=synthetic";

/// `tests.test_pdf_background.xml(...)`
pub fn pdf_xml(pair: Option<&str>, objects: &str, locked: &str) -> String {
    let digest = sha256_hex(&pdf_fixture());
    let default = format!("<pdfData pageIndex=\"0\" hash=\"{digest}\"><pdfRect l=\"2\" t=\"3\" r=\"22\" b=\"13\"/></pdfData>");
    let pair = pair.map(str::to_string).unwrap_or(default);
    format!("<CompareNoteView><noteResource isLocked=\"{locked}\"><pageList><page width=\"30\" height=\"20\">\
<pdfDataList>{pair}</pdfDataList><layerList><layer>{objects}</layer></layerList>\
</page></pageList><contentFileList><contentFile id=\"{digest}\" bindNumber=\"7\"/></contentFileList>\
</noteResource><downloadUrls><downloadUrl><id>{digest}</id><url>{URL}</url></downloadUrl></downloadUrls></CompareNoteView>")
}

/// RGBA of pixel (x, y) of a contract PNG.
pub fn pixel(png: &[u8], x: usize, y: usize) -> [u8; 4] {
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap()) as usize;
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap()) as i64;
    let raw = notes_preview::pdf::rgba(png, w as i64, h).unwrap();
    let at = (y * w + x) * 4;
    raw[at..at + 4].try_into().unwrap()
}
