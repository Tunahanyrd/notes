//! `ssl.create_default_context()` of CPython 3.14 on OpenSSL 3, rebuilt with
//! the openssl crate, and the per-connection part of `SSLContext.wrap_socket`.
use crate::error::NetError;
use openssl::ssl::{
    ErrorCode, HandshakeError, Ssl, SslContext, SslContextBuilder, SslMethod, SslOptions, SslStream, SslVerifyMode,
    SslVersion,
};
use openssl::x509::verify::{X509CheckFlags, X509VerifyFlags};
use openssl::x509::{X509VerifyResult, X509};
use std::io::{self, Read, Write};
use std::net::IpAddr;

/// `ssl._DEFAULT_CIPHERS` (`PY_SSL_DEFAULT_CIPHER_STRING`) of CPython 3.14.
pub const PYTHON_CIPHERS: &str =
    "@SECLEVEL=2:ECDH+AESGCM:ECDH+CHACHA20:ECDH+AES:DHE+AES:!aNULL:!eNULL:!aDSS:!SHA1:!AESCCM";

/// `create_default_context().options` on this system: OP_ALL without
/// DONT_INSERT_EMPTY_FRAGMENTS and LEGACY_SERVER_CONNECT, plus NO_SSLv3,
/// CIPHER_SERVER_PREFERENCE, ENABLE_MIDDLEBOX_COMPAT, NO_COMPRESSION
/// (`0x82520050`).
pub fn python_options() -> SslOptions {
    // SSL_OP_LEGACY_SERVER_CONNECT (bit 2) has no named constant in the crate.
    let legacy_server_connect = SslOptions::from_bits_retain(0x4);
    (SslOptions::ALL - SslOptions::DONT_INSERT_EMPTY_FRAGMENTS - legacy_server_connect)
        | SslOptions::NO_SSLV2
        | SslOptions::NO_SSLV3
        | SslOptions::CIPHER_SERVER_PREFERENCE
        | SslOptions::ENABLE_MIDDLEBOX_COMPAT
        | SslOptions::NO_COMPRESSION
}

/// `create_default_context().verify_flags`: VERIFY_X509_STRICT |
/// VERIFY_X509_PARTIAL_CHAIN | VERIFY_X509_TRUSTED_FIRST (`0x88020`).
pub fn python_verify_flags() -> X509VerifyFlags {
    X509VerifyFlags::X509_STRICT | X509VerifyFlags::PARTIAL_CHAIN | X509VerifyFlags::TRUSTED_FIRST
}

fn ssl_err(e: openssl::error::ErrorStack) -> NetError {
    NetError::os("SSLError", e.to_string())
}

/// `ssl.create_default_context()`: TLS client method, minimum TLS 1.2, the
/// Python cipher string (TLS 1.3 suites at OpenSSL defaults), CERT_REQUIRED,
/// the system trust store (`set_default_verify_paths`, honours
/// `SSL_CERT_FILE`/`SSL_CERT_DIR` like Python), Python's verify flags and
/// host flags (X509_CHECK_FLAG_NO_PARTIAL_WILDCARDS; common-name fallback
/// left on, as `hostname_checks_common_name` is True). No ALPN, no
/// post-handshake auth, no keylog (SSLKEYLOGFILE is deliberately ignored).
pub(crate) fn default_context(extra_roots: &[X509]) -> Result<SslContext, NetError> {
    Ok(context_builder(extra_roots)?.build())
}

pub(crate) fn context_builder(extra_roots: &[X509]) -> Result<SslContextBuilder, NetError> {
    let mut b = SslContextBuilder::new(SslMethod::tls_client()).map_err(ssl_err)?;
    let current = b.options();
    b.clear_options(current);
    b.set_options(python_options());
    b.set_min_proto_version(Some(SslVersion::TLS1_2)).map_err(ssl_err)?;
    b.set_max_proto_version(None).map_err(ssl_err)?;
    b.set_cipher_list(PYTHON_CIPHERS).map_err(ssl_err)?;
    b.set_verify(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT);
    b.set_default_verify_paths().map_err(ssl_err)?;
    // The system bundle by its well-known path, for an OpenSSL whose
    // OPENSSLDIR does not exist here (AppImage on Arch/Fedora, Homebrew-less
    // macOS); see notes_platform::tools::extra_ca_file.
    if let Some(bundle) = notes_platform::tools::extra_ca_file() {
        b.set_ca_file(&bundle).map_err(ssl_err)?;
    }
    for cert in extra_roots {
        b.cert_store_mut().add_cert(cert.clone()).map_err(ssl_err)?;
    }
    let param = b.verify_param_mut();
    let flags = param.flags();
    param.clear_flags(flags).map_err(ssl_err)?;
    param.set_flags(python_verify_flags()).map_err(ssl_err)?;
    param.set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS);
    Ok(b)
}

