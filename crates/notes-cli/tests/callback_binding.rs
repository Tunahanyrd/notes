use sha2::{Digest, Sha256};
use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn decrypted_callback_binds_exact_local_input_snapshots() {
    let directory = std::env::temp_dir().join(format!(
        "notes-callback-binding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    let state = "0123456789abcdef-synthetic-callback-state-123";
    let transaction = serde_json::json!({
        "schemaVersion": 3, "purpose": "experimental-signin-preparation",
        "createdUnixSeconds": 1000, "expiresUnixSeconds": 1600,
        "state": state, "callbackIvHex": "000102030405060708090a0b0c0d0e0f"
    });
    // Independent Java vectors: windows-account-region-v7/CallbackFixture.java.
    let callback = serde_json::json!({
        "schemaVersion": 1, "outcome": "authorization_code_captured", "capturedUnixSeconds": 1001,
        "parameters": {"state": "59b24fcf01976acae963981ba6573d330e7d55755b2cfbacd7577c52b2ff7248492f165b6c7fbda725971ca0ab438db4",
            "code": "e3b4057e6e8b2471e9960714ed86e26de1bf84512a452ee2edc0596f5981d832",
            "auth_server_url": "707ce501d7de5cc505ce32ab09571640366e2c9663d4b7c19ddbb4a15dbbb653",
            "api_server_url": "348a5a3456ad3b25c776ebcf95c87667b3f5d4b126b22f1dd84b42cdf0a0226e",
            "retValue": "0d627626c3db6b0a56de6fe581e18fc7eec2eac59921b28e0e448116fbcf0899"
        }
    });
    for (name, value) in [
        ("transaction", transaction),
        ("callback", callback),
        ("profile", serde_json::json!({"clientId":"synthetic"})),
    ] {
        fs::write(
            directory.join(format!("{name}.json")),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    {
        use std::io::Write;
        use std::process::Stdio;
        let mut verifier = Command::new(env!("CARGO_BIN_EXE_notes-cli"))
            .arg("verify-callback-state")
            .arg(&directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        verifier.stdin.take().unwrap().write_all(b"59b24fcf01976acae963981ba6573d330e7d55755b2cfbacd7577c52b2ff7248492f165b6c7fbda725971ca0ab438db4").unwrap();
        let result = verifier.wait_with_output().unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            "Encrypted callback state verified.\n"
        );
        assert!(!directory.join("decoded-callback.json").exists());
    }
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_notes-cli"))
            .arg("decrypt-callback")
            .arg(&directory)
            .output()
            .unwrap()
    };
    let result = run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("decoded-callback.json")).unwrap())
            .unwrap();
    assert_eq!(output["schemaVersion"], 2);
    assert_eq!(output["code"], "synthetic-code-for-tests-only");
    assert_eq!(output["loginId"], "synthetic-user@example.invalid");
    assert!(!String::from_utf8_lossy(&result.stdout).contains("synthetic-user"));
    for name in ["transaction", "callback", "profile"] {
        assert_eq!(
            output["bindings"][format!("{name}Sha256")],
            format!(
                "{:x}",
                Sha256::digest(fs::read(directory.join(format!("{name}.json"))).unwrap())
            )
        );
    }
    assert!(!String::from_utf8_lossy(&result.stdout).contains("synthetic-code"));
    assert!(!run().status.success()); // no overwrite of consumed decode output
    fs::remove_dir_all(directory).unwrap();
}
