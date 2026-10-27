//! Replacement for the Python module `pdf_background`.
use crate::pdf_layout::{self, PdfPlacement};
use crate::pixels as px;
#[cfg(feature = "python")]
use crate::preview_worker::encode;
#[cfg(feature = "python")]
use crate::previews::{url_host, BROKER_HOSTS, FILE_HOSTS};
use crate::pyhelp;
use crate::sandbox;
use crate::xarea;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyOSError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyByteArray, PyBytes, PyCFunction, PyDict, PyFrozenSet, PyTuple};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MAX_PDF_BYTES: u64 = 128 * 1024 * 1024;
pub const RENDER_TIMEOUT: u64 = 40;
const DEFAULT_MESSAGE: &str = "PDF sayfası güvenle görüntülenemedi.";

#[cfg(feature = "python")]
#[pyclass(extends=pyo3::exceptions::PyValueError, subclass, module="samsung_notes.pdf_background", name="PdfBackgroundError")]
pub struct PdfBackgroundError {
    #[pyo3(get, set)]
    message: Py<PyAny>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PdfBackgroundError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(py: Python<'_>, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        PdfBackgroundError { message: DEFAULT_MESSAGE.into_pyobject(py).unwrap().into_any().unbind() }
    }

    #[cfg(feature = "python")]
    #[pyo3(signature=(message=None))]
    fn __init__(slf: &Bound<'_, Self>, message: Option<Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let message = match message {
            Some(m) => m,
            None => DEFAULT_MESSAGE.into_pyobject(py)?.into_any(),
        };
        slf.setattr("args", (message.clone(),))?;
        slf.borrow_mut().message = message.unbind();
        Ok(())
    }
}

const EXPIRED_MESSAGE: &str = "PDF indirme bağlantısının süresi dolmuş.";

#[cfg(feature = "python")]
/// The stored signed download URL was refused (observed: HTTP 403); a fresh note read has new ones.
#[pyclass(extends=PdfBackgroundError, subclass, module="samsung_notes.pdf_background", name="PdfLinkExpired")]
pub struct PdfLinkExpired {}

#[cfg(feature = "python")]
#[pymethods]
impl PdfLinkExpired {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(py: Python<'_>, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyClassInitializer<Self> {
        PyClassInitializer::from(PdfBackgroundError { message: EXPIRED_MESSAGE.into_pyobject(py).unwrap().into_any().unbind() })
            .add_subclass(PdfLinkExpired {})
    }

