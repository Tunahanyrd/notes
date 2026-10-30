// The system CA bundle is found by its well-known path, so an OpenSSL bundled
// in an AppImage (OPENSSLDIR of the Ubuntu build host, absent on Arch/Fedora)
// can still verify Samsung's certificates. Before: None on Linux, and every
// sync from the AppImage on Arch failed with CERTIFICATE_VERIFY_FAILED.
// One test in this binary: it changes SSL_CERT_FILE.

#[test]
fn the_distribution_ca_bundle_is_found_unless_ssl_cert_file_is_set() {
    std::env::remove_var("SSL_CERT_FILE");
    let known = ["/etc/ssl/certs/ca-certificates.crt", "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
                 "/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/ca-bundle.pem", "/etc/ssl/cert.pem"];
    if known.iter().any(|p| std::path::Path::new(p).is_file()) {
        let found = notes_platform::tools::extra_ca_file().expect("a system CA bundle");
        assert!(found.is_file());
    }
    std::env::set_var("SSL_CERT_FILE", "/nonexistent/bundle.pem");
    assert_eq!(notes_platform::tools::extra_ca_file(), None);
    std::env::remove_var("SSL_CERT_FILE");
}
