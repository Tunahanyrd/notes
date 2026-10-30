//! TLS trust-store check: one GET of a public page, prints the HTTP status or
//! the error. Used to verify the CA bundle lookup with a bundled OpenSSL,
//! e.g. `LD_LIBRARY_PATH=<AppImage>/usr/lib cargo run --example tls_probe`.
use std::time::Duration;

fn main() {
    let url = std::env::args().nth(1).unwrap_or_else(|| "https://www.samsung.com/".into());
    match notes_net::get(&url, &[], 4096, Duration::from_secs(15)) {
        Ok((status, _, _)) => println!("ok {status}"),
        Err(e) => println!("error {e}"),
    }
}
