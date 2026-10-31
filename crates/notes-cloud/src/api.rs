//! `samsung_notes.cloud_api.CloudAPI`: construction, credentials, receipts,
//! reads, the immutable operation directory, publication and readback.
//! Mutations live in [`crate::writes`]; page/erase kinds in [`crate::ops`].
use crate::auth::{self, PortedRenewal, Renewal};
use crate::guards;
use crate::json;
use crate::transport::{set_header, Headers, NoNetwork, Request, Response, Transport};
use crate::util::{self, check_uuid, dget, dget2, ditem, is_num, load_json, num, private_write, save_json, string_ok, OsRandom, Random, R};
use notes_host::snapshot::{self, Snapshot};
use notes_host::{Fail, Kind, Val};
use rs_store::val::py_eq;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, ThreadId};

/// `document.MAX_BODY_BYTES`
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// `CloudAPI.MAX_CONTENT_FILE`
pub const MAX_CONTENT_FILE: usize = 64 * 1024 * 1024;
/// `_call` default response bound.
pub const DEFAULT_CALL_LIMIT: usize = 2 * 1024 * 1024;

/// `time.time()`-like clock in seconds.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `changes_parser(raw)`: returns the parsed feed (a dict; its `page` key is
/// returned when present, like `parsed.get('page', parsed)`).
pub type ChangesParser = Arc<dyn Fn(Option<&[u8]>) -> R<Val> + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}

/// Collaborators of [`CloudApi`] (the Python constructor keywords plus the
/// renewal and randomness seams).
#[derive(Clone)]
pub struct Options {
    pub transport: Arc<dyn Transport>,
    pub renewal: Arc<dyn Renewal>,
    pub clock: Clock,
    pub random: Arc<dyn Random>,
    pub changes_parser: Option<ChangesParser>,
    pub container_directory: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            transport: Arc::new(NoNetwork),
            renewal: Arc::new(PortedRenewal),
            clock: system_clock(),
            random: Arc::new(OsRandom),
            changes_parser: None,
            container_directory: None,
        }
    }
}

impl Options {
    /// Enable immutable container-backed operation receipts for a schema 7 library.
    pub fn with_container_store(mut self, directory: impl Into<PathBuf>) -> Self {
        self.container_directory = Some(directory.into());
        self
    }
    pub fn with_transport(mut self, transport: impl Transport + 'static) -> Self {
        self.transport = Arc::new(transport);
        self
    }
    pub fn with_clock(mut self, clock: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }
    pub fn with_random(mut self, random: impl Random + 'static) -> Self {
        self.random = Arc::new(random);
        self
    }
    pub fn with_renewal(mut self, renewal: impl Renewal + 'static) -> Self {
        self.renewal = Arc::new(renewal);
        self
    }
    pub fn with_changes_parser(mut self, parser: impl Fn(Option<&[u8]>) -> R<Val> + Send + Sync + 'static) -> Self {
        self.changes_parser = Some(Arc::new(parser));
        self
    }
}

/// `threading.RLock()`
#[derive(Default)]
pub(crate) struct RLock {
    state: Mutex<(Option<ThreadId>, usize)>,
    cv: Condvar,
}

pub(crate) struct RGuard<'a>(&'a RLock);

impl RLock {
    pub(crate) fn lock(&self) -> RGuard<'_> {
        let me = thread::current().id();
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match s.0 {
                None => {
                    *s = (Some(me), 1);
                    break;
                }
                Some(t) if t == me => {
                    s.1 += 1;
                    break;
                }
                _ => s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner()),
            }
        }
        RGuard(self)
    }
}

impl Drop for RGuard<'_> {
    fn drop(&mut self) {
        let mut s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        s.1 -= 1;
        if s.1 == 0 {
            s.0 = None;
            self.0.cv.notify_one();
        }
    }
}

/// A `snapshot` argument: a stored `DocumentSnapshot.to_dict()` value
/// (`isinstance(snapshot, dict)` → `from_dict`; any other value is "not a
/// DocumentSnapshot") or an already parsed snapshot.
#[derive(Clone, Copy)]
pub enum SnapIn<'a> {
    Dict(&'a Val),
    Doc(&'a Snapshot),
}

impl<'a> From<&'a Val> for SnapIn<'a> {
    fn from(v: &'a Val) -> Self {
        SnapIn::Dict(v)
    }
}

impl<'a> From<&'a Snapshot> for SnapIn<'a> {
    fn from(s: &'a Snapshot) -> Self {
        SnapIn::Doc(s)
    }
}

impl SnapIn<'_> {
    /// `from_dict` for dicts; `None` when the value is not a snapshot.
    pub fn resolve(self) -> R<Option<Snapshot>> {
        match self {
            SnapIn::Doc(s) => Ok(Some(s.clone())),
            SnapIn::Dict(v @ Val::Dict(_)) => snapshot::from_dict(v).map(Some),
            SnapIn::Dict(_) => Ok(None),
        }
    }
}

/// ConflictError() with the default message.
pub fn conflict() -> Fail {
    Fail::conflict("Not başka bir cihazda değişti; yerel metin korunuyor.")
}

/// UnsupportedDocumentError(message)
pub fn unsupported(message: &str) -> Fail {
    Fail { kind: Kind::Unsupported, code: "unsupported_document".into(), message: message.into(), status: 0, ambiguous: false }
}

pub fn write_uncertain(message: &str) -> Fail {
    Fail::ambiguous("write_uncertain", message)
}

/// `isinstance(snapshot, DocumentSnapshot) and snapshot.server_id in (None, server_id)`
pub fn resolve_for(snapshot: SnapIn<'_>, server_id: &str) -> R<Snapshot> {
    match snapshot.resolve()? {
        Some(s) if s.server_id.is_none() || s.server_id.as_deref() == Some(server_id) => Ok(s),
        _ => Err(conflict()),
    }
}

/// `document.normalize_commit` (DocumentError).
pub fn normalize_commit(value: Option<&str>) -> R<String> {
    guards::normalize_commit(value).map_err(|_| Fail::document("Senkronizasyon sürümü eksik veya geçersiz."))
}

