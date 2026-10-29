//! Real network for the pure engine: `notes_net` (pure blocking HTTPS with
//! the `send_once` / `previews._http` / `fetch_pdf` contracts) plugged into
//! the notes-cloud `Transport` seam and the preview environment.
use std::path::Path;
use std::time::Duration;

use notes_cloud::transport::{Request, Response, Transport};
use notes_host::{Fail, Kind};
use notes_net::{DownloadError, Family, NetError, SendError};

/// `previews._http` timeout and `fetch_pdf`'s download timeout.
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(5);
pub const PDF_TIMEOUT: Duration = Duration::from_secs(25);

fn fail_of(e: &NetError) -> Fail {
    match e {
        NetError::Raised { family, class, message } => {
            let kind = match family {
                Family::Os => Kind::Os,
                Family::Value => Kind::Value,
                _ => Kind::Other,
            };
            Fail::new(kind, format!("{}: {}", class, message))
        }
        NetError::NotGlobal => Fail::os("address is not global"),
        NetError::TooLarge => Fail::value("response too large"),
    }
}

/// `CloudAPI._call` without an injected transport: `notes_cloud.send_once(
/// directory, name, request, max_bytes=limit, quiet=True)`.
pub struct SendOnce;

impl Transport for SendOnce {
    fn send(&self, directory: &Path, name: &str, request: &Request, limit: usize) -> Result<Response, Fail> {
        let wire = notes_net::HttpRequest {
            method: request.method.clone(),
            url: request.url.clone(),
            headers: request.headers.clone(),
            data: request.data.clone(),
        };
        match notes_net::send_once(directory, name, &wire, limit, true) {
            Ok((status, body)) => Ok(Response { status: status.map(i64::from), body, content_type: None }),
            Err(SendError::SizeBound) => Err(Fail::value("unsupported response size bound")),
            Err(SendError::InvalidRequest(e)) | Err(SendError::Escaped(e)) | Err(SendError::Setup(e)) => Err(fail_of(&e)),
            Err(SendError::Claim(e)) | Err(SendError::Receipt(e)) => Err(Fail::from(e)),
        }
    }
    fn writes_receipts(&self) -> bool {
        true
    }
}

/// `previews._http(url, headers)`.
pub fn preview_http(url: &str, headers: &[(String, String)]) -> Result<(u16, Vec<u8>, Option<String>), Fail> {
    let max = notes_preview::MAX_DOWNLOAD;
    match notes_net::get(url, headers, max, PREVIEW_TIMEOUT) {
        Ok(v) => Ok(v),
        Err(NetError::NotGlobal) => Err(Fail::new(Kind::Preview, "Bu sayfanın görüntüsü henüz açılamıyor.")),
        Err(NetError::TooLarge) => Err(Fail::new(Kind::Preview, "Bu sayfa önizleme boyutu sınırını aşıyor.")),
        Err(e @ NetError::Raised { family: Family::Os | Family::Value, .. }) => {
            let _ = e;
            Err(Fail::new(Kind::Preview, notes_preview::env::DOWNLOAD_FAILED))
        }
        Err(e) => Err(fail_of(&e)),
    }
}

/// `fetch_pdf`'s default `download(url, headers)` into `dest`.
pub fn pdf_download(url: &str, headers: &[(String, String)], dest: &Path, max_bytes: u64, check: &dyn Fn() -> Result<(), Fail>) -> Result<(u16, Option<String>), Fail> {
    let mut checker = || check();
    match notes_net::download(url, headers, dest, max_bytes, PDF_TIMEOUT, &mut checker) {
        Ok(v) => Ok(v),
        Err(DownloadError::Check(f)) => Err(f),
        Err(DownloadError::Net(NetError::NotGlobal)) => Err(Fail::new(Kind::PdfBackground, "PDF sunucusu doğrulanamadı.")),
        Err(DownloadError::Net(NetError::TooLarge)) => Err(Fail::new(Kind::PdfBackground, "PDF önizleme boyutu sınırını aşıyor.")),
        Err(DownloadError::Net(e)) => Err(fail_of(&e)),
    }
}
