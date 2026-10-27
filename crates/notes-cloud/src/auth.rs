//! Offline credential reading (`_expiry`, `_uat_context`, `load_bound_inputs`)
//! and the renewal seam owned by the auth engineer.
use crate::api::CloudApi;
use crate::util::{self, dget, dget2, is_num, load_json, num, num_add, safe_text, string_ok, R};
use notes_host::{Fail, Val};
use rs_store::val::py_eq;
use std::io::Read;
use std::path::Path;

/// cloud_api._expiry(token): `received + lifetime` (int stays int) or 0.
pub fn expiry(token: &Val) -> R<Val> {
    if !matches!(token, Val::Dict(_)) {
        return Ok(Val::Int(0));
    }
    let mut lifetime = token.get("expiresIn").cloned();
    if token.get("credentialKind").map(|k| py_eq(k, &Val::s("NotesServiceOAuth"))).unwrap_or(false) {
        let empty = Val::Dict(vec![]);
        let response = token.get("response").unwrap_or(&empty);
        let fallback = dget(response, "expires_in")?.cloned();
        lifetime = match dget(response, "access_token_expires_in")? {
            Some(v) => Some(v.clone()),
            None => fallback,
        };
    }
    let received = token.get("receivedUnixSeconds");
    let ok = is_num(received)
        && is_num(lifetime.as_ref())
        && num(received.unwrap()).is_finite()
        && num(lifetime.as_ref().unwrap()).is_finite()
        && num(lifetime.as_ref().unwrap()) > 0.0;
    Ok(if ok { num_add(received.unwrap(), lifetime.as_ref().unwrap()) } else { Val::Int(0) })
}