/// One extra MIME part: (name, filename, body), content type octet-stream.
pub type Appended = (String, String, Vec<u8>);

/// Every MIME part of a snapshot body (`tools.notes_payload._parts`).
pub fn snapshot_parts(snapshot: &Snapshot) -> R<Vec<(Option<String>, Option<String>, String, Vec<u8>)>> {
    let limits = rs_doc::doc::DocLimits::default();
    let parts = rs_doc::mime::parts(&snapshot.raw_body, &snapshot.content_type, limits.max_readonly_parts, &limits.payload, &mut rs_doc::mime::NoHooks)
        .map_err(Fail::from)?;
    Ok(parts
        .into_iter()
        .map(|p| (p.name, p.filename, p.content_type, snapshot.raw_body[p.body].to_vec()))
        .collect())
}

/// `CloudAPI._opaque_content_fingerprint(snapshot, appended=...)`; with
/// `removed` it is `cloud_page_ops._opaque_without` (callers check presence).
pub fn opaque_fingerprint(snapshot: &Snapshot, appended: &[Appended], removed: Option<&[String]>) -> R<String> {
    if snapshot.raw_body.is_empty() || snapshot.raw_body.len() > MAX_BODY_BYTES {
        return Err(unsupported("Belge boyutu sınırını aşıyor."));
    }
    let parts = snapshot_parts(snapshot)?;
    type Rec = (bool, String, bool, String, String, String);
    let mut records: Vec<Rec> = Vec::new();
    for (name, filename, ctype, body) in &parts {
        if ctype == "application/xml" || ctype == "text/xml" {
            continue;
        }
        if let Some(removed) = removed {
            if name.as_deref() == Some("strokeResource") && filename.as_ref().is_some_and(|f| removed.contains(f)) {
                continue;
            }
        }
        records.push((
            name.is_some(),
            name.clone().unwrap_or_default(),
            filename.is_some(),
            filename.clone().unwrap_or_default(),
            ctype.clone(),
            util::sha256_hex(body),
        ));
    }
    for (name, filename, body) in appended {
        records.push((true, name.clone(), true, filename.clone(), "application/octet-stream".into(), util::sha256_hex(body)));
    }
    records.sort();
    let value = Val::List(
        records
            .into_iter()
            .map(|(a, b, c, d, e, f)| Val::List(vec![Val::Bool(a), Val::Str(b), Val::Bool(c), Val::Str(d), Val::Str(e), Val::Str(f)]))
            .collect(),
    );
    Ok(util::sha256_hex(json::dumps(&value, &json::COMPACT).as_bytes()))
}

/// `document.multipart(xml)` with the injectable boundary source.
pub fn multipart(random: &dyn Random, xml: &[u8]) -> (Vec<u8>, String) {
    let boundary = format!("notes-app-{}", random.token_hex(16));
    let mut raw = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"noteResource\"\r\nContent-Type: application/xml; charset=UTF-8\r\n\r\n"
    )
    .into_bytes();
    raw.extend_from_slice(xml);
    raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (util::gzip_mtime0(&raw), format!("multipart/form-data;boundary={boundary}"))
}

/// Ink multipart (noteResource + strokeResource parts), gzip'd, with its
/// content type; `UnsupportedDocumentError` when over the body bound.
pub fn ink_multipart(random: &dyn Random, upload_xml: &[u8], parts: &[Appended]) -> R<(Vec<u8>, String)> {
    let boundary = format!("notes-ink-{}", random.token_hex(16));
    let mut raw = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"noteResource\"\r\nContent-Type: application/xml; charset=UTF-8\r\n\r\n"
    )
    .into_bytes();
    raw.extend_from_slice(upload_xml);
    for (_, filename, body) in parts {
        raw.extend_from_slice(
            format!(
                "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"strokeResource\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(body);
    }
    raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    if raw.len() > MAX_BODY_BYTES {
        return Err(unsupported("Çizim gönderimi boyut sınırını aşıyor."));
    }
    Ok((util::gzip_mtime0(&raw), format!("multipart/form-data;boundary={boundary}")))
}

/// `CloudAPI`
pub struct CloudApi {
    pub session_path: PathBuf,
    pub work_dir: PathBuf,
    pub profile: Val,
    pub endpoints: Val,
    pub base: String,
    pub play: String,
    pub initial_cursor: String,
    identity: Mutex<String>,
    pub(crate) lock: RLock,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) renewal: Arc<dyn Renewal>,
    pub(crate) clock: Clock,
    pub(crate) random: Arc<dyn Random>,
    pub(crate) changes_parser: Option<ChangesParser>,
    container_directory: Option<PathBuf>,
}

fn sha_identity(client: &str, user: &str) -> String {
    util::sha256_hex(format!("{client}\0{user}").as_bytes())
}

fn concat_str<'a>(v: &'a Val, what: &str) -> R<&'a str> {
    match v {
        Val::Str(s) => Ok(s),
        other => Err(Fail::type_(format!("can only concatenate str (not \"{}\") to str{}", other.type_name(), what))),
    }
}

impl CloudApi {
    /// `CloudAPI(session_path, work_dir, transport=..., changes_parser=..., clock=...)`
    pub fn new(session_path: impl Into<PathBuf>, work_dir: impl Into<PathBuf>, options: Options) -> R<CloudApi> {
        let session_path = session_path.into();
        let work_dir = work_dir.into();
        if let Some(parent) = work_dir.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        util::mkdir_private(&work_dir, true)?;
        let meta = std::fs::symlink_metadata(&work_dir)?;
        if meta.file_type().is_symlink() || std::fs::metadata(&work_dir)?.permissions().mode() & 0o077 != 0 {
            return Err(Fail::api("unsafe_storage", "Oturum çalışma dizini özel olmalı."));
        }
        let profile = load_json(&session_path.join("cloud").join("profile.json"), util::DEFAULT_LIMIT)?;
        let endpoints = load_json(&session_path.join("cloud").join("endpoints.json"), util::DEFAULT_LIMIT)?;
        let base = util::endpoint(&endpoints, "default")?;
        let play = util::endpoint(&endpoints, "play")?;
        let initial_cursor = match dget(&profile, "initialCursor")? {
            Some(Val::Str(s)) => s.clone(),
            _ => return Err(Fail::api("invalid_profile", "Başlangıç senkronizasyon bilgisi eksik.")),
        };
        let api = CloudApi {
            session_path,
            work_dir,
            profile,
            endpoints,
            base,
            play,
            initial_cursor,
            identity: Mutex::new(String::new()),
            lock: RLock::default(),
            transport: options.transport,
            renewal: options.renewal,
            clock: options.clock,
            random: options.random,
            changes_parser: options.changes_parser,
            container_directory: options.container_directory,
        };
        api.validate_identity()?;
        Ok(api)
    }

