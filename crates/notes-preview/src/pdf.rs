//! `pdf_background`: PDF placements referenced by page metadata, the private
//! PDF cache, the bounded download and the isolated Poppler render.
use crate::env::PreviewEnv;
use crate::fetch::{filtered_headers, url};
use crate::payload::{parse_payload, Parsed};
use crate::util::{pdf, private_directory, token_hex, PDF_DEFAULT};
use crate::{BROKER_HOSTS, FILE_HOSTS, MAX_PIXELS};
use notes_host::{Fail, Kind};
use rs_ink::core::payload::CanvasPreviewCandidate;
use rs_render::pdf_background as rp;
use rs_render::pixels as px;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MAX_PDF_BYTES: u64 = rp::MAX_PDF_BYTES;

/// `pdf_background.PdfBackground` (frozen dataclass).
#[derive(Clone, Debug, PartialEq)]
pub struct PdfBackground {
    pub page_index: i64,
    pub pdf_page_index: i64,
    pub width: i64,
    pub height: i64,
    pub rect: (i64, i64, i64, i64),
    pub annotation_objects: i64,
    pub sha256: String,
    pub download_url: Option<String>,
    /// The native canvas layer composited over the PDF page, when available.
    pub overlay: Option<CanvasPreviewCandidate>,
}

impl PdfBackground {
    pub fn is_dark_mode(&self) -> bool {
        false
    }

    /// PDF background only; native canvas objects are separate render layers.
    pub fn complete(&self) -> bool {
        self.annotation_objects == 0 || self.overlay.is_some()
    }
}

fn texts(parsed: &Parsed) -> Result<Vec<String>, Fail> {
    rs_render::pdf_layout::parse_all(&parsed.xml_documents()).map_err(|_| pdf(PDF_DEFAULT))
}

/// `pdf_page_indices(parsed)`
pub fn pdf_page_indices(parsed: &Parsed) -> Result<BTreeSet<usize>, Fail> {
    rs_render::pdf_layout::pdf_pages(&texts(parsed)?).map_err(|_| pdf(PDF_DEFAULT))
}

/// `inspect_pdf_backgrounds(parsed)` on an already parsed payload.
pub fn inspect_pdf_backgrounds(parsed: &Parsed) -> Result<Vec<PdfBackground>, Fail> {
    if parsed.result.locked {
        return Err(pdf("Kilitli PDF notu görüntülenemiyor."));
    }
    let texts = texts(parsed)?;
    let files: HashSet<String> = parsed.result.content_files.iter().map(|f| f.file_hash.clone()).collect();
    let urls: HashMap<String, String> = parsed.result.download_urls.iter().map(|u| (u.id.to_lowercase(), u.url.clone())).collect();
    let placements = rs_render::pdf_layout::placements(&texts, &files, &urls, MAX_PIXELS)
        .map_err(|e| e.map(pdf).unwrap_or_else(|| pdf(PDF_DEFAULT)))?;
    Ok(placements.into_iter().map(|p| PdfBackground {
        page_index: p.page_index, pdf_page_index: p.pdf_page_index, width: p.width, height: p.height, rect: p.rect,
        annotation_objects: p.annotation_objects, sha256: p.sha256, download_url: p.download_url, overlay: None,
    }).collect())
}

/// `inspect_pdf_backgrounds(raw, content_type)`: parse, then inspect;
/// `PayloadError` becomes `PdfBackgroundError(str(error))`.
pub fn inspect_pdf_backgrounds_raw(raw: &[u8], content_type: &str) -> Result<Vec<PdfBackground>, Fail> {
    let parsed = parse_payload(raw.to_vec(), content_type).map_err(|e| if e.kind == Kind::Payload { pdf(&e.message) } else { e })?;
    inspect_pdf_backgrounds(&parsed)
}

/// `pdf_provider_available()`
pub fn pdf_provider_available() -> bool {
    rp::provider_available()
}

/// `_valid_cached_pdf(path, digest)` (OSError while reading propagates).
pub fn valid_cached_pdf(path: &Path, digest: &str) -> Result<bool, Fail> {
    Ok(rp::valid_cached_pdf(path, digest)?)
}

/// `cached_pdf(candidate, directory, account=account)`
pub fn cached_pdf(candidate: &PdfBackground, directory: &Path, account: &str) -> Result<Option<PathBuf>, Fail> {
    let path = directory.join(rp::cache_name(account, &candidate.sha256));
    Ok(if valid_cached_pdf(&path, &candidate.sha256)? { Some(path) } else { None })
}

/// `(url, headers, temporary, bound, check) -> (status, Location)`
pub type PdfTransport<'a> = &'a dyn Fn(&str, &[(String, String)], &Path, u64, &dyn Fn() -> Result<(), Fail>)
    -> Result<(u16, Option<String>), Fail>;

