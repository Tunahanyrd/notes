//! `previews._url` and `previews.fetch_preview`.
use crate::env::PreviewEnv;
use crate::util::{preview, PREVIEW_DEFAULT};
use crate::{BROKER_HOSTS, FILE_HOSTS, MAX_DOWNLOAD};
use notes_host::Fail;
use rs_ink::core::payload::CanvasPreviewCandidate;

pub type Headers = Vec<(String, String)>;
/// `(url, headers) -> (status, body, Location)`
pub type Transport<'a> = &'a dyn Fn(&str, &[(String, String)]) -> Result<(u16, Vec<u8>, Option<String>), Fail>;

/// `previews._url(value, hosts)`: Some(hostname) or `PreviewError`.
pub fn url(value: Option<&str>, hosts: &[&str]) -> Result<String, Fail> {
    let Some(value) = value else { return Err(preview(PREVIEW_DEFAULT)) };
    rs_render::previews::allowed_url(value, |netloc| hosts.contains(&netloc)).ok_or_else(|| preview(PREVIEW_DEFAULT))
}

/// `{k: v for k, v in api._headers().items() if k.lower() not in (...)}`
pub fn filtered_headers(headers: Headers) -> Headers {
    headers.into_iter().filter(|(k, _)| {
        let lower = k.to_lowercase();
        lower != "content-type" && lower != "content-encoding"
    }).collect()
}

/// `fetch_preview(candidate, api, transport=env.http, guard=guard)`.
pub fn fetch_preview(candidate: &CanvasPreviewCandidate, env: &dyn PreviewEnv,
                     guard: &dyn Fn() -> Result<(), Fail>) -> Result<Vec<u8>, Fail> {
    fetch_preview_with(candidate, &|| env.headers(), &|u, h| env.http(u, h), guard)
}

/// `fetch_preview` with an explicit header source and transport (tests).
pub fn fetch_preview_with(candidate: &CanvasPreviewCandidate, headers: &dyn Fn() -> Result<Headers, Fail>,
                          transport: Transport<'_>, guard: &dyn Fn() -> Result<(), Fail>) -> Result<Vec<u8>, Fail> {
    guard()?;
    url(candidate.download_url.as_deref(), BROKER_HOSTS)?;
    let headers = filtered_headers(headers()?);
    guard()?;
    let broker = candidate.download_url.as_deref().unwrap_or_default();
    let (mut status, mut raw, location) = transport(broker, &headers)?;
    if status == 302 {
        url(location.as_deref(), FILE_HOSTS)?;
        // The signed file URL authenticates this request. Never forward Cloud
        // Authorization, account/device headers, cookies, or Referer.
        guard()?;
        let (s, r, _) = transport(location.as_deref().unwrap_or_default(),
                                  &[("Accept".to_string(), "application/octet-stream".to_string())])?;
        status = s;
        raw = r;
    }
    guard()?;
    if status != 200 || raw.is_empty() || raw.len() > MAX_DOWNLOAD {
        return Err(preview("Sayfa görüntüsü alınamadı. Eşitleyip yeniden deneyebilirsin."));
    }
    if crate::util::sha256_hex(&raw) != candidate.sha256 {
        return Err(preview("Sayfa dosyasının bütünlüğü doğrulanamadı."));
    }
    Ok(raw)
}
