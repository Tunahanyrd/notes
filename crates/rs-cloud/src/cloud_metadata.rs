//! Replacement for the Python module `cloud_metadata`: the account folder
//! graph (FolderResource), note inventory and organization writes.
//! Parsing/serialization is pure Rust (`parse_*`, `etree`); CloudMetadata
//! drives the duck-typed `api` object through Python attribute calls.
#[cfg(feature = "python")]
use crate::cloud_api::{self as api_mod, check_uuid};
use crate::etree::{self, Element};
use crate::glue::*;
use crate::pyjson::{self, Value};
use crate::util;
use base64::Engine as _;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyInt, PyList, PyString, PyTuple};

pub const MAX_XML: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 20000;
const RESULT_LIMIT: usize = 96 * 1024 * 1024;

/// Error raised as APIError(code, message).
#[derive(Debug, Clone, PartialEq)]
pub struct MetaError(pub &'static str, pub &'static str);

fn invalid(message: &'static str) -> MetaError {
    MetaError("invalid_metadata", message)
}

fn tag(e: &Element) -> String {
    e.local()
}

/// `_xml(raw)`
pub fn xml(raw: &[u8]) -> Result<Element, MetaError> {
    if raw.len() > MAX_XML || raw.windows(2).any(|w| w == b"<!") || raw.contains(&0) {
        return Err(invalid("Klasör yanıtı güvenle okunamıyor."));
    }
    let root = etree::parse_bytes(raw, true).map_err(|_| invalid("Klasör yanıtı çözümlenemedi."))?;
    if root.count() > MAX_NODES * 20 {
        return Err(invalid("Klasör yanıtı çözümlenemedi."));
    }
    Ok(root)
}

/// `_children(e)`: name -> text ('' for None), duplicates rejected.
pub fn children(e: &Element) -> Result<Vec<(String, String)>, MetaError> {
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
pub fn number(value: Option<&str>) -> Result<i64, MetaError> {
    let err = invalid("Klasör zaman bilgisi geçersiz.");
    let v = value.ok_or(err.clone())?;
    if v.is_empty() || v.chars().count() > 20 {
        return Err(err);
    }
    match util::py_int(v) {
        Some(n) if (0..=i64::MAX as i128).contains(&n) => Ok(n as i64),
        _ => Err(err),
    }
}

/// `base64.b64decode(''.join(value.split()), validate=True).decode()`
pub fn decode(value: &str) -> Result<String, MetaError> {
    let err = invalid("Klasör metni çözümlenemedi.");
    let joined: String = value.chars().filter(|c| !util::py_isspace(*c)).collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireCanonical),
    );
    let bytes = engine.decode(joined.as_bytes()).map_err(|_| err.clone())?;
    String::from_utf8(bytes).map_err(|_| err)
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub parent_id: String,
    pub name: String,
    pub state: String,
    pub sync_modified_time: i64,
    pub created_time: i64,
    pub modified_time: i64,
    pub trashed_time: i64,
    pub restore_path: String,
    pub color: String,
    pub order: String,
    pub raw_xml: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FolderChanges {
    pub nodes: Vec<Node>,
    pub last_change_point: String,
    pub has_resource: bool,
    pub sync_modified_time: Option<i64>,
    pub encrypted: bool,
}

/// parse_folder_changes
pub fn parse_folder_changes(raw: &[u8]) -> Result<FolderChanges, MetaError> {
    let root = xml(raw)?;
    if tag(&root) != "folderchangesview" {
        return Err(invalid("Beklenmeyen klasör yanıtı."));
    }
    let resources: Vec<&Element> = root.children.iter().filter(|e| tag(e) == "folderresource").collect();
    let cursors: Vec<String> = root.children.iter().filter(|e| tag(e) == "lastchangepoint").map(|e| e.text.clone().unwrap_or_default()).collect();
    if resources.len() > 1 || cursors.len() != 1 || !util::is_safe_string(&cursors[0], 65536) {
        return Err(invalid("Klasör senkronizasyon bilgisi eksik."));
    }
    let mut result = FolderChanges { nodes: Vec::new(), last_change_point: cursors[0].clone(), has_resource: !resources.is_empty(), sync_modified_time: None, encrypted: false };
    let Some(resource) = resources.first() else { return Ok(result) };
    let values = children(resource)?;
    let encrypted = field(&values, "encrypted").unwrap_or("false").to_lowercase();
    if encrypted != "true" && encrypted != "false" {
        return Err(invalid("Şifreleme durumu geçersiz."));
    }
    result.encrypted = encrypted == "true";
    if result.encrypted {
        return Err(MetaError("encrypted_metadata", "Uçtan uca şifreli klasörler henüz desteklenmiyor."));
    }
    result.sync_modified_time = Some(number(field(&values, "syncmodifiedtime"))?);
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
        let sync = number(field(&fields, "syncmodifiedtime"))?;
        let created = number(Some(field(&fields, "createdtime").unwrap_or("0")))?;
        let modified = number(Some(field(&fields, "lastmodifiedtime").unwrap_or("0")))?;
        let trashed = number(Some(field(&fields, "trashedtime").unwrap_or("0")))?;
        let restore = decode(field(&fields, "restorepath").unwrap_or(""))?;
        result.nodes.push(Node {
            id: identifier,
            kind: kind.unwrap().to_string(),
            parent_id: parent.to_string(),
            name,
            state: state.unwrap().to_string(),
            sync_modified_time: sync,
            created_time: created,
            modified_time: modified,
            trashed_time: trashed,
            restore_path: restore,
            color: field(&fields, "color").unwrap_or("").to_string(),
            order: field(&fields, "order").unwrap_or("").to_string(),
            raw_xml: etree::tostring_unicode(node),
        });
    }
    Ok(result)
}

#[derive(Clone, Debug, PartialEq)]
pub struct NoteEntry {
    pub server_id: String,
    pub resource_id: String,
    pub is_deleted: bool,
    pub commit_id: Option<String>,
    pub sync_modified_time: i64,
    pub sdk_version: Option<String>,
    pub is_updated_by_device: bool,
}

/// parse_note_list
pub fn parse_note_list(raw: &[u8]) -> Result<(Vec<NoteEntry>, Option<String>), MetaError> {
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
        notes.push(NoteEntry {
            server_id: server.unwrap().to_string(),
            resource_id: resource.unwrap().to_string(),
            is_deleted: deleted == "true",
            commit_id: field(&values, "commitid").map(str::to_string),
            sync_modified_time: number(field(&values, "syncmodifiedtime"))?,
            sdk_version: field(&values, "sdkversion").map(str::to_string),
            is_updated_by_device: field(&values, "isupdatedbydevice").unwrap_or("false") == "true",
        });
    }
    Ok((notes, token))
}

