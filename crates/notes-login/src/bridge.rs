//! Ports of `tools/exchange_account_code.py` (`safe_text`, `prepare`,
//! `load_bound_inputs`) and `tools/notes_auth_bridge.py` (`uat_request`,
//! `authenticate`, `authorize_notes`, `exchange_notes_code`). Same requests,
//! receipts, claim files and private outputs; no automatic retry.
use crate::callback::constant_time_eq;
use crate::error::{Error, Result};
use crate::files::{load, read_limited, save_json};
use crate::http::{perform_once, HttpRequest, Network};
use crate::json::{self, eq_int, eq_str, py_eq, Value};
use crate::runtime::{hex, token_urlsafe, unix_seconds, Runtime};
use crate::urls::urlencode;
use sha2::{Digest, Sha256};
use std::path::Path;

/// `exchange_account_code.safe_text(value, maximum)`
pub fn safe_text(value: Option<&Value>, maximum: usize) -> bool {
    match value {
        Some(Value::Str(s)) => !s.is_empty() && s.len() <= maximum && s.bytes().all(|b| b > 32 && b < 127),
        _ => false,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn is_token_host(host: &str) -> bool {
    // (?:[a-z]{2}-)?auth2\.samsungosp\.com
    let rest = match host.as_bytes() {
        [a, b, b'-', ..] if a.is_ascii_lowercase() && b.is_ascii_lowercase() => &host[3..],
        _ => host,
    };
    rest == "auth2.samsungosp.com" || host == "auth2.samsungosp.com"
}

fn full(s: &str, min: usize, max: usize, allowed: impl Fn(u8) -> bool) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(allowed)
}


/// `d[key]` → KeyError when absent.
fn index<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    match value {
        Value::Object(_) => value.get(key).ok_or_else(|| Error::key(key)),
        _ => Err(Error::type_("object is not subscriptable by key")),
    }
}

/// `str(value)` for the scalar form values urlencode receives.
fn py_str(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Str(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::BigInt(s) => s.clone(),
        Value::Float(f) => json::float_repr(*f).replace("Infinity", "inf").replace("NaN", "nan"),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Null => "None".into(),
        // Python would send repr(); never produced by our own files.
        _ => return Err(Error::type_("unsupported form value type")),
    })
}

const TOKEN_HEADERS: [(&str, &str); 2] = [("Content-Type", "application/x-www-form-urlencoded"), ("Accept", "application/json")];

/// `exchange_account_code.prepare(transaction, profile, decoded, now)`
pub fn prepare(transaction: &Value, profile: &Value, decoded: &Value, now: f64) -> Result<HttpRequest> {
    let timing_ok = match (transaction.get("createdUnixSeconds"), transaction.get("expiresUnixSeconds")) {
        (Some(Value::Int(c)), Some(Value::Int(e))) => e.checked_sub(*c) == Some(600) && (*c as f64) <= now && now < (*e as f64),
        _ => false,
    };
    if !eq_int(transaction.get("schemaVersion"), 3)
        || !eq_str(transaction.get("purpose"), "experimental-signin-preparation")
        || !timing_ok
    {
        return Err(Error::value("invalid or expired transaction"));
    }
    if !eq_int(decoded.get("schemaVersion"), 2) || !eq_str(decoded.get("hostSource"), "decrypted-state-bound-samsung-callback") {
        return Err(Error::value("callback must be decrypted and validated first"));
    }
    let host = match decoded.get("authServerUrl") {
        None => "",
        Some(Value::Str(s)) => s.as_str(),
        Some(_) => return Err(Error::value("unsupported regional token host")),
    };
    if !is_token_host(host) {
        return Err(Error::value("unsupported regional token host"));
    }
    let verifier = match transaction.get("codeVerifier") {
        None => "",
        Some(Value::Str(s)) => s.as_str(),
        Some(_) => return Err(Error::value("invalid PKCE verifier")),
    };
    if !full(verifier, 43, 128, |b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-')) {
        return Err(Error::value("invalid PKCE verifier"));
    }
    let client = match profile.get("clientId") {
        Some(Value::Str(s)) if full(s, 1, 128, |b| b.is_ascii_alphanumeric()) => s.as_str(),
        _ => return Err(Error::value("invalid OAuth client identity")),
    };
    let (code, physical, os_version, model) =
        (decoded.get("code"), profile.get("devicePhysicalAddressText"), profile.get("deviceOSVersion"), profile.get("deviceModelID"));
    if ![code, physical, os_version, model].iter().all(|v| safe_text(*v, 8192)) {
        return Err(Error::value("invalid code or client metadata"));
    }
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    let form = urlencode(&[
        ("grant_type", "authorization_code".into()),
        ("client_id", client.to_string()),
        ("code", s(code)),
        ("code_verifier", verifier.to_string()),
        ("physical_address_text", s(physical)),
    ]);
    let (os_version, model) = (s(os_version), s(model));
    Ok(HttpRequest::new(
        &format!("https://{host}/auth/oauth2/token"),
        Some(form.into_bytes()),
        &[
            TOKEN_HEADERS[0],
            TOKEN_HEADERS[1],
            ("x-osp-clientosversion", os_version.as_str()),
            ("x-osp-clientmodel", model.as_str()),
            ("x-osp-packagename", "SAMSUNGELECTRONICSCO.LTD.SamsungAccount"),
            ("x-osp-packageversion", "1.26.6021.0"),
            ("User-Agent", "SamsungNotesLinuxResearch/0.1"),
        ],
        Some("POST"),
    ))
}

