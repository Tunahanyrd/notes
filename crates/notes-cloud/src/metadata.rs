//! `samsung_notes.cloud_metadata`: the account folder graph
//! (FolderResource), the note inventory and organization writes.
use crate::api::{conflict, normalize_commit, write_uncertain, CloudApi, SnapIn, DEFAULT_CALL_LIMIT};
use crate::etree::{self, Element};
use crate::guards;
use crate::json;
use crate::transport::{set_header, Request};
use crate::util::{self, check_uuid, ditem, load_json, private_write, save_json, save_once, R};
use base64::Engine as _;
use notes_host::snapshot::{self, Snapshot};
use notes_host::{Fail, Kind, Val};
use rs_store::val::py_eq;
use std::path::Path;
use std::sync::Arc;

pub const MAX_XML: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 20000;

/// `cloud_metadata.INITIAL_FOLDER_CURSOR`
pub fn initial_folder_cursor() -> String {
    util::b64encode(
        br#"{"version":0,"encryptedGuid":"","requestTimeStamp":0,"modifiedTime":0,"appVersion":"0","sdkVersion":"0","purpose":"INITIAL","type":"FOLDER"}"#,
    )
}

fn invalid(message: &str) -> Fail {
    Fail::api("invalid_metadata", message)
}

fn tag(e: &Element) -> String {
    e.local()
}

/// `_xml(raw)`
pub fn xml(raw: &[u8]) -> R<Element> {
    if raw.len() > MAX_XML || raw.windows(2).any(|w| w == b"<!") || raw.contains(&0) {
        return Err(invalid("Klasör yanıtı güvenle okunamıyor."));
    }
    let root = etree::parse_bytes(raw, true).map_err(|_| invalid("Klasör yanıtı çözümlenemedi."))?;
    if root.count() > MAX_NODES * 20 {
        return Err(invalid("Klasör yanıtı çözümlenemedi."));
    }
    Ok(root)
}

/// `_children(e)`: name → text ('' for None), duplicates rejected.
pub fn children(e: &Element) -> R<Vec<(String, String)>> {
    let mut values: Vec<(String, String)> = Vec::new();
    for c in &e.children {
        let name = tag(c);
        if values.iter().any(|(k, _)| *k == name) {
            return Err(invalid("Tekrarlanan klasör alanı."));
        }
        values.push((name, c.text.clone().unwrap_or_default()));
    }
    Ok(values)
}

fn field<'a>(values: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `_number(value)`
pub fn number(value: Option<&str>) -> R<i64> {
    let err = || invalid("Klasör zaman bilgisi geçersiz.");
    let v = value.ok_or_else(err)?;
    if v.is_empty() || v.chars().count() > 20 {
        return Err(err());
    }
    match util::py_int(v) {
        Some(n) if (0..=i64::MAX as i128).contains(&n) => Ok(n as i64),
        _ => Err(err()),
    }
}

/// `base64.b64decode(''.join(value.split()), validate=True).decode()`
pub fn decode(value: &str) -> R<String> {
    let err = || invalid("Klasör metni çözümlenemedi.");
    let joined: String = value.chars().filter(|c| !util::py_isspace(*c)).collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireCanonical),
    );
    let bytes = engine.decode(joined.as_bytes()).map_err(|_| err())?;
    String::from_utf8(bytes).map_err(|_| err())
}

fn b64(text: &str) -> String {
    util::b64encode(text.as_bytes())
}

/// `cloud_metadata.parse_folder_changes(raw)`
pub fn parse_folder_changes(raw: &[u8]) -> R<Val> {
    let root = xml(raw)?;
    if tag(&root) != "folderchangesview" {
        return Err(invalid("Beklenmeyen klasör yanıtı."));
    }
    let resources: Vec<&Element> = root.children.iter().filter(|e| tag(e) == "folderresource").collect();
    let cursors: Vec<String> = root.children.iter().filter(|e| tag(e) == "lastchangepoint").map(|e| e.text.clone().unwrap_or_default()).collect();
    if resources.len() > 1 || cursors.len() != 1 || !util::is_safe_string(&cursors[0], 65536) {
        return Err(invalid("Klasör senkronizasyon bilgisi eksik."));
    }
    let mut nodes = Vec::new();
    let mut sync = Val::Null;
    if let Some(resource) = resources.first() {
        let values = children(resource)?;
        let encrypted = field(&values, "encrypted").unwrap_or("false").to_lowercase();
        if encrypted != "true" && encrypted != "false" {
            return Err(invalid("Şifreleme durumu geçersiz."));
        }
        if encrypted == "true" {
            return Err(Fail::api("encrypted_metadata", "Uçtan uca şifreli klasörler henüz desteklenmiyor."));
        }
        sync = Val::Int(number(field(&values, "syncmodifiedtime"))?);
        let lists: Vec<&Element> = resource.children.iter().filter(|e| tag(e) == "nodelist").collect();
        if lists.len() != 1 || lists[0].children.len() > MAX_NODES {
            return Err(invalid("Klasör listesi geçersiz."));
        }
        let mut seen = std::collections::HashSet::new();
        for node in &lists[0].children {
            if tag(node) != "node" {
                return Err(invalid("Bilinmeyen klasör kaydı."));
            }
            let (identifier, kind) = (node.get("id"), node.get("type"));
            let fields = children(node)?;
            let id_ok = identifier.map(|i| util::is_safe_string(i, 1024) && !seen.contains(i)).unwrap_or(false);
            if !id_ok || !matches!(kind, Some("folder") | Some("note")) {
                return Err(invalid("Klasör kimliği geçersiz."));
            }
            let identifier = identifier.unwrap().to_string();
            seen.insert(identifier.clone());
            let state = field(&fields, "state");
            if !matches!(state, Some("normal") | Some("trashed") | Some("deleted")) {
                return Err(invalid("Bilinmeyen klasör durumu."));
            }
            let parent = field(&fields, "parentfoldernodeid").unwrap_or("");
            if !util::is_safe_string(parent, 1024) {
                return Err(invalid("Üst klasör bilgisi eksik."));
            }
            let name = decode(field(&fields, "name").unwrap_or(""))?;
            let node_sync = number(field(&fields, "syncmodifiedtime"))?;
            let created = number(Some(field(&fields, "createdtime").unwrap_or("0")))?;
            let modified = number(Some(field(&fields, "lastmodifiedtime").unwrap_or("0")))?;
            let trashed = number(Some(field(&fields, "trashedtime").unwrap_or("0")))?;
            let restore = decode(field(&fields, "restorepath").unwrap_or(""))?;
            nodes.push(Val::dict(vec![
                ("id", Val::s(identifier)),
                ("type", Val::s(kind.unwrap())),
                ("parent_id", Val::s(parent)),
                ("name", Val::s(name)),
                ("state", Val::s(state.unwrap())),
                ("sync_modified_time", Val::Int(node_sync)),
                ("created_time", Val::Int(created)),
                ("modified_time", Val::Int(modified)),
                ("trashed_time", Val::Int(trashed)),
                ("restore_path", Val::s(restore)),
                ("color", Val::s(field(&fields, "color").unwrap_or(""))),
                ("order", Val::s(field(&fields, "order").unwrap_or(""))),
                ("raw_xml", Val::s(etree::tostring_unicode(node))),
            ]));
        }
    }
    Ok(Val::dict(vec![
        ("nodes", Val::List(nodes)),
        ("last_change_point", Val::s(&cursors[0])),
        ("has_resource", Val::Bool(!resources.is_empty())),
        ("sync_modified_time", sync),
        ("encrypted", Val::Bool(false)),
    ]))
}

