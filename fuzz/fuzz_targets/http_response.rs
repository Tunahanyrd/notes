//! notes-net response parsing over an in-memory stream (no sockets): status
//! line and interim responses, header block, Content-Length / chunked body
//! and trailers, redirect Location check, gzip Content-Encoding with the
//! decoded bound. Input: one byte (method + recv size), then the response.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((sel, raw)) = notes_fuzz::selector(data) else { return };
    let method = ["GET", "POST", "HEAD", "PUT"][(sel & 3) as usize];
    let chunk = 1 + ((sel >> 2) as usize) * 97;
    let _ = notes_net::fuzz_exchange(raw, method, chunk, 1 << 20);
});
