use std::process::Command;

#[test]
fn signin_preparation_writes_private_new_files_and_refuses_overwrite() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{SystemTime, UNIX_EPOCH},
    };
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../notes-protocol/tests/fixtures/signin/"
    );
    let output = std::env::temp_dir().join(format!(
        "notes-signin-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_notes-cli"))
            .arg("prepare-signin")
            .arg(format!("{fixture}entrypoint.json"))
            .arg(format!("{fixture}profile.json"))
            .arg(&output)
            .output()
            .unwrap()
    };
    let result = run();
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let url = fs::read_to_string(output.join("url.txt")).unwrap();
    let transaction = fs::read(output.join("transaction.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&transaction).unwrap();
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(!stdout.contains(&url));
    assert!(!stdout.contains(parsed["state"].as_str().unwrap()));
    assert!(!stdout.contains(parsed["codeVerifier"].as_str().unwrap()));
    assert_eq!(parsed["loginPerformed"], false);
    assert_eq!(parsed["schemaVersion"], 3);
    assert_eq!(parsed["callbackIvHex"].as_str().unwrap().len(), 32);
    assert_eq!(parsed["expectedCallback"], "samsungnotes-sa");
    assert_eq!(
        parsed["expiresUnixSeconds"].as_u64().unwrap()
            - parsed["createdUnixSeconds"].as_u64().unwrap(),
        600
    );
    for name in ["url.txt", "transaction.json"] {
        assert_eq!(
            fs::metadata(output.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let second = run();
    assert!(!second.status.success());
    assert_eq!(
        fs::read(output.join("transaction.json")).unwrap(),
        transaction
    );
    assert_eq!(fs::read_to_string(output.join("url.txt")).unwrap(), url);
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn entrypoint_inspection_never_echoes_uri_check_number_or_key() {
    let input = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../notes-protocol/tests/fixtures/entrypoint.json"
    );
    let result = Command::new(env!("CARGO_BIN_EXE_notes-cli"))
        .args(["inspect-entrypoint", input])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    assert_eq!(String::from_utf8(result.stdout).unwrap(),
        "Entrypoint metadata parsed\nKDF iterations: 200\nPublic key bytes (unvalidated): 16\nChannel present: true\nLogin performed: false\n");
}

#[test]
fn reports_counts_without_echoing_note_identifiers_or_cursor() {
    let input = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../notes-protocol/tests/fixtures/changes.xml"
    );
    let result = Command::new(env!("CARGO_BIN_EXE_notes-cli"))
        .args(["inspect-changes", input])
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("Changes: 2"));
    assert!(text.contains("Deleted: 1"));
    assert!(text.contains("More pages: true"));
    assert!(text.contains("Unrecognized elements retained: 2"));
    assert!(!text.contains("synthetic-live-id"));
    assert!(!text.contains("opaque+/cursor="));
}

#[test]
fn unavailable_commands_fail_instead_of_claiming_a_connection() {
    let result = Command::new(env!("CARGO_BIN_EXE_notes-cli"))
        .arg("sync")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}

#[test]
fn tree_inspection_reports_structure_without_echoing_private_fields() {
    let input = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../notes-protocol/tests/fixtures/tree.xml"
    );
    let result = Command::new(env!("CARGO_BIN_EXE_notes-cli"))
        .args(["inspect-tree", input])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    assert_eq!(String::from_utf8(result.stdout).unwrap(),
        "Pages: 1\nLayers: 1\nPage objects: 1\nContent files: 1\nFloating objects: 1\nBody text present: true\n");
}
