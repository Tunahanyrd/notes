//! Port of tools/test_notes_login.py (PrivateStateTests) plus handler tests.
// macOS skips the XDG desktop-handler cases (see notes_platform::url_scheme).
#![cfg_attr(target_os = "macos", allow(dead_code, unused_imports))]
mod common;
use common::*;
use notes_login::files::{ensure_state, load_json, private_write, save_json};
use notes_login::handler::{desktop_quote, desktop_entry, install_handler, HandlerExec, DESKTOP_NAME};
use notes_login::json::Value;
use std::os::unix::fs::PermissionsExt;

#[test]
fn state_directory_is_private_and_symlink_is_rejected() {
    let tmp = temp_dir("state");
    let state = tmp.join("state");
    ensure_state(&state).unwrap();
    assert_eq!(mode(&state), 0o700);
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ensure_state(&state).is_err());
    let link = tmp.join("linked");
    std::os::unix::fs::symlink(&state, &link).unwrap();
    assert!(ensure_state(&link).is_err());
}

#[test]
fn private_write_never_overwrites_or_follows_existing_symlink() {
    let tmp = temp_dir("pw");
    let file = tmp.join("file");
    private_write(&file, b"synthetic").unwrap();
    assert_eq!(mode(&file), 0o600);
    assert_eq!(private_write(&file, b"changed").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    let link = tmp.join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert_eq!(private_write(&link, b"changed").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&file).unwrap(), b"synthetic");
}

#[test]
fn load_json_is_bounded() {
    let tmp = temp_dir("lj");
    let file = tmp.join("input.json");
    std::fs::write(&file, "{\"value\":\"123456789\"}").unwrap();
    assert!(matches!(load_json(&file, 5), Err(notes_login::Error::Value(_))));
}

#[test]
fn dispatch_rejects_outside_transaction_without_capture() {
    let sb = Sandbox::new("dispatch-outside");
    ensure_state(&sb.layout.state).unwrap();
    let outside = sb.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    save_json(
        &sb.layout.state.join("pending.json"),
        &obj(vec![("transactionDirectory", Value::Str(outside.to_str().unwrap().into()))]),
    )
    .unwrap();
    let rt = FakeRuntime::new(1001.0);
    let err = notes_login::dispatch(&sb.layout, &rt, "ms-app://synthetic/?state=synthetic").unwrap_err();
    assert!(matches!(err, notes_login::Error::Value(_)));
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none(), "nothing written outside");
}