/// `cloud_metadata.parse_note_list(raw)`
pub fn parse_note_list(raw: &[u8]) -> R<Val> {
    let root = xml(raw)?;
    if tag(&root) != "notelistview" {
        return Err(invalid("Beklenmeyen not envanteri."));
    }
    let lists: Vec<&Element> = root.children.iter().filter(|e| tag(e) == "notelist").collect();
    let tokens: Vec<String> = root.children.iter().filter(|e| tag(e) == "notepagetoken").map(|e| e.text.clone().unwrap_or_default()).collect();
    if lists.len() != 1 || tokens.len() > 1 || lists[0].children.len() > MAX_NODES {
        return Err(invalid("Not envanteri geçersiz."));
    }
    let token = tokens.first().cloned();
    if let Some(t) = &token {
        if !t.is_empty() && !util::is_safe_string(t, 65536) {
            return Err(invalid("Not sayfası konumu geçersiz."));
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut notes = Vec::new();
    for n in &lists[0].children {
        let values = children(n)?;
        if tag(n) != "note" {
            return Err(invalid("Bilinmeyen envanter kaydı."));
        }
        let (server, resource) = (field(&values, "serverid"), field(&values, "resourceid"));
        let deleted = field(&values, "isdeleted").unwrap_or("false").to_lowercase();
        let ok = server.map(|s| util::is_safe_string(s, 1024) && !seen.contains(s)).unwrap_or(false)
            && resource.map(|r| util::is_safe_string(r, 1024)).unwrap_or(false)
            && (deleted == "true" || deleted == "false");
        if !ok {
            return Err(invalid("Not envanteri kimliği geçersiz."));
        }
        seen.insert(server.unwrap().to_string());
        notes.push(Val::dict(vec![
            ("server_id", Val::s(server.unwrap())),
            ("resource_id", Val::s(resource.unwrap())),
            ("is_deleted", Val::Bool(deleted == "true")),
            ("commit_id", Val::opt_str(field(&values, "commitid"))),
            ("sync_modified_time", Val::Int(number(field(&values, "syncmodifiedtime"))?)),
            ("sdk_version", Val::opt_str(field(&values, "sdkversion"))),
            ("is_updated_by_device", Val::Bool(field(&values, "isupdatedbydevice").unwrap_or("false") == "true")),
        ]));
    }
    Ok(Val::dict(vec![("notes", Val::List(notes)), ("note_page_token", Val::opt_str(token.as_deref()))]))
}

/// `_set(node, key, value)`
pub fn set_child(node: &mut Element, key: &str, value: &str) -> R<()> {
    let lower = key.to_lowercase();
    let hits: Vec<usize> = node.children.iter().enumerate().filter(|(_, c)| tag(c) == lower).map(|(i, _)| i).collect();
    if hits.len() > 1 {
        return Err(invalid("Tekrarlanan klasör alanı."));
    }
    let i = match hits.first() {
        Some(i) => *i,
        None => {
            node.children.push(Element::new(key));
            node.children.len() - 1
        }
    };
    node.children[i].text = Some(value.to_string());
    Ok(())
}

#[derive(PartialEq)]
struct Semantic(String, Vec<(String, String)>, String, Vec<Semantic>);

fn semantic(e: &Element) -> Semantic {
    let mut attrs = e.attrib.clone();
    attrs.sort();
    Semantic(tag(e), attrs, util::py_strip(e.text.as_deref().unwrap_or("")).to_string(), e.children.iter().map(semantic).collect())
}

/// `document.one(parent, name)` index.
fn one_index(parent: &Element, name: &str) -> R<usize> {
    let found: Vec<usize> = parent.children.iter().enumerate().filter(|(_, c)| tag(c) == name).map(|(i, _)| i).collect();
    if found.len() != 1 {
        return Err(Fail::document("Belgenin yapısı güvenli düzenleme için uygun değil."));
    }
    Ok(found[0])
}

/// `_document_content(raw)`: ignore only syncModifiedTime and isFavorite.
fn document_content(raw: &[u8]) -> R<Semantic> {
    let mut root = xml(raw)?;
    root.remove_attr("syncModifiedTime");
    let i = one_index(&root, "attributes")?;
    root.children[i].remove_attr("isFavorite");
    Ok(semantic(&root))
}

/// `cloud_metadata.document_organization(snapshot)` → `{'favorite', 'state'}`
pub fn document_organization<'a>(snapshot: impl Into<SnapIn<'a>>) -> R<Val> {
    let snapshot = match snapshot.into().resolve()? {
        Some(s) => s,
        None => return Err(Fail::new(Kind::Attribute, "object has no attribute 'resource_xml'")),
    };
    organization_of(&snapshot)
}

fn organization_of(snapshot: &Snapshot) -> R<Val> {
    let root = xml(&snapshot.resource_xml)?;
    let attrs = &root.children[one_index(&root, "attributes")?];
    let value = attrs.get("isFavorite").unwrap_or("false");
    if value != "true" && value != "false" {
        return Err(invalid("Favori durumu geçersiz."));
    }
    Ok(Val::dict(vec![("favorite", Val::Bool(value == "true")), ("state", Val::s(attrs.get("state").unwrap_or("normal")))]))
}

fn node_by_id<'a>(nodes: &'a [Val], id: &str) -> Option<&'a Val> {
    nodes.iter().find(|n| n.get("id").and_then(|v| v.as_str()) == Some(id))
}