/// `exchange_account_code.load_bound_inputs(directory)` →
/// `(transaction, profile, decoded)`.
pub fn load_bound_inputs(directory: &Path) -> Result<(Value, Value, Value)> {
    let decoded = load(&directory.join("decoded-callback.json"))?;
    if !decoded.is_object() || !eq_int(decoded.get("schemaVersion"), 2) {
        return Err(Error::value("callback binding schema is required"));
    }
    let bindings = match decoded.get("bindings") {
        Some(b @ Value::Object(_)) => b.clone(),
        _ => return Err(Error::value("callback input bindings are required")),
    };
    let mut parsed = Vec::new();
    for (name, field, limit) in
        [("transaction", "transactionSha256", 65536), ("callback", "callbackSha256", 65536), ("profile", "profileSha256", 32768)]
    {
        let raw = read_limited(&directory.join(format!("{name}.json")), limit)?;
        let ok = match bindings.get(field) {
            Some(Value::Str(expected)) => {
                raw.len() <= limit
                    && expected.len() == 64
                    && expected.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    && constant_time_eq(expected.as_bytes(), sha256_hex(&raw).as_bytes())
            }
            _ => false,
        };
        if !ok {
            return Err(Error::value("callback input binding does not match"));
        }
        let value = json::loads(&raw)?;
        if !value.is_object() {
            return Err(Error::value("bound input must be a JSON object"));
        }
        parsed.push(value);
    }
    let profile = parsed.pop().unwrap();
    let _callback = parsed.pop().unwrap();
    let transaction = parsed.pop().unwrap();
    Ok((transaction, profile, decoded))
}

/// `notes_auth_bridge.uat_request(transaction, profile, decoded, now)`
pub fn uat_request(transaction: &Value, profile: &Value, decoded: &Value, now: f64) -> Result<HttpRequest> {
    let base = prepare(transaction, profile, decoded, now)?;
    let username = match decoded.get("loginId") {
        Some(Value::Str(s)) if !s.is_empty() && s.len() <= 8192 && !s.chars().any(|c| (c as u32) < 32 || c as u32 == 127) => s.clone(),
        _ => return Err(Error::value("callback login identity is missing or invalid")),
    };
    // dict(parse_qsl(base.data)) round-trips the encoded form exactly.
    let mut data = base.data.clone().unwrap_or_default();
    data.extend_from_slice(b"&");
    data.extend_from_slice(urlencode(&[("serviceType", "M".into()), ("username", username)]).as_bytes());
    let url = format!("{}/authenticate", base.url.strip_suffix("/token").unwrap_or(&base.url));
    Ok(HttpRequest::new(&url, Some(data), &base.header_pairs(), Some("POST")))
}

fn bindings(decoded: &Value) -> Result<&Value> {
    index(decoded, "bindings")
}

