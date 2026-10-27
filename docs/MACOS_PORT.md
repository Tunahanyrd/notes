# macOS port

Status (2026-10-27): **prepared, not yet run on a Mac.** The code compiles for
`aarch64-apple-darwin` (whole engine workspace including tests, plus the
desktop shell; checked from Linux with `tools/macos/cross_check.sh`), and Linux
behaviour is unchanged (all notes-* crate tests, HTTP conformance PASS, PyO3
port `rust` and `rust-strict` suites green). Nothing has been executed on macOS
yet; that is what the checklist below is for.

Türkçe özet en altta.

## How the app works on macOS

| concern | Linux (unchanged) | macOS |
|---|---|---|
| engine | pure Rust `notes-engine` binary as a child process (default) or in-process | pure Rust engine **in-process** (Tauri shell links `notes-engine`); no Python, no sidecar |
| library (`--data-dir` default) | `$XDG_STATE_HOME/samsung-notes-linux` (`~/.local/state/...`) | `~/Library/Application Support/org.linuxnotes.desktop/samsung-notes-linux` |
| sign-in state / sessions | `$XDG_STATE_HOME/samsung-notes-research` | `~/Library/Application Support/org.linuxnotes.desktop/samsung-notes-research` |
| explicit `XDG_STATE_HOME` / `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` | honoured when absolute (XDG spec: unset, empty or relative → default) | **also honoured** (this is how you run an isolated test profile) |
| desktop runtime file dir | `$XDG_RUNTIME_DIR/samsung-notes-linux/linux-notes-*` (0700), else `/tmp/linux-notes-*` | per-user `$TMPDIR/linux-notes-*` |
| "Save as file" target | `xdg-user-dir DOWNLOAD`, else `$HOME` | `~/Downloads`, else `$HOME` |
| open URL/file | `xdg-open` | `open` |
| `ms-app://` sign-in callback | XDG desktop entry + `xdg-mime`, handled by a separate `notes-engine __notes-login-callback` process | declared in the bundle (`desktop/src-tauri/Info.plist`, `CFBundleURLTypes`); LaunchServices sends an Apple Event to the running app, Tauri raises `RunEvent::Opened`, the shell calls `notes_engine::deliver_login_callback` (same `notes_login::dispatch`). Nothing is written to user configuration |
| PDF page previews | Poppler `pdftoppm` in **bubblewrap** (namespaces, read-only `/usr`, no network) + rlimits + PDEATHSIG | Homebrew/MacPorts `pdftoppm` under `env -i` + **Seatbelt** (`sandbox-exec`: no network, writes only to the private output dir) + rlimits (no `RLIMIT_AS` enforcement) |
| ink previews | GPL `ink-render` in bubblewrap; resvg in-process | `ink-render` under `env -i` + Seatbelt (no network, no writes); resvg in-process |
| Save as PDF | ImageMagick `magick` from `PATH` | `magick` from `/opt/homebrew/bin`, `/usr/local/bin`, `/opt/local/bin`, then `PATH` (Finder-launched apps get a minimal `PATH`) |
| loopback socket | `SOCK_CLOEXEC` | `fcntl(FD_CLOEXEC)` + `sin_len` |
| TLS trust store | distribution OpenSSL defaults | static OpenSSL defaults **plus** Apple's `/etc/ssl/cert.pem` (unless `SSL_CERT_FILE` is set) |

There is **no data migration**: the first macOS run starts with an empty
library and signs in fresh.

### Reduced preview isolation (read this)

bubblewrap does not exist on macOS. The fallback in
`crates/notes-platform/src/isolation.rs` clears the environment and, when
`/usr/bin/sandbox-exec` works (checked once per process), applies a Seatbelt
profile that denies all network access and every file write except inside the
helper's private output directory. Compared with Linux the helper **can read
any file your user can read**, there are no namespaces, `RLIMIT_AS` is not
enforced by XNU, and there is no parent-death signal (a helper can outlive a
crashed app until its own timeout: 35 s for renderers, 180 s for ink
preparation). `sandbox-exec` is deprecated by Apple but still ships; if it
misbehaves, `NOTES_MACOS_SANDBOX_EXEC=0` disables only the Seatbelt layer.

