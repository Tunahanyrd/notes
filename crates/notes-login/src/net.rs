//! Production [`Network`] on `notes_net` (pure blocking HTTPS: system
//! OpenSSL trust store, TLS ≥ 1.2, hostname verification, one request per
//! connection, no redirects, cookies, proxies or retries). It maps one call
//! to `Client::new(timeout).open(request)` + one `Response::read(read_limit)`
//! — urllib's `opener.open(request, timeout)` + `response.read(n)`.
use crate::http::{HttpRequest, HttpResponse, NetError, Network};
use std::time::Duration;

/// The real network. Tests inject fakes through [`Network`] instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotesNet;

fn convert(e: &notes_net::NetError, head: Option<HttpResponse>) -> NetError {
    NetError {
        message: format!("{}: {e}", e.python_class()),
        head,
        escaped: e.family() == Some(notes_net::Family::Escape),
    }
}

impl Network for NotesNet {
    fn request(&self, request: &HttpRequest, timeout: Duration, read_limit: usize) -> Result<HttpResponse, NetError> {
        if let Some(e) = notes_net::request_url_error(&request.url) {
            return Err(convert(&e, None));
        }
        let wire = notes_net::HttpRequest {
            method: request.method.clone(),
            url: request.url.clone(),
            headers: request.headers.clone(),
            data: request.data.clone(),
        };
        let client = notes_net::Client::new(timeout).map_err(|e| convert(&e, None))?;
        let mut response = client.open(&wire).map_err(|e| convert(&e, None))?;
        let head = HttpResponse { status: response.status(), headers: response.headers().items().to_vec(), body: Vec::new() };
        match response.read(read_limit) {
            Ok(body) => Ok(HttpResponse { body, ..head }),
            Err(e) => Err(convert(&e, Some(head))),
        }
    }
}