/// `notes_auth_bridge.authenticate(directory)`: browser code → UAT.
pub fn authenticate(net: &dyn Network, rt: &dyn Runtime, directory: &Path) -> Result<bool> {
    let (txn, profile, decoded) = load_bound_inputs(directory)?;
    let request = uat_request(&txn, &profile, &decoded, rt.now())?;
    let (status, body) = perform_once(net, rt, directory, "uat", ".account-code-claim", &request)?;
    let body = match (status, body) {
        (Some(200), Some(body @ Value::Object(_))) => body,
        _ => return Ok(false),
    };
    let expiry = body.get("userauth_token_expires_in").and_then(Value::strict_number);
    if !["userauth_token", "userId"].iter().all(|k| safe_text(body.get(k), 65536))
        || !expiry.is_some_and(|e| 0.0 < e && e <= 366.0 * 86400.0)
    {
        return Ok(false);
    }
    let expires_in = body.get("userauth_token_expires_in").cloned().unwrap();
    save_json(
        &directory.join("uat-token.json"),
        &Value::obj(vec![
            ("schemaVersion", Value::Int(1)),
            ("credentialKind", Value::s("SamsungUserAuth")),
            ("receivedUnixSeconds", Value::Int(unix_seconds(rt))),
            ("expiresIn", expires_in),
            ("clientId", index(&profile, "clientId")?.clone()),
            ("authServerUrl", index(&decoded, "authServerUrl")?.clone()),
            ("response", body),
            ("sourceBindings", bindings(&decoded)?.clone()),
        ]),
    )?;
    Ok(true)
}

fn number(value: &Value, key: &str) -> Result<f64> {
    index(value, key)?.any_number().ok_or_else(|| Error::type_("unsupported operand type"))
}

/// `notes_auth_bridge.authorize_notes(directory)`: UAT → Notes code (own PKCE).
// Negated float comparisons keep Python's NaN behaviour (`not a <= b`).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn authorize_notes(net: &dyn Network, rt: &dyn Runtime, directory: &Path) -> Result<bool> {
    let (txn, profile, decoded) = load_bound_inputs(directory)?;
    let uat = load(&directory.join("uat-token.json"))?;
    let now = rt.now();
    let invalid = || Error::value("invalid or expired UAT");
    if !eq_str(uat.get("credentialKind"), "SamsungUserAuth") {
        return Err(invalid());
    }
    if !uat.get("sourceBindings").is_some_and(|b| py_eq(b, bindings(&decoded).unwrap_or(&Value::Null))) {
        return Err(invalid());
    }
    // `not received <= now < received + expiresIn` (chained, short-circuit).
    let received = number(&uat, "receivedUnixSeconds")?;
    if !(received <= now) {
        return Err(invalid());
    }
    let received_again = number(&uat, "receivedUnixSeconds")?;
    if !(now < received_again + number(&uat, "expiresIn")?) {
        return Err(invalid());
    }
    let base = prepare(&txn, &profile, &decoded, now)?;
    let target = match profile.get("originalClientId") {
        Some(Value::Str(s)) if !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric()) => s.clone(),
        _ => return Err(Error::value("invalid original application identity")),
    };
    let verifier = token_urlsafe(rt, 32);
    use base64::Engine;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let app = Value::obj(vec![
        ("schemaVersion", Value::Int(1)),
        ("clientId", Value::Str(target.clone())),
        ("codeVerifier", Value::Str(verifier)),
        ("codeChallenge", Value::Str(challenge.clone())),
        ("createdUnixSeconds", Value::Int(now.trunc() as i64)),
        ("sourceBindings", bindings(&decoded)?.clone()),
    ]);
    save_json(&directory.join("notes-app-transaction.json"), &app)?;
    let userauth = py_str(index(index(&uat, "response")?, "userauth_token")?)?;
    let physical = py_str(index(&profile, "devicePhysicalAddressText")?)?;
    let scope = match profile.get("scope") {
        Some(v) if v.truthy() => py_str(v)?,
        _ => String::new(),
    };
    let form = urlencode(&[
        ("response_type", "code".into()),
        ("serviceType", "M".into()),
        ("client_id", target.clone()),
        ("userauth_token", userauth),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".into()),
        ("physical_address_text", physical),
        ("scope", scope),
    ]);
    let url = format!("{}/v2/authorize", base.url.strip_suffix("/token").unwrap_or(&base.url));
    let request = HttpRequest::new(&url, Some(form.into_bytes()), &base.header_pairs(), Some("POST"));
    let (status, body) = perform_once(net, rt, directory, "notes-authorize", ".notes-authorize-claim", &request)?;
    let body = match (status, body) {
        (Some(200), Some(body @ Value::Object(_))) => body,
        _ => return Ok(false),
    };
    let accepted = [Value::Null, Value::Bool(false), Value::s("N"), Value::s("false")];
    for flag in ["requireTncAccepted", "privacyAccepted", "requireChildAccountGraduated"] {
        let value = body.get(flag).cloned().unwrap_or(Value::Null);
        if !accepted.iter().any(|a| py_eq(&value, a)) {
            // "Samsung ek kullanıcı işlemi istiyor; yanıt özel dosyada korundu."
            return Ok(false);
        }
    }
    if !safe_text(body.get("code"), 8192) {
        return Ok(false);
    }
    let app_sha = sha256_hex(&std::fs::read(directory.join("notes-app-transaction.json"))?);
    save_json(
        &directory.join("notes-app-code.json"),
        &Value::obj(vec![
            ("schemaVersion", Value::Int(1)),
            ("credentialKind", Value::s("NotesAuthorizationCode")),
            ("clientId", Value::Str(target)),
            ("receivedUnixSeconds", Value::Int(unix_seconds(rt))),
            ("response", body),
            ("appTransactionSha256", Value::Str(app_sha)),
        ]),
    )?;
    Ok(true)
}