fn reauth() -> Fail {
    Fail::api("reauth_required", "Kalıcı Samsung oturumu geçersiz veya süresi dolmuş; tarayıcıdan tekrar giriş gerekiyor.")
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// exchange_account_code.load_bound_inputs(directory) -> (transaction, profile, decoded)
pub fn load_bound_inputs(directory: &Path) -> R<(Val, Val, Val)> {
    let decoded = load_json(&directory.join("decoded-callback.json"), util::DEFAULT_LIMIT)?;
    if !matches!(decoded, Val::Dict(_)) || !decoded.get("schemaVersion").map(|v| py_eq(v, &Val::Int(2))).unwrap_or(false) {
        return Err(Fail::value("callback binding schema is required"));
    }
    let bindings = match decoded.get("bindings") {
        Some(b @ Val::Dict(_)) => b.clone(),
        _ => return Err(Fail::value("callback input bindings are required")),
    };
    let mut parsed = Vec::new();
    for (name, field, limit) in [("transaction", "transactionSha256", 65536usize), ("callback", "callbackSha256", 65536), ("profile", "profileSha256", 32768)] {
        let f = std::fs::File::open(directory.join(format!("{name}.json")))?;
        let mut raw = Vec::new();
        f.take(limit as u64 + 1).read_to_end(&mut raw)?;
        let ok = raw.len() <= limit
            && matches!(bindings.get(field), Some(Val::Str(e)) if is_hex64(e) && *e == util::sha256_hex(&raw));
        if !ok {
            return Err(Fail::value("callback input binding does not match"));
        }
        let value = crate::json::loads(&raw)?;
        if !matches!(value, Val::Dict(_)) {
            return Err(Fail::value("bound input must be a JSON object"));
        }
        parsed.push(value);
    }
    let profile = parsed.remove(2);
    let transaction = parsed.remove(0);
    Ok((transaction, profile, decoded))
}

/// `re.fullmatch(r'(?:[a-z]{2}-)?auth2\.samsungosp\.com', host)`
pub fn is_auth_host(host: &str) -> bool {
    let rest = match host.as_bytes() {
        [a, b, b'-', ..] if a.is_ascii_lowercase() && b.is_ascii_lowercase() => &host[3..],
        _ => host,
    };
    rest == "auth2.samsungosp.com"
}

/// CloudAPI._uat_context(): (uat, profile, host); any failure is
/// `APIError('reauth_required', ...)`.
pub fn uat_context(api: &CloudApi) -> R<(Val, Val, String)> {
    let inner = || -> R<Option<(Val, Val, String)>> {
        let (_, profile, decoded) = load_bound_inputs(&api.session_path)?;
        let uat = load_json(&api.session_path.join("uat-token.json"), util::DEFAULT_LIMIT)?;
        let original = api.original_account()?;
        let now = api.now();
        let lifetime = dget(&uat, "expiresIn")?;
        let received = dget(&uat, "receivedUnixSeconds")?;
        let host = dget(&uat, "authServerUrl")?;
        let empty = Val::Dict(vec![]);
        let uat_response = dget(&uat, "response")?.unwrap_or(&empty);
        let eq = |a: Option<&Val>, b: Option<&Val>| py_eq(a.unwrap_or(&Val::Null), b.unwrap_or(&Val::Null));
        let bad = !dget(&uat, "schemaVersion")?.map(|v| py_eq(v, &Val::Int(1))).unwrap_or(false)
            || !eq(dget(&uat, "credentialKind")?, Some(&Val::s("SamsungUserAuth")))
            || !is_num(lifetime)
            || !(0.0 < num(lifetime.unwrap()) && num(lifetime.unwrap()) <= 366.0 * 86400.0)
            || !is_num(received)
            || !(num(received.unwrap()) <= now && now < num(&num_add(received.unwrap(), lifetime.unwrap())))
            || !eq(dget(&uat, "sourceBindings")?, dget(&decoded, "bindings")?)
            || !eq(dget(&uat, "clientId")?, dget(&profile, "clientId")?)
            || !eq(dget(&profile, "originalClientId")?, dget(&api.profile, "appId")?)
            || !eq(dget(&profile, "originalClientId")?, dget(&original, "clientId")?)
            || !eq(dget(uat_response, "userId")?, dget2(&original, "response", "userId")?)
            || !eq(host, dget(&decoded, "authServerUrl")?)
            || !eq(host, dget(&original, "authServerUrl")?)
            || !matches!(host, Some(Val::Str(h)) if is_auth_host(h))
            || !eq(dget(&decoded, "hostSource")?, Some(&Val::s("decrypted-state-bound-samsung-callback")))
            || !safe_text(dget(uat_response, "userauth_token")?, 65536)
            || matches!(dget(uat_response, "userauth_token")?, Some(Val::Str(t)) if t == "-1")
            || !["devicePhysicalAddressText", "deviceOSVersion", "deviceModelID"].iter().all(|k| safe_text(profile.get(k), 8192))
            || !matches!(profile.get("scope").filter(|v| v.truthy()).cloned().unwrap_or(Val::s("")), Val::Str(_));
        if bad {
            return Ok(None);
        }
        let host = host.and_then(|h| h.as_str()).unwrap_or_default().to_string();
        Ok(Some((uat.clone(), profile, host)))
    };
    match inner() {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(reauth()),
        Err(e) if e.is_os() || e.is_value() || e.is_key() || e.is_type() || e.kind == notes_host::Kind::Attribute => Err(reauth()),
        Err(e) => Err(e),
    }
}

/// Credential renewal (`renew_cloud_token`, `renew_notes_token`,
/// `_renew_notes_token_locked`, `_credential_lock`, `_replace_credential`,
/// `_auth_request`), owned by the auth port.
///
/// `CloudApi` holds its (reentrant) API lock while calling these; the
/// implementation takes the cross-process `.auth.lock` itself, calls
/// `api.validate_identity()` first like the oracle, may use `api.call()`,
/// `api.directory()`, `api.cloud()`, `api.account()`, `api.identity()`, and
/// returns `api.auth_status()` on success.
pub trait Renewal: Send + Sync {
    /// CloudAPI.renew_cloud_token(): Notes OAuth → new CloudUser token.
    fn renew_cloud_token(&self, api: &CloudApi) -> R<Val>;
    /// CloudAPI.renew_notes_token(operation_id=...): cached UAT → Notes token.
    fn renew_notes_token(&self, api: &CloudApi, operation_id: Option<&str>) -> R<Val>;
}

/// Offline stand-in until the auth port plugs in: runs the oracle's pure
/// pre-checks and then fails exactly like an unrenewable session (no files
/// are written, nothing is sent):
///
/// * Notes token expired → `reauth_required` ("Kalıcı Samsung oturumu …",
///   what `_uat_context` raises) — no UAT grant is attempted;
/// * Notes token unusable → `reauth_required` ("Samsung oturumu yenilenmeli …");
/// * Cloud registration unusable → `registration_required`;
/// * renewal headers invalid → `invalid_profile`;
/// * otherwise the renewal request cannot be made → `renew_failed`
///   ("Cloud oturumu yenilenemedi; yeniden giriş gerekebilir.", the
///   message for a failed/absent HTTP answer).
pub struct StubRenewal;

impl Renewal for StubRenewal {
    fn renew_cloud_token(&self, api: &CloudApi) -> R<Val> {
        api.validate_identity()?;
        let account = api.account()?;
        let cloud = api.cloud()?;
        let mut now = api.now();
        let mut account = account;
        if num(&expiry(&account)?) <= now {
            self.renew_notes_token(api, None)?;
            account = api.account()?;
            now = api.now();
        }
        let empty = Val::Dict(vec![]);
        let response = dget(&account, "response")?.unwrap_or(&empty).clone();
        let received = account.get("receivedUnixSeconds");
        if num(&expiry(&account)?) <= now
            || !string_ok(dget(&response, "access_token")?, 65536)
            || matches!(dget(&response, "access_token")?, Some(Val::Str(t)) if t == "-1")
            || !is_num(received)
            || num(received.unwrap()) > now
        {
            return Err(Fail::api("reauth_required", "Samsung oturumu yenilenmeli; tarayıcıdan tekrar giriş yapın."));
        }
        let token_ok = match cloud.get("accessToken") {
            Some(Val::Str(t)) => {
                let t = t.strip_prefix("Bearer ").unwrap_or(t);
                !t.is_empty() && t != "-1"
            }
            _ => false,
        };
        if !cloud.get("schemaVersion").map(|v| py_eq(v, &Val::Int(1))).unwrap_or(false)
            || !cloud.get("credentialKind").map(|v| py_eq(v, &Val::s("CloudUser"))).unwrap_or(false)
            || !string_ok(cloud.get("accessToken"), 65536)
            || !token_ok
            || !string_ok(cloud.get("dvcId"), 4096)
        {
            return Err(Fail::api("registration_required", "Cloud cihaz kaydı yenilenmeli."));
        }
        let version = dget(&api.profile, "registrationBody")
            .ok()
            .flatten()
            .and_then(|b| b.get("app"))
            .and_then(|a| a.get("version"));
        let values = [api.profile.get("appId"), version, cloud.get("dvcId"), response.get("access_token"), response.get("userId")];
        if !values.iter().all(|v| string_ok(*v, 65536)) {
            return Err(Fail::api("invalid_profile", "Cloud yenileme bilgisi geçersiz."));
        }
        Err(Fail::api("renew_failed", "Cloud oturumu yenilenemedi; yeniden giriş gerekebilir."))
    }

    fn renew_notes_token(&self, api: &CloudApi, _operation_id: Option<&str>) -> R<Val> {
        api.validate_identity()?;
        uat_context(api)?;
        Err(reauth())
    }
}

// ======================================================================
// Ported renewal (CloudAPI.renew_cloud_token / renew_notes_token).
// NEEDS LIVE RENEWAL VALIDATION by the user: verified only against the
// Python oracle and fake transports.
// ======================================================================

use crate::transport::{set_header, Headers, Request};
use crate::util::{check_uuid, ditem, json_once, private_write, save_json, save_once};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

/// `self._clock()` as the JSON value Python writes (`int` clocks stay int).
pub fn clock_val(now: f64) -> Val {
    if now.is_finite() && now.fract() == 0.0 && now.abs() < 9.0e15 {
        Val::Int(now as i64)
    } else {
        Val::Float(now)
    }
}

/// `CloudAPI._credential_lock()`: flock on `work_dir/.auth.lock`.
pub struct CredentialLock {
    file: std::fs::File,
}

impl Drop for CredentialLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: the fd belongs to `self.file`, which is still open here.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn credential_lock(api: &CloudApi) -> R<CredentialLock> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(api.work_dir.join(".auth.lock"))?;
    let meta = file.metadata()?;
    // SAFETY: getuid takes no arguments, has no preconditions and cannot fail.
    if meta.uid() != unsafe { libc::getuid() } || meta.permissions().mode() & 0o077 != 0 {
        return Err(Fail::api("unsafe_storage", "Oturum kilidi özel olmalı."));
    }
    // SAFETY: the fd belongs to `file`, which outlives the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(CredentialLock { file })
}

