fn main() {
    // Resource root baked into the binary: packaged builds set
    // LINUX_NOTES_INSTALL_ROOT (e.g. /usr/lib/linux-notes); source builds fall
    // back to the checkout two levels up. Done here so a packaged binary does
    // not embed the build directory.
    println!("cargo:rerun-if-env-changed=LINUX_NOTES_INSTALL_ROOT");
    println!("cargo:rerun-if-env-changed=LINUX_NOTES_DEFAULT_ENGINE");
    // `packaged` feature (distribution bundles): never bake the build
    // checkout. Without an explicit LINUX_NOTES_INSTALL_ROOT (Flatpak/AUR set
    // one) the baked root is a path that never exists, so main.rs falls back
    // to Tauri's resource directory (deb/rpm: /usr/lib/<product>, AppImage:
    // $APPDIR/usr/lib/<product>). The engine default is the pure
    // notes-engine binary next to the executable (same as unpackaged Linux
    // builds); LINUX_NOTES_ENGINE still overrides at runtime.
    let packaged = std::env::var_os("CARGO_FEATURE_PACKAGED").is_some();
    let root = std::env::var("LINUX_NOTES_INSTALL_ROOT").ok().filter(|v| !v.is_empty())
        .unwrap_or_else(|| if packaged { "/nonexistent/notes-resource-dir".to_string() }
                           else { format!("{}/../..", std::env::var("CARGO_MANIFEST_DIR").unwrap()) });
    println!("cargo:rustc-env=LINUX_NOTES_DEFAULT_ROOT={root}");
    if packaged && std::env::var_os("LINUX_NOTES_DEFAULT_ENGINE").is_none() {
        println!("cargo:rustc-env=LINUX_NOTES_DEFAULT_ENGINE=native");
    }
    // `embedded_engine`: the pure engine is linked in-process. Opt-in feature
    // on Linux; always on for macOS (no child process there, see
    // docs/MACOS_PORT.md and the target dependency in Cargo.toml).
    println!("cargo::rustc-check-cfg=cfg(embedded_engine)");
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if macos || std::env::var_os("CARGO_FEATURE_EMBEDDED_ENGINE").is_some() {
        println!("cargo:rustc-cfg=embedded_engine");
    }
    tauri_build::build()
}