/// `_set(node, key, value)`
pub fn set_child(node: &mut Element, key: &str, value: &str) -> Result<(), MetaError> {
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

pub enum ContentError {
    Meta(MetaError),
    Document,
}

/// `document.one(parent, name)`
fn one<'a>(parent: &'a Element, name: &str) -> Result<&'a Element, ContentError> {
    let found: Vec<&Element> = parent.children.iter().filter(|c| tag(c) == name).collect();
    if found.len() != 1 {
        return Err(ContentError::Document);
    }
    Ok(found[0])
}

fn one_index(parent: &Element, name: &str) -> Result<usize, ContentError> {
    let found: Vec<usize> = parent.children.iter().enumerate().filter(|(_, c)| tag(c) == name).map(|(i, _)| i).collect();
    if found.len() != 1 {
        return Err(ContentError::Document);
    }
    Ok(found[0])
}

/// `_document_content(raw)`: ignore only syncModifiedTime and isFavorite.
fn document_content(raw: &[u8]) -> Result<Semantic, ContentError> {
    let mut root = xml(raw).map_err(ContentError::Meta)?;
    root.remove_attr("syncModifiedTime");
    let i = one_index(&root, "attributes")?;
    root.children[i].remove_attr("isFavorite");
    Ok(semantic(&root))
}

// ------------------------------------------------------------ glue

#[cfg(feature = "python")]
fn meta_err(py: Python<'_>, e: MetaError) -> PyErr {
    api_error(py, e.0, e.1)
}

#[cfg(feature = "python")]
fn content_err(py: Python<'_>, e: ContentError) -> PyErr {
    match e {
        ContentError::Meta(m) => meta_err(py, m),
        ContentError::Document => document_error(py, "Belgenin yapısı güvenli düzenleme için uygun değil."),
    }
}

#[cfg(feature = "python")]
fn raw_of(py: Python<'_>, raw: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    match raw.cast::<PyBytes>() {
        Ok(b) => Ok(b.as_bytes().to_vec()),
        Err(_) => Err(meta_err(py, invalid("Klasör yanıtı güvenle okunamıyor."))),
    }
}

#[cfg(feature = "python")]
fn node_py<'py>(py: Python<'py>, n: &Node) -> PyResult<Bound<'py, PyDict>> {
    dict(py, &[
        ("id", s(py, &n.id)),
        ("type", s(py, &n.kind)),
        ("parent_id", s(py, &n.parent_id)),
        ("name", s(py, &n.name)),
        ("state", s(py, &n.state)),
        ("sync_modified_time", int(py, n.sync_modified_time)),
        ("created_time", int(py, n.created_time)),
        ("modified_time", int(py, n.modified_time)),
        ("trashed_time", int(py, n.trashed_time)),
        ("restore_path", s(py, &n.restore_path)),
        ("color", s(py, &n.color)),
        ("order", s(py, &n.order)),
        ("raw_xml", s(py, &n.raw_xml)),
    ])
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "parse_folder_changes")]
fn parse_folder_changes_py<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let parsed = parse_folder_changes(&raw_of(py, raw)?).map_err(|e| meta_err(py, e))?;
    let nodes = PyList::empty(py);
    for n in &parsed.nodes {
        nodes.append(node_py(py, n)?)?;
    }
    dict(py, &[
        ("nodes", nodes.into_any()),
        ("last_change_point", s(py, &parsed.last_change_point)),
        ("has_resource", boolean(py, parsed.has_resource)),
        ("sync_modified_time", parsed.sync_modified_time.map(|t| int(py, t)).unwrap_or_else(|| none(py))),
        ("encrypted", boolean(py, parsed.encrypted)),
    ])
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "parse_note_list")]
fn parse_note_list_py<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let (notes, token) = parse_note_list(&raw_of(py, raw)?).map_err(|e| meta_err(py, e))?;
    let list = PyList::empty(py);
    let opt = |v: &Option<String>| v.as_ref().map(|x| s(py, x)).unwrap_or_else(|| none(py));
    for n in &notes {
        list.append(dict(py, &[
            ("server_id", s(py, &n.server_id)),
            ("resource_id", s(py, &n.resource_id)),
            ("is_deleted", boolean(py, n.is_deleted)),
            ("commit_id", opt(&n.commit_id)),
            ("sync_modified_time", int(py, n.sync_modified_time)),
            ("sdk_version", opt(&n.sdk_version)),
            ("is_updated_by_device", boolean(py, n.is_updated_by_device)),
        ])?)?;
    }
    dict(py, &[("notes", list.into_any()), ("note_page_token", opt(&token))])
}

#[cfg(feature = "python")]
fn snapshot_from<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if snapshot.is_instance_of::<PyDict>() {
        crate::areas::live(py, "samsung_notes.document", "DocumentSnapshot")?.call_method1("from_dict", (snapshot,))
    } else {
        Ok(snapshot.clone())
    }
}

#[cfg(feature = "python")]
fn resource_of(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    raw_of(py, &snapshot.getattr("resource_xml")?)
}

#[cfg(feature = "python")]
/// document_organization -> (favorite, state)
fn organization(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<(bool, String)> {
    let root = xml(&resource_of(py, snapshot)?).map_err(|e| meta_err(py, e))?;
    let attrs = one(&root, "attributes").map_err(|e| content_err(py, e))?;
    let value = attrs.get("isFavorite").unwrap_or("false");
    if value != "true" && value != "false" {
        return Err(api_error(py, "invalid_metadata", "Favori durumu geçersiz."));
    }
    Ok((value == "true", attrs.get("state").unwrap_or("normal").to_string()))
}

#[cfg(feature = "python")]
#[pyfunction]
fn document_organization<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let snapshot = snapshot_from(py, snapshot)?;
    let (favorite, state) = organization(py, &snapshot)?;
    dict(py, &[("favorite", boolean(py, favorite)), ("state", s(py, &state))])
}

#[cfg(feature = "python")]
fn request_patch<'py>(api: &Bound<'py, PyAny>, url: &str, xml: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let headers = api.call_method1("_headers", (true,))?;
    headers.call_method1("update", (dict(py, &[("Content-Type", s(py, "application/xml;Charset=utf-8")), ("Content-Encoding", s(py, "gzip"))])?,))?;
    request(py, url, Some(&util::gzip_mtime0(xml)), Some("PATCH"), &headers)
}

#[cfg(feature = "python")]
fn get<'py>(o: &Bound<'py, PyAny>, k: &str) -> PyResult<Bound<'py, PyAny>> {
    o.get_item(k)
}

#[cfg(feature = "python")]
fn node_from_raw(py: Python<'_>, current: &Bound<'_, PyAny>) -> PyResult<Element> {
    let raw = str_of(&current.get_item("raw_xml")?)?;
    xml(raw.as_bytes()).map_err(|e| meta_err(py, e))
}

#[cfg(feature = "python")]
fn to_str_value(v: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(v.str()?.to_string())
}