    #[cfg(feature = "python")]
    fn __init__(slf: &Bound<'_, Self>) -> PyResult<()> {
        slf.setattr("args", (EXPIRED_MESSAGE,))?;
        slf.setattr("message", EXPIRED_MESSAGE)?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub fn error(py: Python<'_>, message: &str) -> PyErr {
    match py.get_type::<PdfBackgroundError>().call1((message,)) {
        Ok(value) => PyErr::from_value(value),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
pub fn default_error(py: Python<'_>) -> PyErr {
    error(py, DEFAULT_MESSAGE)
}

#[cfg(feature = "python")]
/// PdfBackground (a real frozen dataclass, see crate::dataclasses) from a placement.
pub fn placement_object<'py>(py: Python<'py>, p: &PdfPlacement) -> PyResult<Bound<'py, PyAny>> {
    crate::dataclasses::pdf_background(py)?.call1((p.page_index, p.pdf_page_index, p.width, p.height,
        PyTuple::new(py, [p.rect.0, p.rect.1, p.rect.2, p.rect.3])?, p.annotation_objects, &p.sha256, p.download_url.as_deref()))
}

#[cfg(feature = "python")]
/// pdf_background._rgba or PdfBackgroundError.
pub fn rgba_or_raise(py: Python<'_>, png: &[u8], width: i64, height: i64) -> PyResult<Vec<u8>> {
    py.detach(|| px::rgba(png, width, height)).map_err(|e| error(py, e.message()))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _rgba<'py>(py: Python<'py>, png: &Bound<'py, PyAny>, width: &Bound<'py, PyAny>, height: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyByteArray>> {
    let (Ok(raw), Some(w), Some(h)) = (png.cast::<PyBytes>(), pyhelp::exact_int(width), pyhelp::exact_int(height)) else {
        return Err(error(py, px::RgbaError::Invalid.message()));
    };
    let pixels = rgba_or_raise(py, raw.as_bytes(), w, h)?;
    Ok(PyByteArray::new(py, &pixels))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn compose_pdf_canvas<'py>(py: Python<'py>, background: &Bound<'py, PyAny>, overlay: &Bound<'py, PyAny>, width: i64, height: i64) -> PyResult<Bound<'py, PyBytes>> {
    let raw = |v: &Bound<'py, PyAny>| -> PyResult<Vec<u8>> {
        match v.cast::<PyBytes>() {
            Ok(b) => rgba_or_raise(py, b.as_bytes(), width, height),
            Err(_) => Err(error(py, px::RgbaError::Invalid.message())),
        }
    };
    let mut under = raw(background)?;
    let over = raw(overlay)?;
    py.detach(|| px::compose(&mut under, &over))
        .map_err(|_| error(py, "PDF üzerindeki çizim katmanı henüz güvenle birleştirilemiyor."))?;
    encode(py, width, height, &under)
}

#[cfg(feature = "python")]
fn payload_documents(parsed: &Bound<'_, PyAny>) -> PyResult<Vec<Vec<u8>>> {
    parsed.getattr("xml_documents")?.try_iter()?.map(|d| d.and_then(|d| d.extract::<Vec<u8>>())).collect()
}

#[cfg(feature = "python")]
fn texts(py: Python<'_>, parsed: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    pdf_layout::parse_all(&payload_documents(parsed)?).map_err(|_| default_error(py))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn pdf_page_indices<'py>(py: Python<'py>, parsed: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyFrozenSet>> {
    let texts = texts(py, parsed)?;
    let pages = pdf_layout::pdf_pages(&texts).map_err(|_| default_error(py))?;
    PyFrozenSet::new(py, pages)
}

#[cfg(feature = "python")]
/// Rust-side inventory used by previews (shares the caller's single parse).
pub fn placements(py: Python<'_>, parsed: &Bound<'_, PyAny>) -> PyResult<Vec<PdfPlacement>> {
    if parsed.getattr("locked")?.is_truthy()? {
        return Err(error(py, "Kilitli PDF notu görüntülenemiyor."));
    }
    let texts = texts(py, parsed)?;
    let mut files = HashSet::new();
    for file in parsed.getattr("content_files")?.try_iter()? {
        files.insert(file?.getattr("file_hash")?.extract::<String>()?);
    }
    let mut urls = HashMap::new();
    for url in parsed.getattr("download_urls")?.try_iter()? {
        let url = url?;
        let id: String = url.getattr("id")?.call_method0("lower")?.extract()?;
        urls.insert(id, url.getattr("url")?.extract::<String>()?);
    }
    py.detach(|| pdf_layout::placements(&texts, &files, &urls, px::MAX_PIXELS))
        .map_err(|e| e.map(|m| error(py, m)).unwrap_or_else(|| default_error(py)))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(raw, content_type=None))]
pub fn inspect_pdf_backgrounds<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>, content_type: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyTuple>> {
    let payload_error = xarea::class(py, "tools.notes_payload", "PayloadError")?;
    let result = (|| -> PyResult<Vec<PdfPlacement>> {
        let parsed = if raw.is_instance(&xarea::class(py, "tools.notes_payload", "PayloadResult")?)? {
            raw.clone()
        } else {
            let kwargs = PyDict::new(py);
            kwargs.set_item("max_parts", xarea::class(py, "tools.notes_payload", "MAX_READONLY_PARTS")?)?;
            xarea::parse_payload(py)?.call((raw, content_type.map(|c| c.clone()).unwrap_or_else(|| py.None().into_bound(py))), Some(&kwargs))?
        };
        placements(py, &parsed)
    })();
    match result {
        Ok(items) => PyTuple::new(py, items.iter().map(|p| placement_object(py, p)).collect::<PyResult<Vec<_>>>()?),
        Err(e) if e.is_instance(py, &payload_error) => Err(error(py, &e.value(py).str()?.to_string())),
        Err(e) => Err(e),
    }
}

pub fn provider_available() -> bool {
    sandbox::pdf_available()
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn pdf_provider_available() -> bool {
    provider_available()
}

/// _valid_cached_pdf: regular file, size bound, %PDF- magic, SHA256.
pub fn valid_cached_pdf(path: &Path, digest: &str) -> std::io::Result<bool> {
    let link = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    if link.file_type().is_symlink() || !link.is_file() || !(8..=MAX_PDF_BYTES).contains(&link.len()) {
        return Ok(false);
    }
    let mut file = std::fs::File::open(path)?;
    let mut head = [0u8; 8];
    let mut read = 0;
    while read < 8 {
        let n = file.read(&mut head[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    if !head[..read].starts_with(b"%PDF-") {
        return Ok(false);
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&head[..read]);
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(hex == digest)
}

#[cfg(feature = "python")]
fn fspath(value: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
    let os = value.py().import("os")?;
    Ok(PathBuf::from(os.getattr("fspath")?.call1((value,))?.extract::<String>()?))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _valid_cached_pdf(py: Python<'_>, path: &Bound<'_, PyAny>, digest: &Bound<'_, PyAny>) -> PyResult<bool> {
    let path = fspath(path)?;
    let Ok(digest) = digest.extract::<String>() else { return Ok(false) };
    py.detach(|| valid_cached_pdf(&path, &digest)).map_err(PyErr::from)
}

pub fn cache_name(account: &str, sha256: &str) -> String {
    px::sha256_hex(format!("{account}:{sha256}").as_bytes()) + ".pdf"
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(candidate, directory, *, account))]
pub fn cached_pdf<'py>(py: Python<'py>, candidate: &Bound<'py, PyAny>, directory: &Bound<'py, PyAny>, account: String) -> PyResult<Bound<'py, PyAny>> {
    let sha: String = candidate.getattr("sha256")?.extract()?;
    let path = pyhelp::path(py, directory)?.div(cache_name(&account, &sha))?;
    let local = fspath(&path)?;
    if py.detach(|| valid_cached_pdf(&local, &sha))? {
        Ok(path)
    } else {
        Ok(py.None().into_bound(py))
    }
}

#[cfg(feature = "python")]
fn header_filter<'py>(py: Python<'py>, api: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let headers = PyDict::new(py);
    for item in api.call_method0("_headers")?.call_method0("items")?.try_iter()? {
        let (key, value): (Bound<PyAny>, Bound<PyAny>) = item?.extract()?;
        let lower: String = key.call_method0("lower")?.extract()?;
        if lower != "content-type" && lower != "content-encoding" {
            headers.set_item(key, value)?;
        }
    }
    Ok(headers)
}

#[cfg(feature = "python")]
/// Default transport of fetch_pdf (stdlib urllib, as in Python): global-only
/// addresses, no redirects, bounded streaming into a private partial file.
fn download<'py>(py: Python<'py>, url: &Bound<'py, PyAny>, headers: &Bound<'py, PyAny>, temporary: &Bound<'py, PyAny>,
                 check: &Bound<'py, PyAny>) -> PyResult<(i64, Bound<'py, PyAny>)> {
    let urlparse = py.import("urllib.parse")?;
    let host = urlparse.getattr("urlsplit")?.call1((url,))?.getattr("hostname")?;
    let socket = py.import("socket")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("type", socket.getattr("SOCK_STREAM")?)?;
    let answers = socket.getattr("getaddrinfo")?.call((host, 443), Some(&kwargs))?;
    let ipaddress = py.import("ipaddress")?;
    let mut any = false;
    for entry in answers.try_iter()? {
        any = true;
        let address = entry?.get_item(4)?.get_item(0)?;
        if !ipaddress.getattr("ip_address")?.call1((address,))?.getattr("is_global")?.is_truthy()? {
            return Err(error(py, "PDF sunucusu doğrulanamadı."));
        }
    }
    if !any {
        return Err(error(py, "PDF sunucusu doğrulanamadı."));
    }
    let request = py.import("urllib.request")?;
    let ssl = py.import("ssl")?;
    let no_redirect = pyhelp::global(py, "samsung_notes.previews", "_NoRedirect")?.call0()?;
    let https = request.getattr("HTTPSHandler")?.call((), Some(&pyhelp::kwargs(py, &[("context", ssl.getattr("create_default_context")?.call0()?)])?))?;
    let opener = request.getattr("build_opener")?.call1((no_redirect, https))?;
    let req = request.getattr("Request")?.call((url,), Some(&pyhelp::kwargs(py, &[
        ("method", "GET".into_pyobject(py)?.into_any()), ("headers", headers.clone())])?))?;
    let http_error = py.import("urllib.error")?.getattr("HTTPError")?;
    let response = match opener.call_method("open", (req,), Some(&pyhelp::kwargs(py, &[("timeout", 25i64.into_pyobject(py)?.into_any())])?)) {
        Ok(r) => r,
        Err(e) if e.is_instance(py, &http_error) => e.value(py).clone().into_any(),
        Err(e) => return Err(e),
    };
    response.call_method0("__enter__")?;
    let result = (|| -> PyResult<(i64, Bound<'py, PyAny>)> {
        let code: i64 = response.getattr("code")?.extract()?;
        if code != 200 {
            return Ok((code, response.getattr("headers")?.call_method1("get", ("Location",))?));
        }
        let length = response.getattr("headers")?.call_method1("get", ("Content-Length",))?;
        if length.is_truthy()? {
            let too_large = !length.call_method0("isdecimal")?.is_truthy()?
                || py.import("builtins")?.getattr("int")?.call1((&length,))?.gt(MAX_PDF_BYTES)?;
            if too_large {
                return Err(error(py, "PDF önizleme boyutu sınırını aşıyor."));
            }
        }
        let target = fspath(temporary)?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(&target)?;
        let mut total: u64 = 0;
        loop {
            check.call0()?;
            let chunk = response.call_method1("read", (65536,))?;
            let chunk = chunk.cast::<PyBytes>()?.as_bytes().to_vec();
            if chunk.is_empty() {
                break;
            }
            total += chunk.len() as u64;
            if total > MAX_PDF_BYTES {
                return Err(error(py, "PDF önizleme boyutu sınırını aşıyor."));
            }
            std::io::Write::write_all(&mut file, &chunk)?;
        }
        file.sync_all()?;
        Ok((200, py.None().into_bound(py)))
    })();
    let none = py.None();
    response.call_method1("__exit__", (none.clone_ref(py), none.clone_ref(py), none))?;
    result
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(candidate, api, directory, *, account, guard=None, transport=None))]
#[allow(clippy::too_many_arguments)]
pub fn fetch_pdf<'py>(py: Python<'py>, candidate: &Bound<'py, PyAny>, api: &Bound<'py, PyAny>, directory: &Bound<'py, PyAny>,
                      account: String, guard: Option<Bound<'py, PyAny>>, transport: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let deadline = pyhelp::monotonic(py)? + 90.0;
    let guard: Option<Py<PyAny>> = guard.filter(|g| g.is_truthy().unwrap_or(true)).map(|g| g.unbind());
    let check = PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
        let py = args.py();
        if let Some(g) = &guard {
            g.call0(py)?;
        }
        if pyhelp::monotonic(py)? > deadline {
            return Err(error(py, "PDF indirme süre sınırını aştı."));
        }
        Ok(())
    })?.into_any();
    check.call0()?;
    let directory = xarea::private_directory(py, directory)?;
    let cached = cached_pdf(py, candidate, &directory, account.clone())?;
    if !cached.is_none() {
        check.call0()?;
        return Ok(cached);
    }
    let sha: String = candidate.getattr("sha256")?.extract()?;
    let path = directory.div(cache_name(&account, &sha))?;
    let temporary = directory.div(format!(".partial-{}", pyhelp::token_hex(py, 12)?))?;
    let result = (|| -> PyResult<Bound<'py, PyAny>> {
        let url = candidate.getattr("download_url")?;
        url_host(py, &url, BROKER_HOSTS)?;
        let headers = header_filter(py, api)?.into_any();
        check.call0()?;
        let fetch = |url: &Bound<'py, PyAny>, headers: &Bound<'py, PyAny>| -> PyResult<(i64, Bound<'py, PyAny>)> {
            match &transport {
                None => download(py, url, headers, &temporary, &check),
                Some(t) => t.call1((url, headers, &temporary, MAX_PDF_BYTES, &check))?.extract(),
            }
        };
        let (mut status, location) = fetch(&url, &headers)?;
        if status == 302 {
            url_host(py, &location, FILE_HOSTS)?;
            check.call0()?;
            let accept = PyDict::new(py);
            accept.set_item("Accept", "application/pdf")?;
            status = fetch(&location, accept.as_any())?.0;
        }
        check.call0()?;
        if status == 401 || status == 403 {
            return Err(PyErr::from_value(py.get_type::<PdfLinkExpired>().call0()?));
        }
        if status != 200 || !valid_cached_pdf(&fspath(&temporary)?, &sha)? {
            return Err(error(py, "PDF dosyasının bütünlüğü doğrulanamadı."));
        }
        py.import("os")?.getattr("replace")?.call1((&temporary, &path))?;
        Ok(path.clone())
    })();
    let cleanup = temporary.call_method("unlink", (), Some(&pyhelp::kwargs(py, &[("missing_ok", true.into_pyobject(py)?.to_owned().into_any())])?));
    let value = match result {
        Err(e) if e.is_instance_of::<PyOSError>(py) => Err(error(py, "PDF dosyası indirilemedi. Yeniden deneyebilirsin.")),
        other => other,
    };
    cleanup?;
    value
}