## Prerequisites

- macOS 13 or 14+ on Apple Silicon (the bundle declares 11.0 as minimum; only
  arm64 is exercised in CI).
- Xcode Command Line Tools: `xcode-select --install`
- Rust via rustup (stable, >= 1.95): <https://rustup.rs>
- Node.js 24 + npm (e.g. `brew install node@24`)
- Python 3 (standard library only, for the conformance harness):
  `brew install python`
- Homebrew packages:

      brew install poppler librsvg imagemagick openssl@3

  `poppler` = PDF previews, `imagemagick` = Save as PDF, `librsvg` = optional
  rasterizer (`SAMSUNG_NOTES_INK_RASTER=rsvg`), `openssl@3` = build-time TLS
  library (linked statically into the bundle). The engine prints one
  `notes-engine: <tool> not found ...; install with: brew install ...` line per
  missing tool on start.
- The GPL renderer source, only for ink previews (`ink-render`):

      tools/fetch_sdocx.sh
      # or by hand:
      git clone https://github.com/twangodev/sdocx.git references/upstream/sdocx
      git -C references/upstream/sdocx checkout --detach 41e9c1099b29a48dc7320501573d7c4858f09303

  `tools/ink-renderer/build.rs` refuses any other commit or a modified checkout.

## Build and run

From the repository root:

    # platform layer + engine crates
    cargo test -p notes-platform -p notes-net -p notes-login -p notes-cloud \
               -p notes-host -p notes-preview -p notes-engine
    cargo build -p notes-engine

    # optional: ink renderer (needs sdocx, see above)
    cargo build --release --manifest-path tools/ink-renderer/Cargo.toml

    # UI
    cd desktop && npm ci && npm run build && npx playwright install chromium webkit
    npx playwright test                                   # same suite as Linux
    npx playwright test -c playwright.webkit.config.ts    # WebKit ~ WKWebView
    cd ..

    # the app bundle (debug, unsigned): desktop/src-tauri/target/debug/bundle/macos/*.app
    rustup target add wasm32-unknown-unknown              # once, for ink-live.wasm
    tools/macos/build_app.sh --with-ink-renderer          # add --release as needed

    # the release disk image: <AppName>-<version>-<arch>.dmg + .sha256
    tools/macos/build_app.sh --release --dmg --with-ink-renderer --version 1.2.3 --out dist-macos

    # quick dev run without a bundle (URL scheme NOT registered this way)
    cargo run --manifest-path desktop/src-tauri/Cargo.toml

`build_app.sh` fetches sdocx and builds the live pen core
(`tools/fetch_sdocx.sh` + `tools/build_ink_wasm.sh` → `dist/ink-live.wasm`),
builds the frontend, replaces the Samsung UI icons with the free Lucide set
from `desktop/assets-free`, bundles `desktop/dist`, `LICENSE`, `NOTICE` (and
`ink-render` + its license with `--with-ink-renderer`), uses
`desktop/src-tauri/icons/icon.icns`, links OpenSSL and SQLite statically, and
verifies the bundle: frontend and a valid `ink-live.wasm` present, `ms-app` in
`Info.plist`, bundle version, no Samsung icon by hash (whole `dist/`), no
Homebrew/MacPorts dylib linked, the notes-engine embedded in the executable
(no sidecar), executable architecture. With `--dmg` it stages the `.app` next
to an `/Applications` link, creates a compressed (UDZO) image with `hdiutil`,
mounts it read-only, repeats every check on the `.app` inside the image, and
writes the `.sha256`. (Tauri's own dmg target is not used: it drives Finder
through AppleScript, which is unreliable on headless CI runners.)

The architecture is the build host's. A universal (arm64 + x86_64) binary is
not built: OpenSSL is linked statically from the host's single-architecture
Homebrew prefix, so `lipo` would need a second OpenSSL build. CI builds arm64
on `macos-14` and x86_64 on the Intel image `macos-15-intel` (optional; it
never blocks the arm64 image).

### Gatekeeper: the dmg is unsigned and not notarized