/// `fetch_pdf(candidate, api, directory, account=..., guard=...)` with the
/// engine's transport ([`PreviewEnv::download`]).
pub fn fetch_pdf(candidate: &PdfBackground, env: &dyn PreviewEnv, directory: &Path, account: &str,
                 guard: Option<&dyn Fn() -> Result<(), Fail>>) -> Result<PathBuf, Fail> {
    fetch_pdf_with(candidate, &|| env.headers(), directory, account, guard,
                   &|u, h, p, b, c| env.download(u, h, p, b, c))
}

/// `fetch_pdf(..., transport=transport)`: one bounded GET, optionally through
/// the known broker redirect; account headers never reach the file origin.
pub fn fetch_pdf_with(candidate: &PdfBackground, headers: &dyn Fn() -> Result<Vec<(String, String)>, Fail>,
                      directory: &Path, account: &str, guard: Option<&dyn Fn() -> Result<(), Fail>>,
                      transport: PdfTransport<'_>) -> Result<PathBuf, Fail> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let check = || -> Result<(), Fail> {
        if let Some(g) = guard {
            g()?;
        }
        if Instant::now() > deadline {
            return Err(pdf("PDF indirme süre sınırını aştı."));
        }
        Ok(())
    };
    check()?;
    let directory = private_directory(directory)?;
    if let Some(path) = cached_pdf(candidate, &directory, account)? {
        check()?;
        return Ok(path);
    }
    let path = directory.join(rp::cache_name(account, &candidate.sha256));
    let temporary = directory.join(format!(".partial-{}", token_hex(12)));
    let result = (|| -> Result<PathBuf, Fail> {
        url(candidate.download_url.as_deref(), BROKER_HOSTS)?;
        let headers = filtered_headers(headers()?);
        check()?;
        let broker = candidate.download_url.as_deref().unwrap_or_default();
        let (mut status, location) = transport(broker, &headers, &temporary, MAX_PDF_BYTES, &check)?;
        if status == 302 {
            url(location.as_deref(), FILE_HOSTS)?;
            check()?;
            status = transport(location.as_deref().unwrap_or_default(),
                               &[("Accept".to_string(), "application/pdf".to_string())], &temporary, MAX_PDF_BYTES, &check)?.0;
        }
        check()?;
        if status == 401 || status == 403 {
            return Err(link_expired());
        }
        if status != 200 || !valid_cached_pdf(&temporary, &candidate.sha256)? {
            return Err(pdf("PDF dosyasının bütünlüğü doğrulanamadı."));
        }
        std::fs::rename(&temporary, &path)?;
        Ok(path.clone())
    })();
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|e| if e.is_os() { pdf("PDF dosyası indirilemedi. Yeniden deneyebilirsin.") } else { e })
}

/// `pdf_background.PdfLinkExpired()` (PdfBackgroundError subclass): the
/// stored signed download URL was refused (HTTP 401/403). Marked with
/// `code == "link_expired"` so callers can catch the subclass.
pub fn link_expired() -> Fail {
    let mut f = pdf("PDF indirme bağlantısının süresi dolmuş.");
    f.code = LINK_EXPIRED.into();
    f
}

pub const LINK_EXPIRED: &str = "link_expired";

/// `isinstance(error, PdfLinkExpired)`
pub fn is_link_expired(f: &Fail) -> bool {
    f.kind == Kind::PdfBackground && f.code == LINK_EXPIRED
}

/// `render_pdf_background(path, candidate)`: one page in the bwrap-isolated,
/// rlimited Poppler (`pdftoppm`) sandbox, placed on the note page canvas.
pub fn render_pdf_background(path: &Path, candidate: &PdfBackground) -> Result<Vec<u8>, Fail> {
    let pixels = rp::render_page_pixels(path, &candidate.sha256, candidate.pdf_page_index,
                                        (candidate.width, candidate.height), candidate.rect)
        .map_err(|e| match e {
            rp::RenderError::Message(m) => pdf(m),
            rp::RenderError::Generic => pdf(PDF_DEFAULT),
            rp::RenderError::Encode(m) => Fail::value(m),
        })?;
    px::rgba_png(candidate.width, candidate.height, &pixels).map_err(Fail::value)
}

/// `pdf_background._rgba(png, width, height)`
pub fn rgba(png: &[u8], width: i64, height: i64) -> Result<Vec<u8>, Fail> {
    px::rgba(png, width, height).map_err(|e| pdf(e.message()))
}

/// `compose_pdf_canvas(background, overlay, width, height)`
pub fn compose_pdf_canvas(background: &[u8], overlay: &[u8], width: i64, height: i64) -> Result<Vec<u8>, Fail> {
    let mut under = rgba(background, width, height)?;
    let over = rgba(overlay, width, height)?;
    px::compose(&mut under, &over).map_err(|_| pdf("PDF üzerindeki çizim katmanı henüz güvenle birleştirilemiyor."))?;
    px::rgba_png(width, height, &under).map_err(Fail::value)
}

/// `page_image.light_from_dark(png, width, height)`
pub fn light_from_dark(png: &[u8], width: i64, height: i64) -> Result<Vec<u8>, Fail> {
    let mut pixels = rgba(png, width, height)?;
    px::light_from_dark_pixels(&mut pixels);
    px::rgba_png(width, height, &pixels).map_err(Fail::value)
}