/// `notes_auth_bridge.exchange_notes_code(directory)`: Notes code → Notes
/// account token (verifier from the app transaction).
pub fn exchange_notes_code(net: &dyn Network, rt: &dyn Runtime, directory: &Path) -> Result<bool> {
    let (txn, profile, decoded) = load_bound_inputs(directory)?;
    let app_raw = std::fs::read(directory.join("notes-app-transaction.json"))?;
    if app_raw.len() > 65536 {
        return Err(Error::value("app transaction exceeds limit"));
    }
    let app = json::loads(&app_raw)?;
    let grant = load(&directory.join("notes-app-code.json"))?;
    let now = rt.now();
    if !app.is_object() || !grant.is_object() {
        return Err(Error::type_("unexpected application grant shape"));
    }
    let digest_ok = match grant.get("appTransactionSha256") {
        None => constant_time_eq(b"", sha256_hex(&app_raw).as_bytes()),
        Some(Value::Str(s)) if s.is_ascii() => constant_time_eq(s.as_bytes(), sha256_hex(&app_raw).as_bytes()),
        Some(_) => false,
    };
    let null = Value::Null;
    let binding_ok = !eq_str(grant.get("credentialKind"), "NotesAuthorizationCode")
        || !eq_int(app.get("schemaVersion"), 1)
        || !eq_int(grant.get("schemaVersion"), 1)
        || !py_eq(app.get("sourceBindings").unwrap_or(&null), bindings(&decoded)?)
        || !py_eq(app.get("clientId").unwrap_or(&null), profile.get("originalClientId").unwrap_or(&null))
        || !py_eq(grant.get("clientId").unwrap_or(&null), app.get("clientId").unwrap_or(&null))
        || !digest_ok;
    if binding_ok {
        return Err(Error::value("application grant binding failed"));
    }
    let response = index(&grant, "response")?;
    if !response.is_object() {
        return Err(Error::type_("unexpected grant response shape"));
    }
    let lifetime = match response.get("code_expires_in") {
        Some(Value::Int(l)) if 0 < *l && *l <= 600 => *l as f64,
        _ => return Err(Error::value("application grant expired or has unknown lifetime")),
    };
    let received = number(&grant, "receivedUnixSeconds")?;
    if !(received <= now && now < received + lifetime) {
        return Err(Error::value("application grant expired or has unknown lifetime"));
    }
    let mut token_txn = txn.clone();
    token_txn.set("codeVerifier", index(&app, "codeVerifier")?.clone());
    let mut token_profile = profile.clone();
    token_profile.set("clientId", index(&app, "clientId")?.clone());
    let mut token_decoded = decoded.clone();
    token_decoded.set("code", index(response, "code")?.clone());
    let request = prepare(&token_txn, &token_profile, &token_decoded, now)?;
    let (status, body) = perform_once(net, rt, directory, "notes-token", ".notes-code-claim", &request)?;
    let body = match (status, body) {
        (Some(200), Some(body @ Value::Object(_))) => body,
        _ => return Ok(false),
    };
    if !["access_token", "userId"].iter().all(|k| safe_text(body.get(k), 65536)) {
        return Ok(false);
    }
    let uat = load(&directory.join("uat-token.json"))?;
    if !py_eq(index(&body, "userId")?, index(index(&uat, "response")?, "userId")?) {
        return Err(Error::value("token account does not match authenticated account"));
    }
    save_json(
        &directory.join("notes-account-token.json"),
        &Value::obj(vec![
            ("schemaVersion", Value::Int(1)),
            ("credentialKind", Value::s("NotesServiceOAuth")),
            ("clientId", index(&app, "clientId")?.clone()),
            ("receivedUnixSeconds", Value::Int(unix_seconds(rt))),
            ("authServerUrl", index(&decoded, "authServerUrl")?.clone()),
            ("response", body),
            ("cloudEligibilityVerified", Value::Bool(false)),
        ]),
    )?;
    Ok(true)
}
