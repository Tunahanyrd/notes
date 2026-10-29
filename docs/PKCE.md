# Offline PKCE S256 component

> Note: The evidence records referenced here (sanitized measurements, raw analysis output) are kept private and are not part of the public repository.

`notes_protocol::pkce` is a standard preparation component for the independent client.
It does not connect to Samsung; it does not create a sign-in URL, callback, token request or user
interface. The presence of `code_verifier`, `code_challenge` and
`code_challenge_method` among the Windows Account constants is the research rationale. This code does not prove that Samsung
accepts S256 or an independent Linux client.

`CodeVerifier::generate()` takes 32 random bytes from the operating system and
converts them to 43 characters of unpadded base64url. An RNG error becomes
`RandomUnavailable` directly; there is no weak source or `plain` fallback.
`from_secret()` only accepts 43–128 ASCII unreserved characters; it does not strip
whitespace or apply Unicode normalization. This check does not measure entropy. For a new operation,
`generate()` should be used. `s256_challenge()` returns the SHA-256 result as
unpadded base64url; the method constant is `S256_METHOD`.
[RFC 7636 §4.1–4.2 and Appendix B](https://www.rfc-editor.org/rfc/rfc7636.txt).

The verifier is held in a private field inside `Zeroizing<String>`; `Debug` only
writes `CodeVerifier([REDACTED])`. There is no `Display`, automatic serialization or `Clone`.
`expose_secret()` is the accessor that must be called explicitly for a future token request;
its result must not be logged. The verifier and the random byte buffer
are cleared on drop. No full memory-clearing guarantee is given for the caller's `from_secret()` input,
operating system copies or intermediate state of crypto operations.
[Zeroizing documentation](https://docs.rs/zeroize/1.9.1/zeroize/struct.Zeroizing.html).

Dependencies are pinned with Cargo.lock:

| Library | Version | Use |
| --- | --- | --- |
| getrandom | 0.4.3 | Operating system CSPRNG; error-returning `fill` |
| sha2 | 0.10.9 | RustCrypto SHA-256 |
| base64 | 0.23.1 | URL-safe, unpadded encoding; alloc feature only |
| zeroize | 1.9.1 | Clearing owned secret buffers on drop |

The [getrandom](https://docs.rs/getrandom/0.4.3/getrandom/fn.fill.html),
[sha2](https://docs.rs/sha2/0.10.9/sha2/) and
[base64](https://docs.rs/base64/0.23.1/base64/) APIs are used; no cryptographic
algorithm was reimplemented. Because the package manifest of `sha2` 0.11.1 requires Rust 1.88,
0.10.9 was chosen for the project's 1.85 target. The MSRVs declared by the Linux dependencies
do not exceed 1.85; some old transitive packages do not declare an MSRV.
Build/test was done with the current Rust **1.99.1**; **building on Rust 1.85 has
not been verified yet**. The workspace kept `rust-version = "1.85"`.
In the 2026-10-27 dependency update (parking_lot 0.13 → Rust 1.95, rusqlite 0.41 →
1.89) the workspace `rust-version` was raised to 1.95; `sha2` stays at 0.10.9 (a separate decision).

Validation: six PKCE tests cover the RFC Appendix B vector, boundary lengths,
unreserved characters, rejection of invalid input, the size of the generated value and
Debug/error secrecy. One compile-fail doctest checks that `Display` is absent.
The random value test only checks format/OS source access;
it is not a statistical entropy measurement. In the workspace **33 tests and 1
doctest passed**; Clippy `-D warnings` and format check clean.
Evidence: the private evidence archive.

The security design was based on the scope of OWASP ASVS 10.1.2, 11.2.1 and 11.5.1:
generating a new verifier, established crypto libraries and strong randomness.
Transaction/state binding, callback validation, single use and the session
lifecycle are not implemented yet; this component alone is not a secure OAuth client
or an ASVS compliance assessment.
