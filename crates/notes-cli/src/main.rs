use notes_protocol::callback::{
    decrypt_callback, validate_auth_host, verify_callback_state, EncryptedCallback,
};
use notes_protocol::entrypoint::{EntryPointMetadata, MAX_ENTRYPOINT_BYTES};
use notes_protocol::signin::{PreparedSignIn, MAX_PROFILE_BYTES};
use notes_protocol::tree::parse_note_tree;
use notes_protocol::{parse_note_changes, MAX_XML_BYTES};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|_| "could not open the input file")?;
    let mut input = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|_| "could not read the input file")?;
    if input.len() > limit {
        return Err("input exceeds the size limit".into());
    }
    Ok(input)
}

fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "could not create a private output file")?;
    file.write_all(data)
        .and_then(|_| file.sync_all())
        .map_err(|_| "could not write the output file".into())
}

fn prepare_signin(
    entrypoint_path: &Path,
    profile_path: &Path,
    output: &Path,
) -> Result<(), String> {
    let metadata = EntryPointMetadata::parse(&read_bounded(entrypoint_path, MAX_ENTRYPOINT_BYTES)?)
        .map_err(|e| e.to_string())?;
    let profile = read_bounded(profile_path, MAX_PROFILE_BYTES)?;
    let prepared =
        PreparedSignIn::prepare(&metadata, &profile, "tr-TR").map_err(|e| e.to_string())?;
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is invalid")?
        .as_secs();
    // Never overwrite an old transaction, and never follow a preexisting output
    // directory symlink. No user account token or cookie is captured here.
    DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| "output directory must be new and its parent must exist")?;
    let transaction = serde_json::json!({
        "schemaVersion": 3, "purpose": "experimental-signin-preparation",
        "createdUnixSeconds": created, "expiresUnixSeconds": created + 600,
        "state": prepared.state(), "codeVerifier": prepared.verifier().expose_secret(),
        "expectedCallback": prepared.expected_callback(),
        "callbackIvHex": prepared.callback_iv_hex(),
        "loginPerformed": false
    });
    write_private(
        &output.join("transaction.json"),
        &serde_json::to_vec(&transaction).map_err(|_| "could not serialize transaction")?,
    )?;
    write_private(&output.join("url.txt"), prepared.request_url().as_bytes())?;
    println!("Sign-in request prepared offline\nPrivate files: url.txt, transaction.json\nLocal expiry: 10 minutes\nLogin performed: false");
    Ok(())
}

fn decrypt_saved_callback(directory: &Path) -> Result<(), String> {
    let transaction_bytes = read_bounded(&directory.join("transaction.json"), 65536)?;
    let callback_bytes = read_bounded(&directory.join("callback.json"), 65536)?;
    let profile_bytes = read_bounded(&directory.join("profile.json"), MAX_PROFILE_BYTES)?;
    let txn: serde_json::Value =
        serde_json::from_slice(&transaction_bytes).map_err(|_| "invalid transaction JSON")?;
    let captured: serde_json::Value =
        serde_json::from_slice(&callback_bytes).map_err(|_| "invalid callback JSON")?;
    let text = |value: &serde_json::Value, key: &str| -> Result<String, String> {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "required callback field is missing".into())
    };
    if txn["schemaVersion"] != 3
        || txn["purpose"] != "experimental-signin-preparation"
        || captured["schemaVersion"] != 1
        || captured["outcome"] != "authorization_code_captured"
    {
        return Err("unsupported transaction or callback outcome".into());
    }
    let params = &captured["parameters"];
    let state = text(&txn, "state")?;
    let created = txn["createdUnixSeconds"]
        .as_u64()
        .ok_or("invalid creation time")?;
    let expires = txn["expiresUnixSeconds"]
        .as_u64()
        .ok_or("invalid expiry time")?;
    let received = captured["capturedUnixSeconds"]
        .as_u64()
        .ok_or("invalid capture time")?;
    if expires.checked_sub(created) != Some(600) || received < created || received >= expires {
        return Err("callback was not captured within the transaction lifetime".into());
    }
    let iv = text(&txn, "callbackIvHex")?;
    verify_callback_state(&text(params, "state")?, &state, &iv).map_err(|e| e.to_string())?;
    let code = text(params, "code")?;
    let auth_host = text(params, "auth_server_url")?;
    let api_host = text(params, "api_server_url")?;
    let ret_value = params
        .get("retValue")
        .map(|value| value.as_str().ok_or("invalid encrypted login identifier"))
        .transpose()?;
    let result = decrypt_callback(
        EncryptedCallback {
            code: &code,
            auth_server_url: &auth_host,
            api_server_url: &api_host,
            ret_value,
        },
        &state,
        &iv,
    )
    .map_err(|e| e.to_string())?;
    validate_auth_host(result.auth_server_url()).map_err(|e| e.to_string())?;
    let output = serde_json::json!({
        "schemaVersion": 2, "code": result.code(), "loginId": result.login_id(),
        "bindings": {
            "transactionSha256": format!("{:x}", Sha256::digest(&transaction_bytes)),
            "callbackSha256": format!("{:x}", Sha256::digest(&callback_bytes)),
            "profileSha256": format!("{:x}", Sha256::digest(&profile_bytes))
        },
        "authServerUrl": result.auth_server_url(), "apiServerUrl": result.api_server_url(),
        "hostSource": "decrypted-state-bound-samsung-callback", "tokenExchangePerformed": false
    });
    write_private(
        &directory.join("decoded-callback.json"),
        &serde_json::to_vec(&output).map_err(|_| "could not serialize decoded callback")?,
    )?;
    println!(
        "Callback decrypted and token host validated; private output saved. No token request sent."
    );
    Ok(())
}