/// _read_ppm on a file path.
pub fn read_ppm(path: &Path, width: usize, height: usize) -> Result<Vec<u8>, Option<&'static str>> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| None)?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > (width * height * 3 + 1024) as u64 {
        return Err(None);
    }
    let data = std::fs::read(path).map_err(|_| None)?;
    match px::parse_ppm(&data, width, height) {
        Ok(pixels) => Ok(pixels.to_vec()),
        Err(true) => Err(Some("PDF görüntüsünün boyutu doğrulanamadı.")),
        Err(false) => Err(None),
    }
}

fn temp_directory() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(std::env::temp_dir);
    let template = std::ffi::CString::new(base.join("samsung-pdf-render-XXXXXX").into_os_string().into_encoded_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut raw = template.into_bytes_with_nul();
    // SAFETY: `raw` is a writable, NUL-terminated buffer ending in XXXXXX that outlives the call; mkdtemp rewrites only those six bytes in place.
    let made = unsafe { libc::mkdtemp(raw.as_mut_ptr() as *mut libc::c_char) };
    if made.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    raw.pop();
    Ok(PathBuf::from(String::from_utf8_lossy(&raw).into_owned()))
}

pub enum RenderError {
    Message(&'static str),
    Generic,
    Encode(&'static str),
}

/// Render one PDF page with Poppler in the bwrap sandbox and place it at
/// the note rect on an opaque white page canvas. Returns RGBA pixels.
pub fn render_page_pixels(pdf: &Path, sha256: &str, page: i64, canvas: (i64, i64), rect: (i64, i64, i64, i64)) -> Result<Vec<u8>, RenderError> {
    if !provider_available() {
        return Err(RenderError::Message("Yerel PDF görüntüleyicisi kurulu değil."));
    }
    if !valid_cached_pdf(pdf, sha256).unwrap_or(false) {
        return Err(RenderError::Message("PDF dosyası doğrulanamadı."));
    }
    let (left, top, right, bottom) = rect;
    let (width, height) = (right - left, bottom - top);
    let area = canvas.0 as i128 * canvas.1 as i128;
    if !(0 < area && area <= px::MAX_PIXELS as i128) {
        return Err(RenderError::Message("PDF sayfası önizleme sınırını aşıyor."));
    }
    // pdf_background_worker argument gate (exit 65).
    if !(0..=100_000).contains(&page) || width.min(height) <= 0 || width * height > 4 * 1024 * 1024 {
        return Err(RenderError::Message("PDF sayfası güvenle açılamadı."));
    }
    let source = std::fs::canonicalize(pdf).map_err(|_| RenderError::Message("PDF sayfası güvenle açılamadı."))?;
    let output = temp_directory().map_err(|_| RenderError::Message("PDF sayfası süre sınırı içinde açılamadı."))?;
    let result = (|| {
        let args = sandbox::pdf_args(&source.to_string_lossy(), &output.to_string_lossy(), page as u32, width as u32, height as u32);
        let outcome = sandbox::run(&args, None, sandbox::PDF_LIMITS, false, Duration::from_secs(RENDER_TIMEOUT))
            .map_err(|_| RenderError::Message("PDF sayfası süre sınırı içinde açılamadı."))?;
        if outcome.code != Some(0) {
            return Err(RenderError::Message("PDF sayfası güvenle açılamadı."));
        }
        read_ppm(&output.join("page.ppm"), width as usize, height as usize).map_err(|e| match e {
            Some(m) => RenderError::Message(m),
            None => RenderError::Generic,
        })
    })();
    let _ = std::fs::remove_dir_all(&output);
    let rgb = result?;
    Ok(px::place_rgb(&rgb, width as usize, height as usize, canvas.0 as usize, canvas.1 as usize, left as usize, top as usize))
}

#[cfg(feature = "python")]
pub fn render_error(py: Python<'_>, e: RenderError) -> PyErr {
    match e {
        RenderError::Message(m) => error(py, m),
        RenderError::Generic => default_error(py),
        RenderError::Encode(m) => PyValueError::new_err(m),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn render_pdf_background<'py>(py: Python<'py>, path: &Bound<'py, PyAny>, candidate: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let pdf = fspath(path)?;
    let sha: String = candidate.getattr("sha256")?.extract()?;
    let page: i64 = candidate.getattr("pdf_page_index")?.extract()?;
    let canvas: (i64, i64) = (candidate.getattr("width")?.extract()?, candidate.getattr("height")?.extract()?);
    let rect: (i64, i64, i64, i64) = candidate.getattr("rect")?.extract()?;
    let png = py.detach(|| {
        let pixels = render_page_pixels(&pdf, &sha, page, canvas, rect)?;
        px::rgba_png(canvas.0, canvas.1, &pixels).map_err(RenderError::Encode)
    }).map_err(|e| render_error(py, e))?;
    Ok(PyBytes::new(py, &png))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _read_ppm<'py>(py: Python<'py>, path: &Bound<'py, PyAny>, width: usize, height: usize) -> PyResult<Bound<'py, PyBytes>> {
    let path = fspath(path)?;
    let pixels = read_ppm(&path, width, height).map_err(|e| match e {
        Some(m) => error(py, m),
        None => default_error(py),
    })?;
    Ok(PyBytes::new(py, &pixels))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PdfBackgroundError>()?;
    m.add("PdfBackground", crate::dataclasses::pdf_background(m.py())?)?;
    m.add_class::<PdfLinkExpired>()?;
    m.add_function(wrap_pyfunction!(_rgba, m)?)?;
    m.add_function(wrap_pyfunction!(compose_pdf_canvas, m)?)?;
    m.add_function(wrap_pyfunction!(pdf_page_indices, m)?)?;
    m.add_function(wrap_pyfunction!(inspect_pdf_backgrounds, m)?)?;
    m.add_function(wrap_pyfunction!(pdf_provider_available, m)?)?;
    m.add_function(wrap_pyfunction!(_valid_cached_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(cached_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_pdf, m)?)?;
    m.add_function(wrap_pyfunction!(render_pdf_background, m)?)?;
    m.add_function(wrap_pyfunction!(_read_ppm, m)?)?;
    m.add("MAX_PDF_BYTES", MAX_PDF_BYTES)?;
    rs_common::export(m, &["PdfBackgroundError", "PdfLinkExpired", "PdfBackground", "_rgba", "compose_pdf_canvas", "pdf_page_indices",
        "inspect_pdf_backgrounds", "pdf_provider_available", "_valid_cached_pdf", "cached_pdf", "fetch_pdf",
        "render_pdf_background", "_read_ppm"])
}