/// `CloudAPI._replace_credential(filename, value, output)`
pub fn replace_credential(api: &CloudApi, filename: &str, value: &Val, output: &Path) -> R<()> {
    let target = api.work_dir.join(filename);
    if target.exists() {
        let previous = std::fs::read(&target)?;
        if previous == crate::json::receipt_bytes(value) {
            return Ok(());
        }
        save_once(&output.join(format!("previous-{filename}")), &previous)?;
    }
    let temporary = api.work_dir.join(format!(".credential-{}", api.random().token_hex(12)));
    save_json(&temporary, value)?;
    std::fs::rename(&temporary, &target)?;
    std::fs::File::open(&api.work_dir)?.sync_all()?;
    Ok(())
}

/// `CloudAPI._auth_request(suffix, fields, profile, host)`
pub fn auth_request(suffix: &str, fields: &[(&str, String)], profile: &Val, host: &str) -> R<Request> {
    let text = |k: &str| -> R<String> {
        match ditem(profile, k)? {
            Val::Str(s) => Ok(s.clone()),
            other => Err(Fail::type_(format!("header value must be str, not {}", other.type_name()))),
        }
    };
    let mut headers: Headers = Vec::new();
    set_header(&mut headers, "Content-Type", "application/x-www-form-urlencoded");
    set_header(&mut headers, "Accept", "application/json");
    set_header(&mut headers, "x-osp-clientosversion", &text("deviceOSVersion")?);
    set_header(&mut headers, "x-osp-clientmodel", &text("deviceModelID")?);
    set_header(&mut headers, "x-osp-packagename", "SAMSUNGELECTRONICSCO.LTD.SamsungAccount");
    set_header(&mut headers, "x-osp-packageversion", "1.26.6021.0");
    set_header(&mut headers, "User-Agent", "SamsungNotesLinux/0.1");
    Ok(Request::new("POST", format!("https://{host}/auth/oauth2/{suffix}"), &headers, Some(util::urlencode(fields).into_bytes())))
}