fn s_of<'a>(node: &'a Val, key: &str) -> &'a str {
    node.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn graph_nodes(graph: &Val) -> Vec<Val> {
    match graph.get("nodes") {
        Some(Val::List(items)) => items.clone(),
        _ => vec![],
    }
}

/// `CloudMetadata`
pub struct CloudMetadata {
    pub api: Arc<CloudApi>,
    pub initial_cursor: String,
}

impl CloudMetadata {
    pub fn new(api: Arc<CloudApi>) -> CloudMetadata {
        CloudMetadata { api, initial_cursor: initial_folder_cursor() }
    }

    /// `CloudMetadata.folder_changes(cursor=None)`
    pub fn folder_changes(&self, cursor: Option<&str>) -> R<Val> {
        let cursor = cursor.unwrap_or(&self.initial_cursor).to_string();
        if !util::is_safe_string(&cursor, 65536) {
            return Err(Fail::api("invalid_cursor", "Klasör senkronizasyon konumu geçersiz."));
        }
        let headers = self.api.headers(false)?;
        let url = format!("{}/samsungnotes/v3/folder/changes?{}", self.api.base, util::urlencode(&[("lastChangePoint", cursor.clone())]));
        let request = Request::new("GET", url, &headers, None);
        let output = self.api.directory("metadata-reads")?;
        save_json(
            &output.join("request.json"),
            &Val::dict(vec![
                ("schemaVersion", Val::Int(1)),
                ("kind", Val::s("folder_changes")),
                ("accountFingerprint", Val::s(self.api.identity())),
                ("cursor", Val::s(cursor)),
            ]),
        )?;
        let response = self.api.call(&output, "folders", &request, MAX_XML)?;
        CloudApi::read_status(&response)?;
        let mut result = parse_folder_changes(response.body.as_deref().unwrap_or_default())?;
        result.set("account_fingerprint", Val::s(self.api.identity()));
        Ok(result)
    }

    /// `CloudMetadata.inventory()`
    pub fn inventory(&self) -> R<Val> {
        self.folder_changes(None)
    }

    /// `CloudMetadata.note_list(modified_after=1464416139848, page_token=None)`
    pub fn note_list(&self, modified_after: i64, page_token: Option<&str>) -> R<Val> {
        if modified_after < 0 {
            return Err(Fail::api("invalid_cursor", "Not envanteri zamanı geçersiz."));
        }
        let mut query = vec![("modifiedAfter", Val::Int(modified_after))];
        let mut pairs = vec![("modifiedAfter", modified_after.to_string())];
        if let Some(token) = page_token {
            if !util::is_safe_string(token, 65536) {
                return Err(Fail::api("invalid_cursor", "Not sayfası konumu geçersiz."));
            }
            query.push(("notePageToken", Val::s(token)));
            pairs.push(("notePageToken", token.to_string()));
        }
        let url = format!("{}/samsungnotes/v3/notes/list?{}", self.api.base, util::urlencode(&pairs));
        let request = Request::new("GET", url, &self.api.headers(false)?, None);
        let output = self.api.directory("metadata-reads")?;
        save_json(
            &output.join("request.json"),
            &Val::dict(vec![
                ("schemaVersion", Val::Int(1)),
                ("kind", Val::s("note_list")),
                ("accountFingerprint", Val::s(self.api.identity())),
                ("query", Val::dict(query)),
            ]),
        )?;
        let response = self.api.call(&output, "note-list", &request, MAX_XML)?;
        CloudApi::read_status(&response)?;
        parse_note_list(response.body.as_deref().unwrap_or_default())
    }

    /// `CloudMetadata.note_inventory(max_pages=256)`
    pub fn note_inventory(&self, max_pages: i64) -> R<Val> {
        if !(1..=256).contains(&max_pages) {
            return Err(Fail::api("invalid_limit", "Envanter sınırı geçersiz."));
        }
        let mut token: Option<String> = None;
        let mut seen: Vec<String> = Vec::new();
        let mut notes: Vec<(String, Val)> = Vec::new();
        for _ in 0..max_pages {
            let page = self.note_list(1464416139848, token.as_deref())?;
            if let Some(Val::List(items)) = page.get("notes") {
                for note in items {
                    let id = s_of(note, "server_id").to_string();
                    match notes.iter_mut().find(|(k, _)| *k == id) {
                        Some((_, old)) => {
                            if !py_eq(old, note) {
                                return Err(Fail::conflict("Not envanteri okuma sırasında değişti."));
                            }
                            *old = note.clone();
                        }
                        None => notes.push((id, note.clone())),
                    }
                }
            }
            if notes.len() > MAX_NODES {
                return Err(Fail::api("inventory_limit", "Not envanteri sınırı aşıldı."));
            }
            token = match page.get("note_page_token") {
                Some(Val::Str(t)) if !t.is_empty() => Some(t.clone()),
                _ => None,
            };
            let Some(t) = &token else {
                return Ok(Val::dict(vec![("notes", Val::List(notes.into_iter().map(|(_, v)| v).collect())), ("complete", Val::Bool(true))]));
            };
            if seen.contains(t) {
                return Err(Fail::api("invalid_cursor", "Not sayfası konumu tekrarlanıyor."));
            }
            seen.push(t.clone());
        }
        Err(Fail::api("inventory_limit", "Not envanteri tamamlanamadı; önceki kayıtlar korunuyor."))
    }