    pub(crate) fn save_operation_result(&self, output: &Path, result: &Val) -> R<()> {
        crate::result_store::save_result(self.container_directory.as_deref(), output, result)
    }

    pub(crate) fn load_operation_result(&self, output: &Path) -> R<Val> {
        crate::result_store::load_result(self.container_directory.as_deref(), output)
    }

    /// `self._clock()`
    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    /// `int(self._clock() * 1000)`
    pub fn now_ms(&self) -> i64 {
        (self.now() * 1000.0) as i64
    }

    /// `self._identity` (account fingerprint).
    pub fn identity(&self) -> String {
        self.identity.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn random(&self) -> &dyn Random {
        &*self.random
    }

    /// `CloudAPI._original_account()`
    pub fn original_account(&self) -> R<Val> {
        load_json(&self.session_path.join("notes-account-token.json"), util::DEFAULT_LIMIT)
    }

    /// `CloudAPI._cloud()`
    pub fn cloud(&self) -> R<Val> {
        let path = self.work_dir.join("cloud-token.json");
        if path.exists() {
            let cached = load_json(&path, util::DEFAULT_LIMIT)?;
            if py_eq(dget(&cached, "accountFingerprint")?.unwrap_or(&Val::Null), &Val::s(self.identity())) {
                return Ok(cached);
            }
        }
        load_json(&self.session_path.join("cloud").join("cloud-token.json"), util::DEFAULT_LIMIT)
    }

    /// `CloudAPI._account()`
    pub fn account(&self) -> R<Val> {
        let original = self.original_account()?;
        let path = self.work_dir.join("notes-account-token.json");
        if path.exists() {
            let cached = load_json(&path, util::DEFAULT_LIMIT)?;
            let client = concat_str(ditem(&original, "clientId")?, "")?.to_string();
            let user = ditem(ditem(&original, "response")?, "userId")?;
            let identity = sha_identity(&client, concat_str(user, "")?);
            let eq = |a: Option<&Val>, b: &Val| py_eq(a.unwrap_or(&Val::Null), b);
            let ok = eq(dget(&cached, "accountFingerprint")?, &Val::s(identity))
                && eq(dget(&cached, "credentialKind")?, &Val::s("NotesServiceOAuth"))
                && eq(dget(&cached, "schemaVersion")?, &Val::Int(1))
                && eq(dget(&cached, "clientId")?, &Val::s(client))
                && eq(dget2(&cached, "response", "userId")?, user)
                && eq(dget(&cached, "authServerUrl")?, original.get("authServerUrl").unwrap_or(&Val::Null));
            if ok {
                return Ok(cached);
            }
        }
        Ok(original)
    }

    /// `CloudAPI._validate_identity()`
    pub fn validate_identity(&self) -> R<()> {
        let account = self.original_account()?;
        let eq = |a: Option<&Val>, b: Option<&Val>| py_eq(a.unwrap_or(&Val::Null), b.unwrap_or(&Val::Null));
        let bad = !eq(dget(&account, "schemaVersion")?, Some(&Val::Int(1)))
            || !eq(dget(&account, "credentialKind")?, Some(&Val::s("NotesServiceOAuth")))
            || !eq(dget(&account, "clientId")?, dget(&self.profile, "appId")?)
            || !string_ok(dget2(&account, "response", "userId")?, 4096);
        if bad {
            return Err(Fail::api("identity_mismatch", "Notes hesabı ile Cloud uygulama kimliği uyuşmuyor."));
        }
        let client = concat_str(ditem(&account, "clientId")?, "")?;
        let user = concat_str(ditem(ditem(&account, "response")?, "userId")?, "")?;
        *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = sha_identity(client, user);
        Ok(())
    }

    /// `CloudAPI.auth_status()`
    pub fn auth_status(&self) -> R<Val> {
        let cloud = self.cloud()?;
        let account = self.account()?;
        let now = self.now();
        let cloud_expiry = auth::expiry(&cloud)?;
        let account_expiry = auth::expiry(&account)?;
        let received = dget(&cloud, "receivedUnixSeconds")?;
        let valid = dget(&cloud, "credentialKind")?.map(|v| py_eq(v, &Val::s("CloudUser"))).unwrap_or(false)
            && dget(&cloud, "schemaVersion")?.map(|v| py_eq(v, &Val::Int(1))).unwrap_or(false)
            && string_ok(dget(&cloud, "accessToken")?, 65536)
            && !matches!(dget(&cloud, "accessToken")?, Some(Val::Str(t)) if t == "-1")
            && is_num(received)
            && num(received.unwrap()) <= now
            && now < num(&cloud_expiry);
        let token = dget2(&account, "response", "access_token")?;
        let account_received = dget(&account, "receivedUnixSeconds")?;
        let mut renewable = num(&account_expiry) > now
            && string_ok(token, 65536)
            && !matches!(token, Some(Val::Str(t)) if t == "-1")
            && is_num(account_received)
            && num(account_received.unwrap()) <= now;
        let uat_expires = match auth::uat_context(self) {
            Ok((uat, _, _)) => util::num_add(ditem(&uat, "receivedUnixSeconds")?, ditem(&uat, "expiresIn")?),
            Err(e) if e.is_api() => Val::Int(0),
            Err(e) => return Err(e),
        };
        let uat_live = num(&uat_expires) > now;
        renewable = renewable || uat_live;
        Ok(Val::dict(vec![
            ("valid", Val::Bool(valid)),
            ("needs_login", Val::Bool(!valid && !renewable)),
            ("uat_expires_at", uat_expires),
            ("notes_renewable", Val::Bool(uat_live)),
            ("cloud_expires_at", cloud_expiry),
            ("notes_expires_at", account_expiry),
            ("renewable", Val::Bool(renewable)),
        ]))
    }

    /// `CloudAPI.renew_cloud_token()` through the [`Renewal`] seam.
    pub fn renew_cloud_token(&self) -> R<Val> {
        let _guard = self.lock.lock();
        self.renewal.renew_cloud_token(self)
    }

    /// `CloudAPI.renew_notes_token(operation_id=...)` through the [`Renewal`] seam.
    pub fn renew_notes_token(&self, operation_id: Option<&str>) -> R<Val> {
        let _guard = self.lock.lock();
        self.renewal.renew_notes_token(self, operation_id)
    }

    /// `notes_cloud.changes_request(cloud, endpoints, profile, cursor, now).header_items()`
    fn changes_headers(&self, cloud: &Val, now: f64) -> R<Headers> {
        let received = ditem(cloud, "receivedUnixSeconds")?;
        let lifetime = ditem(cloud, "expiresIn")?;
        let token = dget(cloud, "accessToken")?;
        if !dget(cloud, "credentialKind")?.map(|v| py_eq(v, &Val::s("CloudUser"))).unwrap_or(false)
            || !dget(cloud, "schemaVersion")?.map(|v| py_eq(v, &Val::Int(1))).unwrap_or(false)
        {
            return Err(Fail::value("valid Cloud user token required"));
        }
        let numeric = |v: &Val| matches!(v, Val::Int(_) | Val::BigInt(_) | Val::Float(_) | Val::Bool(_));
        if !numeric(received) {
            return Err(Fail::type_("'<=' not supported"));
        }
        if !numeric(lifetime) {
            return Err(Fail::type_("unsupported operand type(s) for +"));
        }
        let end = num(received) + num(lifetime);
        if !(num(received) <= now && now < end) || !util::safe_text(token, 65536) {
            return Err(Fail::value("valid Cloud user token required"));
        }
        let cursor = &self.initial_cursor;
        if cursor.chars().count() > 65536 || cursor.chars().any(|c| (c as u32) < 32) {
            return Err(Fail::value("invalid opaque cursor"));
        }
        let notes = match dget(&self.profile, "notesHeaders")? {
            None => Val::Dict(vec![]),
            Some(v @ Val::Dict(_)) => v.clone(),
            Some(_) => return Err(Fail::type_("cannot convert dictionary update sequence element")),
        };
        let allowed = ["x-sn-sdk-ver", "x-sn-app-ver", "x-sn-xml-ver", "x-sn-model-name", "x-sc-network", "x-sc-e2ee-state"];
        let keys = notes.keys();
        let required = ["x-sn-sdk-ver", "x-sn-app-ver", "x-sn-xml-ver", "x-sn-model-name"];
        let values_ok = match &notes {
            Val::Dict(items) => items.iter().all(|(_, v)| util::safe_text(Some(v), 1024)),
            _ => false,
        };
        if !keys.iter().all(|k| allowed.contains(k)) || !values_ok || !required.iter().all(|k| keys.contains(k)) {
            return Err(Fail::value("explicit Notes protocol headers required"));
        }
        let mut headers: Headers = Vec::new();
        if let Val::Dict(items) = &notes {
            for (k, v) in items {
                set_header(&mut headers, k, v.as_str().unwrap_or_default());
            }
        }
        set_header(&mut headers, "Authorization", &format!("Bearer {}", token.and_then(|t| t.as_str()).unwrap_or_default()));
        set_header(&mut headers, "X-SN-Transaction-ID", &format!("ND-{}", self.random.choice("abcdefghijklmnopqrstuvwxyz", 10)));
        set_header(&mut headers, "Accept-Encoding", "gzip");
        set_header(&mut headers, "User-Agent", "SamsungNotesLinuxResearch/0.1");
        util::endpoint(&self.endpoints, "default")?;
        // urllib.request.Request(...).header_items(): capitalised names.
        Ok(Request::new("GET", String::new(), &headers, None).headers)
    }

    /// `CloudAPI._headers(mutation)`: the Python dict the oracle builds
    /// (capitalised `urllib` names plus `X-SN-Transaction-ID`).
    pub fn headers(&self, mutation: bool) -> R<Headers> {
        let _guard = self.lock.lock();
        self.validate_identity()?;
        let mut cloud = self.cloud()?;
        if num(&auth::expiry(&cloud)?) <= self.now() {
            self.renew_cloud_token()?;
            cloud = self.cloud()?;
        }
        let items = match self.changes_headers(&cloud, self.now()) {
            Ok(items) => items,
            Err(e) if e.is_value() || e.is_key() || e.is_type() => {
                return Err(Fail::api("reauth_required", "Geçerli Samsung oturumu gerekli."))
            }
            Err(e) => return Err(e),
        };
        let mut headers: Headers = items.into_iter().filter(|(k, _)| k.to_lowercase() != "x-sn-transaction-id").collect();
        let prefix = if mutation { "NU-" } else { "ND-" };
        set_header(&mut headers, "X-SN-Transaction-ID", &format!("{prefix}{}", self.random.choice("abcdefghijklmnopqrstuvwxyz", 10)));
        Ok(headers)
    }

    /// `CloudAPI._directory(kind)`
    pub fn directory(&self, kind: &str) -> R<PathBuf> {
        let root = self.work_dir.join(kind);
        util::mkdir_private(&root, true)?;
        let directory = root.join(self.random.uuid4());
        util::mkdir_private(&directory, false)?;
        Ok(directory)
    }

    /// `CloudAPI._call(directory, name, request, limit=...)`
    pub fn call(&self, directory: &Path, name: &str, request: &Request, limit: usize) -> R<Response> {
        if !self.transport.writes_receipts() {
            let response = self.transport.send(directory, name, request, limit)?;
            if response.body.as_ref().is_some_and(|b| b.len() > limit) {
                return Err(Fail::api("invalid_response", "Sunucu yanıtı boyut sınırını aşıyor."));
            }
            let status = response.status.map(Val::Int).unwrap_or(Val::Null);
            save_json(&directory.join(format!("{name}-receipt.json")), &Val::dict(vec![("httpStatus", status), ("automaticRetry", Val::Bool(false))]))?;
            if let Some(body) = &response.body {
                private_write(&directory.join(format!("{name}-response.body")), body)?;
            }
            let ctype = Val::opt_str(response.content_type.as_deref());
            save_json(&directory.join(format!("{name}-response-meta.json")), &Val::dict(vec![("contentType", ctype)]))?;
            return Ok(response);
        }
        let mut response = self.transport.send(directory, name, request, limit)?;
        let meta = directory.join(format!("{name}-response-meta.json"));
        response.content_type = if meta.exists() {
            match dget(&load_json(&meta, util::DEFAULT_LIMIT)?, "contentType")? {
                None => Some(String::new()),
                Some(Val::Str(s)) => Some(s.clone()),
                Some(_) => None,
            }
        } else {
            Some(String::new())
        };
        Ok(response)
    }

    /// `CloudAPI._read_status(status, raw)`
    pub fn read_status(response: &Response) -> R<()> {
        match response.status {
            Some(404) => Err(Fail::api("not_found", "Not sunucuda bulunamadı.")),
            Some(401) | Some(403) => Err(Fail::api("reauth_required", "Samsung oturumunu yenilemek gerekiyor.")),
            Some(200) if response.body.is_some() => Ok(()),
            _ => Err(Fail::api("cloud_unavailable", "Cloud yanıtı alınamadı; yerel veriler korunuyor.")),
        }
    }

    /// `CloudAPI.changes(cursor=None)`
    pub fn changes(&self, cursor: Option<&str>) -> R<Val> {
        let cursor = cursor.unwrap_or(&self.initial_cursor).to_string();
        if cursor.chars().count() > 65536 || cursor.chars().any(|c| (c as u32) < 32) {
            return Err(Fail::api("invalid_cursor", "Senkronizasyon konumu geçersiz."));
        }
        let output = self.directory("reads")?;
        save_json(
            &output.join("request.json"),
            &Val::dict(vec![
                ("schemaVersion", Val::Int(1)),
                ("kind", Val::s("changes")),
                ("accountFingerprint", Val::s(self.identity())),
                ("cursor", Val::s(cursor.clone())),
            ]),
        )?;
        let url = format!("{}/samsungnotes/v3/notes/changes?{}", self.base, util::urlencode(&[("lastChangePoint", cursor)]));
        let request = Request::new("GET", url, &self.headers(false)?, None);
        let response = self.call(&output, "changes", &request, DEFAULT_CALL_LIMIT)?;
        Self::read_status(&response)?;
        if let Some(parser) = &self.changes_parser {
            let parsed = parser(response.body.as_deref())?;
            return Ok(match dget(&parsed, "page")? {
                Some(page) => page.clone(),
                None => parsed,
            });
        }
        let raw = response.body.unwrap_or_default();
        let source = output.join("changes-response.body");
        let target = output.join("changes.json");
        let parsed = (|| -> R<Val> {
            export_changes(&source, &target)?;
            let parsed = load_json(&target, 8 * 1024 * 1024)?;
            if !py_eq(dget(&parsed, "sourceSha256")?.unwrap_or(&Val::Null), &Val::s(util::sha256_hex(&raw))) {
                return Err(Fail::value("source hash"));
            }
            Ok(ditem(&parsed, "page")?.clone())
        })();
        match parsed {
            Ok(page) => Ok(page),
            Err(e) if e.is_os() || e.is_value() || e.is_key() || e.kind == Kind::Other => {
                Err(Fail::api("feed_parse_failed", "Cloud değişiklik listesi çözümlenemedi."))
            }
            Err(e) => Err(e),
        }
    }

    /// `CloudAPI._url(server_id)`
    pub fn url(&self, server_id: &str) -> R<String> {
        if !util::is_safe_string(server_id, 1024) {
            return Err(Fail::api("invalid_id", "Cloud not kimliği geçersiz."));
        }
        Ok(format!("{}/samsungnotes/v3/notes/{}", self.base, util::quote(server_id)))
    }

    /// `CloudAPI.get_note(server_id)` → `DocumentSnapshot`.
    pub fn get_note(&self, server_id: &str) -> R<Snapshot> {
        let mut headers = self.headers(false)?;
        set_header(&mut headers, "Content-Type", "application/xml;Charset=utf-8");
        set_header(&mut headers, "Content-Encoding", "gzip");
        set_header(&mut headers, "Accept", "multipart/form-data");
        let url = format!("{}/compareForDownsync", self.url(server_id)?);
        let request = Request::new("POST", url, &headers, Some(util::gzip_mtime0(b"<noteTree ver=\"0\" isStrokeGrouped=\"false\" />")));
        let output = self.directory("reads")?;
        save_json(
            &output.join("request.json"),
            &Val::dict(vec![
                ("schemaVersion", Val::Int(1)),
                ("kind", Val::s("get_note")),
                ("accountFingerprint", Val::s(self.identity())),
                ("server_id", Val::s(server_id)),
            ]),
        )?;
        let response = self.call(&output, "downsync", &request, MAX_BODY_BYTES)?;
        Self::read_status(&response)?;
        let raw = response.body.unwrap_or_default();
        let ctype = response.content_type.unwrap_or_default();
        match snapshot::parse_document(&raw, &ctype, Some(server_id)) {
            Ok(s) => Ok(s),
            Err(e) if e.is_value() || e.is_type() || e.kind == Kind::XmlParse => {
                Err(Fail::api("unsupported_payload", "Notun içeriği bu sürümde çözümlenemedi; özgün yanıt korundu."))
            }
            Err(e) => Err(e),
        }
    }

    /// `CloudAPI.get_note(server_id).to_dict()`
    pub fn get_note_dict(&self, server_id: &str) -> R<Val> {
        Ok(snapshot::to_dict(&self.get_note(server_id)?))
    }

    /// `CloudAPI._operation(operation_id, intent)` → (directory, replay).
    /// `reconcile` decides how an already published operation is replayed.
    pub fn operation_with(
        &self,
        operation_id: &str,
        intent: Vec<(&str, Val)>,
        reconcile: &dyn Fn(&CloudApi, &str) -> R<Val>,
    ) -> R<(PathBuf, Option<Val>)> {
        check_uuid(operation_id)?;
        let root = self.work_dir.join("operations");
        util::mkdir_private(&root, true)?;
        let output = root.join(operation_id);
        let mut items = vec![("schemaVersion", Val::Int(1)), ("accountFingerprint", Val::s(self.identity()))];
        for (k, v) in intent {
            match items.iter_mut().find(|(key, _)| *key == k) {
                Some(slot) => slot.1 = v,
                None => items.push((k, v)),
            }
        }
        let value = Val::dict(items);
        match std::fs::DirBuilder::new().mode_private().create(&output) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !py_eq(&load_json(&output.join("intent.json"), 2 * 1024 * 1024)?, &value) {
                    return Err(Fail::api("operation_mismatch", "Bu işlem kimliği farklı bir kayıt için kullanılmış."));
                }
                if output.join("result.json").exists() {
                    return Ok((output.clone(), Some(self.load_operation_result(&output)?)));
                }
                if output.join("published.json").exists() {
                    return Ok((output, Some(reconcile(self, operation_id)?)));
                }
                if output.join(".mutation-claim").exists() {
                    return Err(Fail::ambiguous("write_uncertain", "Önceki işlemin sonucu belirsiz; tekrar gönderilmedi."));
                }
                return Ok((output, None));
            }
            Err(e) => return Err(e.into()),
        }
        save_json(&output.join("intent.json"), &value)?;
        Ok((output, None))
    }

    /// `CloudAPI._operation` with `CloudAPI.reconcile` replay.
    pub fn operation(&self, operation_id: &str, intent: Vec<(&str, Val)>) -> R<(PathBuf, Option<Val>)> {
        self.operation_with(operation_id, intent, &|api, id| api.reconcile(id))
    }

    /// `CloudAPI._publish(output, url, payload, content_type)`
    pub fn publish(&self, output: &Path, url: &str, payload: Vec<u8>, content_type: &str) -> R<Val> {
        self.publish_with(output, url, payload, content_type, "Sunucu kaydı kabul etmedi; yerel metin korunuyor.", &|api, id| api.reconcile(id))
    }

    /// The shared publish body (`cloud_ink_erase._publish` differs only in
    /// the rejection message and its reconcile).
    pub fn publish_with(
        &self,
        output: &Path,
        url: &str,
        payload: Vec<u8>,
        content_type: &str,
        rejected: &str,
        reconcile: &dyn Fn(&CloudApi, &str) -> R<Val>,
    ) -> R<Val> {
        let mut headers = self.headers(true)?;
        set_header(&mut headers, "Content-Type", content_type);
        set_header(&mut headers, "Content-Encoding", "gzip");
        let payload_file = output.join("request.multipart.gz");
        let mut payload = payload;
        if payload_file.exists() {
            let old = util::read_bytes(&payload_file)?;
            let meta = load_json(&output.join("request-meta.json"), util::DEFAULT_LIMIT)?;
            let meta_sha = ditem(&meta, "sha256")?;
            if old.len() > MAX_BODY_BYTES || !py_eq(&Val::s(util::sha256_hex(&old)), meta_sha) {
                return Err(Fail::api("operation_mismatch", "Hazırlanmış gönderimin bütünlüğü değişti."));
            }
            let old_type = match ditem(&meta, "content_type")? {
                Val::Str(s) => s.clone(),
                other => return Err(Fail::type_(format!("content type must be str, not {}", other.type_name()))),
            };
            if decode_parts(&old, &old_type)? != decode_parts(&payload, content_type)? {
                return Err(Fail::api("operation_mismatch", "Hazırlanmış gönderimin içeriği değişti."));
            }
            payload = old;
            set_header(&mut headers, "Content-Type", &old_type);
        } else {
            private_write(&payload_file, &payload)?;
            save_json(
                &output.join("request-meta.json"),
                &Val::dict(vec![("content_type", Val::s(content_type)), ("sha256", Val::s(util::sha256_hex(&payload)))]),
            )?;
        }
        private_write(&output.join(".mutation-claim"), b"claimed before write; never automatically repeat\n")?;
        let request = Request::new("POST", url, &headers, Some(payload));
        let response = match self.call(output, "publish", &request, DEFAULT_CALL_LIMIT) {
            Ok(r) => r,
            Err(e) if e.is_os() || e.is_value() || e.is_api() => {
                return Err(write_uncertain("Gönderimin sonucu doğrulanamadı; tekrar gönderilmedi."))
            }
            Err(e) => return Err(e),
        };
        if response.status != Some(201) || response.body.is_none() {
            return Err(match response.status {
                Some(409) => conflict(),
                Some(400 | 401 | 403 | 404 | 413 | 422) => Fail::api("write_rejected", rejected),
                _ => write_uncertain("Gönderimin sonucu doğrulanamadı; tekrar gönderilmedi."),
            });
        }
        let result = match guards::default_view(response.body.as_deref().unwrap_or_default()) {
            Ok(pairs) => pairs,
            Err(_) => return Err(write_uncertain("Sunucu kaydetti ancak sürüm yanıtı doğrulanamadı.")),
        };
        let view = Val::Dict(pairs_to_val(result));
        if guards::normalize_commit(view.get("commitId").and_then(|v| v.as_str())).is_err() {
            return Err(write_uncertain("Sunucu kaydetti ancak sürüm yanıtı doğrulanamadı."));
        }
        save_json(&output.join("published.json"), &view)?;
        let name = output.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        reconcile(self, &name)
    }

    /// `CloudAPI.reconcile(operation_id)`
    pub fn reconcile(&self, operation_id: &str) -> R<Val> {
        let output = self.work_dir.join("operations").join(check_uuid(operation_id)?);
        if output.join("result.json").exists() {
            return self.load_operation_result(&output);
        }
        let intent = load_json(&output.join("intent.json"), 2 * 1024 * 1024)?;
        if !py_eq(dget(&intent, "accountFingerprint")?.unwrap_or(&Val::Null), &Val::s(self.identity())) {
            return Err(Fail::api("identity_mismatch", "İşlem farklı bir hesaba ait."));
        }
        let kind = match dget(&intent, "kind")? {
            Some(Val::Str(k)) => k.clone(),
            _ => String::new(),
        };
        if crate::ops::PAGE_KINDS.contains(&kind.as_str()) {
            return crate::ops::reconcile_page(self, operation_id);
        }
        if crate::ops::SHAPE_KINDS.contains(&kind.as_str()) {
            return crate::ops::reconcile_shape(self, operation_id);
        }
        let checked = (|| -> R<(String, Snapshot)> {
            let published = load_json(&output.join("published.json"), util::DEFAULT_LIMIT)?;
            let expected = load_json(&output.join("expected.json"), 2 * 1024 * 1024)?;
            let server_val = ditem(&published, "resourceId")?.clone();
            if !py_eq(dget(&expected, "server_id")?.unwrap_or(&server_val), &server_val) {
                return Err(Fail::value("server"));
            }
            let server = match &server_val {
                Val::Str(s) => s.clone(),
                _ => return Err(Fail::api("invalid_id", "Cloud not kimliği geçersiz.")),
            };
            let snapshot = self.get_note(&server)?;
            let commit = normalize_commit(snapshot.commit_id.as_deref())?;
            let published_commit = match ditem(&published, "commitId")? {
                Val::Str(s) => normalize_commit(Some(s))?,
                _ => return Err(Fail::document("Senkronizasyon sürümü eksik veya geçersiz.")),
            };
            if !py_eq(&Val::opt_str(snapshot.local_id.as_deref()), ditem(&expected, "local_id")?)
                || !py_eq(&Val::s(&snapshot.title), ditem(&expected, "title")?)
                || commit != published_commit
            {
                return Err(Fail::value("readback"));
            }
            crate::writes::verify_kind(self, &output, &kind, &snapshot, &expected)?;
            if let Some(inventory) = dget(&expected, "page_inventory")? {
                let current = rs_doc::doc::page_inventory(&snapshot.resource_xml).map_err(Fail::from)?;
                if !py_eq(&Val::s(current), inventory) {
                    return Err(Fail::value("page inventory"));
                }
            }
            Ok((server, snapshot))
        })();
        let (server, snapshot) = match checked {
            Ok(v) => v,
            Err(e) if e.is_os() || e.is_value() || e.is_key() || e.is_type() || e.is_api() => {
                return Err(write_uncertain("Kayıt yanıtı alındı; sunucudan geri okuyarak henüz doğrulanamadı."))
            }
            Err(e) => return Err(e),
        };
        let result = result_val(&server, &snapshot);
        self.save_operation_result(&output, &result)?;
        Ok(result)
    }

    /// `CloudAPI.upload_content_file(data)`
    pub fn upload_content_file(&self, data: &[u8]) -> R<Val> {
        if data.is_empty() || data.len() > MAX_CONTENT_FILE {
            return Err(Fail::api("invalid_file", "Dosya boyutu desteklenmiyor."));
        }
        let file_hash = util::sha256_hex(data);
        let mut headers = self.headers(true)?;
        let authorization = crate::transport::get_header(&headers, "Authorization").unwrap_or("").to_string();
        let token = authorization.strip_prefix("Bearer ").unwrap_or(&authorization).to_string();
        if token.is_empty() {
            return Err(Fail::api("reauth_required", "Geçerli Samsung oturumu gerekli."));
        }
        let file_digest = util::sha256(data);
        let token_digest = util::sha256(token.as_bytes());
        let key: Vec<u8> = file_digest.iter().zip(token_digest.iter()).map(|(a, b)| a ^ b).collect();
        let body = format!(
            "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><request><fileInfoList><fileInfo><hash>{file_hash}</hash><contentType>application/octet-stream</contentType><size>{}</size><validationKey>{}</validationKey></fileInfo></fileInfoList></request>",
            data.len(),
            util::hex(&key)
        );
        set_header(&mut headers, "Content-Type", "application/xml;Charset=utf-8");
        let output = self.directory("uploads")?;
        save_json(
            &output.join("request.json"),
            &Val::dict(vec![
                ("schemaVersion", Val::Int(1)),
                ("kind", Val::s("upload_content_file")),
                ("accountFingerprint", Val::s(self.identity())),
                ("sha256", Val::s(&file_hash)),
                ("size", Val::Int(data.len() as i64)),
            ]),
        )?;
        let request = Request::new("POST", format!("{}/samsungnotes/v3/binaries/createUploadURLs", self.base), &headers, Some(body.into_bytes()));
        let response = self.call(&output, "create-upload-urls", &request, DEFAULT_CALL_LIMIT)?;
        if matches!(response.status, Some(401 | 403)) {
            return Err(Fail::api("reauth_required", "Samsung oturumunu yenilemek gerekiyor."));
        }
        if response.status != Some(200) || response.body.is_none() {
            return Err(Fail::api("upload_unavailable", "Dosya yükleme adresi alınamadı."));
        }
        let (kind, fields) = match guards::find_upload_entry(response.body.as_deref().unwrap_or_default(), &file_hash) {
            guards::UploadAnswer::Unparsable => return Err(Fail::api("upload_unavailable", "Dosya yükleme yanıtı çözümlenemedi.")),
            guards::UploadAnswer::Ambiguous => return Err(Fail::api("upload_unavailable", "Dosya yükleme yanıtı belirsiz.")),
            guards::UploadAnswer::Missing => return Err(Fail::api("upload_unavailable", "Dosya yükleme yanıtında bu dosya yok.")),
            guards::UploadAnswer::Found(k, f) => (k, f),
        };
        if kind == "failed" {
            return Err(Fail::api("upload_rejected", "Sunucu dosyayı kabul etmedi."));
        }
        let field = |k: &str| fields.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        let upload_token = field("uploadtoken").unwrap_or_default();
        let token_ok = upload_token.len() >= 8
            && upload_token.len() <= 128
            && upload_token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !(kind == "duplicated" && upload_token.is_empty()) && !token_ok {
            return Err(Fail::api("upload_unavailable", "Dosya yükleme anahtarı geçersiz."));
        }
        if kind == "created" {
            let url = field("url").unwrap_or_default();
            let trusted = match util::urlsplit(&url) {
                Ok(target) => {
                    let (user, password) = target.userinfo();
                    target.scheme == "https"
                        && target.hostname().unwrap_or_default().ends_with(".samsungcloud.com")
                        && !user.is_some_and(|u| !u.is_empty())
                        && !password.is_some_and(|p| !p.is_empty())
                }
                Err(()) => return Err(Fail::value("Invalid IPv6 URL")),
            };
            if !trusted {
                return Err(Fail::api("upload_unavailable", "Dosya yükleme adresi güvenilir değil."));
            }
            let mut put_headers = self.headers(true)?;
            set_header(&mut put_headers, "Content-Type", "application/octet-stream");
            set_header(&mut put_headers, "Range", &format!("bytes=0-{}", data.len() as i64 - 1));
            let put = Request::new("PUT", url, &put_headers, Some(data.to_vec()));
            let status = match self.call(&output, "upload-put", &put, DEFAULT_CALL_LIMIT) {
                Ok(r) => r.status,
                Err(e) if e.is_os() || e.is_value() || e.is_api() => {
                    return Err(Fail::ambiguous("upload_uncertain", "Dosya yüklemesinin sonucu bilinmiyor."))
                }
                Err(e) => return Err(e),
            };
            if !matches!(status, Some(200 | 204)) {
                let rejected = matches!(status, Some(400 | 401 | 403 | 413));
                return Err(Fail {
                    kind: Kind::Api,
                    code: if rejected { "upload_rejected" } else { "upload_uncertain" }.into(),
                    message: "Dosya yüklenemedi.".into(),
                    status: 0,
                    ambiguous: !rejected,
                });
            }
        }
        save_json(
            &output.join("result.json"),
            &Val::dict(vec![("sha256", Val::s(&file_hash)), ("size", Val::Int(data.len() as i64)), ("status", Val::s(&kind))]),
        )?;
        Ok(Val::dict(vec![
            ("sha256", Val::s(file_hash)),
            ("size", Val::Int(data.len() as i64)),
            ("uploadToken", Val::s(upload_token)),
            ("status", Val::s(kind)),
        ]))
    }
}