#[cfg(feature = "python")]
fn folder_name_check(py: Python<'_>, name: &Bound<'_, PyAny>) -> PyResult<String> {
    let bad = || api_error(py, "invalid_folder_name", "Klasör adı boş olamaz veya yol ayırıcı içeremez.");
    let text = match name.cast::<PyString>() {
        Ok(s) => s.to_str().map_err(|_| bad())?.to_string(),
        Err(_) => return Err(bad()),
    };
    if !util::is_safe_string(&text, 255) || util::py_strip(&text).is_empty() || text.contains('/') || text.contains('\\') {
        return Err(bad());
    }
    Ok(text)
}

#[cfg(feature = "python")]
#[pyclass(subclass, dict, module = "samsung_notes.cloud_metadata", name = "CloudMetadata")]
pub struct CloudMetadata;

#[cfg(feature = "python")]
impl CloudMetadata {
    #[cfg(feature = "python")]
    fn validate_parent(py: Python<'_>, nodes: &Bound<'_, PyAny>, parent_id: &Bound<'_, PyAny>, identifier: &Bound<'_, PyAny>) -> PyResult<()> {
        let seen = pyo3::types::PySet::new(py, [identifier])?;
        let mut parent_id = parent_id.clone();
        while parent_id.ne("root")? {
            let parent = nodes.call_method1("get", (&parent_id,))?;
            if seen.contains(&parent_id)? || seen.len() >= 64 || parent.is_none() || get(&parent, "type")?.ne("folder")? || get(&parent, "state")?.ne("normal")? {
                return Err(conflict(py, Some("Hedef klasör kullanılamıyor veya döngü oluşturuyor.")));
            }
            seen.add(&parent_id)?;
            parent_id = get(&parent, "parent_id")?;
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn folder_path(py: Python<'_>, nodes: &Bound<'_, PyAny>, parent: &Bound<'_, PyAny>) -> PyResult<String> {
        if parent.eq("root")? {
            return Ok("uncategorized:///".into());
        }
        let mut names: Vec<String> = Vec::new();
        let seen = pyo3::types::PySet::empty(py)?;
        let mut parent = parent.clone();
        while parent.ne("root")? {
            if seen.contains(&parent)? || !nodes.contains(&parent)? || seen.len() >= 64 {
                return Err(conflict(py, Some("Klasör ağacı tutarsız.")));
            }
            seen.add(&parent)?;
            let node = nodes.get_item(&parent)?;
            if get(&node, "type")?.ne("folder")? || get(&node, "state")?.ne("normal")? {
                return Err(conflict(py, None));
            }
            names.push(folder_name_check(py, &get(&node, "name")?)?);
            parent = get(&node, "parent_id")?;
        }
        names.reverse();
        Ok(format!("uncategorized:///{}", names.join("/")))
    }

    #[cfg(feature = "python")]
    fn restore_parent(py: Python<'_>, nodes: &Bound<'_, PyAny>, path: &str) -> PyResult<String> {
        if path.is_empty() || path == "uncategorized:///" {
            return Ok("root".into());
        }
        let mut found = Vec::new();
        for node in nodes.call_method0("values")?.try_iter()? {
            let node = node?;
            if get(&node, "type")?.eq("folder")? && get(&node, "state")?.eq("normal")? && CloudMetadata::folder_path(py, nodes, &get(&node, "id")?)? == path {
                found.push(str_of(&get(&node, "id")?)?);
            }
        }
        if found.len() != 1 {
            return Err(conflict(py, Some("Özgün geri yükleme klasörü bulunamadı veya birden fazla eşleşme var.")));
        }
        Ok(found.remove(0))
    }

    #[cfg(feature = "python")]
    fn graph_nodes<'py>(py: Python<'py>, graph: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let nodes = PyDict::new(py);
        for n in graph.get_item("nodes")?.try_iter()? {
            let n = n?;
            nodes.set_item(n.get_item("id")?, &n)?;
        }
        Ok(nodes)
    }

    #[cfg(feature = "python")]
    fn send<'py>(api: &Bound<'py, PyAny>, output: &Bound<'py, PyAny>, request: &Bound<'py, PyAny>, expected: i64) -> PyResult<Bound<'py, PyAny>> {
        let py = api.py();
        let out = fspath(output)?;
        private_write(&out.join(".mutation-claim"), b"claimed; never automatically repeat\n")?;
        let called = api.call_method1("_call", (output, "publish", request)).and_then(|r| {
            let items: Vec<Bound<'py, PyAny>> = r.try_iter()?.collect::<PyResult<_>>()?;
            if items.len() != 3 {
                return Err(pyo3::exceptions::PyValueError::new_err("expected 3 values to unpack"));
            }
            Ok(items)
        });
        let items = try_map(py, called, &[K::OS, K::Value, K::Api], || uncertain(py, "write_uncertain", "Düzenleme sonucu belirsiz; tekrar gönderilmedi."))?;
        let (status, raw) = (&items[0], &items[1]);
        let code = status_code(status);
        if code != Some(expected) || raw.is_none() {
            if code == Some(409) {
                return Err(conflict(py, None));
            }
            if matches!(code, Some(400) | Some(401) | Some(403) | Some(404) | Some(413) | Some(422)) {
                return Err(api_error(py, "write_rejected", "Sunucu düzenlemeyi kabul etmedi."));
            }
            return Err(uncertain(py, "write_uncertain", "Düzenleme sonucu belirsiz; tekrar gönderilmedi."));
        }
        save_json(&out.join("published.json"), dict(py, &[("httpStatus", status.clone())])?.as_any())?;
        Ok(raw.clone())
    }

    #[cfg(feature = "python")]
    fn folder_resource(now: i64, graph_time: &Bound<'_, PyAny>, node: Element) -> PyResult<Vec<u8>> {
        let mut root = Element::new("FolderResource");
        let base = if graph_time.is_truthy()? { graph_time.extract::<i64>()? } else { 0 };
        set_child(&mut root, "syncModifiedTime", &now.max(base).to_string()).ok();
        let mut list = Element::new("nodeList");
        list.children.push(node);
        root.children.push(list);
        Ok(etree::tostring_utf8_decl(&root))
    }

    #[cfg(feature = "python")]
    fn publish_folder<'py>(slf: &Bound<'py, PyAny>, output: &Bound<'py, PyAny>, mut node: Element, graph: &Bound<'py, PyAny>, now: i64) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let api = slf.getattr("api")?;
        set_child(&mut node, "syncModifiedTime", &now.to_string()).map_err(|e| meta_err(py, e))?;
        set_child(&mut node, "requestTime", &now.to_string()).map_err(|e| meta_err(py, e))?;
        let values = children(&node).map_err(|e| meta_err(py, e))?;
        let state = field(&values, "state").ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("state"))?.to_string();
        let mut device = vec![("model", Value::s("Linux Notes")), ("ver", Value::s("0.1"))];
        if state != "normal" {
            device.push(("deviceName", Value::Str(b64("Linux Notes"))));
            device.push(("deviceId", pyjson::from_py(&api.call_method0("_cloud")?.get_item("dvcId")?)?));
        }
        set_child(&mut node, "requestDevice", &pyjson::dumps(&Value::obj(device), &pyjson::COMPACT)).map_err(|e| meta_err(py, e))?;
        let node_id = node.get("id").map(str::to_string);
        let final_values = children(&node).map_err(|e| meta_err(py, e))?;
        let xml = CloudMetadata::folder_resource(now, &graph.get_item("sync_modified_time")?, node)?;
        save_once(py, &fspath(output)?.join("request.xml"), &xml)?;
        let url = format!("{}/samsungnotes/v3/folder", str_of(&api.getattr("base")?)?);
        let request = request_patch(&api, &url, &xml)?;
        CloudMetadata::send(&api, output, &request, 200)?;
        let verified = (|| -> PyResult<Bound<'py, PyAny>> {
            let after = slf.call_method0("inventory")?;
            let mut found = None;
            for n in after.get_item("nodes")?.try_iter()? {
                let n = n?;
                if n.get_item("id")?.eq(node_id.clone())? {
                    found = Some(n);
                    break;
                }
            }
            let found = found.ok_or_else(|| pyo3::exceptions::PyStopIteration::new_err(()))?;
            let name = decode(field(&final_values, "name").ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("name"))?).map_err(|e| meta_err(py, e))?;
            if found.get_item("type")?.ne("folder")?
                || found.get_item("state")?.ne(field(&final_values, "state"))?
                || found.get_item("name")?.ne(name)?
                || found.get_item("sync_modified_time")?.ne(now)?
                || found.get_item("parent_id")?.ne(field(&final_values, "parentfoldernodeid").ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("parentfoldernodeid"))?)?
            {
                return Err(value_error());
            }
            Ok(found)
        })();
        let found = try_map(py, verified, &[K::Api, K::Value, K::StopIter], || uncertain(py, "write_uncertain", "Klasör düzenlemesi gönderildi; geri okuma henüz doğrulanamadı."))?;
        let result = dict(py, &[("node", found), ("verified", boolean(py, true))])?;
        save_json(&fspath(output)?.join("result.json"), result.as_any())?;
        Ok(result.into_any())
    }

    #[cfg(feature = "python")]
    fn favorite<'py>(slf: &Bound<'py, PyAny>, output: &Bound<'py, PyAny>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, favorite: bool) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let api = slf.getattr("api")?;
        let out = fspath(output)?;
        let original = resource_of(py, snapshot)?;
        let mut resource = xml(&original).map_err(|e| meta_err(py, e))?;
        if organization(py, snapshot)?.0 == favorite {
            let result = dict(py, &[
                ("server_id", server_id.clone()),
                ("local_id", snapshot.getattr("local_id")?),
                ("commit_id", snapshot.getattr("commit_id")?),
                ("snapshot", snapshot.call_method0("to_dict")?),
                ("favorite", boolean(py, favorite)),
                ("verified", boolean(py, true)),
            ])?;
            save_json(&out.join("result.json"), result.as_any())?;
            return Ok(result.into_any());
        }
        let now = millis(&api.call_method0("_clock")?)?;
        if now <= number(resource.get("syncModifiedTime")).map_err(|e| meta_err(py, e))? {
            return Err(api_error(py, "clock_skew", "Yerel saat son Cloud düzenlemesinden ileride olmalı."));
        }
        let tree = one(&resource, "notetree").map_err(|e| content_err(py, e))?.clone();
        let ai = one_index(&resource, "attributes").map_err(|e| content_err(py, e))?;
        resource.children[ai].set("isFavorite", if favorite { "true" } else { "false" });
        resource.set("syncModifiedTime", &now.to_string());
        resource.tag = "NoteResource".into();
        let mut kept = Vec::new();
        for mut element in std::mem::take(&mut resource.children) {
            match tag(&element).as_str() {
                "bodytext" | "floatingobjectlist" => {}
                "pagelist" | "contentfilelist" => {
                    element.clear();
                    kept.push(element);
                }
                "notetree" | "attributes" => kept.push(element),
                _ => return Err(api_error(py, "unsupported_metadata", "Bilinmeyen belge alanı; güvenli favori değişikliği yapılamıyor.")),
            }
        }
        resource.children = kept;
        let tree_payload = util::gzip_mtime0(&etree::tostring_ascii(&tree));
        let headers = api.call_method1("_headers", (true,))?;
        headers.call_method1("update", (dict(py, &[("Content-Type", s(py, "application/xml;Charset=utf-8")), ("Content-Encoding", s(py, "gzip"))])?,))?;
        let compare_url = str_of(&api.call_method1("_url", (server_id,))?)? + "/compareForUpsync";
        let compare = request(py, &compare_url, Some(&tree_payload), Some("POST"), &headers)?;
        let attempt = out.join(util::uuid4());
        util::mkdir_private(&attempt, false)?;
        let result = api.call_method1("_call", (py_path(py, &attempt)?, "compare", &compare))?;
        let (status, raw) = (result.get_item(0)?, result.get_item(1)?);
        api.call_method1("_read_status", (&status, &raw))?;
        let guard = api.call_method1("_compare_guard", (&raw, snapshot.getattr("commit_id")?, pyo3::types::PySet::empty(py)?));
        api_mod::guarded(py, guard, "Sunucunun karşılaştırma yanıtı güvenli düzenlemeye uygun değil.")?;
        let xml_bytes = etree::tostring_utf8_decl(&resource);
        save_once(py, &out.join("request.xml"), &xml_bytes)?;
        let (payload, ctype) = rs_doc::doc::multipart(&xml_bytes);
        headers.set_item("Content-Type", ctype)?;
        let url = str_of(&api.call_method1("_url", (server_id,))?)? + "/update";
        let request = request(py, &url, Some(&payload), Some("POST"), &headers)?;
        let raw = CloudMetadata::send(&api, output, &request, 201)?;
        let checked = (|| -> PyResult<Bound<'py, PyAny>> {
            let published = api_mod::default_view_py(py, &raw)?;
            let after = api.call_method1("get_note", (server_id,))?;
            let same_content = match (document_content(&resource_of(py, &after)?), document_content(&original)) {
                (Ok(a), Ok(b)) => a == b,
                (Err(e), _) | (_, Err(e)) => return Err(content_err(py, e)),
            };
            if published.get_item("resourceId")?.ne(server_id)?
                || after.getattr("local_id")?.ne(snapshot.getattr("local_id")?)?
                || api_mod::normalize_commit(py, &after.getattr("commit_id")?)? != api_mod::normalize_commit(py, &published.get_item("commitId")?)?
                || organization(py, &after)?.0 != favorite
                || !same_content
            {
                return Err(value_error());
            }
            Ok(after)
        })();
        let after = try_map(py, checked, &[K::Value, K::Key, K::Api], || uncertain(py, "write_uncertain", "Favori değişikliği gönderildi; geri okuma henüz doğrulanamadı."))?;
        let result = dict(py, &[
            ("server_id", server_id.clone()),
            ("local_id", snapshot.getattr("local_id")?),
            ("commit_id", after.getattr("commit_id")?),
            ("snapshot", after.call_method0("to_dict")?),
            ("favorite", boolean(py, favorite)),
            ("verified", boolean(py, true)),
        ])?;
        save_json(&out.join("result.json"), result.as_any())?;
        Ok(result.into_any())
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl CloudMetadata {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        CloudMetadata
    }

    #[cfg(feature = "python")]
    fn __init__(slf: &Bound<'_, Self>, api: &Bound<'_, PyAny>) -> PyResult<()> {
        slf.as_any().setattr("api", api)?;
        slf.as_any().setattr("initial_cursor", initial_folder_cursor())
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (cursor = None))]
    fn folder_changes<'py>(slf: &Bound<'py, Self>, cursor: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let api = this.getattr("api")?;
        let cursor = match cursor {
            Some(c) if !c.is_none() => c.clone(),
            _ => this.getattr("initial_cursor")?,
        };
        if !is_string(&cursor, 65536) {
            return Err(api_error(py, "invalid_cursor", "Klasör senkronizasyon konumu geçersiz."));
        }
        let headers = api.call_method0("_headers")?;
        let url = format!("{}/samsungnotes/v3/folder/changes?{}", str_of(&api.getattr("base")?)?, util::urlencode(&[("lastChangePoint", str_of(&cursor)?)]));
        let request = request(py, &url, None, Some("GET"), &headers)?;
        let output = api.call_method1("_directory", ("metadata-reads",))?;
        save_json(&fspath(&output)?.join("request.json"), dict(py, &[
            ("schemaVersion", int(py, 1)),
            ("kind", s(py, "folder_changes")),
            ("accountFingerprint", api.getattr("_identity")?),
            ("cursor", cursor.clone()),
        ])?.as_any())?;
        let result = api.call_method("_call", (&output, "folders", &request), Some(&dict(py, &[("limit", int(py, MAX_XML as i64))])?))?;
        let (status, raw) = (result.get_item(0)?, result.get_item(1)?);
        api.call_method1("_read_status", (&status, &raw))?;
        let parsed = parse_folder_changes_py(py, &raw)?;
        parsed.set_item("account_fingerprint", api.getattr("_identity")?)?;
        Ok(parsed.into_any())
    }

    #[cfg(feature = "python")]
    fn inventory<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        slf.as_any().call_method0("folder_changes")
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (*, modified_after = None, page_token = None))]
    fn note_list<'py>(slf: &Bound<'py, Self>, modified_after: Option<&Bound<'py, PyAny>>, page_token: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let api = slf.as_any().getattr("api")?;
        let modified_after = modified_after.cloned().unwrap_or_else(|| int(py, 1464416139848));
        let ok = modified_after.get_type().is(py.get_type::<PyInt>()) && modified_after.extract::<i128>().map(|v| 0 <= v && v < (1i128 << 63)).unwrap_or(false);
        if !ok {
            return Err(api_error(py, "invalid_cursor", "Not envanteri zamanı geçersiz."));
        }
        let query = dict(py, &[("modifiedAfter", modified_after.clone())])?;
        let mut pairs = vec![("modifiedAfter", modified_after.str()?.to_string())];
        if let Some(t) = page_token.filter(|t| !t.is_none()) {
            if !is_string(t, 65536) {
                return Err(api_error(py, "invalid_cursor", "Not sayfası konumu geçersiz."));
            }
            query.set_item("notePageToken", t)?;
            pairs.push(("notePageToken", str_of(t)?));
        }
        let url = format!("{}/samsungnotes/v3/notes/list?{}", str_of(&api.getattr("base")?)?, util::urlencode(&pairs));
        let headers = api.call_method0("_headers")?;
        let request = request(py, &url, None, Some("GET"), &headers)?;
        let output = api.call_method1("_directory", ("metadata-reads",))?;
        save_json(&fspath(&output)?.join("request.json"), dict(py, &[
            ("schemaVersion", int(py, 1)),
            ("kind", s(py, "note_list")),
            ("accountFingerprint", api.getattr("_identity")?),
            ("query", query.into_any()),
        ])?.as_any())?;
        let result = api.call_method("_call", (&output, "note-list", &request), Some(&dict(py, &[("limit", int(py, MAX_XML as i64))])?))?;
        let (status, raw) = (result.get_item(0)?, result.get_item(1)?);
        api.call_method1("_read_status", (&status, &raw))?;
        Ok(parse_note_list_py(py, &raw)?.into_any())
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (*, max_pages = None))]
    fn note_inventory<'py>(slf: &Bound<'py, Self>, max_pages: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let max_pages = max_pages.cloned().unwrap_or_else(|| int(py, 256));
        let pages = if max_pages.get_type().is(py.get_type::<PyInt>()) { max_pages.extract::<i64>().ok().filter(|n| (1..=256).contains(n)) } else { None };
        let Some(pages) = pages else { return Err(api_error(py, "invalid_limit", "Envanter sınırı geçersiz.")) };
        let mut token = none(py);
        let seen = pyo3::types::PySet::empty(py)?;
        let notes = PyDict::new(py);
        for _ in 0..pages {
            let page = this.call_method("note_list", (), Some(&dict(py, &[("page_token", token.clone())])?))?;
            for note in page.get_item("notes")?.try_iter()? {
                let note = note?;
                let key = note.get_item("server_id")?;
                if let Some(old) = notes.get_item(&key)? {
                    if old.ne(&note)? {
                        return Err(conflict(py, Some("Not envanteri okuma sırasında değişti.")));
                    }
                }
                notes.set_item(&key, &note)?;
            }
            if notes.len() > MAX_NODES {
                return Err(api_error(py, "inventory_limit", "Not envanteri sınırı aşıldı."));
            }
            token = page.get_item("note_page_token")?;
            if !token.is_truthy()? {
                let list = PyList::new(py, notes.values().iter())?;
                return Ok(dict(py, &[("notes", list.into_any()), ("complete", boolean(py, true))])?.into_any());
            }
            if seen.contains(&token)? {
                return Err(api_error(py, "invalid_cursor", "Not sayfası konumu tekrarlanıyor."));
            }
            seen.add(&token)?;
        }
        Err(api_error(py, "inventory_limit", "Not envanteri tamamlanamadı; önceki kayıtlar korunuyor."))
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _folder_name(py: Python<'_>, name: &Bound<'_, PyAny>) -> PyResult<String> {
        folder_name_check(py, name)
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _validate_parent(py: Python<'_>, nodes: &Bound<'_, PyAny>, parent_id: &Bound<'_, PyAny>, identifier: &Bound<'_, PyAny>) -> PyResult<()> {
        CloudMetadata::validate_parent(py, nodes, parent_id, identifier)
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _restore_parent(py: Python<'_>, nodes: &Bound<'_, PyAny>, path: &str) -> PyResult<String> {
        CloudMetadata::restore_parent(py, nodes, path)
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _folder_path(py: Python<'_>, nodes: &Bound<'_, PyAny>, parent: &Bound<'_, PyAny>) -> PyResult<String> {
        CloudMetadata::folder_path(py, nodes, parent)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (name, parent_id = None, *, operation_id, folder_id = None))]
    fn create_folder<'py>(slf: &Bound<'py, Self>, name: &Bound<'py, PyAny>, parent_id: Option<&Bound<'py, PyAny>>, operation_id: &Bound<'py, PyAny>, folder_id: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let name = folder_name_check(py, name)?;
        let parent_id = parent_id.cloned().unwrap_or_else(|| s(py, "root"));
        if !is_string(&parent_id, 1024) {
            return Err(api_error(py, "invalid_metadata_change", "Üst klasör geçersiz."));
        }
        let folder_id = match folder_id.filter(|f| !f.is_none()) {
            Some(f) => check_uuid(py, f)?,
            None => util::uuid5(&check_uuid(py, operation_id)?, "folder"),
        };
        let intent = dict(py, &[("kind", s(py, "create_folder")), ("folder_id", s(py, &folder_id)), ("name", s(py, &name)), ("parent_id", parent_id.clone())])?;
        let op = this.call_method1("_operation", (operation_id, intent))?;
        let (output, replay) = (op.get_item(0)?, op.get_item(1)?);
        if !replay.is_none() {
            return Ok(replay);
        }
        let graph = this.call_method0("inventory")?;
        let nodes = CloudMetadata::graph_nodes(py, &graph)?;
        if nodes.contains(&folder_id)? {
            return Err(conflict(py, Some("Klasör kimliği Cloud üzerinde zaten var.")));
        }
        CloudMetadata::validate_parent(py, nodes.as_any(), &parent_id, &s(py, &folder_id))?;
        let now = millis(&this.getattr("api")?.call_method0("_clock")?)?;
        let mut node = Element::new("node");
        node.set("id", &folder_id);
        node.set("type", "folder");
        let now_s = now.to_string();
        for (k, v) in [
            ("parentFolderNodeId", str_of(&parent_id)?),
            ("createdTime", now_s.clone()),
            ("lastModifiedTime", now_s.clone()),
            ("name", b64(&name)),
            ("state", "normal".into()),
            ("trashedTime", "0".into()),
            ("color", "0".into()),
            ("order", "0".into()),
        ] {
            set_child(&mut node, k, &v).map_err(|e| meta_err(py, e))?;
        }
        CloudMetadata::publish_folder(this, &output, node, &graph, now)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (folder_id, changes, *, baseline_node, operation_id))]
    fn update_folder<'py>(slf: &Bound<'py, Self>, folder_id: &Bound<'py, PyAny>, changes: &Bound<'py, PyAny>, baseline_node: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let single = changes.cast::<PyDict>().ok().filter(|d| d.len() == 1);
        let Some(changes_d) = (if is_string(folder_id, 1024) { single } else { None }) else {
            return Err(api_error(py, "invalid_metadata_change", "Tek klasör düzenlemesi gerekli."));
        };
        let (key, value) = changes_d.iter().next().unwrap();
        let key = opt_str(&key).unwrap_or_default();
        let mut value = value;
        match key.as_str() {
            "name" => value = s(py, &folder_name_check(py, &value)?),
            "parent_id" => {
                if !is_string(&value, 1024) {
                    return Err(api_error(py, "invalid_metadata_change", "Üst klasör geçersiz."));
                }
            }
            "trashed" => {
                if !value.get_type().is(py.get_type::<pyo3::types::PyBool>()) {
                    return Err(api_error(py, "invalid_metadata_change", "Çöp kutusu durumu geçersiz."));
                }
            }
            _ => return Err(api_error(py, "invalid_metadata_change", "Desteklenmeyen klasör düzenlemesi.")),
        }
        let intent = dict(py, &[("kind", s(py, "update_folder")), ("folder_id", folder_id.clone()), ("changes", changes.clone()), ("baseline_node", baseline_node.clone())])?;
        let op = this.call_method1("_operation", (operation_id, intent))?;
        let (output, replay) = (op.get_item(0)?, op.get_item(1)?);
        if !replay.is_none() {
            return Ok(replay);
        }
        let graph = this.call_method0("inventory")?;
        let nodes = CloudMetadata::graph_nodes(py, &graph)?;
        let current = nodes.as_any().call_method1("get", (folder_id,))?;
        if current.ne(baseline_node)? || !current.is_truthy()? || current.get_item("type")?.ne("folder")? || current.get_item("state")?.eq("deleted")? {
            return Err(conflict(py, Some("Klasör başka bir cihazda değişmiş.")));
        }
        let applied = match key.as_str() {
            "name" => current.get_item("name")?.eq(&value)?,
            "parent_id" => current.get_item("parent_id")?.eq(&value)?,
            _ => current.get_item("state")?.eq("trashed")? == value.is_truthy()?,
        };
        if applied {
            let result = dict(py, &[("node", current.clone()), ("verified", boolean(py, true))])?;
            save_json(&fspath(&output)?.join("result.json"), result.as_any())?;
            return Ok(result.into_any());
        }
        let mut node = node_from_raw(py, &current)?;
        let now = millis(&this.getattr("api")?.call_method0("_clock")?)?;
        if int(py, now).le(current.get_item("sync_modified_time")?)? {
            return Err(api_error(py, "clock_skew", "Yerel saat Cloud düzenlemesinden ileride olmalı."));
        }
        if (key == "name" || key == "parent_id") && current.get_item("state")?.ne("normal")? {
            return Err(conflict(py, Some("Çöp kutusundaki klasör önce geri yüklenmeli.")));
        }
        let set = |node: &mut Element, k: &str, v: &str| set_child(node, k, v).map_err(|e| meta_err(py, e));
        match key.as_str() {
            "name" => set(&mut node, "name", &b64(&str_of(&value)?))?,
            "parent_id" => {
                CloudMetadata::validate_parent(py, nodes.as_any(), &value, folder_id)?;
                set(&mut node, "parentFolderNodeId", &str_of(&value)?)?;
            }
            _ => {
                let trash = value.is_truthy()?;
                for n in graph.get_item("nodes")?.try_iter()? {
                    let n = n?;
                    if n.get_item("parent_id")?.eq(folder_id)? && n.get_item("state")?.ne("deleted")? {
                        return Err(api_error(py, "folder_not_empty", "Bu sürüm yalnız boş klasörleri çöp kutusuna taşıyabiliyor."));
                    }
                }
                set(&mut node, "state", if trash { "trashed" } else { "normal" })?;
                set(&mut node, "trashedTime", &if trash { now.to_string() } else { "0".into() })?;
                if trash {
                    let path = CloudMetadata::folder_path(py, nodes.as_any(), folder_id)?;
                    set(&mut node, "restorePath", &b64(&path))?;
                    set(&mut node, "parentFolderNodeId", "root")?;
                } else {
                    let path = str_of(&current.get_item("restore_path")?)?;
                    let suffix = str_of(&current.get_item("name")?)?;
                    if !path.ends_with(&suffix) {
                        return Err(conflict(py, Some("Klasörün geri yükleme yolu geçersiz.")));
                    }
                    // Python `path[:-len(suffix)]` (empty suffix gives '').
                    let mut parent_path = if suffix.is_empty() { String::new() } else { path[..path.len() - suffix.len()].to_string() };
                    if parent_path != "uncategorized:///" {
                        parent_path = parent_path.trim_end_matches('/').to_string();
                    }
                    let parent = CloudMetadata::restore_parent(py, nodes.as_any(), &parent_path)?;
                    set(&mut node, "parentFolderNodeId", &parent)?;
                    node.children.retain(|c| tag(c) != "restorepath");
                }
            }
        }
        set(&mut node, "lastModifiedTime", &now.to_string())?;
        CloudMetadata::publish_folder(this, &output, node, &graph, now)
    }

    #[cfg(feature = "python")]
    fn _operation<'py>(slf: &Bound<'py, Self>, operation_id: &Bound<'py, PyAny>, intent: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let py = slf.py();
        let api = slf.as_any().getattr("api")?;
        api.call_method0("_validate_identity")?;
        let root = fspath(&api.getattr("work_dir")?)?.join("metadata-operations");
        util::mkdir_private(&root, true)?;
        let output = root.join(check_uuid(py, operation_id)?);
        let value = PyDict::new(py);
        for (k, v) in intent.cast::<PyDict>()?.iter() {
            value.set_item(k, v)?;
        }
        value.set_item("schemaVersion", 1)?;
        value.set_item("accountFingerprint", api.getattr("_identity")?)?;
        let out_py = py_path(py, &output)?;
        if output.exists() {
            if load_json(py, &output.join("intent.json"), MAX_XML)?.ne(&value)? {
                return Err(api_error(py, "operation_mismatch", "İşlem farklı bir düzenlemeye ait."));
            }
            if output.join("result.json").exists() {
                return PyTuple::new(py, [out_py, load_json(py, &output.join("result.json"), RESULT_LIMIT)?]);
            }
            if output.join(".mutation-claim").exists() {
                return Err(uncertain(py, "write_uncertain", "İşlem daha önce gönderilmiş olabilir; tekrar gönderilmedi."));
            }
        } else {
            util::mkdir_private(&output, false)?;
            save_json(&output.join("intent.json"), value.as_any())?;
        }
        PyTuple::new(py, [out_py, none(py)])
    }

    #[cfg(feature = "python")]
    #[pyo3(signature = (server_id, snapshot, changes, *, operation_id, baseline_node = None))]
    fn update_organization<'py>(slf: &Bound<'py, Self>, server_id: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>, changes: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>, baseline_node: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.as_any();
        let api = this.getattr("api")?;
        let baseline_node = baseline_node.cloned().unwrap_or_else(|| none(py));
        let snapshot = snapshot_from(py, snapshot)?;
        let cls = crate::areas::live(py, "samsung_notes.document", "DocumentSnapshot")?;
        let single = changes.cast::<PyDict>().ok().filter(|d| d.len() == 1);
        if !snapshot.is_instance(&cls)? || snapshot.getattr("server_id")?.ne(server_id)? || single.is_none() {
            return Err(conflict(py, None));
        }
        let (key, value) = single.unwrap().iter().next().unwrap();
        let key = opt_str(&key).unwrap_or_default();
        let is_bool = value.get_type().is(py.get_type::<pyo3::types::PyBool>());
        let valid = match key.as_str() {
            "favorite" | "trashed" => is_bool,
            "folder_id" => is_string(&value, 1024),
            _ => false,
        };
        if !valid {
            return Err(api_error(py, "invalid_metadata_change", "Desteklenmeyen düzenleme isteği."));
        }
        let local_id = snapshot.getattr("local_id")?;
        let intent = dict(py, &[
            ("kind", s(py, "organization")),
            ("server_id", server_id.clone()),
            ("local_id", local_id.clone()),
            ("baseline_commit", snapshot.getattr("commit_id")?),
            ("changes", changes.clone()),
            ("snapshot_sha256", s(py, &api_mod::resource_sha(&snapshot)?)),
            ("baseline_node", baseline_node.clone()),
        ])?;
        let op = this.call_method1("_operation", (operation_id, intent))?;
        let (output, replay) = (op.get_item(0)?, op.get_item(1)?);
        if !replay.is_none() {
            return Ok(replay);
        }
        let fresh = api.call_method1("get_note", (server_id,))?;
        if fresh.getattr("local_id")?.ne(&local_id)?
            || fresh.getattr("resource_xml")?.ne(snapshot.getattr("resource_xml")?)?
            || api_mod::normalize_commit(py, &fresh.getattr("commit_id")?)? != api_mod::normalize_commit(py, &snapshot.getattr("commit_id")?)?
        {
            return Err(conflict(py, None));
        }
        let root = xml(&resource_of(py, &fresh)?).map_err(|e| meta_err(py, e))?;
        if ["isLocked", "isEncrypted"].iter().any(|k| root.get(k).unwrap_or("false") != "false") {
            return Err(api_error(py, "protected_metadata", "Kilitli veya şifreli not henüz düzenlenemiyor."));
        }
        if key == "favorite" {
            return CloudMetadata::favorite(this, &output, server_id, &fresh, value.is_truthy()?);
        }
        let graph = this.call_method0("inventory")?;
        let nodes = CloudMetadata::graph_nodes(py, &graph)?;
        let current = nodes.as_any().call_method1("get", (&local_id,))?;
        let has_current = current.is_truthy()?;
        if current.ne(&baseline_node)? || (has_current && (current.get_item("type")?.ne("note")? || current.get_item("state")?.eq("deleted")?)) {
            return Err(conflict(py, Some("Notun klasör veya çöp kutusu durumu değişmiş.")));
        }
        if has_current {
            let applied = if key == "folder_id" { current.get_item("parent_id")?.eq(&value)? } else { current.get_item("state")?.eq("trashed")? == value.is_truthy()? };
            if applied {
                let result = dict(py, &[
                    ("server_id", server_id.clone()),
                    ("local_id", local_id.clone()),
                    ("commit_id", fresh.getattr("commit_id")?),
                    ("snapshot", fresh.call_method0("to_dict")?),
                    ("node", current.clone()),
                    ("verified", boolean(py, true)),
                ])?;
                save_json(&fspath(&output)?.join("result.json"), result.as_any())?;
                return Ok(result.into_any());
            }
        }
        let set = |node: &mut Element, k: &str, v: &str| set_child(node, k, v).map_err(|e| meta_err(py, e));
        let mut node = if current.is_none() {
            let mut n = Element::new("node");
            n.set("id", &str_of(&local_id)?);
            n.set("type", "note");
            let or_zero = |v: Bound<'py, PyAny>| -> PyResult<String> { if v.is_truthy()? { to_str_value(&v) } else { Ok("0".into()) } };
            let title = fresh.getattr("title")?;
            let title = if title.is_truthy()? { str_of(&title)? } else { " ".into() };
            for (k, v) in [
                ("parentFolderNodeId", "root".to_string()),
                ("createdTime", or_zero(fresh.getattr("created_ms")?)?),
                ("lastModifiedTime", or_zero(fresh.getattr("modified_ms")?)?),
                ("name", b64(&title)),
                ("state", "normal".into()),
                ("trashedTime", "0".into()),
                ("color", "-1".into()),
                ("order", "999999".into()),
            ] {
                set(&mut n, k, &v)?;
            }
            n
        } else {
            node_from_raw(py, &current)?
        };
        if key == "folder_id" {
            if has_current && current.get_item("state")?.ne("normal")? {
                return Err(conflict(py, Some("Çöp kutusundaki not önce geri yüklenmeli.")));
            }
            if value.ne("root")? {
                let target = nodes.get_item(&value)?;
                let ok = match target {
                    Some(t) => t.get_item("type")?.eq("folder")? && t.get_item("state")?.eq("normal")?,
                    None => false,
                };
                if !ok {
                    return Err(conflict(py, Some("Hedef klasör artık kullanılamıyor.")));
                }
            }
            set(&mut node, "parentFolderNodeId", &str_of(&value)?)?;
        } else {
            let trash = value.is_truthy()?;
            set(&mut node, "state", if trash { "trashed" } else { "normal" })?;
            if trash {
                let kids = children(&node).map_err(|e| meta_err(py, e))?;
                let parent = field(&kids, "parentfoldernodeid").ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("parentfoldernodeid"))?.to_string();
                let path = CloudMetadata::folder_path(py, nodes.as_any(), &s(py, &parent))?;
                set(&mut node, "restorePath", &b64(&path))?;
                set(&mut node, "parentFolderNodeId", "root")?;
                set(&mut node, "trashedTime", &millis(&api.call_method0("_clock")?)?.to_string())?;
            } else {
                let path = if has_current { str_of(&current.get_item("restore_path")?)? } else { "uncategorized:///".into() };
                let parent = CloudMetadata::restore_parent(py, nodes.as_any(), &path)?;
                set(&mut node, "parentFolderNodeId", &parent)?;
                set(&mut node, "trashedTime", "0")?;
                node.children.retain(|c| tag(c) != "restorepath");
            }
        }
        let now = millis(&api.call_method0("_clock")?)?;
        if has_current && int(py, now).le(current.get_item("sync_modified_time")?)? {
            return Err(api_error(py, "clock_skew", "Yerel saat son Cloud düzenlemesinden ileride olmalı."));
        }
        set(&mut node, "syncModifiedTime", &now.to_string())?;
        set(&mut node, "requestTime", &now.to_string())?;
        let device = Value::obj(vec![
            ("model", Value::s("Linux Notes")),
            ("ver", Value::s("0.1")),
            ("deviceName", Value::Str(b64("Linux Notes"))),
            ("deviceId", pyjson::from_py(&api.call_method0("_cloud")?.get_item("dvcId")?)?),
        ]);
        set(&mut node, "requestDevice", &pyjson::dumps(&device, &pyjson::COMPACT))?;
        let wanted = children(&node).map_err(|e| meta_err(py, e))?;
        let raw = CloudMetadata::folder_resource(now, &graph.get_item("sync_modified_time")?, node)?;
        let url = format!("{}/samsungnotes/v3/folder", str_of(&api.getattr("base")?)?);
        let request = request_patch(&api, &url, &raw)?;
        save_once(py, &fspath(&output)?.join("request.xml"), &raw)?;
        CloudMetadata::send(&api, &output, &request, 200)?;
        let checked = (|| -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
            let after = this.call_method0("inventory")?;
            let mut updated = None;
            for n in after.get_item("nodes")?.try_iter()? {
                let n = n?;
                if n.get_item("id")?.eq(&local_id)? {
                    updated = Some(n);
                    break;
                }
            }
            let updated = updated.ok_or_else(|| pyo3::exceptions::PyStopIteration::new_err(()))?;
            if updated.get_item("state")?.ne(field(&wanted, "state"))?
                || updated.get_item("parent_id")?.ne(field(&wanted, "parentfoldernodeid"))?
                || updated.get_item("sync_modified_time")?.ne(now)?
            {
                return Err(value_error());
            }
            let observed = api.call_method1("get_note", (server_id,))?;
            if observed.getattr("local_id")?.ne(&local_id)? {
                return Err(value_error());
            }
            Ok((updated, observed))
        })();
        let (updated, observed) = try_map(py, checked, &[K::Api, K::StopIter, K::Value], || uncertain(py, "write_uncertain", "Düzenleme gönderildi; Cloud sonucu henüz doğrulanamadı."))?;
        let result = dict(py, &[
            ("server_id", server_id.clone()),
            ("local_id", local_id.clone()),
            ("commit_id", observed.getattr("commit_id")?),
            ("snapshot", observed.call_method0("to_dict")?),
            ("node", updated),
            ("verified", boolean(py, true)),
        ])?;
        save_json(&fspath(&output)?.join("result.json"), result.as_any())?;
        Ok(result.into_any())
    }
}

/// INITIAL_FOLDER_CURSOR
pub fn initial_folder_cursor() -> String {
    base64::engine::general_purpose::STANDARD.encode(
        br#"{"version":0,"encryptedGuid":"","requestTimeStamp":0,"modifiedTime":0,"appVersion":"0","sdkVersion":"0","purpose":"INITIAL","type":"FOLDER"}"#,
    )
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_folder_changes_py, m)?)?;
    m.add_function(wrap_pyfunction!(parse_note_list_py, m)?)?;
    m.add_function(wrap_pyfunction!(document_organization, m)?)?;
    m.add_class::<CloudMetadata>()?;
    rs_common::export(m, &["parse_folder_changes", "parse_note_list", "document_organization", "CloudMetadata"])
}
