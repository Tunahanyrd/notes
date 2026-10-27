//! Small runtime helpers: `threading.Event`, dict access with Python error
//! classes, JSON conversion between the HTTP layer and the store values.
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use notes_host::{Fail, Val};
use rs_engine::pyjson::{JStr, JValue};

pub type R<T> = Result<T, Fail>;

/// `threading.Event`
#[derive(Default)]
pub struct Event {
    flag: Mutex<bool>,
    changed: Condvar,
}

impl Event {
    pub fn new() -> Event {
        Event::default()
    }
    pub fn set(&self) {
        *self.flag.lock().unwrap() = true;
        self.changed.notify_all();
    }
    pub fn clear(&self) {
        *self.flag.lock().unwrap() = false;
    }
    pub fn is_set(&self) -> bool {
        *self.flag.lock().unwrap()
    }
    /// Returns the flag after waiting at most `timeout`.
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut flag = self.flag.lock().unwrap();
        while !*flag {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            flag = self.changed.wait_timeout(flag, deadline - now).unwrap().0;
        }
        *flag
    }
}

fn wide_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied()).map(|r| r.unwrap_or('\u{FFFF}')).collect()
}

fn jstr(s: &JStr) -> String {
    match s {
        JStr::Utf8(s) => s.clone(),
        JStr::Wide(w) => wide_to_string(w),
    }
}

/// HTTP JSON body value -> store value (the conversion a Python `str`/`int`
/// /`float` goes through when it reaches the Rust store; lone surrogates
/// become U+FFFF exactly like rs-store's PyO3 conversion).
pub fn from_json(v: &JValue) -> Val {
    match v {
        JValue::Null => Val::Null,
        JValue::Bool(b) => Val::Bool(*b),
        JValue::Int(text) => match text.parse::<i64>() {
            Ok(i) => Val::Int(i),
            Err(_) => {
                let (neg, digits) = match text.strip_prefix('-') {
                    Some(d) => (true, d),
                    None => (false, text.as_str()),
                };
                let digits = digits.trim_start_matches('0');
                Val::BigInt(format!("{}{}", if neg { "-" } else { "" }, if digits.is_empty() { "0" } else { digits }))
            }
        },
        JValue::Float(text) => Val::Float(text.parse::<f64>().unwrap_or(f64::NAN)),
        JValue::Str(s) => Val::Str(jstr(s)),
        JValue::Array(items) => Val::List(items.iter().map(from_json).collect()),
        JValue::Object(items) => Val::Dict(items.iter().map(|(k, v)| (jstr(k), from_json(v))).collect()),
    }
}

/// `json.dumps(value, ensure_ascii=False, allow_nan=False)` (server.py `Handler.json`).
pub fn dumps(v: &Val) -> R<Vec<u8>> {
    rs_store::val::dumps(v, false, false).map(String::into_bytes).map_err(|e| Fail::value(format!("{:?}", e)))
}

/// `d[key]`
pub fn key<'a>(d: &'a Val, name: &str) -> R<&'a Val> {
    match d {
        Val::Dict(_) => d.get(name).ok_or_else(|| Fail::key(format!("'{}'", name))),
        Val::Null => Err(Fail::type_("'NoneType' object is not subscriptable")),
        other => Err(Fail::type_(format!("'{}' object is not subscriptable", other.type_name()))),
    }
}

/// `d[key]` that must be a str (used for ids and SQL text columns).
pub fn key_str<'a>(d: &'a Val, name: &str) -> R<&'a str> {
    match key(d, name)? {
        Val::Str(s) => Ok(s.as_str()),
        other => Err(Fail::type_(format!("expected str, got {}", other.type_name()))),
    }
}

/// Python truthiness of `d[key]`.
pub fn truthy(d: &Val, name: &str) -> R<bool> {
    Ok(key(d, name)?.truthy())
}

/// `x == 'literal'`
pub fn is_str(v: &Val, s: &str) -> bool {
    matches!(v, Val::Str(x) if x == s)
}

/// `json.loads(text)` with the ValueError a decode failure raises.
pub fn loads(v: &Val) -> R<Val> {
    match v {
        Val::Str(s) => rs_store::val::loads(s).map_err(|e| Fail::value(format!("{:?}", e))),
        Val::Bytes(b) => rs_store::val::loads(&String::from_utf8_lossy(b)).map_err(|e| Fail::value(format!("{:?}", e))),
        other => Err(Fail::type_(format!("the JSON object must be str, bytes or bytearray, not {}", other.type_name()))),
    }
}

/// `store.meta(key, default)`
pub fn meta_or(store: &rs_store::storage::StoreCore, name: &str, default: Val) -> R<Val> {
    Ok(store.meta(name)?.unwrap_or(default))
}