trait PrivateMode {
    fn mode_private(&mut self) -> &mut Self;
}

impl PrivateMode for std::fs::DirBuilder {
    fn mode_private(&mut self) -> &mut Self {
        use std::os::unix::fs::DirBuilderExt;
        self.mode(0o700)
    }
}

/// `{'server_id', 'local_id', 'commit_id', 'snapshot', 'verified'}`
pub fn result_val(server: &str, snapshot: &Snapshot) -> Val {
    Val::dict(vec![
        ("server_id", Val::s(server)),
        ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
        ("commit_id", Val::opt_str(snapshot.commit_id.as_deref())),
        ("snapshot", snapshot::to_dict(snapshot)),
        ("verified", Val::Bool(true)),
    ])
}

pub fn pairs_to_val(pairs: Vec<(String, String)>) -> Vec<(String, Val)> {
    pairs.into_iter().map(|(k, v)| (k, Val::Str(v))).collect()
}

type DecodedPart = (Option<String>, Option<String>, String, Vec<u8>);

/// `[(p.name, p.filename, p.content_type, p.body) for p in parse_payload(gunzip(data), ctype).parts]`
pub fn decode_parts(data: &[u8], content_type: &str) -> R<Vec<DecodedPart>> {
    let raw = util::gunzip_limited(data, MAX_BODY_BYTES + 1)?;
    let limits = rs_doc::mime::Limits::default();
    let parsed = rs_doc::payload::parse_payload(&raw, content_type, None, &limits, &mut rs_doc::mime::NoHooks).map_err(Fail::from)?;
    Ok(parsed
        .parts
        .iter()
        .map(|p| (p.name.clone(), p.filename.clone(), p.content_type.clone(), raw[p.body.clone()].to_vec()))
        .collect())
}

