//! Ports of `tools/notes_cloud.py` (`endpoint`, `registration_request`,
//! `register`, `accept_registration`) and `SessionManager._register`
//! (one Cloud endpoint discovery GET + one registration POST per login).
use crate::bridge::safe_text;
use crate::error::{Error, Result};
use crate::files::{load, mkdir_private, save_json};
use crate::http::{get_ok, send_once, HttpRequest, Network, SEND_ONCE_MAX_BYTES};
use crate::json::{self, eq_int, eq_str, py_eq, Value};
use crate::runtime::{unix_seconds, Runtime};
use crate::urls::{urlencode, urlsplit};
use std::path::Path;

const ALLOWED_HOSTS: [&str; 2] = ["api.samsungcloud.com", "play.samsungcloud.com"];
pub const DISCOVERY_URL: &str = "https://ers.samsungcloud.com/ers/v1/endpoints";

/// `notes_cloud.endpoint(endpoints, role)`
pub fn endpoint(endpoints: &Value, role: &str) -> Result<String> {
    let Some(Value::Str(value)) = endpoints.get(role) else {
        return Err(Error::value("missing Cloud endpoint"));
    };
    let parsed = urlsplit(value).map_err(|_| Error::value("invalid Cloud endpoint"))?;
    let host = parsed.hostname();
    let ok = parsed.scheme == "https"
        && host.as_deref().is_some_and(|h| ALLOWED_HOSTS.contains(&h))
        && Some(parsed.netloc.as_str()) == host.as_deref()
        && (parsed.path.is_empty() || parsed.path == "/")
        && parsed.query.is_empty()
        && parsed.fragment.is_empty();
    if !ok {
        return Err(Error::value("Cloud endpoint outside observed allowlist"));
    }
    Ok(value.trim_end_matches('/').to_string())
}

fn index<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    match value {
        Value::Object(_) => value.get(key).ok_or_else(|| Error::key(key)),
        _ => Err(Error::type_("object is not subscriptable by key")),
    }
}

/// `notes_cloud.registration_request(account, endpoints, profile, now)`
pub fn registration_request(account: &Value, endpoints: &Value, profile: &Value, now: f64) -> Result<HttpRequest> {
    if !eq_str(account.get("credentialKind"), "NotesServiceOAuth") || !eq_int(account.get("schemaVersion"), 1) {
        return Err(Error::value("Notes-specific Account token required"));
    }
    let response = index(account, "response")?;
    if !response.is_object() || !profile.is_object() {
        return Err(Error::type_("unexpected registration input shape"));
    }
    let (client_id, token, user, cdid) =
        (account.get("clientId"), response.get("access_token"), response.get("userId"), profile.get("clientDeviceId"));
    if ![client_id, token, user, cdid].iter().all(|v| safe_text(*v, 65536)) {
        return Err(Error::value("invalid registration identity"));
    }
    if !py_eq(profile.get("appId").unwrap_or(&Value::Null), client_id.unwrap()) {
        return Err(Error::value("Cloud app identity must match Notes OAuth client"));
    }
    let lifetime = match response.get("access_token_expires_in") {
        Some(v) => Some(v),
        None => response.get("expires_in"),
    };
    let lifetime = lifetime.and_then(Value::strict_number);
    let lifetime_ok = match lifetime {
        Some(l) if l > 0.0 => {
            let received = index(account, "receivedUnixSeconds")?.any_number().ok_or_else(|| Error::type_("unsupported operand type"))?;
            received <= now && now < received + l
        }
        _ => false,
    };
    if !lifetime_ok {
        return Err(Error::value("Notes Account token expired or unknown lifetime"));
    }
    let body = match profile.get("registrationBody") {
        Some(b @ Value::Object(items)) if items.len() == 2 && b.get("app").is_some() && b.get("device").is_some() => b,
        _ => return Err(Error::value("registration body must have app and device objects")),
    };
    let (app, device) = (body.get("app").unwrap(), body.get("device").unwrap());
    let (Value::Object(app_items), Value::Object(device_items)) = (app, device) else {
        return Err(Error::value("invalid registration body"));
    };
    // Values in body order: app then device (dict order of `body.values()`).
    let Value::Object(sections) = body else { unreachable!() };
    let values_ok = sections.iter().all(|(_, section)| match section {
        Value::Object(items) => items.iter().all(|(_, v)| matches!(v, Value::Str(s) if s.chars().count() <= 1024 && !s.chars().any(|c| (c as u32) < 32))),
        _ => false,
    });
    let _ = (app_items, device_items);
    if !values_ok {
        return Err(Error::value("invalid registration metadata"));
    }
    let version = app.get("version");
    if !safe_text(version, 128) || !safe_text(app.get("sdkVersion"), 128) {
        return Err(Error::value("application and SDK versions required"));
    }
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    let (client_id, cdid, token, user, version) = (s(client_id), s(cdid), s(token), s(user), s(version));
    let url = format!(
        "{}/identity/v2/user/register?{}",
        endpoint(endpoints, "play")?,
        urlencode(&[("deviceCurrentTime", ((now * 1000.0).trunc() as i64).to_string())])
    );
    Ok(HttpRequest::new(
        &url,
        Some(json::dumps(body, &json::COMPACT).into_bytes()),
        &[
            ("x-sc-app-id", client_id.as_str()),
            ("x-sc-cdid", cdid.as_str()),
            ("x-sc-access-token", token.as_str()),
            ("x-sc-uid", user.as_str()),
            ("x-sc-app-version", version.as_str()),
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
            ("User-Agent", "SamsungNotesLinuxResearch/0.1"),
        ],
        Some("POST"),
    ))
}

