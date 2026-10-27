//! Port of `tools/capture_oauth_callback.py`: `validate` and `capture`.
//! Pure validation plus a single-use `.callback-claim` (O_EXCL, 0600) taken
//! only after state/destination/expiry validation; `callback.json` is written
//! with `json.dump` default separators.
use crate::error::{Error, Result};
use crate::files::{private_write, read_limited};
use crate::json::{self, eq_int, eq_str, Value};
use crate::urls::{parse_qsl_strict, urlsplit, SplitResult, UrlError};
use std::path::Path;

pub const MAX_CALLBACK_BYTES: usize = 64 * 1024;

/// `state_verifier(state, transaction) -> bool` (errors propagate).
pub type StateVerifier<'a> = &'a dyn Fn(&str, &Value) -> Result<bool>;

fn cb(message: &str) -> Error {
    Error::callback(message)
}

fn destination_mismatch(expected: &SplitResult, received: &SplitResult) -> std::result::Result<bool, UrlError> {
    // Same evaluation order as the Python `or` chain (port may raise).
    Ok(expected.scheme != "ms-app"
        || expected.hostname().is_none()
        || expected.username().is_some()
        || expected.password().is_some()
        || expected.port()?.is_some()
        || !expected.query.is_empty()
        || !expected.fragment.is_empty()
        || received.username().is_some()
        || received.password().is_some()
        || received.port()?.is_some()
        || !received.fragment.is_empty()
        || (received.scheme.as_str(), received.hostname(), received.path.as_str())
            != (expected.scheme.as_str(), expected.hostname(), expected.path.as_str()))
}

/// `capture_oauth_callback.validate(transaction, callback, now, state_verifier)`
pub fn validate(transaction: &Value, callback: &str, now: f64, state_verifier: Option<StateVerifier<'_>>) -> Result<Value> {
    let schema = transaction.get("schemaVersion");
    if !(eq_int(schema, 2) || eq_int(schema, 3)) || !eq_str(transaction.get("purpose"), "experimental-signin-preparation") {
        return Err(cb("unsupported transaction; prepare a new v2 transaction"));
    }
    let created = transaction.get("createdUnixSeconds");
    let expires = transaction.get("expiresUnixSeconds");
    let timing_ok = match (created, expires) {
        (Some(Value::Int(c)), Some(Value::Int(e))) => {
            e.checked_sub(*c) == Some(600) && (*c as f64) <= now && now < (*e as f64)
        }
        _ => false,
    };
    if !timing_ok {
        return Err(cb("transaction expired or has invalid timing"));
    }
    let expected_state = match transaction.get("state") {
        Some(Value::Str(s)) if s.chars().count() == 43 && s.is_ascii() => s.clone(),
        _ => return Err(cb("invalid transaction binding")),
    };
    let Some(Value::Str(expected_callback)) = transaction.get("expectedCallback") else {
        return Err(cb("invalid transaction binding"));
    };
    if callback.len() > MAX_CALLBACK_BYTES || callback.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
        return Err(cb("invalid callback URI"));
    }
    let parsed = (|| -> std::result::Result<std::result::Result<Vec<(String, String)>, ()>, UrlError> {
        let expected = urlsplit(expected_callback)?;
        let received = urlsplit(callback)?;
        if destination_mismatch(&expected, &received)? {
            return Ok(Err(()));
        }
        Ok(Ok(parse_qsl_strict(&received.query, 32)?))
    })();
    let pairs = match parsed {
        Err(UrlError) => return Err(cb("invalid callback URI")),
        Ok(Err(())) => return Err(cb("callback destination does not match transaction")),
        Ok(Ok(pairs)) => pairs,
    };
    let mut fields: Vec<(String, Value)> = Vec::new();
    for (key, value) in pairs {
        if fields.iter().any(|(k, _)| *k == key) || value.chars().count() > 8192 {
            return Err(cb("ambiguous or oversized callback parameters"));
        }
        fields.push((key, Value::Str(value)));
    }
    let field = |name: &str| -> Option<&str> { fields.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_str()) };
    let state = field("state").unwrap_or("");
    let state_matches = match state_verifier {
        Some(verifier) => verifier(state, transaction)?,
        None => state.is_ascii() && constant_time_eq(state.as_bytes(), expected_state.as_bytes()),
    };
    if !state_matches {
        return Err(cb("callback state does not match transaction"));
    }
    let outcome = if let Some(error) = field("error") {
        if field("code").is_some() || error.is_empty() {
            return Err(cb("invalid authorization error response"));
        }
        "authorization_error"
    } else {
        let code = field("code").unwrap_or("");
        if code.is_empty() || !code.is_ascii() || code.chars().any(|c| (c as u32) <= 32 || c as u32 == 127) {
            return Err(cb("authorization code missing or malformed"));
        }
        "authorization_code_captured"
    };
    Ok(Value::obj(vec![
        ("schemaVersion", Value::Int(1)),
        ("outcome", Value::s(outcome)),
        ("capturedUnixSeconds", Value::Int(now.trunc() as i64)),
        ("parameters", Value::Object(fields)),
        ("tokenExchangePerformed", Value::Bool(false)),
        ("stateValidation", Value::s(if state_verifier.is_some() { "encrypted-full-state" } else { "literal-full-state" })),
    ]))
}

/// `hmac.compare_digest` for ASCII strings.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `capture_oauth_callback.capture(directory, callback, now, state_verifier)`
pub fn capture(directory: &Path, callback: &str, now: f64, state_verifier: Option<StateVerifier<'_>>) -> Result<String> {
    let raw = read_limited(&directory.join("transaction.json"), 64 * 1024)?;
    if raw.len() > 64 * 1024 {
        return Err(cb("transaction file exceeds size limit"));
    }
    let transaction = json::loads(&raw).map_err(|_| cb("invalid transaction file"))?;
    if !transaction.is_object() {
        return Err(cb("invalid transaction file"));
    }
    let result = validate(&transaction, callback, now, state_verifier)?;
    match private_write(&directory.join(".callback-claim"), b"claimed\n") {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(cb("transaction callback was already claimed"));
        }
        Err(e) => return Err(e.into()),
    }
    private_write(&directory.join("callback.json"), json::dumps(&result, &json::DEFAULT).as_bytes())?;
    Ok(result.get("outcome").and_then(Value::as_str).unwrap_or_default().to_string())
}