/// The in-process `notes-cli export-changes SOURCE TARGET`: same validation
/// and the same private output bytes as the CLI.
pub fn export_changes(source: &Path, target: &Path) -> R<()> {
    use std::io::Read;
    let fail = |m: &str| Fail::new(Kind::Other, m.to_string());
    let f = std::fs::File::open(source).map_err(|_| fail("could not open the input file"))?;
    let mut raw = Vec::new();
    f.take(notes_protocol::MAX_XML_BYTES as u64 + 1).read_to_end(&mut raw).map_err(|_| fail("could not read the input file"))?;
    if raw.len() > notes_protocol::MAX_XML_BYTES {
        return Err(fail("input exceeds the size limit"));
    }
    let xml = std::str::from_utf8(&raw).map_err(|_| fail("change feed is not UTF-8"))?;
    let page = notes_protocol::parse_note_changes(xml).map_err(|e| fail(&e.to_string()))?;
    // Exactly the CLI's `json!` value (objects become serde_json maps).
    let output = serde_json::to_vec(&serde_json::json!({"schemaVersion": 1,
        "sourceSha256": util::sha256_hex(&raw), "page": page}))
    .map_err(|_| fail("could not serialize change feed"))?;
    private_write(target, &output).map_err(|_| fail("could not create a private output file"))
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<CloudApi>();
}
