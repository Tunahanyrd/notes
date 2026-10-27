//! Offline hypothesis check. Prints equality only, never any callback value.
use openssl::symm::{decrypt, Cipher};
use std::{env, fs};
use zeroize::Zeroizing;
fn main() {
    let path = env::args_os()
        .nth(1)
        .expect("private probe input path required");
    let raw = Zeroizing::new(fs::read(path).expect("private input unavailable"));
    assert!(raw.len() <= 65536);
    let data: serde_json::Value = serde_json::from_slice(&raw).expect("invalid input");
    let hex = |name: &str| {
        let s = data[name].as_str().expect("missing field");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("invalid hex"))
            .collect::<Vec<_>>()
    };
    let state = data["expectedState"].as_str().expect("missing state");
    let iv = hex("iv");
    let ciphertext = hex("receivedState");
    let result = decrypt(
        Cipher::aes_128_cbc(),
        &state.as_bytes()[..16],
        Some(&iv),
        &ciphertext,
    );
    match result {
        Ok(value) => {
            let plaintext = Zeroizing::new(value);
            println!(
                "CBC decrypt succeeded; full original state equality: {}",
                plaintext.as_slice() == state.as_bytes()
            );
        }
        Err(_) => println!("CBC decrypt failed; no state equality established"),
    }
}