fn str_of(v: &Val, what: &str) -> R<String> {
    match v {
        Val::Str(s) => Ok(s.clone()),
        other => Err(Fail::type_(format!("{what}: expected str, got {}", other.type_name()))),
    }
}

/// `CloudAPI._renew_notes_token_locked(operation_id=...)`
pub fn renew_notes_token_locked(api: &CloudApi, operation_id: Option<&str>) -> R<Val> {
    let (uat, profile, host) = uat_context(api)?;
    let identity = api.identity();
    let pending = api.work_dir.join("notes-renewal-current.json");
    let mut operation_id = operation_id.map(str::to_string);
    if operation_id.is_none() && pending.exists() {
        let pointer = load_json(&pending, util::DEFAULT_LIMIT)?;
        if py_eq(dget(&pointer, "accountFingerprint")?.unwrap_or(&Val::Null), &Val::s(&identity)) {
            let pointed = util::check_uuid_val(ditem(&pointer, "operationId")?)?;
            if !api.work_dir.join("notes-renewals").join(&pointed).join("completed.json").exists() {
                operation_id = Some(pointed);
            }
        }
    }
    let operation_id = check_uuid(&match operation_id.filter(|o| !o.is_empty()) {
        Some(o) => o,
        None => api.random().uuid4(),
    })?;
    let parent = api.work_dir.join("notes-renewals");
    util::mkdir_private(&parent, true)?;
    let output = parent.join(&operation_id);
    util::mkdir_private(&output, true)?;
    let meta = std::fs::symlink_metadata(&output)?;
    if meta.file_type().is_symlink() || std::fs::metadata(&output)?.permissions().mode() & 0o077 != 0 {
        return Err(Fail::api("unsafe_storage", "Oturum yenileme kaydı özel olmalı."));
    }
    let app_id = ditem(&api.profile, "appId")?.clone();
    let source_bindings = ditem(&uat, "sourceBindings")?.clone();
    let uat_user = ditem(ditem(&uat, "response")?, "userId")?.clone();
    let binding = Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        ("operationId", Val::s(&operation_id)),
        ("accountFingerprint", Val::s(&identity)),
        ("clientId", app_id.clone()),
        ("uatSha256", Val::s(util::sha256_hex(&std::fs::read(api.session_path.join("uat-token.json"))?))),
        ("sourceBindings", source_bindings.clone()),
        ("authServerUrl", Val::s(&host)),
    ]);
    json_once(&output.join("binding.json"), &binding)?;
    let accepted = output.join("accepted-token.json");
    if accepted.exists() {
        let token = load_json(&accepted, util::DEFAULT_LIMIT)?;
        let eq = |a: Option<&Val>, b: &Val| py_eq(a.unwrap_or(&Val::Null), b);
        if !eq(dget(&token, "accountFingerprint")?, &Val::s(&identity))
            || !eq(dget(&token, "clientId")?, &app_id)
            || !eq(dget2(&token, "response", "userId")?, &uat_user)
            || num(&expiry(&token)?) <= api.now()
        {
            return Err(Fail::api("invalid_response", "Kaydedilmiş oturum yenilemesi geçersiz."));
        }
        // A completed old operation is evidence, not permission to roll a
        // newer credential back. Only recover publication after a crash.
        if !output.join("completed.json").exists() {
            replace_credential(api, "notes-account-token.json", &token, &output)?;
            json_once(&output.join("completed.json"), &Val::dict(vec![("published", Val::Bool(true))]))?;
        }
        return api.auth_status();
    }
    if output.join("authorize-claimed").exists() || output.join("token-claimed").exists() {
        return Err(Fail::api("auth_renewal_uncertain", "Önceki oturum yenilemesi doğrulanamadı; aynı giriş kodu tekrar gönderilmedi."));
    }
    replace_credential(
        api,
        "notes-renewal-current.json",
        &Val::dict(vec![("operationId", Val::s(&operation_id)), ("accountFingerprint", Val::s(&identity))]),
        &output,
    )?;
    let verifier = api.random().token_urlsafe(32);
    let challenge = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(util::sha256(verifier.as_bytes()))
    };
    save_json(&output.join("pkce.json"), &Val::dict(vec![("verifier", Val::s(&verifier)), ("challenge", Val::s(&challenge))]))?;
    let app = str_of(&app_id, "clientId")?;
    let physical = str_of(ditem(&profile, "devicePhysicalAddressText")?, "physical")?;
    let scope = match profile.get("scope").filter(|v| v.truthy()) {
        Some(v) => str_of(v, "scope")?,
        None => String::new(),
    };
    let fields = vec![
        ("response_type", "code".to_string()),
        ("serviceType", "M".to_string()),
        ("client_id", app.clone()),
        ("userauth_token", str_of(ditem(ditem(&uat, "response")?, "userauth_token")?, "uat")?),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".to_string()),
        ("physical_address_text", physical.clone()),
        ("scope", scope),
    ];
    let request = auth_request("v2/authorize", &fields, &profile, &host)?;
    private_write(&output.join("authorize-claimed"), b"one-request-before-transport\n")?;
    let response = api.call(&output, "authorize", &request, 128 * 1024)?;
    if response.status != Some(200) || response.body.is_none() {
        let code = if matches!(response.status, Some(400 | 401 | 403)) { "reauth_required" } else { "auth_renewal_uncertain" };
        return Err(Fail::api(code, "Samsung oturumu yenilenemedi; mevcut notlar korunuyor."));
    }
    let grant = (|| -> R<(String, i64)> {
        let grant = crate::json::loads(response.body.as_deref().unwrap_or_default())?;
        for k in ["requireTncAccepted", "privacyAccepted", "requireChildAccountGraduated"] {
            let v = dget(&grant, k)?.cloned().unwrap_or(Val::Null);
            let allowed = [Val::Null, Val::Bool(false), Val::s("N"), Val::s("false")].iter().any(|a| py_eq(&v, a));
            if !allowed {
                return Err(Fail::api("account_action_required", "Samsung ek kullanıcı işlemi istiyor; tarayıcıdan giriş gerekiyor."));
            }
        }
        let lifetime = dget(&grant, "code_expires_in")?;
        let code = dget(&grant, "code")?;
        match (code, lifetime) {
            (Some(Val::Str(c)), Some(Val::Int(l))) if safe_text(code, 8192) && 0 < *l && *l <= 600 => Ok((c.clone(), *l)),
            _ => Err(Fail::value("grant")),
        }
    })();
    let (code, lifetime) = match grant {
        Ok(g) => g,
        Err(e) if e.is_value() || e.is_type() || e.kind == notes_host::Kind::Attribute => {
            return Err(Fail::api("invalid_response", "Uygulama yetkilendirme yanıtı geçersiz."))
        }
        Err(e) => return Err(e),
    };
    let grant_received = api.now();
    save_json(
        &output.join("grant.json"),
        &Val::dict(vec![
            ("receivedUnixSeconds", clock_val(grant_received)),
            ("expiresIn", Val::Int(lifetime)),
            ("accountFingerprint", Val::s(&identity)),
        ]),
    )?;
    let fields = vec![
        ("grant_type", "authorization_code".to_string()),
        ("client_id", app.clone()),
        ("code", code),
        ("code_verifier", verifier),
        ("physical_address_text", physical),
    ];
    if api.now() >= grant_received + lifetime as f64 {
        return Err(Fail::api("auth_renewal_uncertain", "Yeni yetkilendirme kodunun süresi doldu; kod tekrar kullanılmadı."));
    }
    let request = auth_request("token", &fields, &profile, &host)?;
    private_write(&output.join("token-claimed"), b"one-code-exchange-before-transport\n")?;
    let response = api.call(&output, "token", &request, 128 * 1024)?;
    if response.status != Some(200) || response.body.is_none() {
        return Err(Fail::api("auth_renewal_uncertain", "Oturum tokenı doğrulanamadı; aynı kod tekrar gönderilmedi."));
    }
    let parsed = (|| -> R<Val> {
        let response = crate::json::loads(response.body.as_deref().unwrap_or_default())?;
        let fallback = dget(&response, "expires_in")?.cloned();
        let lifetime = dget(&response, "access_token_expires_in")?.cloned().or(fallback);
        let token = dget(&response, "access_token")?;
        let ok = safe_text(token, 65536)
            && !matches!(token, Some(Val::Str(t)) if t == "-1")
            && py_eq(dget(&response, "userId")?.unwrap_or(&Val::Null), &uat_user)
            && is_num(lifetime.as_ref())
            && 0.0 < num(lifetime.as_ref().unwrap())
            && num(lifetime.as_ref().unwrap()) <= 366.0 * 86400.0;
        if !ok {
            return Err(Fail::value("token"));
        }
        Ok(response)
    })();
    let notes_response = match parsed {
        Ok(r) => r,
        Err(e) if e.is_value() || e.is_type() || e.kind == notes_host::Kind::Attribute => {
            return Err(Fail::api("invalid_response", "Yenilenen Notes oturumu hesapla eşleşmiyor veya geçersiz."))
        }
        Err(e) => return Err(e),
    };
    let renewed = Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        ("credentialKind", Val::s("NotesServiceOAuth")),
        ("clientId", app_id),
        ("authServerUrl", Val::s(&host)),
        ("receivedUnixSeconds", clock_val(api.now())),
        ("response", notes_response),
        ("accountFingerprint", Val::s(&identity)),
        ("sourceBindings", source_bindings),
    ]);
    save_json(&accepted, &renewed)?;
    replace_credential(api, "notes-account-token.json", &renewed, &output)?;
    json_once(&output.join("completed.json"), &Val::dict(vec![("published", Val::Bool(true))]))?;
    api.auth_status()
}