fn verify_saved_callback_state(directory: &Path) -> Result<(), String> {
    let txn: serde_json::Value =
        serde_json::from_slice(&read_bounded(&directory.join("transaction.json"), 65536)?)
            .map_err(|_| "invalid transaction JSON")?;
    if txn["schemaVersion"] != 3 || txn["purpose"] != "experimental-signin-preparation" {
        return Err("unsupported transaction".into());
    }
    let expected = txn["state"].as_str().ok_or("missing transaction state")?;
    let iv = txn["callbackIvHex"]
        .as_str()
        .ok_or("missing transaction IV")?;
    let mut input = Vec::new();
    std::io::stdin()
        .take(32769)
        .read_to_end(&mut input)
        .map_err(|_| "could not read encrypted callback state")?;
    if input.len() > 32768 {
        return Err("encrypted callback state exceeds limit".into());
    }
    let value = std::str::from_utf8(&input).map_err(|_| "invalid encrypted callback state")?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    verify_callback_state(value, expected, iv).map_err(|e| e.to_string())?;
    println!("Encrypted callback state verified.");
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() == 3 && args[0] == "export-changes" {
        let raw = read_bounded(Path::new(&args[1]), MAX_XML_BYTES)?;
        let xml = std::str::from_utf8(&raw).map_err(|_| "change feed is not UTF-8")?;
        let page = parse_note_changes(xml).map_err(|e| e.to_string())?;
        let output = serde_json::json!({"schemaVersion": 1,
            "sourceSha256": format!("{:x}", Sha256::digest(&raw)), "page": page});
        write_private(
            Path::new(&args[2]),
            &serde_json::to_vec(&output).map_err(|_| "could not serialize change feed")?,
        )?;
        println!("Validated change feed saved privately. No durable sync cursor advanced.");
        return Ok(());
    }
    if args.len() == 2 && args[0] == "verify-callback-state" {
        return verify_saved_callback_state(Path::new(&args[1]));
    }
    if args.len() == 2 && args[0] == "decrypt-callback" {
        return decrypt_saved_callback(Path::new(&args[1]));
    }
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("notes-cli inspect-changes FILE.xml\nnotes-cli inspect-tree FILE.xml\nnotes-cli inspect-entrypoint FILE.json\nnotes-cli export-changes FILE.xml NEW_OUTPUT.json\nnotes-cli prepare-signin ENTRYPOINT.json PROFILE.json NEW_OUTPUT_DIR\nnotes-cli verify-callback-state SESSION_DIR < ENCRYPTED_STATE\nnotes-cli decrypt-callback SESSION_DIR\n\nOffline inspection and login cryptography. Numeric summaries only; private fields go to private files.\nHTTP login and controlled synchronization are implemented separately in tools/notes_auth_bridge.py and tools/notes_sync_probe.py.");
        return Ok(());
    }
    if args.len() == 4 && args[0] == "prepare-signin" {
        return prepare_signin(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
        );
    }
    if args.len() != 2
        || (args[0] != "inspect-changes"
            && args[0] != "inspect-tree"
            && args[0] != "inspect-entrypoint")
    {
        return Err(
            "usage: notes-cli <inspect-changes|inspect-tree|inspect-entrypoint> FILE".into(),
        );
    }
    let file = File::open(&args[1]).map_err(|_| "could not open the input file")?;
    if args[0] == "inspect-entrypoint" {
        let mut input = Vec::new();
        file.take((MAX_ENTRYPOINT_BYTES + 1) as u64)
            .read_to_end(&mut input)
            .map_err(|_| "could not read the input file")?;
        let metadata = EntryPointMetadata::parse(&input).map_err(|e| e.to_string())?;
        println!("Entrypoint metadata parsed\nKDF iterations: {}\nPublic key bytes (unvalidated): {}\nChannel present: {}\nLogin performed: false",
            metadata.iterations(), metadata.unvalidated_public_key_bytes().len(), metadata.channel_present());
        return Ok(());
    }
    let mut input = String::new();
    file.take((MAX_XML_BYTES + 1) as u64)
        .read_to_string(&mut input)
        .map_err(|_| "could not read UTF-8 XML")?;
    if args[0] == "inspect-tree" {
        let tree = parse_note_tree(&input).map_err(|e| e.to_string())?;
        let layers: usize = tree.pages.iter().map(|p| p.layers.len()).sum();
        let objects: usize = tree
            .pages
            .iter()
            .flat_map(|p| &p.layers)
            .map(|l| l.objects.len())
            .sum();
        println!("Pages: {}\nLayers: {}\nPage objects: {}\nContent files: {}\nFloating objects: {}\nBody text present: {}",
            tree.pages.len(), layers, objects, tree.content_files.len(), tree.floating_objects.len(), tree.body_text.is_some());
        return Ok(());
    }
    let page = parse_note_changes(&input).map_err(|e| e.to_string())?;
    let deleted = page
        .changes
        .iter()
        .filter(|c| c.is_deleted == Some(true))
        .count();
    let unknown_deletion = page
        .changes
        .iter()
        .filter(|c| c.is_deleted.is_none())
        .count();
    let extensions = page.extensions.len()
        + page
            .changes
            .iter()
            .map(|c| c.extensions.len())
            .sum::<usize>();
    println!("Changes: {}\nDeleted: {}\nDeletion state unspecified: {}\nMore pages: {}\nUnrecognized elements retained: {}",
             page.changes.len(), deleted, unknown_deletion, page.has_next, extensions);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