/// `notes_cloud.register(session, endpoints_file, profile_file)`
pub fn register(net: &dyn Network, rt: &dyn Runtime, session: &Path, endpoints_file: &Path, profile_file: &Path) -> Result<bool> {
    let account = load(&session.join("notes-account-token.json"))?;
    let endpoints = load(endpoints_file)?;
    let profile = load(profile_file)?;
    let request = registration_request(&account, &endpoints, &profile, rt.now())?;
    let output = session.join("cloud");
    mkdir_private(&output)?;
    save_json(&output.join("endpoints.json"), &endpoints)?;
    save_json(&output.join("profile.json"), &profile)?;
    let (status, raw) = send_once(net, rt, &output, "register", &request, SEND_ONCE_MAX_BYTES)?;
    match (status, raw) {
        (Some(200), Some(raw)) => accept_registration(rt, &output, &raw),
        _ => Ok(false),
    }
}

/// `notes_cloud.accept_registration(directory, raw)`
pub fn accept_registration(rt: &dyn Runtime, directory: &Path, raw: &[u8]) -> Result<bool> {
    let body = json::loads(raw)?;
    if !body.is_object() || !safe_text(body.get("dvcId"), 4096) {
        return Err(Error::value("registration response has no device identity"));
    }
    let tokens = match body.get("tokens") {
        Some(Value::List(items)) if items.len() <= 16 => items,
        _ => return Err(Error::value("registration token list invalid")),
    };
    let users: Vec<&Value> = tokens.iter().filter(|t| t.is_object() && eq_str(t.get("type"), "user")).collect();
    if users.len() != 1 || !safe_text(users[0].get("accessToken"), 65536) {
        return Err(Error::value("registration has no unique user token"));
    }
    let expiry = users[0].get("expiresIn");
    if !expiry.and_then(Value::strict_number).is_some_and(|e| 0.0 < e && e <= 366.0 * 86400.0) {
        return Err(Error::value("Cloud token expiry invalid"));
    }
    save_json(
        &directory.join("cloud-token.json"),
        &Value::obj(vec![
            ("schemaVersion", Value::Int(1)),
            ("credentialKind", Value::s("CloudUser")),
            ("receivedUnixSeconds", Value::Int(unix_seconds(rt))),
            ("expiresIn", expiry.unwrap().clone()),
            ("dvcId", body.get("dvcId").unwrap().clone()),
            ("accessToken", users[0].get("accessToken").unwrap().clone()),
        ]),
    )?;
    Ok(true)
}