/// The body of `CloudAPI.renew_cloud_token()` after the locks.
pub fn renew_cloud_token_locked(api: &CloudApi) -> R<Val> {
    api.validate_identity()?;
    let mut account = api.account()?;
    let cloud = api.cloud()?;
    let mut now = api.now();
    if num(&expiry(&account)?) <= now {
        renew_notes_token_locked(api, None)?;
        account = api.account()?;
        now = api.now();
    }
    let empty = Val::Dict(vec![]);
    let response = dget(&account, "response")?.unwrap_or(&empty).clone();
    let received = account.get("receivedUnixSeconds");
    if num(&expiry(&account)?) <= now
        || !string_ok(dget(&response, "access_token")?, 65536)
        || matches!(dget(&response, "access_token")?, Some(Val::Str(t)) if t == "-1")
        || !is_num(received)
        || num(received.unwrap()) > now
    {
        return Err(Fail::api("reauth_required", "Samsung oturumu yenilenmeli; tarayıcıdan tekrar giriş yapın."));
    }
    let eq = |a: Option<&Val>, b: &Val| py_eq(a.unwrap_or(&Val::Null), b);
    let bad_registration = !eq(dget(&cloud, "schemaVersion")?, &Val::Int(1))
        || !eq(dget(&cloud, "credentialKind")?, &Val::s("CloudUser"))
        || !string_ok(dget(&cloud, "accessToken")?, 65536)
        || {
            let t = cloud.get("accessToken").and_then(|v| v.as_str()).unwrap_or("");
            let t = t.strip_prefix("Bearer ").unwrap_or(t);
            t.is_empty() || t == "-1"
        }
        || !string_ok(dget(&cloud, "dvcId")?, 4096);
    if bad_registration {
        return Err(Fail::api("registration_required", "Cloud cihaz kaydı yenilenmeli."));
    }
    let version = ditem(ditem(ditem(&api.profile, "registrationBody")?, "app")?, "version")?.clone();
    let pairs: Vec<(&str, Val)> = vec![
        ("Content-Type", Val::s("application/json")),
        ("Accept", Val::s("application/json")),
        ("User-Agent", Val::s("SamsungNotesLinux/0.1")),
        ("x-sc-app-id", ditem(&api.profile, "appId")?.clone()),
        ("x-sc-app-version", version),
        ("x-sc-dvc-id", ditem(&cloud, "dvcId")?.clone()),
        ("x-sc-access-token", ditem(&response, "access_token")?.clone()),
        ("x-sc-uid", ditem(&response, "userId")?.clone()),
    ];
    if !pairs.iter().all(|(_, v)| string_ok(Some(v), 65536)) {
        return Err(Fail::api("invalid_profile", "Cloud yenileme bilgisi geçersiz."));
    }
    let mut headers: Headers = Vec::new();
    for (k, v) in &pairs {
        set_header(&mut headers, k, v.as_str().unwrap_or_default());
    }
    let token = cloud.get("accessToken").and_then(|v| v.as_str()).unwrap_or("");
    let previous = token.strip_prefix("Bearer ").unwrap_or(token);
    let payload = crate::json::dumps(&Val::dict(vec![("previousToken", Val::s(previous))]), &crate::json::COMPACT);
    let url = format!(
        "{}/identity/v2/user/token/renew?{}",
        api.play,
        util::urlencode(&[("deviceCurrentTime", ((now * 1000.0) as i64).to_string())])
    );
    let request = Request::new("POST", url, &headers, Some(payload.into_bytes()));
    let output = api.directory("renewals")?;
    let answer = api.call(&output, "renew", &request, crate::api::DEFAULT_CALL_LIMIT)?;
    if answer.status != Some(200) || answer.body.is_none() {
        let code = if matches!(answer.status, Some(400 | 401 | 403)) { "reauth_required" } else { "renew_failed" };
        return Err(Fail::api(code, "Cloud oturumu yenilenemedi; yeniden giriş gerekebilir."));
    }
    let parsed = (|| -> R<(String, Val)> {
        let result = crate::json::loads(answer.body.as_deref().unwrap_or_default())?;
        let token = dget(&result, "accessToken")?.cloned().unwrap_or(Val::Null);
        let lifetime = dget(&result, "expiresIn")?.cloned().unwrap_or(Val::Null);
        let token = match token {
            Val::Str(t) => Val::s(t.strip_prefix("Bearer ").unwrap_or(&t)),
            other => other,
        };
        let ok = string_ok(Some(&token), 65536)
            && token.as_str() != Some("-1")
            && is_num(Some(&lifetime))
            && 0.0 < num(&lifetime)
            && num(&lifetime) <= 366.0 * 86400.0;
        if !ok {
            return Err(Fail::value("renewal"));
        }
        Ok((token.as_str().unwrap_or_default().to_string(), lifetime))
    })();
    let (token, lifetime) = match parsed {
        Ok(v) => v,
        Err(e) if e.is_value() || e.is_type() || e.kind == notes_host::Kind::Attribute => {
            return Err(Fail::api("invalid_response", "Cloud yenileme yanıtı geçersiz."))
        }
        Err(e) => return Err(e),
    };
    let renewed = Val::dict(vec![
        ("schemaVersion", Val::Int(1)),
        ("credentialKind", Val::s("CloudUser")),
        ("receivedUnixSeconds", clock_val(api.now())),
        ("expiresIn", lifetime),
        ("dvcId", ditem(&cloud, "dvcId")?.clone()),
        ("accessToken", Val::s(token.strip_prefix("Bearer ").unwrap_or(&token))),
        ("accountFingerprint", Val::s(api.identity())),
    ]);
    save_json(&output.join("accepted-token.json"), &renewed)?;
    replace_credential(api, "cloud-token.json", &renewed, &output)?;
    api.auth_status()
}

/// The Rust port of the oracle's renewal (the default). Requests go through
/// the API's [`crate::transport::Transport`]. NEEDS LIVE RENEWAL VALIDATION.
pub struct PortedRenewal;

impl Renewal for PortedRenewal {
    fn renew_cloud_token(&self, api: &CloudApi) -> R<Val> {
        let _lock = credential_lock(api)?;
        renew_cloud_token_locked(api)
    }
    fn renew_notes_token(&self, api: &CloudApi, operation_id: Option<&str>) -> R<Val> {
        let _lock = credential_lock(api)?;
        api.validate_identity()?;
        renew_notes_token_locked(api, operation_id)
    }
}