/// `str.encode('idna')` fast path for ASCII names (wrap_socket's
/// `_encode_hostname`): empty inner labels and labels of 64+ bytes fail.
fn idna_check(host: &str) -> Result<(), NetError> {
    if !host.is_ascii() {
        return Err(NetError::value_class("UnicodeError", "non-ASCII host names are not supported"));
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels[..labels.len() - 1].iter().any(|l| l.is_empty()) {
        return Err(NetError::value_class("UnicodeError", "encoding with 'idna' codec failed (UnicodeEncodeError: empty label)"));
    }
    if labels.iter().any(|l| l.len() >= 64) {
        return Err(NetError::value_class("UnicodeError", "encoding with 'idna' codec failed (UnicodeEncodeError: label too long)"));
    }
    Ok(())
}

/// `context.wrap_socket(sock, server_hostname=host)` with the handshake.
pub(crate) fn wrap<S: Read + Write>(ctx: &SslContext, stream: S, host: &str) -> Result<SslStream<S>, NetError> {
    idna_check(host)?;
    let mut ssl = Ssl::new(ctx).map_err(ssl_err)?;
    let ip: Option<IpAddr> = host.parse().ok();
    match ip {
        Some(ip) => ssl.param_mut().set_ip(ip).map_err(ssl_err)?,
        None => {
            ssl.set_hostname(host).map_err(ssl_err)?; // SNI only for names
            ssl.param_mut().set_host(host).map_err(ssl_err)?;
        }
    }
    match ssl.connect(stream) {
        Ok(s) => Ok(s),
        Err(HandshakeError::SetupFailure(e)) => Err(ssl_err(e)),
        Err(HandshakeError::WouldBlock(_)) => Err(NetError::os("TimeoutError", "_ssl.c: The handshake operation timed out")),
        Err(HandshakeError::Failure(mid)) => {
            let verify = mid.ssl().verify_result();
            if verify != X509VerifyResult::OK {
                let message = if verify.as_raw() == 62 {
                    // X509_V_ERR_HOSTNAME_MISMATCH
                    format!("[SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed: Hostname mismatch, certificate is not valid for '{host}'.")
                } else if verify.as_raw() == 64 {
                    // X509_V_ERR_IP_ADDRESS_MISMATCH
                    format!("[SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed: IP address mismatch, certificate is not valid for '{host}'.")
                } else {
                    format!("[SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed: {}", verify.error_string())
                };
                return Err(NetError::os("SSLCertVerificationError", message));
            }
            let error = mid.error();
            if let Some(io) = error.io_error() {
                if matches!(io.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) {
                    return Err(NetError::os("TimeoutError", "_ssl.c: The handshake operation timed out"));
                }
                return Err(NetError::from_io(io));
            }
            if error.code() == ErrorCode::SYSCALL || error.code() == ErrorCode::ZERO_RETURN {
                return Err(NetError::os("SSLEOFError", "EOF occurred in violation of protocol"));
            }
            Err(NetError::os("SSLError", error.to_string()))
        }
    }
}

/// `SSL_R_UNEXPECTED_EOF_WHILE_READING` (OpenSSL 3), reported by Python as
/// SSLEOFError and swallowed by `suppress_ragged_eofs=True`.
fn unexpected_eof(error: &openssl::ssl::Error) -> bool {
    error.ssl_error().is_some_and(|stack| {
        stack.errors().iter().any(|e| e.library_code() == 20 && e.reason_code() == 294)
    })
}

/// One `SSLSocket.recv_into`: 0 at a clean or ragged EOF.
pub(crate) fn read<S: Read + Write>(stream: &mut SslStream<S>, buf: &mut [u8]) -> Result<usize, NetError> {
    loop {
        match stream.ssl_read(buf) {
            Ok(n) => return Ok(n),
            Err(e) => {
                if e.code() == ErrorCode::ZERO_RETURN {
                    return Ok(0);
                }
                if let Some(io) = e.io_error() {
                    if io.kind() == io::ErrorKind::Interrupted {
                        continue; // PEP 475
                    }
                    if matches!(io.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) {
                        return Err(NetError::os("TimeoutError", "The read operation timed out"));
                    }
                    return Err(NetError::from_io(io));
                }
                if e.code() == ErrorCode::SYSCALL || unexpected_eof(&e) {
                    return Ok(0);
                }
                if e.code() == ErrorCode::WANT_READ || e.code() == ErrorCode::WANT_WRITE {
                    continue;
                }
                return Err(NetError::os("SSLError", e.to_string()));
            }
        }
    }
}

/// `SSLSocket.sendall(data)`
pub(crate) fn write_all<S: Read + Write>(stream: &mut SslStream<S>, mut data: &[u8]) -> Result<(), NetError> {
    while !data.is_empty() {
        match stream.ssl_write(data) {
            Ok(n) => data = &data[n..],
            Err(e) => {
                if let Some(io) = e.io_error() {
                    if io.kind() == io::ErrorKind::Interrupted {
                        continue; // PEP 475
                    }
                    if matches!(io.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) {
                        return Err(NetError::os("TimeoutError", "The write operation timed out"));
                    }
                    return Err(NetError::from_io(io));
                }
                if e.code() == ErrorCode::WANT_READ || e.code() == ErrorCode::WANT_WRITE {
                    continue;
                }
                if e.code() == ErrorCode::SYSCALL || e.code() == ErrorCode::ZERO_RETURN {
                    return Err(NetError::os("SSLEOFError", "EOF occurred in violation of protocol"));
                }
                return Err(NetError::os("SSLError", e.to_string()));
            }
        }
    }
    Ok(())
}
