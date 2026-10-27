# Security policy

## Reporting a vulnerability

Please report vulnerabilities **privately**. The preferred channel is GitHub
Security Advisories: <https://github.com/Tunahanyrd/notes/security/advisories/new>.
If you cannot use it, e-mail <tunahanyrd@gmail.com>. Do not open a public
issue for a security problem.

Include a description, affected version or commit, reproduction steps and the
impact you expect. **Never include real credentials or personal data**: no
access/refresh tokens, cookies, authorization codes, signed download URLs,
account e-mails or ids, note contents or SQLite libraries. Use placeholders or
a synthetic library (for example a copy of the synthetic golden library in
`crates/notes-engine/conformance/golden/library`) to reproduce.

We aim to acknowledge reports within a week. Fixes are released as soon as
practical and credited to the reporter unless you prefer otherwise.

## Scope

In scope:

- handling of Samsung account and Cloud tokens, session files and the sign-in
  callback (`ms-app` handler, callback validation and decryption);
- the local HTTP API: bearer-token authentication, Host/Origin checks,
  request parsing and size limits, path handling for static files;
- the renderer sandbox (bubblewrap, resource limits, the reduced fallback used
  in Flatpak/macOS) and handling of untrusted note payloads, PDFs and images;
- file writes (exports, preview cache, data directory permissions);
- packaging (desktop entries, Flatpak permissions, scheme handler
  registration).

Out of scope: vulnerabilities in Samsung's services themselves (report those
to Samsung), and issues that need an attacker who already controls the user's
account or local session.

## Supported versions

Until 1.0, only the latest release and the `main` branch receive security
fixes.
