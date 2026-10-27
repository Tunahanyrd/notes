//! Sign-in inputs from the browser and the ms-app handler: the callback URI
//! (urlsplit, strict query parsing, transaction validation), the encrypted
//! callback fields and state, URL helpers of the engine and the Cloud layer.
//! Input: one selector byte, then a UTF-8 string.
#![no_main]
use libfuzzer_sys::fuzz_target;
use notes_login::json::Value;

const STATE: &str = "SyntheticStateSyntheticStateSyntheticState0";
const IV: &str = "000102030405060708090a0b0c0d0e0f";

fn transaction() -> Value {
    Value::obj(vec![
        ("schemaVersion", Value::Int(3)),
        ("purpose", Value::s("experimental-signin-preparation")),
        ("createdUnixSeconds", Value::Int(1_700_000_000)),
        ("expiresUnixSeconds", Value::Int(1_700_000_600)),
        ("state", Value::s(STATE)),
        ("expectedCallback", Value::s("ms-app://s-1-15-2-synthetic/")),
    ])
}

fuzz_target!(|data: &[u8]| {
    let Some((sel, raw)) = notes_fuzz::selector(data) else { return };
    let Ok(text) = std::str::from_utf8(raw) else { return };
    match sel % 6 {
        0 => {
            let _ = notes_login::callback::validate(&transaction(), text, 1_700_000_100.0, None);
        }
        1 => {
            if let Ok(split) = notes_login::urls::urlsplit(text) {
                let _ = split.hostname();
                let _ = split.port();
                let _ = split.username();
                let _ = split.password();
            }
            let _ = notes_login::urls::parse_qsl_strict(text, 32);
            let _ = notes_login::urls::unquote_strict(text);
            let _ = notes_login::urls::unquote_plus_strict(text);
            let _ = notes_login::urls::request_unwrap(text);
        }
        2 => {
            let mut parts = text.splitn(4, '\n');
            let code = parts.next().unwrap_or("");
            let auth = parts.next().unwrap_or("");
            let api = parts.next().unwrap_or("");
            let ret = parts.next();
            let fields = notes_protocol::callback::EncryptedCallback { code, auth_server_url: auth, api_server_url: api, ret_value: ret };
            let _ = notes_protocol::callback::decrypt_callback(fields, STATE, IV);
            let _ = notes_protocol::callback::verify_callback_state(code, STATE, IV);
            let _ = notes_protocol::callback::validate_auth_host(auth);
        }
        3 => {
            if let Ok(split) = rs_engine::urlparse::urlsplit(text) {
                let _ = rs_engine::urlparse::parse_qs(&split.query, 10);
            }
            let _ = rs_engine::urlparse::unquote(text, true);
            let _ = rs_engine::urlparse::unquote_plus(text);
        }
        4 => {
            if let Ok(split) = notes_cloud::util::urlsplit(text) {
                let _ = split.hostname();
                let _ = split.userinfo();
            }
            let _ = rs_render::previews::urlsplit(text);
            let _ = rs_render::previews::allowed_url(text, |h| h.ends_with(".samsungcloud.com"));
        }
        _ => {
            let _ = notes_login::handler::desktop_quote(text);
            let _ = rs_engine::mime::guess_type(text);
        }
    }
});