/// `base64(json.dumps({...INITIAL cursor...}, separators=(',', ':')))`
fn initial_cursor() -> String {
    use base64::Engine;
    let cursor = Value::obj(vec![
        ("version", Value::Int(0)),
        ("encryptedGuid", Value::s("")),
        ("requestTimeStamp", Value::Int(0)),
        ("modifiedTime", Value::Int(0)),
        ("appVersion", Value::s("0")),
        ("sdkVersion", Value::s("0")),
        ("purpose", Value::s("INITIAL")),
        ("type", Value::s("NOTE")),
    ]);
    base64::engine::general_purpose::STANDARD.encode(json::dumps(&cursor, &json::COMPACT))
}

/// The `app-cloud-profile.json` value `_register` writes.
pub fn app_cloud_profile(client_device_id: Value) -> Value {
    use base64::Engine;
    Value::obj(vec![
        ("appId", Value::s("tcl0p60gnz")),
        ("clientDeviceId", client_device_id),
        (
            "registrationBody",
            Value::obj(vec![
                ("app", Value::obj(vec![("version", Value::s("4.3.831.0")), ("sdkVersion", Value::s("0.1.0"))])),
                (
                    "device",
                    Value::obj(vec![
                        ("osType", Value::s("win")),
                        ("osVersion", Value::s("1")),
                        ("platformVersion", Value::s("Linux")),
                        ("type", Value::s("pc")),
                        ("countryCode", Value::s("TUR")),
                        ("modelName", Value::s("LinuxResearch")),
                        ("alias", Value::s("Linux Notes")),
                        ("osUserId", Value::s("0")),
                    ]),
                ),
            ]),
        ),
        (
            "notesHeaders",
            Value::obj(vec![
                ("x-sn-sdk-ver", Value::s("5500")),
                ("x-sn-app-ver", Value::s("4.3.831.0")),
                ("x-sn-xml-ver", Value::s("2")),
                ("x-sn-model-name", Value::Str(base64::engine::general_purpose::STANDARD.encode(b"LinuxResearch"))),
                ("x-sc-network", Value::s("wifi")),
                ("x-sc-e2ee-state", Value::s("NONE")),
            ]),
        ),
        ("initialCursor", Value::Str(initial_cursor())),
    ])
}

/// The discovery request `_register` sends.
pub fn discovery_request() -> HttpRequest {
    HttpRequest::new(
        DISCOVERY_URL,
        None,
        &[
            ("x-sc-app-id", "tcl0p60gnz"),
            ("x-sc-device-model", "LinuxResearch"),
            ("x-sc-os-type", "win"),
            ("x-sc-os-version", "1"),
            ("x-sc-device-cc", "TUR"),
            ("Accept", "application/json"),
            ("User-Agent", "SamsungNotesLinux/0.1"),
        ],
        None,
    )
}

/// `SessionManager._register(session, login_profile)` with the
/// `notes_cloud.py register` subprocess performed in-process. Returns whether
/// registration succeeded (the subprocess exit status).
pub fn register_session(net: &dyn Network, rt: &dyn Runtime, session: &Path, login_profile: &Value) -> Result<bool> {
    let raw = get_ok(net, &discovery_request(), 20, 128 * 1024, "discovery failed")?;
    if raw.len() > 128 * 1024 {
        return Err(Error::value("discovery response too large"));
    }
    let endpoints = json::loads(&raw)?;
    let profile = app_cloud_profile(index(login_profile, "devicePhysicalAddressText")?.clone());
    save_json(&session.join("app-endpoints.json"), &endpoints)?;
    save_json(&session.join("app-cloud-profile.json"), &profile)?;
    // `python3 notes_cloud.py register ...`: any exception there is exit 1.
    Ok(register(net, rt, session, &session.join("app-endpoints.json"), &session.join("app-cloud-profile.json")).unwrap_or(false))
}