#[test]
fn desktop_exec_rejects_expansion_and_newline() {
    assert_eq!(desktop_quote("/usr/bin/python3").unwrap(), "/usr/bin/python3");
    for value in ["/tmp/%u.py", "/tmp/line\nbreak.py", "/tmp/line\rbreak.py"] {
        assert!(desktop_quote(value).is_err());
    }
    assert_eq!(desktop_quote("/opt/My Apps/notes-engine").unwrap(), "\"/opt/My Apps/notes-engine\"");
    assert_eq!(desktop_quote("/a/$x\"y").unwrap(), "\"/a/\\\\$x\\\\\"y\"");
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn engine_handler_fresh_install_uses_hidden_subcommand() {
    let sb = Sandbox::new("handler-fresh");
    std::fs::create_dir_all(&sb.layout.config_home).unwrap();
    std::fs::write(sb.layout.config_home.join("mimeapps.list"), "[Default Applications]\n").unwrap();
    let rt = FakeRuntime::new(1001.0);
    let exec = HandlerExec::Engine { executable: "/usr/lib/samsung-notes/notes-engine".into() };
    install_handler(&sb.layout, &exec, &rt).unwrap();
    let desktop = sb.layout.data_home.join("applications").join(DESKTOP_NAME);
    let text = std::fs::read_to_string(&desktop).unwrap();
    assert!(text.ends_with("Exec=/usr/lib/samsung-notes/notes-engine __notes-login-callback %u\n"));
    assert_eq!(mode(&desktop), 0o600);
    assert_eq!(
        std::fs::read(sb.layout.state.join("mimeapps-before-install.list")).unwrap(),
        b"[Default Applications]\n"
    );
    let receipt = std::fs::read_to_string(sb.layout.state.join("handler-install.json")).unwrap();
    assert!(receipt.contains("\"previousDefault\": \"\""));
    assert!(sb.log("xdg-mime").contains("default samsung-notes-research-callback.desktop x-scheme-handler/ms-app"));
    assert!(sb.log("desktop-file-validate").contains(DESKTOP_NAME));
    // Second call: receipt present, already the engine line → nothing changes.
    install_handler(&sb.layout, &exec, &rt).unwrap();
    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), text);
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn existing_foreign_handler_is_never_replaced() {
    let sb = Sandbox::new("handler-foreign");
    std::fs::write(sb.home.join(".fake-xdg-mime-default"), "other.desktop\n").unwrap();
    let rt = FakeRuntime::new(1001.0);
    let exec = HandlerExec::Engine { executable: "/usr/bin/notes-engine".into() };
    assert!(install_handler(&sb.layout, &exec, &rt).is_err());
    assert!(!sb.layout.data_home.join("applications").join(DESKTOP_NAME).exists());
    assert!(!sb.layout.state.join("handler-install.json").exists());
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn python_handler_is_switched_to_engine_with_backup_and_receipt() {
    let sb = Sandbox::new("handler-switch");
    let rt = FakeRuntime::new(1001.0);
    let python = HandlerExec::Python { interpreter: "/usr/bin/python3".into(), script: "/repo/tools/notes_login.py".into() };
    install_handler(&sb.layout, &python, &rt).unwrap();
    let desktop = sb.layout.data_home.join("applications").join(DESKTOP_NAME);
    let before = std::fs::read(&desktop).unwrap();
    let receipt_before = std::fs::read(sb.layout.state.join("handler-install.json")).unwrap();
    let engine = HandlerExec::Engine { executable: "/usr/bin/notes-engine".into() };
    install_handler(&sb.layout, &engine, &rt).unwrap();
    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), desktop_entry(&engine).unwrap());
    assert_eq!(mode(&desktop), 0o600);
    assert_eq!(std::fs::read(sb.layout.state.join("handler-install.json")).unwrap(), receipt_before);
    let names: Vec<String> = std::fs::read_dir(&sb.layout.state).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    let backup = names.iter().find(|n| n.starts_with("handler-before-switch-")).expect("backup");
    assert_eq!(std::fs::read(sb.layout.state.join(backup)).unwrap(), before);
    assert!(names.iter().any(|n| n.starts_with("handler-switch-") && n.ends_with(".json")));
    // Idempotent afterwards; staged files never linger.
    install_handler(&sb.layout, &engine, &rt).unwrap();
    let apps: Vec<_> = std::fs::read_dir(sb.layout.data_home.join("applications")).unwrap().collect();
    assert_eq!(apps.len(), 1);
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn edited_python_handler_is_not_switched() {
    let sb = Sandbox::new("handler-edited");
    let rt = FakeRuntime::new(1001.0);
    let python = HandlerExec::Python { interpreter: "/usr/bin/python3".into(), script: "/repo/tools/notes_login.py".into() };
    install_handler(&sb.layout, &python, &rt).unwrap();
    let desktop = sb.layout.data_home.join("applications").join(DESKTOP_NAME);
    let mut text = std::fs::read_to_string(&desktop).unwrap();
    text.push_str("X-User-Edit=true\n");
    std::fs::set_permissions(&desktop, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&desktop, &text).unwrap();
    let engine = HandlerExec::Engine { executable: "/usr/bin/notes-engine".into() };
    assert!(install_handler(&sb.layout, &engine, &rt).is_err());
    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), text);
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn handler_changed_since_installation_is_refused() {
    let sb = Sandbox::new("handler-changed");
    let rt = FakeRuntime::new(1001.0);
    let engine = HandlerExec::Engine { executable: "/usr/bin/notes-engine".into() };
    install_handler(&sb.layout, &engine, &rt).unwrap();
    std::fs::write(sb.home.join(".fake-xdg-mime-default"), "other.desktop\n").unwrap();
    assert!(install_handler(&sb.layout, &engine, &rt).is_err());
}

#[cfg(not(target_os = "macos"))] // XDG desktop-entry handler contract (Linux); macOS: Info.plist
#[test]
fn moved_engine_binary_handler_is_switched_again() {
    let sb = Sandbox::new("handler-moved");
    let rt = FakeRuntime::new(1001.0);
    let old = HandlerExec::Engine { executable: "/opt/old/notes-engine".into() };
    install_handler(&sb.layout, &old, &rt).unwrap();
    let new = HandlerExec::Engine { executable: "/usr/bin/notes-engine".into() };
    install_handler(&sb.layout, &new, &rt).unwrap();
    let desktop = sb.layout.data_home.join("applications").join(DESKTOP_NAME);
    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), desktop_entry(&new).unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_install_handler_touches_no_desktop_configuration() {
    // The ms-app scheme is declared by the .app bundle (Info.plist); sign-in
    // must not run xdg tools or write desktop entries.
    let sb = Sandbox::new("macos-handler");
    let rt = FakeRuntime::new(1_000_000.0);
    let engine = HandlerExec::Engine { executable: "/Applications/Notlar.app/Contents/MacOS/linux-notes-desktop".into() };
    install_handler(&sb.layout, &engine, &rt).unwrap();
    assert!(sb.log("xdg-mime").is_empty());
    assert!(!sb.layout.data_home.join("applications").join(DESKTOP_NAME).exists());
    assert!(!sb.layout.state.join("handler-install.json").exists());
}
