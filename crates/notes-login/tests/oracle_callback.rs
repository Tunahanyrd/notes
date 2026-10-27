//! Port of tools/test_capture_oauth_callback.py.
mod common;
use common::*;
use notes_login::callback::{capture, validate};
use notes_login::error::Error;
use notes_login::json::{self, Value};
use notes_login::urls::urlencode;

const STATE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const CALLBACK: &str = "ms-app://s-1-15-2-123/";

fn transaction() -> Value {
    obj(vec![
        ("schemaVersion", Value::Int(2)),
        ("purpose", Value::s("experimental-signin-preparation")),
        ("createdUnixSeconds", Value::Int(1000)),
        ("expiresUnixSeconds", Value::Int(1600)),
        ("state", Value::s(STATE)),
        ("expectedCallback", Value::s(CALLBACK)),
    ])
}

fn response(changes: &[(&str, &str)]) -> String {
    let mut fields: Vec<(&str, String)> = vec![("code", "SYNTHETIC-only-code".into()), ("state", STATE.into())];
    for (k, v) in changes {
        match fields.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v.to_string(),
            None => fields.push((k, v.to_string())),
        }
    }
    format!("{CALLBACK}?{}", urlencode(&fields))
}

fn is_callback_error<T: std::fmt::Debug>(r: Result<T, Error>) -> bool {
    matches!(r, Err(Error::Callback(_)))
}

#[test]
fn valid_bound_response_and_private_single_use_storage() {
    let dir = temp_dir("cb-valid");
    std::fs::write(dir.join("transaction.json"), json::dumps(&transaction(), &json::DEFAULT)).unwrap();
    assert_eq!(capture(&dir, &response(&[]), 1001.0, None).unwrap(), "authorization_code_captured");
    let captured = json::loads(&std::fs::read(dir.join("callback.json")).unwrap()).unwrap();
    assert_eq!(captured.get("parameters").unwrap().get("code").unwrap().as_str(), Some("SYNTHETIC-only-code"));
    assert!(matches!(captured.get("tokenExchangePerformed"), Some(Value::Bool(false))));
    assert_eq!(mode(&dir.join("callback.json")), 0o600);
    assert!(is_callback_error(capture(&dir, &response(&[]), 1002.0, None)));
}

#[test]
fn invalid_state_does_not_consume_transaction() {
    let dir = temp_dir("cb-state");
    std::fs::write(dir.join("transaction.json"), json::dumps(&transaction(), &json::DEFAULT)).unwrap();
    let wrong = "B".repeat(43);
    assert!(is_callback_error(capture(&dir, &response(&[("state", &wrong)]), 1001.0, None)));
    assert!(!dir.join(".callback-claim").exists());
}

#[test]
fn wrong_destination_userinfo_fragment_and_duplicate_state() {
    for uri in [
        response(&[]).replace("s-1-15-2-123", "s-1-15-2-999"),
        response(&[]).replace("ms-app:", "https:"),
        response(&[]).replace("s-1-", "user@s-1-"),
        response(&[]) + "#code=other",
        response(&[]) + "&state=" + STATE,
        response(&[]).replace("/?", "/wrong?"),
    ] {
        assert!(is_callback_error(validate(&transaction(), &uri, 1001.0, None)), "{}", &uri[..20]);
    }
}

#[test]
fn expired_future_and_v1_transactions_are_rejected() {
    for now in [999.0, 1600.0, 9000.0] {
        assert!(is_callback_error(validate(&transaction(), &response(&[]), now, None)));
    }
    let mut old = transaction();
    old.set("schemaVersion", Value::Int(1));
    assert!(is_callback_error(validate(&old, &response(&[]), 1001.0, None)));
}

#[test]
fn missing_code_invalid_encoding_and_controls() {
    for uri in [
        response(&[("code", "")]),
        response(&[("code", "x\nsecret")]),
        response(&[("code", "é")]),
        response(&[]) + "\r",
        response(&[]) + "&other=%ff",
    ] {
        assert!(is_callback_error(validate(&transaction(), &uri, 1001.0, None)));
    }
}

#[test]
fn error_response_is_bound_and_cannot_also_contain_code() {
    let uri = format!("{CALLBACK}?{}", urlencode(&[("error", "access_denied".into()), ("state", STATE.into())]));
    let result = validate(&transaction(), &uri, 1001.0, None).unwrap();
    assert_eq!(result.get("outcome").unwrap().as_str(), Some("authorization_error"));
    assert!(is_callback_error(validate(&transaction(), &response(&[("error", "access_denied")]), 1001.0, None)));
}

#[test]
fn callback_json_uses_default_json_dump_separators() {
    let dir = temp_dir("cb-bytes");
    std::fs::write(dir.join("transaction.json"), json::dumps(&transaction(), &json::DEFAULT)).unwrap();
    capture(&dir, &response(&[]), 1001.75, None).unwrap();
    let text = std::fs::read_to_string(dir.join("callback.json")).unwrap();
    assert_eq!(
        text,
        format!(
            "{{\"schemaVersion\": 1, \"outcome\": \"authorization_code_captured\", \"capturedUnixSeconds\": 1001, \
\"parameters\": {{\"code\": \"SYNTHETIC-only-code\", \"state\": \"{STATE}\"}}, \"tokenExchangePerformed\": false, \
\"stateValidation\": \"literal-full-state\"}}"
        )
    );
}