    /// `CloudMetadata._folder_name(name)`
    pub fn folder_name(name: &str) -> R<String> {
        if !util::is_safe_string(name, 255) || util::py_strip(name).is_empty() || name.contains('/') || name.contains('\\') {
            return Err(Fail::api("invalid_folder_name", "Klasör adı boş olamaz veya yol ayırıcı içeremez."));
        }
        Ok(name.to_string())
    }

    fn folder_name_val(name: &Val) -> R<String> {
        match name {
            Val::Str(s) => Self::folder_name(s),
            _ => Err(Fail::api("invalid_folder_name", "Klasör adı boş olamaz veya yol ayırıcı içeremez.")),
        }
    }

    /// `CloudMetadata.create_folder(name, parent_id='root', operation_id=..., folder_id=None)`
    pub fn create_folder(&self, name: &str, parent_id: &str, operation_id: &str, folder_id: Option<&str>) -> R<Val> {
        let name = Self::folder_name(name)?;
        if !util::is_safe_string(parent_id, 1024) {
            return Err(Fail::api("invalid_metadata_change", "Üst klasör geçersiz."));
        }
        let folder_id = match folder_id {
            Some(f) => check_uuid(f)?,
            None => util::uuid5(&check_uuid(operation_id)?, "folder"),
        };
        let (output, replay) = self.operation(
            operation_id,
            vec![("kind", Val::s("create_folder")), ("folder_id", Val::s(&folder_id)), ("name", Val::s(&name)), ("parent_id", Val::s(parent_id))],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let graph = self.inventory()?;
        let nodes = graph_nodes(&graph);
        if node_by_id(&nodes, &folder_id).is_some() {
            return Err(Fail::conflict("Klasör kimliği Cloud üzerinde zaten var."));
        }
        Self::validate_parent(&nodes, parent_id, &folder_id)?;
        let now = self.api.now_ms();
        let mut node = Element::new("node");
        node.set("id", &folder_id);
        node.set("type", "folder");
        for (key, value) in [
            ("parentFolderNodeId", parent_id.to_string()),
            ("createdTime", now.to_string()),
            ("lastModifiedTime", now.to_string()),
            ("name", b64(&name)),
            ("state", "normal".to_string()),
            ("trashedTime", "0".to_string()),
            ("color", "0".to_string()),
            ("order", "0".to_string()),
        ] {
            set_child(&mut node, key, &value)?;
        }
        self.publish_folder(&output, node, &graph, now)
    }

    /// `CloudMetadata._validate_parent(nodes, parent_id, identifier)`
    fn validate_parent(nodes: &[Val], parent_id: &str, identifier: &str) -> R<()> {
        let mut seen: Vec<String> = vec![identifier.to_string()];
        let mut parent_id = parent_id.to_string();
        while parent_id != "root" {
            let parent = node_by_id(nodes, &parent_id);
            let bad = seen.contains(&parent_id)
                || seen.len() >= 64
                || match parent {
                    None => true,
                    Some(p) => s_of(p, "type") != "folder" || s_of(p, "state") != "normal",
                };
            if bad {
                return Err(Fail::conflict("Hedef klasör kullanılamıyor veya döngü oluşturuyor."));
            }
            seen.push(parent_id.clone());
            parent_id = s_of(parent.unwrap(), "parent_id").to_string();
        }
        Ok(())
    }

    /// `CloudMetadata.update_folder(folder_id, changes, baseline_node=..., operation_id=...)`
    pub fn update_folder(&self, folder_id: &str, changes: &Val, baseline_node: &Val, operation_id: &str) -> R<Val> {
        let single = match changes {
            Val::Dict(items) if items.len() == 1 => Some(items[0].clone()),
            _ => None,
        };
        let Some((key, value)) = single.filter(|_| util::is_safe_string(folder_id, 1024)) else {
            return Err(Fail::api("invalid_metadata_change", "Tek klasör düzenlemesi gerekli."));
        };
        match key.as_str() {
            "name" => {
                Self::folder_name_val(&value)?;
            }
            "parent_id" => {
                if !util::string_ok(Some(&value), 1024) {
                    return Err(Fail::api("invalid_metadata_change", "Üst klasör geçersiz."));
                }
            }
            "trashed" => {
                if !matches!(value, Val::Bool(_)) {
                    return Err(Fail::api("invalid_metadata_change", "Çöp kutusu durumu geçersiz."));
                }
            }
            _ => return Err(Fail::api("invalid_metadata_change", "Desteklenmeyen klasör düzenlemesi.")),
        }
        let (output, replay) = self.operation(
            operation_id,
            vec![("kind", Val::s("update_folder")), ("folder_id", Val::s(folder_id)), ("changes", changes.clone()), ("baseline_node", baseline_node.clone())],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let graph = self.inventory()?;
        let nodes = graph_nodes(&graph);
        let current = node_by_id(&nodes, folder_id).cloned();
        let ok = match &current {
            Some(c) => py_eq(c, baseline_node) && c.truthy() && s_of(c, "type") == "folder" && s_of(c, "state") != "deleted",
            None => false,
        };
        if !ok {
            return Err(Fail::conflict("Klasör başka bir cihazda değişmiş."));
        }
        let current = current.unwrap();
        let noop = match key.as_str() {
            "name" => py_eq(current.get("name").unwrap_or(&Val::Null), &value),
            "parent_id" => py_eq(current.get("parent_id").unwrap_or(&Val::Null), &value),
            _ => (s_of(&current, "state") == "trashed") == matches!(value, Val::Bool(true)),
        };
        if noop {
            let result = Val::dict(vec![("node", current.clone()), ("verified", Val::Bool(true))]);
            self.api.save_operation_result(&output, &result)?;
            return Ok(result);
        }
        let mut node = xml(s_of(&current, "raw_xml").as_bytes())?;
        let now = self.api.now_ms();
        if now <= current.get("sync_modified_time").and_then(|v| v.as_i64()).unwrap_or(0) {
            return Err(Fail::api("clock_skew", "Yerel saat Cloud düzenlemesinden ileride olmalı."));
        }
        if (key == "name" || key == "parent_id") && s_of(&current, "state") != "normal" {
            return Err(Fail::conflict("Çöp kutusundaki klasör önce geri yüklenmeli."));
        }
        match key.as_str() {
            "name" => set_child(&mut node, "name", &b64(value.as_str().unwrap_or_default()))?,
            "parent_id" => {
                let target = value.as_str().unwrap_or_default();
                Self::validate_parent(&nodes, target, folder_id)?;
                set_child(&mut node, "parentFolderNodeId", target)?;
            }
            _ => {
                let trashed = matches!(value, Val::Bool(true));
                if nodes.iter().any(|n| s_of(n, "parent_id") == folder_id && s_of(n, "state") != "deleted") {
                    return Err(Fail::api("folder_not_empty", "Bu sürüm yalnız boş klasörleri çöp kutusuna taşıyabiliyor."));
                }
                set_child(&mut node, "state", if trashed { "trashed" } else { "normal" })?;
                set_child(&mut node, "trashedTime", &if trashed { now.to_string() } else { "0".into() })?;
                if trashed {
                    let path = Self::folder_path(&nodes, folder_id)?;
                    set_child(&mut node, "restorePath", &b64(&path))?;
                    set_child(&mut node, "parentFolderNodeId", "root")?;
                } else {
                    let path = s_of(&current, "restore_path");
                    let suffix = s_of(&current, "name");
                    if !path.ends_with(suffix) {
                        return Err(Fail::conflict("Klasörün geri yükleme yolu geçersiz."));
                    }
                    // Python `path[:-len(suffix)]` is '' for an empty suffix.
                    let mut parent_path = if suffix.is_empty() { String::new() } else { path[..path.len() - suffix.len()].to_string() };
                    if parent_path != "uncategorized:///" {
                        parent_path = parent_path.trim_end_matches('/').to_string();
                    }
                    let parent = Self::restore_parent(&nodes, &parent_path)?;
                    set_child(&mut node, "parentFolderNodeId", &parent)?;
                    node.children.retain(|c| tag(c) != "restorepath");
                }
            }
        }
        set_child(&mut node, "lastModifiedTime", &now.to_string())?;
        self.publish_folder(&output, node, &graph, now)
    }

    fn graph_sync(graph: &Val, now: i64) -> i64 {
        let sync = graph.get("sync_modified_time").and_then(|v| v.as_i64()).filter(|v| *v != 0).unwrap_or(0);
        now.max(sync)
    }

    fn patch(&self, output: &Path, xml: &[u8]) -> R<Option<Vec<u8>>> {
        let mut headers = self.api.headers(true)?;
        set_header(&mut headers, "Content-Type", "application/xml;Charset=utf-8");
        set_header(&mut headers, "Content-Encoding", "gzip");
        let request = Request::new("PATCH", format!("{}/samsungnotes/v3/folder", self.api.base), &headers, Some(util::gzip_mtime0(xml)));
        self.send(output, &request, 200)
    }

    /// `CloudMetadata._publish_folder(output, node, graph, now)`
    fn publish_folder(&self, output: &Path, mut node: Element, graph: &Val, now: i64) -> R<Val> {
        set_child(&mut node, "syncModifiedTime", &now.to_string())?;
        set_child(&mut node, "requestTime", &now.to_string())?;
        let mut device = vec![("model", Val::s("Linux Notes")), ("ver", Val::s("0.1"))];
        let state = children(&node)?.into_iter().find(|(k, _)| k == "state").map(|(_, v)| v).ok_or_else(|| Fail::key("'state'"))?;
        if state != "normal" {
            device.push(("deviceName", Val::s(b64("Linux Notes"))));
            device.push(("deviceId", ditem(&self.api.cloud()?, "dvcId")?.clone()));
        }
        set_child(&mut node, "requestDevice", &json::dumps(&Val::dict(device), &json::COMPACT))?;
        let mut root = Element::new("FolderResource");
        set_child(&mut root, "syncModifiedTime", &Self::graph_sync(graph, now).to_string())?;
        let mut list = Element::new("nodeList");
        list.children.push(node.clone());
        root.children.push(list);
        let xml_bytes = etree::tostring_utf8_decl(&root);
        save_once(&output.join("request.xml"), &xml_bytes)?;
        self.patch(output, &xml_bytes)?;
        let id = node.get("id").unwrap_or_default().to_string();
        let found = (|| -> R<Val> {
            let after = self.inventory()?;
            let nodes = graph_nodes(&after);
            let found = node_by_id(&nodes, &id).cloned().ok_or_else(|| Fail::value("StopIteration"))?;
            let values = children(&node)?;
            let get = |k: &str| field(&values, k).map(str::to_string).ok_or_else(|| Fail::key(format!("'{k}'")));
            if s_of(&found, "type") != "folder"
                || s_of(&found, "state") != get("state")?
                || s_of(&found, "name") != decode(&get("name")?)?
                || found.get("sync_modified_time").and_then(|v| v.as_i64()) != Some(now)
                || s_of(&found, "parent_id") != get("parentfoldernodeid")?
            {
                return Err(Fail::value("readback"));
            }
            Ok(found)
        })();
        let found = match found {
            Ok(f) => f,
            Err(e) if e.is_api() || e.is_value() => {
                return Err(write_uncertain("Klasör düzenlemesi gönderildi; geri okuma henüz doğrulanamadı."))
            }
            Err(e) => return Err(e),
        };
        let result = Val::dict(vec![("node", found), ("verified", Val::Bool(true))]);
        self.api.save_operation_result(&output, &result)?;
        Ok(result)
    }

    /// `CloudMetadata._operation(operation_id, intent)`
    fn operation(&self, operation_id: &str, intent: Vec<(&str, Val)>) -> R<(std::path::PathBuf, Option<Val>)> {
        self.api.validate_identity()?;
        let root = self.api.work_dir.join("metadata-operations");
        util::mkdir_private(&root, true)?;
        let output = root.join(check_uuid(operation_id)?);
        let mut items = intent;
        for (k, v) in [("schemaVersion", Val::Int(1)), ("accountFingerprint", Val::s(self.api.identity()))] {
            match items.iter_mut().find(|(key, _)| *key == k) {
                Some(slot) => slot.1 = v,
                None => items.push((k, v)),
            }
        }
        let value = Val::dict(items);
        if output.exists() {
            if !py_eq(&load_json(&output.join("intent.json"), MAX_XML)?, &value) {
                return Err(Fail::api("operation_mismatch", "İşlem farklı bir düzenlemeye ait."));
            }
            if output.join("result.json").exists() {
                return Ok((output.clone(), Some(self.api.load_operation_result(&output)?)));
            }
            if output.join(".mutation-claim").exists() {
                return Err(Fail::ambiguous("write_uncertain", "İşlem daha önce gönderilmiş olabilir; tekrar gönderilmedi."));
            }
        } else {
            util::mkdir_private(&output, false)?;
            save_json(&output.join("intent.json"), &value)?;
        }
        Ok((output, None))
    }

    /// `CloudMetadata.update_organization(server_id, snapshot, changes, operation_id=..., baseline_node=None)`
    pub fn update_organization<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        changes: &Val,
        operation_id: &str,
        baseline_node: &Val,
    ) -> R<Val> {
        let snapshot = match snapshot.into().resolve()? {
            Some(s) if s.server_id.as_deref() == Some(server_id) => s,
            _ => return Err(conflict()),
        };
        let (key, value) = match changes {
            Val::Dict(items) if items.len() == 1 => items[0].clone(),
            _ => return Err(conflict()),
        };
        let valid = match key.as_str() {
            "favorite" | "trashed" => matches!(value, Val::Bool(_)),
            "folder_id" => util::string_ok(Some(&value), 1024),
            _ => false,
        };
        if !valid {
            return Err(Fail::api("invalid_metadata_change", "Desteklenmeyen düzenleme isteği."));
        }
        let local_id = Val::opt_str(snapshot.local_id.as_deref());
        let (output, replay) = self.operation(
            operation_id,
            vec![
                ("kind", Val::s("organization")),
                ("server_id", Val::s(server_id)),
                ("local_id", local_id.clone()),
                ("baseline_commit", Val::opt_str(snapshot.commit_id.as_deref())),
                ("changes", changes.clone()),
                ("snapshot_sha256", Val::s(util::sha256_hex(&snapshot.resource_xml))),
                ("baseline_node", baseline_node.clone()),
            ],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let fresh = self.api.get_note(server_id)?;
        if fresh.local_id != snapshot.local_id
            || fresh.resource_xml != snapshot.resource_xml
            || normalize_commit(fresh.commit_id.as_deref())? != normalize_commit(snapshot.commit_id.as_deref())?
        {
            return Err(conflict());
        }
        let root = xml(&fresh.resource_xml)?;
        if ["isLocked", "isEncrypted"].iter().any(|k| root.get(k).unwrap_or("false") != "false") {
            return Err(Fail::api("protected_metadata", "Kilitli veya şifreli not henüz düzenlenemiyor."));
        }
        if key == "favorite" {
            return self.favorite(&output, server_id, &fresh, matches!(value, Val::Bool(true)));
        }
        let graph = self.inventory()?;
        let nodes = graph_nodes(&graph);
        let current = snapshot.local_id.as_deref().and_then(|id| node_by_id(&nodes, id)).cloned();
        let current_val = current.clone().unwrap_or(Val::Null);
        let bad_current = current.as_ref().is_some_and(|c| c.truthy() && (s_of(c, "type") != "note" || s_of(c, "state") == "deleted"));
        if !py_eq(&current_val, baseline_node) || bad_current {
            return Err(Fail::conflict("Notun klasör veya çöp kutusu durumu değişmiş."));
        }
        if let Some(c) = current.as_ref().filter(|c| c.truthy()) {
            let noop = (key == "folder_id" && py_eq(c.get("parent_id").unwrap_or(&Val::Null), &value))
                || (key == "trashed" && (s_of(c, "state") == "trashed") == matches!(value, Val::Bool(true)));
            if noop {
                let result = Val::dict(vec![
                    ("server_id", Val::s(server_id)),
                    ("local_id", local_id.clone()),
                    ("commit_id", Val::opt_str(fresh.commit_id.as_deref())),
                    ("snapshot", snapshot::to_dict(&fresh)),
                    ("node", c.clone()),
                    ("verified", Val::Bool(true)),
                ]);
                self.api.save_operation_result(&output, &result)?;
                return Ok(result);
            }
        }
        let mut node = match &current {
            None => {
                let mut node = Element::new("node");
                node.set("id", snapshot.local_id.as_deref().unwrap_or_default());
                node.set("type", "note");
                let title = if fresh.title.is_empty() { " " } else { fresh.title.as_str() };
                for (field_name, field_value) in [
                    ("parentFolderNodeId", "root".to_string()),
                    ("createdTime", fresh.created_ms.filter(|v| *v != 0).unwrap_or(0).to_string()),
                    ("lastModifiedTime", fresh.modified_ms.filter(|v| *v != 0).unwrap_or(0).to_string()),
                    ("name", b64(title)),
                    ("state", "normal".into()),
                    ("trashedTime", "0".into()),
                    ("color", "-1".into()),
                    ("order", "999999".into()),
                ] {
                    set_child(&mut node, field_name, &field_value)?;
                }
                node
            }
            Some(c) => xml(s_of(c, "raw_xml").as_bytes())?,
        };
        let current_truthy = current.as_ref().is_some_and(|c| c.truthy());
        if key == "folder_id" {
            if current_truthy && s_of(current.as_ref().unwrap(), "state") != "normal" {
                return Err(Fail::conflict("Çöp kutusundaki not önce geri yüklenmeli."));
            }
            let target = value.as_str().unwrap_or_default();
            if target != "root" {
                let usable = node_by_id(&nodes, target).is_some_and(|n| s_of(n, "type") == "folder" && s_of(n, "state") == "normal");
                if !usable {
                    return Err(Fail::conflict("Hedef klasör artık kullanılamıyor."));
                }
            }
            set_child(&mut node, "parentFolderNodeId", target)?;
        } else {
            let trashed = matches!(value, Val::Bool(true));
            set_child(&mut node, "state", if trashed { "trashed" } else { "normal" })?;
            if trashed {
                let parent = children(&node)?
                    .into_iter()
                    .find(|(k, _)| k == "parentfoldernodeid")
                    .map(|(_, v)| v)
                    .ok_or_else(|| Fail::key("'parentfoldernodeid'"))?;
                let path = Self::folder_path(&nodes, &parent)?;
                set_child(&mut node, "restorePath", &b64(&path))?;
                set_child(&mut node, "parentFolderNodeId", "root")?;
                set_child(&mut node, "trashedTime", &self.api.now_ms().to_string())?;
            } else {
                let path = if current_truthy { s_of(current.as_ref().unwrap(), "restore_path").to_string() } else { "uncategorized:///".to_string() };
                let parent = Self::restore_parent(&nodes, &path)?;
                set_child(&mut node, "parentFolderNodeId", &parent)?;
                set_child(&mut node, "trashedTime", "0")?;
                node.children.retain(|c| tag(c) != "restorepath");
            }
        }
        let now = self.api.now_ms();
        if current_truthy && now <= current.as_ref().unwrap().get("sync_modified_time").and_then(|v| v.as_i64()).unwrap_or(0) {
            return Err(Fail::api("clock_skew", "Yerel saat son Cloud düzenlemesinden ileride olmalı."));
        }
        set_child(&mut node, "syncModifiedTime", &now.to_string())?;
        set_child(&mut node, "requestTime", &now.to_string())?;
        let device = Val::dict(vec![
            ("model", Val::s("Linux Notes")),
            ("ver", Val::s("0.1")),
            ("deviceName", Val::s(b64("Linux Notes"))),
            ("deviceId", ditem(&self.api.cloud()?, "dvcId")?.clone()),
        ]);
        set_child(&mut node, "requestDevice", &json::dumps(&device, &json::COMPACT))?;
        let mut resource = Element::new("FolderResource");
        set_child(&mut resource, "syncModifiedTime", &Self::graph_sync(&graph, now).to_string())?;
        let mut list = Element::new("nodeList");
        list.children.push(node.clone());
        resource.children.push(list);
        let raw = etree::tostring_utf8_decl(&resource);
        let payload = util::gzip_mtime0(&raw);
        let mut headers = self.api.headers(true)?;
        set_header(&mut headers, "Content-Type", "application/xml;Charset=utf-8");
        set_header(&mut headers, "Content-Encoding", "gzip");
        save_once(&output.join("request.xml"), &raw)?;
        let request = Request::new("PATCH", format!("{}/samsungnotes/v3/folder", self.api.base), &headers, Some(payload));
        self.send(&output, &request, 200)?;
        let wanted_id = snapshot.local_id.clone().unwrap_or_default();
        let verified = (|| -> R<(Val, Snapshot)> {
            let after = self.inventory()?;
            let nodes = graph_nodes(&after);
            let updated = node_by_id(&nodes, &wanted_id).cloned().ok_or_else(|| Fail::value("StopIteration"))?;
            let wanted = children(&node)?;
            let get = |k: &str| field(&wanted, k).map(str::to_string).ok_or_else(|| Fail::key(format!("'{k}'")));
            if s_of(&updated, "state") != get("state")?
                || s_of(&updated, "parent_id") != get("parentfoldernodeid")?
                || updated.get("sync_modified_time").and_then(|v| v.as_i64()) != Some(now)
            {
                return Err(Fail::value("readback"));
            }
            let observed = self.api.get_note(server_id)?;
            if observed.local_id != snapshot.local_id {
                return Err(Fail::value("readback"));
            }
            Ok((updated, observed))
        })();
        let (updated, observed) = match verified {
            Ok(v) => v,
            Err(e) if e.is_api() || e.is_value() => {
                return Err(write_uncertain("Düzenleme gönderildi; Cloud sonucu henüz doğrulanamadı."))
            }
            Err(e) => return Err(e),
        };
        let result = Val::dict(vec![
            ("server_id", Val::s(server_id)),
            ("local_id", local_id),
            ("commit_id", Val::opt_str(observed.commit_id.as_deref())),
            ("snapshot", snapshot::to_dict(&observed)),
            ("node", updated),
            ("verified", Val::Bool(true)),
        ]);
        self.api.save_operation_result(&output, &result)?;
        Ok(result)
    }

    /// `CloudMetadata._restore_parent(nodes, path)`
    fn restore_parent(nodes: &[Val], path: &str) -> R<String> {
        if path.is_empty() || path == "uncategorized:///" {
            return Ok("root".into());
        }
        let mut found = Vec::new();
        for node in nodes {
            if s_of(node, "type") == "folder" && s_of(node, "state") == "normal" && Self::folder_path(nodes, s_of(node, "id"))? == path {
                found.push(s_of(node, "id").to_string());
            }
        }
        if found.len() != 1 {
            return Err(Fail::conflict("Özgün geri yükleme klasörü bulunamadı veya birden fazla eşleşme var."));
        }
        Ok(found.remove(0))
    }

    /// `CloudMetadata._folder_path(nodes, parent)`
    fn folder_path(nodes: &[Val], parent: &str) -> R<String> {
        if parent == "root" {
            return Ok("uncategorized:///".into());
        }
        let mut names: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut parent = parent.to_string();
        while parent != "root" {
            let node = node_by_id(nodes, &parent);
            if seen.contains(&parent) || node.is_none() || seen.len() >= 64 {
                return Err(Fail::conflict("Klasör ağacı tutarsız."));
            }
            seen.push(parent.clone());
            let node = node.unwrap();
            if s_of(node, "type") != "folder" || s_of(node, "state") != "normal" {
                return Err(conflict());
            }
            Self::folder_name_val(node.get("name").unwrap_or(&Val::Null))?;
            names.push(s_of(node, "name").to_string());
            parent = s_of(node, "parent_id").to_string();
        }
        names.reverse();
        Ok(format!("uncategorized:///{}", names.join("/")))
    }

    /// `CloudMetadata._send(output, request, expected=...)`
    fn send(&self, output: &Path, request: &Request, expected: i64) -> R<Option<Vec<u8>>> {
        private_write(&output.join(".mutation-claim"), b"claimed; never automatically repeat\n")?;
        let response = match self.api.call(output, "publish", request, DEFAULT_CALL_LIMIT) {
            Ok(r) => r,
            Err(e) if e.is_os() || e.is_value() || e.is_api() => {
                return Err(Fail::ambiguous("write_uncertain", "Düzenleme sonucu belirsiz; tekrar gönderilmedi."))
            }
            Err(e) => return Err(e),
        };
        if response.status != Some(expected) || response.body.is_none() {
            return Err(match response.status {
                Some(409) => conflict(),
                Some(400 | 401 | 403 | 404 | 413 | 422) => Fail::api("write_rejected", "Sunucu düzenlemeyi kabul etmedi."),
                _ => Fail::ambiguous("write_uncertain", "Düzenleme sonucu belirsiz; tekrar gönderilmedi."),
            });
        }
        save_json(&output.join("published.json"), &Val::dict(vec![("httpStatus", Val::Int(expected))]))?;
        Ok(response.body)
    }

    /// `CloudMetadata._favorite(output, server_id, snapshot, favorite)`
    fn favorite(&self, output: &Path, server_id: &str, snapshot: &Snapshot, favorite: bool) -> R<Val> {
        let mut resource = xml(&snapshot.resource_xml)?;
        let local_id = Val::opt_str(snapshot.local_id.as_deref());
        if organization_of(snapshot)?.get("favorite") == Some(&Val::Bool(favorite)) {
            let result = Val::dict(vec![
                ("server_id", Val::s(server_id)),
                ("local_id", local_id),
                ("commit_id", Val::opt_str(snapshot.commit_id.as_deref())),
                ("snapshot", snapshot::to_dict(snapshot)),
                ("favorite", Val::Bool(favorite)),
                ("verified", Val::Bool(true)),
            ]);
            self.api.save_operation_result(&output, &result)?;
            return Ok(result);
        }
        let now = self.api.now_ms();
        if now <= number(resource.get("syncModifiedTime"))? {
            return Err(Fail::api("clock_skew", "Yerel saat son Cloud düzenlemesinden ileride olmalı."));
        }
        let tree_i = one_index(&resource, "notetree")?;
        let attrs_i = one_index(&resource, "attributes")?;
        resource.children[attrs_i].set("isFavorite", if favorite { "true" } else { "false" });
        resource.set("syncModifiedTime", &now.to_string());
        resource.tag = "NoteResource".into();
        let tree = resource.children[tree_i].clone();
        let mut kept = Vec::new();
        for mut element in std::mem::take(&mut resource.children) {
            let name = tag(&element);
            if name == "bodytext" || name == "floatingobjectlist" {
                continue;
            }
            if name == "pagelist" || name == "contentfilelist" {
                element.clear();
            } else if name != "notetree" && name != "attributes" {
                return Err(Fail::api("unsupported_metadata", "Bilinmeyen belge alanı; güvenli favori değişikliği yapılamıyor."));
            }
            kept.push(element);
        }
        resource.children = kept;
        let mut headers = self.api.headers(true)?;
        set_header(&mut headers, "Content-Type", "application/xml;Charset=utf-8");
        set_header(&mut headers, "Content-Encoding", "gzip");
        let request = Request::new("POST", format!("{}/compareForUpsync", self.api.url(server_id)?), &headers, Some(util::gzip_mtime0(&etree::tostring_ascii(&tree))));
        let attempt = output.join(self.api.random().uuid4());
        util::mkdir_private(&attempt, false)?;
        let response = self.api.call(&attempt, "compare", &request, DEFAULT_CALL_LIMIT)?;
        CloudApi::read_status(&response)?;
        guards::compare_guard(response.body.as_deref(), snapshot.commit_id.as_deref(), &[]).map_err(|e| match e {
            guards::GuardError::Conflict => conflict(),
            guards::GuardError::Invalid => Fail::conflict("Sunucunun karşılaştırma yanıtı güvenli düzenlemeye uygun değil."),
        })?;
        let xml_bytes = etree::tostring_utf8_decl(&resource);
        save_once(&output.join("request.xml"), &xml_bytes)?;
        let (payload, content_type) = crate::api::multipart(self.api.random(), &xml_bytes);
        set_header(&mut headers, "Content-Type", &content_type);
        let raw = self.send(output, &Request::new("POST", format!("{}/update", self.api.url(server_id)?), &headers, Some(payload)), 201)?;
        let verified = (|| -> R<Snapshot> {
            let published = match guards::default_view(raw.as_deref().unwrap_or_default()) {
                Ok(pairs) => pairs,
                Err(guards::ViewError::Value(m)) => return Err(Fail::value(m)),
                Err(guards::ViewError::Parse(m)) => return Err(Fail::new(Kind::XmlParse, m)),
            };
            let get = |k: &str| published.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).ok_or_else(|| Fail::key(format!("'{k}'")));
            let after = self.api.get_note(server_id)?;
            if get("resourceId")? != server_id
                || after.local_id != snapshot.local_id
                || normalize_commit(after.commit_id.as_deref())? != normalize_commit(Some(&get("commitId")?))?
                || organization_of(&after)?.get("favorite") != Some(&Val::Bool(favorite))
                || document_content(&after.resource_xml)? != document_content(&snapshot.resource_xml)?
            {
                return Err(Fail::value("readback"));
            }
            Ok(after)
        })();
        let after = match verified {
            Ok(a) => a,
            Err(e) if e.is_value() || e.is_key() || e.is_api() => {
                return Err(write_uncertain("Favori değişikliği gönderildi; geri okuma henüz doğrulanamadı."))
            }
            Err(e) => return Err(e),
        };
        let result = Val::dict(vec![
            ("server_id", Val::s(server_id)),
            ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
            ("commit_id", Val::opt_str(after.commit_id.as_deref())),
            ("snapshot", snapshot::to_dict(&after)),
            ("favorite", Val::Bool(favorite)),
            ("verified", Val::Bool(true)),
        ]);
        self.api.save_operation_result(&output, &result)?;
        Ok(result)
    }
}