Releases ship `<AppName>-<version>-arm64.dmg` (and `-x86_64.dmg` when the
Intel runner succeeds) without an Apple signature or notarization ticket.
macOS therefore blocks the first launch ("cannot be opened because the
developer cannot be verified" / "is damaged"). To open it:

1. verify the download against `SHA256SUMS` (or the `.sha256` next to it):
   `shasum -a 256 -c <AppName>-<version>-arm64.dmg.sha256`
2. drag the app to Applications, then either right-click it → **Open** →
   **Open**, or remove the quarantine flag:
   `xattr -dr com.apple.quarantine "/Applications/<AppName>.app"`
   (on recent macOS: System Settings → Privacy & Security → **Open Anyway**).

Signing and notarization later need an Apple Developer Program membership
(Developer ID Application certificate). The disabled stub step in
`.github/workflows/macos.yml` ("Code signing and notarization") documents the
plan and the repository secrets it would read — `APPLE_CERTIFICATE` (base64
.p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`,
`APPLE_TEAM_ID`, and for `xcrun notarytool` an App Store Connect API key:
`APPLE_API_ISSUER`, `APPLE_API_KEY_ID`, `APPLE_API_KEY_P8` (base64). Steps:
temporary keychain import, `codesign --options runtime --timestamp` of the
`.app` (hardened runtime; the embedded engine spawns only external tools, so
no extra entitlements are expected — verify), dmg creation, `notarytool
submit --wait`, `stapler staple`, new `.sha256`. No secret exists today and the
step is `if: false`.

Useful environment variables (same names as on Linux):

| variable | effect |
|---|---|
| `LINUX_NOTES_DATA_DIR` | library directory |
| `XDG_STATE_HOME` | base of library default and sign-in state (isolation) |
| `LINUX_NOTES_OFFLINE=1` | no network at all |
| `LINUX_NOTES_PROJECT_ROOT` | resource root (default: `Notlar.app/Contents/Resources`, else the build checkout) |
| `LINUX_NOTES_ENGINE=native` | use a `notes-engine` binary child instead of in-process (debugging) |
| `NOTES_MACOS_SANDBOX_EXEC=0` | disable the Seatbelt layer of the preview helpers |

## Test checklist for Yusuf

Never use the project owner's account, library or exported data. Everything
below uses a synthetic library or **your own** Samsung account in a throwaway
directory.

Set up an isolated profile once per terminal:

    export NOTES_TEST=~/notes-macos-test
    mkdir -p "$NOTES_TEST" && chmod 700 "$NOTES_TEST"
    export XDG_STATE_HOME="$NOTES_TEST/state" XDG_CONFIG_HOME="$NOTES_TEST/config" XDG_DATA_HOME="$NOTES_TEST/data"
    # after tools/macos/build_app.sh (the product name may change with branding.json):
    export APP=$(ls -d "$PWD"/desktop/src-tauri/target/debug/bundle/macos/*.app | head -1)
    export EXE=$(ls -d "$APP"/Contents/MacOS/* | head -1)

### 1. Build and automated tests

1. `cargo test` command above — all pass? (note any failing test names)
2. `tools/macos/build_app.sh --with-ink-renderer` succeeds and prints `built: ...Notlar.app`.
3. `cd desktop && npx playwright test` (Chromium) and the WebKit config: pass/fail counts.
4. Optional: conformance replay against the frozen golden fixtures (no
   Python package needed; the goldens were recorded on Linux from the
   historical Python reference, which is not part of the public repo):

       python3 crates/notes-engine/conformance/run.py --replay \
           --candidate target/debug/notes-engine --report "$NOTES_TEST/conformance-macos.json"

   Expected on Linux: `... 0 failed ... => PASS`. On macOS some preview
   requests differ because the recording environment had tools macOS lacks
   (`bwrap`, `xdg-user-dir`); send the report.

### 2. Offline engine with a synthetic library (no network)

    cp -R crates/notes-engine/conformance/golden/library/main "$NOTES_TEST/offline-lib"
    chmod -R go-rwx "$NOTES_TEST/offline-lib"
    target/debug/notes-engine --offline --data-dir "$NOTES_TEST/offline-lib" --port 0 \
        --runtime-file "$NOTES_TEST/rt.json" 2> "$NOTES_TEST/engine-stderr.log"

Without `--no-browser` it opens the UI in your default browser.

- [ ] note list appears; folders, favourites, trash render
- [ ] open notes: text note, handwriting note, PDF-backed note — page previews appear
      (PDF pages need Poppler; handwriting needs `ink-render`)
- [ ] thumbnails on cards
- [ ] right-click menu, keyboard shortcuts (they are **Ctrl**-based, not Cmd — known)
- [ ] Save as file: TXT and PDF land in `~/Downloads`
- [ ] draw a stroke and save in the handwriting note (offline: it is queued locally only)
- [ ] stop with Ctrl+C; restart; the drawing is still pending

### 3. The app, offline

    LINUX_NOTES_OFFLINE=1 LINUX_NOTES_DATA_DIR="$NOTES_TEST/offline-lib" "$EXE" 2> "$NOTES_TEST/app-stderr.log"

- [ ] window opens with the UI (no "Uygulama kaynakları bulunamadı" error)
- [ ] the same checks as step 2 inside the app window
- [ ] quitting the app (Cmd+Q) leaves no `notes-engine`/`pdftoppm`/`ink-render` process behind (`ps aux | grep -E 'pdftoppm|ink-render'`)

### 4. Sign in with your own account (isolated directories)

1. Make sure the bundled app is known to LaunchServices: open it once from
   Finder, then check
   `/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -dump | grep -B3 -A3 'ms-app'`
   shows `Notlar.app`. If another app claims `ms-app`, report its name.
2. Start the bundled app with the isolated profile (keep the terminal open):

       LINUX_NOTES_DATA_DIR="$NOTES_TEST/library" "$EXE" 2> "$NOTES_TEST/signin-stderr.log"

3. Click sign in. The browser opens Samsung's page; sign in with **your** account.
4. When the browser asks to open "Notlar", allow it.
5. Expected: the app comes to the front, status becomes connected, sync starts,
   and the browser shows the local "Giriş tamamlandı" page. If the hand-off
   starts a second copy instead of returning to the running one, quit both and
   start it through LaunchServices instead:
   `open -n "$APP" --env XDG_STATE_HOME="$XDG_STATE_HOME" --env LINUX_NOTES_DATA_DIR="$NOTES_TEST/library"`.
6. Read-only first: let the sync finish, open a few of **your** notes, PDF pages.
7. One write: create a new text note named `macOS test`, edit its title once,
   check it on your phone/tablet. Optionally draw one stroke on a note you
   created yourself and check it on the device.
8. Leave the app open for several hours (or overnight) and note whether it is
   still connected afterwards (token renewal has not been observed live yet).

### What to send back

- macOS version, chip, `rustc -V`, `node -v`, `brew list --versions poppler librsvg imagemagick openssl@3`
- the checklist with pass/fail and short notes, failing test names, conformance report JSON
- stderr logs, redacted first:

      sed -E 's/((token|code|state|access_token|refresh_token|userauth_token)=)[^&" ]+/\1REDACTED/g' \
          "$NOTES_TEST/signin-stderr.log" > signin-stderr.redacted.log

- **never send**: anything under `$NOTES_TEST/state` (sessions, credentials),
  `runtime*.json`, `*.sqlite3`, browser URLs containing `code=`/`state=`, or
  screenshots of real notes. Screenshots of the synthetic library are fine.

## Known differences and limitations

- Preview isolation is weaker than on Linux (see above).
- No parent-death signal; helpers end by timeout if the app crashes.
- `RLIMIT_AS` and CPU pinning are not applied on macOS.
- Keyboard shortcuts use Ctrl; macOS users expect Cmd (UI change, not done).
- The URL scheme works only from the bundled `.app` (not `cargo run`); the
  callback URL passes through `url::Url` before dispatch (normalisation of
  unusual characters is a theoretical risk — report a failed sign-in).
- Unsigned and not notarized: Gatekeeper prompts; quarantine must be cleared.
- The canvas-cache (SPI) decoder is unavailable on every platform since
  2026-10-27 (it was Python logic; no Rust port yet).
- UI reference fonts (`<state>/samsung-notes-research/ui-reference/fonts`) are
  absent; the UI uses system fonts.
- There is no Python engine any more (2026-10-27). The historical Python
  reference (not part of the public repo) is not used on any platform; the
  `rs-*` PyO3 bindings build only with their default-off `python` feature.
- Samsung UI icons (`desktop/public/windows-assets`) have no redistribution
  rights; bundles use `desktop/assets-free` (Lucide, ISC). Do not share a
  bundle built with `--keep-private-icons`.
- GPL-3.0: a bundle that contains `ink-render` must ship its license and
  Corresponding Source offer (`licenses/` in the bundle, root `NOTICE`).
- If the bundle identifier changes (see `branding.json`), the data folder name
  `org.linuxnotes.desktop` stays (`notes_platform::dirs::MACOS_APP_SUPPORT`).

## Where the platform code lives

Platform layer: `crates/notes-platform` (std + libc only), one module per concern:

| module | functions | macOS-specific code |
|---|---|---|
| `dirs` | `state_home`, `data_home`, `config_home`, `cache_home`, `runtime_dir`, `library_dir`, `research_dir`, `downloads_dir`, `macos_app_support` | `default_base`, `downloads_dir` |
| `open` | `OPENER`, `command()` | `OPENER = "open"` |
| `process` | `detach_session`, `set_parent_death_signal`, `set_limit(Limit)`, `pin_lowest_cpu`, `capture` | PDEATHSIG → `Ok(false)`, best-effort `RLIMIT_AS`, no pinning |
| `tools` | `Tool`, `locate`, `program`, `available`, `install_hint`, `with_install_hint`, `missing_report`, `extra_ca_file` | Homebrew/MacPorts search, no bwrap, `/etc/ssl/cert.pem` |
| `isolation` | `prefix`, `macos_prefix`, `seatbelt_profile`, `seatbelt_usable` | Seatbelt self-test |
| `net` | `bind_tcp_v4` | `fcntl(FD_CLOEXEC)`, `sin_len` |
| `url_scheme` | `SCHEME`, `REGISTRATION` | `AppBundle` |

`#[cfg(target_os = "macos")]` (or `target_vendor = "apple"`) outside that crate:

- `crates/rs-render/src/sandbox.rs`: `pdf_available`, `pdf_args`, `ink_sandbox_available`, `ink_args`
- `desktop/src-tauri/build.rs`: `embedded_engine` cfg (always on for macOS)
- `desktop/src-tauri/Cargo.toml`: macOS target dependency on `notes-engine` (`bundled-sqlite`)
- `desktop/src-tauri/src/main.rs`: `mode()` default `Embedded`, `project_root()` prefers `Contents/Resources`, `RunEvent::Opened` → `deliver_login_callback`
- `desktop/src-tauri/Info.plist`, `desktop/src-tauri/tauri.macos.conf.json` (read only for macOS targets)
- `crates/notes-login/tests/{oracle_notes_login,differential}.rs`: XDG handler cases gated off, one macOS case added
- `tools/rust_build.py`: `sys.platform == 'darwin'` branch

Call sites routed through the platform layer (same Linux result as before):
`rs-engine` (`server::research_dir`, `bind_ipv4`, launcher `--data-dir`
default, `engine_core::download_folder`, `images_to_pdf`), `notes-engine`
(`server::bind_ipv4`, export ImageMagick check/message, launcher browser
open + missing-tool report, `login_impl::show_login_done`,
`deliver_login_callback`), `notes-login` (`Layout::from_env`,
`flow::open_detached`, `install_handler`), `notes-net` (`tls::context_builder` extra CA file), `rs-render` (`ink_render`,
`pdf_background`, both workers) and `notes-preview` (`ink` rsvg path).

Tooling: `tools/macos/cross_check.sh` (Linux → darwin compile check),
`tools/macos/build_app.sh` (bundle), `.github/workflows/macos.yml` (macOS jobs
+ Linux parity job), `desktop/playwright.webkit.config.ts`,
`tools/export_for_collaborator.sh` (private source export).

## For a later non-Unix (Windows) port

No Windows code exists. The platform modules above are the place to add it;
these Unix-only assumptions remain in callers and would need abstraction:

- private modes: `OpenOptionsExt::mode(0o600)`, `DirBuilderExt::mode(0o700)`,
  `PermissionsExt` checks of runtime files (`rs-store`, `rs-engine::engine_core`,
  `notes-login::files`, `notes-net::fetch`, `notes-preview::util`,
  `rs-render` writers, desktop runtime-file validation), and `umask(0o077)`
- `O_NOFOLLOW` opens (same files) and symlink checks
- `flock`: `engine.lock` (`notes-engine::launcher`, `rs-engine::launcher`) and
  the credential lock (`notes-cloud::auth`)
- `setsid`/`pre_exec`, process-group `kill(-pid)`, `getppid` parent checks,
  PDEATHSIG (`rs-render::sandbox`, desktop launcher)
- `SIGTERM`/`SIGINT` handlers (`notes-engine::launcher`)
- raw `libc::socket` loopback binding (`notes_platform::net`)
- `std::os::unix` imports (`OsStrExt`, `FromRawFd`, ...) across crates
- URL-scheme registration (`notes_platform::url_scheme` + `notes-login::handler`)

## Türkçe özet

macOS portu için altyapı hazır, ancak **henüz bir Mac'te çalıştırılmadı**.
Kod `aarch64-apple-darwin` için derleniyor (Linux'tan `tools/macos/cross_check.sh`
ile, testler ve masaüstü kabuğu dahil); Linux davranışı değişmedi (notes-*
testleri, HTTP uygunluk testi PASS, PyO3 `rust`/`rust-strict` yeşil).

- Platform katmanı: `crates/notes-platform` (dizinler, `open`, süreç, araç
  bulma, ön izleme yalıtımı, soket, URL şeması). Linux'ta birebir eski davranış.
- macOS'ta uygulama saf Rust motorunu süreç içinde çalıştırır; veri
  `~/Library/Application Support/org.linuxnotes.desktop` altında; açıkça
  verilen `XDG_*` değişkenleri yalıtılmış test profili için geçerli.
- Giriş: `ms-app` şeması `Info.plist` ile tanımlı; geri çağrı Apple Event ile
  çalışan uygulamaya gelir ve aynı `notes_login::dispatch` koduna verilir.
- Ön izleme: bwrap yok; `env -i` + Seatbelt (ağ yok, yalnız çıktı dizinine
  yazma). Linux'a göre daha zayıf yalıtım (dosya okuma serbest, PDEATHSIG yok).
- Yusuf için: önkoşullar, derleme (`tools/macos/build_app.sh`), sentetik
  kütüphaneyle çevrimdışı test, kendi hesabıyla yalıtılmış dizinde giriş, geri
  gönderilecekler (token'sız loglar) yukarıdaki kontrol listesinde.
- CI: `.github/workflows/macos.yml` (macos-14: cargo test, çevrimdışı duman
  testi, bilgi amaçlı uygunluk, Playwright Chromium/WebKit, imzasız .app) ve
  Linux eşlik işi (aynı testler + darwin derleme kontrolü).
- Dağıtım: `tools/macos/build_app.sh --release --dmg --version X` imzasız,
  notarize edilmemiş `<AppName>-<sürüm>-<mimari>.dmg` + `.sha256` üretir;
  release.yml bunu `macos-dmg` artifact'ı olarak ekler. İlk açılış: sağ tık →
  Aç, ya da `xattr -dr com.apple.quarantine`. İmzalama için Apple Developer ID
  gerekir; CI'da kapalı (`if: false`) belgeli bir adım var.
- Paylaşım: `tools/export_for_collaborator.sh OUT --git` (artifacts, references,
  yedekler, veritabanları, Samsung ikonları/fontları, iç notlar ve canlı Cloud
  probe'ları hariç; Samsung ikonları yerine özgür Lucide seti).
