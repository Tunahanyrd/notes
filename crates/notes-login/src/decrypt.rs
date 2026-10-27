//! In-process ports of `notes-cli prepare-signin`, `decrypt-callback` and
//! `verify-callback-state` (crates/notes-cli/src/main.rs), built on the same
//! notes-protocol functions and producing the same bytes (serde_json compact,
//! sorted keys — the workspace has no `preserve_order`).
use crate::error::{Error, Result};
use crate::files::{private_write, read_limited};
use crate::runtime::Runtime;
use notes_protocol::callback::{decrypt_callback, validate_auth_host, verify_callback_state, EncryptedCallback};
use notes_protocol::entrypoint::{EntryPointMetadata, MAX_ENTRYPOINT_BYTES};
use notes_protocol::signin::{PreparedSignIn, MAX_PROFILE_BYTES};
use sha2::{Digest, Sha256};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let raw = read_limited(path, limit).map_err(|_| Error::value("could not read the input file"))?;
    if raw.len() > limit {
        return Err(Error::value("input exceeds the size limit"));
    }
    Ok(raw)
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    private_write(path, data).map_err(|_| Error::value("could not create or write a private output file"))
}

/// `notes-cli prepare-signin ENTRYPOINT PROFILE NEW_OUTPUT_DIR`. The creation
/// time comes from the runtime clock (floor seconds, like `as_secs`).
pub fn prepare_signin(entrypoint_path: &Path, profile_path: &Path, output: &Path, rt: &dyn Runtime) -> Result<()> {
    let metadata = EntryPointMetadata::parse(&read_bounded(entrypoint_path, MAX_ENTRYPOINT_BYTES)?)
        .map_err(|e| Error::value(e.to_string()))?;
    let profile = read_bounded(profile_path, MAX_PROFILE_BYTES)?;
    let prepared = PreparedSignIn::prepare(&metadata, &profile, "tr-TR").map_err(|e| Error::value(e.to_string()))?;
    let now = rt.now();
    if now < 0.0 {
        return Err(Error::value("system clock is invalid"));
    }
    let created = now.floor() as u64;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| Error::value("output directory must be new and its parent must exist"))?;
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
        &serde_json::to_vec(&transaction).map_err(|_| Error::value("could not serialize transaction"))?,
    )?;
    write_private(&output.join("url.txt"), prepared.request_url().as_bytes())?;
    Ok(())
}

/// `notes-cli decrypt-callback SESSION_DIR` → `decoded-callback.json`.
pub fn decrypt_saved_callback(directory: &Path) -> Result<()> {
    let transaction_bytes = read_bounded(&directory.join("transaction.json"), 65536)?;
    let callback_bytes = read_bounded(&directory.join("callback.json"), 65536)?;
    let profile_bytes = read_bounded(&directory.join("profile.json"), MAX_PROFILE_BYTES)?;
    let txn: serde_json::Value = serde_json::from_slice(&transaction_bytes).map_err(|_| Error::value("invalid transaction JSON"))?;
    let captured: serde_json::Value = serde_json::from_slice(&callback_bytes).map_err(|_| Error::value("invalid callback JSON"))?;
    let text = |value: &serde_json::Value, key: &str| -> Result<String> {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::value("required callback field is missing"))
    };
    if txn["schemaVersion"] != 3
        || txn["purpose"] != "experimental-signin-preparation"
        || captured["schemaVersion"] != 1
        || captured["outcome"] != "authorization_code_captured"
    {
        return Err(Error::value("unsupported transaction or callback outcome"));
    }
    let params = &captured["parameters"];
    let state = text(&txn, "state")?;
    let created = txn["createdUnixSeconds"].as_u64().ok_or_else(|| Error::value("invalid creation time"))?;
    let expires = txn["expiresUnixSeconds"].as_u64().ok_or_else(|| Error::value("invalid expiry time"))?;
    let received = captured["capturedUnixSeconds"].as_u64().ok_or_else(|| Error::value("invalid capture time"))?;
    if expires.checked_sub(created) != Some(600) || received < created || received >= expires {
        return Err(Error::value("callback was not captured within the transaction lifetime"));
    }
    let iv = text(&txn, "callbackIvHex")?;
    verify_callback_state(&text(params, "state")?, &state, &iv).map_err(|e| Error::value(e.to_string()))?;
    let code = text(params, "code")?;
    let auth_host = text(params, "auth_server_url")?;
    let api_host = text(params, "api_server_url")?;
    let ret_value = params
        .get("retValue")
        .map(|value| value.as_str().ok_or_else(|| Error::value("invalid encrypted login identifier")))
        .transpose()?;
    let result = decrypt_callback(
        EncryptedCallback { code: &code, auth_server_url: &auth_host, api_server_url: &api_host, ret_value },
        &state,
        &iv,
    )
    .map_err(|e| Error::value(e.to_string()))?;
    validate_auth_host(result.auth_server_url()).map_err(|e| Error::value(e.to_string()))?;
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
        &serde_json::to_vec(&output).map_err(|_| Error::value("could not serialize decoded callback"))?,
    )?;
    Ok(())
}

/// `notes-cli verify-callback-state SESSION_DIR < CIPHERTEXT`
pub fn verify_saved_callback_state(directory: &Path, ciphertext: &str) -> Result<()> {
    let txn: serde_json::Value = serde_json::from_slice(&read_bounded(&directory.join("transaction.json"), 65536)?)
        .map_err(|_| Error::value("invalid transaction JSON"))?;
    if txn["schemaVersion"] != 3 || txn["purpose"] != "experimental-signin-preparation" {
        return Err(Error::value("unsupported transaction"));
    }
    let expected = txn["state"].as_str().ok_or_else(|| Error::value("missing transaction state"))?;
    let iv = txn["callbackIvHex"].as_str().ok_or_else(|| Error::value("missing transaction IV"))?;
    if ciphertext.len() > 32768 {
        return Err(Error::value("encrypted callback state exceeds limit"));
    }
    let value = ciphertext.strip_suffix('\n').unwrap_or(ciphertext);
    verify_callback_state(value, expected, iv).map_err(|e| Error::value(e.to_string()))?;
    Ok(())
}
