//! Deterministic mutation fuzzing of the sign-in callback surface: the
//! `ms-app://` URI any web page can hand to the scheme handler, its query
//! parsing and the encrypted callback fields. Never a panic.
#[path = "../../notes-host/tests/support/fuzzkit.rs"]
mod fuzzkit;

use notes_login::callback::validate;
use notes_login::json::{self, Value};
use notes_login::urls;
use notes_protocol::callback::{decrypt_callback, verify_callback_state, EncryptedCallback};

const STATE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const IV: &str = "000102030405060708090a0b0c0d0e0f";

fn transaction() -> Value {
    Value::obj(vec![
        ("schemaVersion", Value::Int(2)),
        ("purpose", Value::s("experimental-signin-preparation")),
        ("createdUnixSeconds", Value::Int(1000)),
        ("expiresUnixSeconds", Value::Int(1600)),
        ("state", Value::s(STATE)),
        ("expectedCallback", Value::s("ms-app://s-1-15-2-123/")),
    ])
}

fn seeds() -> Vec<(Vec<u8>, String)> {
    [
        "ms-app://s-1-15-2-123/?code=SYNTHETIC-only-code&state=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "ms-app://s-1-15-2-123/?error=access_denied&state=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "ms-app://s-1-15-2-123/?code=00112233445566778899aabbccddeeff&auth_server_url=00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff&api_server_url=ffeeddccbbaa99887766554433221100&state=0f1e2d3c4b5a69788796a5b4c3d2e1f0&retValue=%41%42",
        "ms-app://user:pw@s-1-15-2-123:99/x?a=1&a=2#frag",
        "http://[::1]:80/?q=%zz&r=%E2%82&s=+",
        "MS-APP://S-1-15-2-123/?code=x;state=y",
    ]
    .iter()
    .map(|s| (s.as_bytes().to_vec(), String::new()))
    .collect()
}

fn target(raw: &[u8], _: &str) {
    let text = String::from_utf8_lossy(raw);
    let _ = validate(&transaction(), &text, 1001.0, None);
    if let Ok(split) = urls::urlsplit(&text) {
        let _ = split.port();
        let _ = split.hostname();
        let _ = split.password();
        let _ = urls::parse_qsl_strict(&split.query, 32);
    }
    let _ = urls::unquote_strict(&text);
    let _ = urls::unquote_plus_strict(&text);
    let _ = json::loads(raw);
    // Field values as the browser would put them in the callback.
    let parts: Vec<&str> = text.split(['&', '=', '?']).collect();
    let pick = |i: usize| parts.get(i % parts.len().max(1)).copied().unwrap_or("");
    let fields = EncryptedCallback { code: pick(1), auth_server_url: pick(3), api_server_url: pick(5), ret_value: Some(pick(7)) };
    let _ = decrypt_callback(fields, pick(9), IV);
    let _ = decrypt_callback(
        EncryptedCallback { code: pick(1), auth_server_url: pick(2), api_server_url: pick(3), ret_value: None },
        STATE,
        pick(4),
    );
    let _ = verify_callback_state(pick(1), STATE, IV);
    let _ = verify_callback_state(pick(2), pick(3), pick(4));
}

#[test]
fn hostile_callbacks_never_panic() {
    let failures = fuzzkit::run("callback", 0x5851_f42d_4c95_7f2d, 3000, seeds(), target);
    assert!(failures.is_empty(), "panics at: {failures:?}");
}
