//! CloudAPI mutations: create/update/update_title, structured editor
//! create/update, page background, PDF pages, ink append (recipes 1–3) and
//! ink clone, plus the kind-specific readback of `CloudAPI.reconcile`.
use crate::api::{
    conflict, ink_multipart, multipart, normalize_commit, opaque_fingerprint, resolve_for, unsupported, Appended, CloudApi, SnapIn,
    MAX_BODY_BYTES,
};
use crate::editor;
use crate::guards::{self, GuardError};
use crate::json;
use crate::transport::{set_header, Request, Response};
use crate::util::{self, check_uuid, ditem, dget, load_json, save_once, json_once, R};
use notes_host::deps::{fail_of_ink, fail_of_ops, PureDeps};
use notes_host::snapshot::{self, Snapshot};
use notes_host::{Fail, Kind, Val};
use rs_ink::core::binary::{RawSamples, MAX_POINTS};
use rs_ink::core::num::Val as IVal;
use rs_store::val::py_eq;
use std::collections::HashSet;
use std::path::Path;

/// `document.text_value(value)` (DocumentError).
pub fn text_value(value: &str) -> R<()> {
    rs_doc::doc::text_value(value, rs_doc::doc::DocLimits::default().max_text)
        .map_err(|_| Fail::document("Metin geçersiz veya desteklenen boyutu aşıyor."))
}

/// `except DocumentError as error: raise UnsupportedDocumentError(str(error))`
fn doc_to_unsupported(f: Fail) -> Fail {
    if f.is_document() {
        unsupported(&f.message)
    } else {
        f
    }
}

/// `except (InkError, DocumentError) as error: raise UnsupportedDocumentError(str(error))`
fn ink_to_unsupported(f: Fail) -> Fail {
    if f.is_document() || f.kind == Kind::Ink {
        unsupported(&f.message)
    } else {
        f
    }
}

fn guard_err(e: GuardError, message: &str) -> Fail {
    match e {
        GuardError::Conflict => conflict(),
        GuardError::Invalid => Fail::conflict(message),
    }
}

fn opt_ids(ids: &[String]) -> Vec<Option<String>> {
    ids.iter().cloned().map(Some).collect()
}

fn now_check(now_ms: Option<i64>, message: &str) -> R<()> {
    match now_ms {
        Some(n) if n <= 0 => Err(unsupported(message)),
        _ => Ok(()),
    }
}

fn now_val(now_ms: Option<i64>) -> Val {
    now_ms.map(Val::Int).unwrap_or(Val::Null)
}

fn sha(data: &[u8]) -> Val {
    Val::s(util::sha256_hex(data))
}

impl CloudApi {
    /// `fresh = self.get_note(server_id)` plus the baseline equality guard.
    pub(crate) fn fresh_matching(&self, server_id: &str, snapshot: &Snapshot, baseline: Option<&str>, opaque: Option<&str>) -> R<Snapshot> {
        let fresh = self.get_note(server_id)?;
        if fresh.local_id != snapshot.local_id
            || normalize_commit(fresh.commit_id.as_deref())? != normalize_commit(baseline)?
            || fresh.resource_xml != snapshot.resource_xml
        {
            return Err(conflict());
        }
        if let Some(opaque) = opaque {
            if opaque_fingerprint(&fresh, &[], None)? != opaque {
                return Err(conflict());
            }
        }
        Ok(fresh)
    }

    /// `int(ET.fromstring(draft.xml).get('syncModifiedTime'))` when a draft exists.
    fn draft_time(output: &Path) -> R<Option<i64>> {
        let draft = output.join("draft.xml");
        if !draft.exists() {
            return Ok(None);
        }
        let root = crate::etree::parse_bytes(&util::read_bytes(&draft)?, false).map_err(|m| Fail::new(Kind::XmlParse, m))?;
        let text = root.get("syncModifiedTime").ok_or_else(|| Fail::type_("int() argument must be a string, a bytes-like object or a real number, not 'NoneType'"))?;
        match util::py_int(text) {
            Some(v) if v >= i64::MIN as i128 && v <= i64::MAX as i128 => Ok(Some(v as i64)),
            Some(_) => Err(Fail::new(Kind::Overflow, "Python int too large")),
            None => Err(Fail::value(format!("invalid literal for int() with base 10: '{text}'"))),
        }
    }

    /// `prepared.json` (`{'now_ms': ...}`) frozen once.
    pub(crate) fn prepared_time(&self, output: &Path, now_ms: Option<i64>) -> R<i64> {
        let prepared = output.join("prepared.json");
        if prepared.exists() {
            let plan = load_json(&prepared, util::DEFAULT_LIMIT)?;
            return match ditem(&plan, "now_ms")? {
                Val::Int(v) => Ok(*v),
                other => Err(Fail::type_(format!("prepared time is {}", other.type_name()))),
            };
        }
        let at = now_ms.unwrap_or_else(|| self.now_ms());
        json_once(&prepared, &Val::dict(vec![("now_ms", Val::Int(at))]))?;
        Ok(at)
    }

    /// gzip + `compare-request.gz` + one compareForUpsync request in a fresh
    /// `checks/<uuid>` attempt directory; returns the 200 answer.
    pub(crate) fn compare_request(&self, output: &Path, server_id: &str, tree_xml: &[u8]) -> R<Response> {
        let tree_payload = util::gzip_mtime0(tree_xml);
        save_once(&output.join("compare-request.gz"), &tree_payload)?;
        let mut headers = self.headers(true)?;
        set_header(&mut headers, "Content-Type", "application/xml;Charset=utf-8");
        set_header(&mut headers, "Content-Encoding", "gzip");
        let request = Request::new("POST", format!("{}/compareForUpsync", self.url(server_id)?), &headers, Some(tree_payload));
        let checks = output.join("checks");
        util::mkdir_private(&checks, true)?;
        let attempt = checks.join(self.random.uuid4());
        util::mkdir_private(&attempt, false)?;
        let response = self.call(&attempt, "compare", &request, crate::api::DEFAULT_CALL_LIMIT)?;
        Self::read_status(&response)?;
        Ok(response)
    }

    /// `CloudAPI.create(title, body, operation_id=..., local_id=None)`
    pub fn create(&self, title: &str, body: &str, operation_id: &str, local_id: Option<&str>) -> R<Val> {
        text_value(title)?;
        text_value(body)?;
        if let Some(l) = local_id {
            check_uuid(l)?;
        }
        self.validate_identity()?;
        let (output, replay) = self.operation(
            operation_id,
            vec![("kind", Val::s("create")), ("title", Val::s(title)), ("body", Val::s(body)), ("local_id", Val::opt_str(local_id))],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let draft = output.join("draft.xml");
        let (identity, full_xml) = if draft.exists() {
            let full_xml = util::read_bytes(&draft)?;
            let saved = snapshot::parse_document(&full_xml, "application/xml", None)?;
            if saved.title != title || saved.body != body || (local_id.is_some() && saved.local_id.as_deref() != local_id) {
                return Err(Fail::api("operation_mismatch", "Hazırlanmış yeni not farklı içerik taşıyor."));
            }
            (Val::opt_str(saved.local_id.as_deref()), full_xml)
        } else {
            // document.new_document draws uuid4() for a missing local id.
            let drawn = match local_id {
                Some(l) => l.to_string(),
                None => self.random.uuid4(),
            };
            let (identity, xml) = rs_doc::doc::new_document(
                title,
                body,
                Some(&drawn),
                rs_doc::doc::NowArg::Int(self.now_ms() as i128),
                rs_doc::doc::DocLimits::default().max_text,
            )
            .map_err(Fail::from)?;
            (Val::s(identity), xml)
        };
        let (payload, content_type) = multipart(self.random(), &full_xml);
        json_once(&output.join("expected.json"), &Val::dict(vec![("title", Val::s(title)), ("body", Val::s(body)), ("local_id", identity)]))?;
        save_once(&draft, &full_xml)?;
        self.publish(&output, &format!("{}/samsungnotes/v3/notes", self.base), payload, &content_type)
    }

    /// `CloudAPI.update(server_id, snapshot, title, body, baseline_commit=None, operation_id=...)`
    pub fn update<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        title: &str,
        body: &str,
        baseline_commit: Option<&str>,
        operation_id: &str,
    ) -> R<Val> {
        let snapshot = resolve_for(snapshot.into(), server_id)?;
        if !snapshot.editable {
            return Err(unsupported(snapshot.read_only_reason.as_deref().unwrap_or("Bu not henüz güvenle düzenlenemiyor.")));
        }
        let baseline = match baseline_commit {
            Some(b) => Some(b.to_string()),
            None => snapshot.commit_id.clone(),
        };
        if normalize_commit(baseline.as_deref())? != normalize_commit(snapshot.commit_id.as_deref())? {
            return Err(conflict());
        }
        text_value(title)?;
        text_value(body)?;
        self.validate_identity()?;
        let (output, replay) = self.operation(
            operation_id,
            vec![
                ("kind", Val::s("update")),
                ("server_id", Val::s(server_id)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("baseline_commit", Val::opt_str(baseline.as_deref())),
                ("title", Val::s(title)),
                ("body", Val::s(body)),
                ("snapshot_sha256", sha(&snapshot.resource_xml)),
            ],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let fresh = self.fresh_matching(server_id, &snapshot, baseline.as_deref(), None)?;
        let prepared_at = match Self::draft_time(&output)? {
            Some(t) => t,
            None => self.now_ms(),
        };
        let src = rs_doc::doc::EditSource {
            editable: fresh.editable,
            read_only_reason: fresh.read_only_reason.as_deref(),
            title: Some(&fresh.title),
            body: Some(&fresh.body),
            resource_xml: &fresh.resource_xml,
        };
        let edit = rs_doc::doc::prepare_edit(&src, title, body, rs_doc::doc::NowArg::Int(prepared_at as i128), &rs_doc::doc::DocLimits::default())
            .map_err(|e| doc_to_unsupported(Fail::from(e)))?;
        let inventory = rs_doc::doc::page_inventory(&snapshot.resource_xml).map_err(Fail::from)?;
        json_once(
            &output.join("expected.json"),
            &Val::dict(vec![
                ("title", Val::s(title)),
                ("body", Val::s(body)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("server_id", Val::s(server_id)),
                ("page_inventory", Val::s(inventory)),
            ]),
        )?;
        save_once(&output.join("draft.xml"), &edit.full_xml)?;
        let response = self.compare_request(&output, server_id, &edit.tree_xml)?;
        guards::compare_guard(response.body.as_deref(), baseline.as_deref(), &opt_ids(&edit.requested_ids))
            .map_err(|e| guard_err(e, "Sunucunun karşılaştırma yanıtı güvenli güncellemeye uygun değil."))?;
        let (payload, content_type) = multipart(self.random(), &edit.upload_xml);
        self.publish(&output, &format!("{}/update", self.url(server_id)?), payload, &content_type)
    }

    /// `CloudAPI.update_title(server_id, snapshot, title, operation_id=...)`
    pub fn update_title<'a>(&self, server_id: &str, snapshot: impl Into<SnapIn<'a>>, title: &str, operation_id: &str) -> R<Val> {
        let snapshot = resolve_for(snapshot.into(), server_id)?;
        text_value(title)?;
        self.validate_identity()?;
        let opaque = opaque_fingerprint(&snapshot, &[], None)?;
        let (output, replay) = self.operation(
            operation_id,
            vec![
                ("kind", Val::s("update_title")),
                ("server_id", Val::s(server_id)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("baseline_commit", Val::opt_str(snapshot.commit_id.as_deref())),
                ("title", Val::s(title)),
                ("snapshot_sha256", sha(&snapshot.resource_xml)),
                ("opaque_sha256", Val::s(&opaque)),
            ],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let fresh = self.fresh_matching(server_id, &snapshot, snapshot.commit_id.as_deref(), Some(&opaque))?;
        let prepared = (|| -> R<(editor_edit::Out, Val)> {
            let prepared_at = match Self::draft_time(&output)? {
                Some(t) => t,
                None => self.now_ms(),
            };
            let edit = editor::prepare_title_edit(&fresh, title, prepared_at)?;
            if !edit.requested_ids.is_empty() {
                return Err(Fail::document("Başlık değişikliği içerik nesnesi isteyemez."));
            }
            let expected = Val::dict(vec![
                ("title", Val::s(title)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("server_id", Val::s(server_id)),
                ("body_sha256", sha(snapshot.body.as_bytes())),
                ("content_sha256", Val::s(editor::title_content_fingerprint(&edit.full_xml)?)),
                ("opaque_sha256", Val::s(&opaque)),
            ]);
            Ok((edit, expected))
        })();
        let (edit, expected) = prepared.map_err(doc_to_unsupported)?;
        json_once(&output.join("expected.json"), &expected)?;
        save_once(&output.join("draft.xml"), &edit.full_xml)?;
        let response = self.compare_request(&output, server_id, &edit.tree_xml)?;
        guards::compare_guard(response.body.as_deref(), snapshot.commit_id.as_deref(), &[])
            .map_err(|e| guard_err(e, "Sunucu başlık değişikliği için beklenmeyen içerik istedi."))?;
        let (payload, content_type) = multipart(self.random(), &edit.upload_xml);
        self.publish(&output, &format!("{}/update", self.url(server_id)?), payload, &content_type)
    }

    /// `CloudAPI.create_editor(title, model, operation_id=..., local_id=None)`
    pub fn create_editor(&self, title: &str, model: &Val, operation_id: &str, local_id: Option<&str>) -> R<Val> {
        text_value(title)?;
        if let Some(l) = local_id {
            check_uuid(l)?;
        }
        let model = editor::normalize_model(model).map_err(doc_to_unsupported)?;
        self.validate_identity()?;
        let (output, replay) = self.operation(
            operation_id,
            vec![("kind", Val::s("create_editor")), ("title", Val::s(title)), ("model", model.clone()), ("local_id", Val::opt_str(local_id))],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let model_hash = editor::editor_model_fingerprint(&model)?;
        let draft = output.join("draft.xml");
        let (identity, full_xml) = if draft.exists() {
            let full_xml = util::read_bytes(&draft)?;
            let mut saved = snapshot::parse_document(&full_xml, "application/xml", None)?;
            let identity = Val::opt_str(saved.local_id.as_deref());
            // A new draft has no Cloud commit; bind one only for offline projection.
            saved.commit_id = Some("local-draft-validation".into());
            let (text_edit, projected_model) = editor::projection(&saved)?;
            if saved.title != title
                || !text_edit
                || editor::editor_model_fingerprint(&projected_model)? != model_hash
                || (local_id.is_some() && saved.local_id.as_deref() != local_id)
            {
                return Err(Fail::api("operation_mismatch", "Hazırlanmış editör notu farklı içerik taşıyor."));
            }
            (identity, full_xml)
        } else {
            let drawn = match local_id {
                Some(l) => l.to_string(),
                None => self.random.uuid4(),
            };
            let (identity, xml) = editor::new_editor_document(title, &model, Some(&drawn), self.now_ms()).map_err(doc_to_unsupported)?;
            (Val::s(identity), xml)
        };
        let text = match ditem(&model, "text")? {
            Val::Str(s) => s.clone(),
            other => return Err(Fail::new(Kind::Attribute, format!("'{}' object has no attribute 'encode'", other.type_name()))),
        };
        let expected = Val::dict(vec![
            ("title", Val::s(title)),
            ("local_id", identity),
            ("body_sha256", sha(text.as_bytes())),
            ("model_sha256", Val::s(&model_hash)),
            ("content_fingerprint_version", Val::Int(2)),
            ("content_sha256", Val::s(editor::editor_content_fingerprint(&full_xml)?)),
        ]);
        json_once(&output.join("expected.json"), &expected)?;
        save_once(&draft, &full_xml)?;
        let (payload, content_type) = multipart(self.random(), &full_xml);
        self.publish(&output, &format!("{}/samsungnotes/v3/notes", self.base), payload, &content_type)
    }

    /// `CloudAPI.update_editor(server_id, snapshot, title, model, baseline_commit=None, operation_id=...)`
    pub fn update_editor<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        title: &str,
        model: &Val,
        baseline_commit: Option<&str>,
        operation_id: &str,
    ) -> R<Val> {
        let snapshot = resolve_for(snapshot.into(), server_id)?;
        let baseline = match baseline_commit {
            Some(b) => Some(b.to_string()),
            None => snapshot.commit_id.clone(),
        };
        if normalize_commit(baseline.as_deref())? != normalize_commit(snapshot.commit_id.as_deref())? {
            return Err(conflict());
        }
        text_value(title)?;
        let model = editor::normalize_model(model).map_err(doc_to_unsupported)?;
        self.validate_identity()?;
        let opaque = opaque_fingerprint(&snapshot, &[], None)?;
        let (output, replay) = self.operation(
            operation_id,
            vec![
                ("kind", Val::s("update_editor")),
                ("server_id", Val::s(server_id)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("baseline_commit", Val::opt_str(baseline.as_deref())),
                ("title", Val::s(title)),
                ("model", model.clone()),
                ("snapshot_sha256", sha(&snapshot.resource_xml)),
                ("opaque_sha256", Val::s(&opaque)),
            ],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let fresh = self.fresh_matching(server_id, &snapshot, baseline.as_deref(), Some(&opaque))?;
        let prepared = (|| -> R<(editor_edit::Out, Val)> {
            let prepared_at = match Self::draft_time(&output)? {
                Some(t) => t,
                None => self.now_ms(),
            };
            let edit = editor::prepare_editor_edit(&fresh, title, &model, prepared_at)?;
            let body = edit.body.clone().unwrap_or_else(|| fresh.body.clone());
            let expected = Val::dict(vec![
                ("title", Val::s(title)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("server_id", Val::s(server_id)),
                ("body_sha256", sha(body.as_bytes())),
                ("model_sha256", Val::s(editor::editor_model_fingerprint(&model)?)),
                ("content_fingerprint_version", Val::Int(2)),
                ("content_sha256", Val::s(editor::editor_content_fingerprint(&edit.full_xml)?)),
                ("opaque_sha256", Val::s(&opaque)),
            ]);
            Ok((edit, expected))
        })();
        let (edit, expected) = prepared.map_err(doc_to_unsupported)?;
        json_once(&output.join("expected.json"), &expected)?;
        save_once(&output.join("draft.xml"), &edit.full_xml)?;
        let response = self.compare_request(&output, server_id, &edit.tree_xml)?;
        guards::compare_guard(response.body.as_deref(), baseline.as_deref(), &edit.requested_ids)
            .map_err(|e| guard_err(e, "Sunucunun istediği içerik düzenlemeyle uyuşmuyor."))?;
        let (payload, content_type) = multipart(self.random(), &edit.upload_xml);
        self.publish(&output, &format!("{}/update", self.url(server_id)?), payload, &content_type)
    }

    /// `CloudAPI.update_page_background(server_id, snapshot, color_argb, operation_id=..., now_ms=None)`
    pub fn update_page_background<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        color_argb: i64,
        operation_id: &str,
        now_ms: Option<i64>,
    ) -> R<Val> {
        let snapshot = resolve_for(snapshot.into(), server_id)?;
        if !(0..=0xFFFF_FFFF).contains(&color_argb) || color_argb >> 24 != 0xFF {
            return Err(unsupported("Sayfa rengi opak bir renk olmalı."));
        }
        now_check(now_ms, "Kayıt zamanı geçersiz.")?;
        self.validate_identity()?;
        let opaque = opaque_fingerprint(&snapshot, &[], None)?;
        let (output, replay) = self.operation(
            operation_id,
            vec![
                ("kind", Val::s("update_page_background")),
                ("server_id", Val::s(server_id)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("baseline_commit", Val::opt_str(snapshot.commit_id.as_deref())),
                ("color_argb", Val::Int(color_argb)),
                ("now_ms", now_val(now_ms)),
                ("snapshot_sha256", sha(&snapshot.resource_xml)),
                ("opaque_sha256", Val::s(&opaque)),
            ],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let fresh = self.fresh_matching(server_id, &snapshot, snapshot.commit_id.as_deref(), Some(&opaque))?;
        let prepared_at = self.prepared_time(&output, now_ms)?;
        let edit = rs_ops::page_style::prepare_page_background(&PureDeps::default(), &fresh, Some(color_argb as i128), Some(prepared_at as i128))
            .map_err(|e| doc_to_unsupported(fail_of_ops(e)))?;
        let expected = Val::dict(vec![
            ("title", Val::s(&snapshot.title)),
            ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
            ("server_id", Val::s(server_id)),
            ("body_sha256", sha(snapshot.body.as_bytes())),
            ("content_fingerprint_version", Val::Int(2)),
            ("content_sha256", Val::s(editor::editor_content_fingerprint(&edit.full_xml)?)),
            ("opaque_sha256", Val::s(&opaque)),
            ("page_ids", Val::List(edit.page_ids.iter().map(Val::s).collect())),
        ]);
        json_once(&output.join("expected.json"), &expected)?;
        save_once(&output.join("draft.xml"), &edit.full_xml)?;
        let response = self.compare_request(&output, server_id, &edit.tree_xml)?;
        guards::compare_page_guard(response.body.as_deref(), snapshot.commit_id.as_deref(), &opt_ids(&edit.page_ids))
            .map_err(|e| guard_err(e, "Sunucunun istediği sayfalar düzenlemeyle uyuşmuyor."))?;
        let (payload, content_type) = multipart(self.random(), &edit.upload_xml);
        self.publish(&output, &format!("{}/update", self.url(server_id)?), payload, &content_type)
    }

    /// `CloudAPI.add_pdf_pages(server_id, snapshot, pdf, file_name=..., page_sizes=...,
    /// page_indices=None, template_type=None, operation_id=..., now_ms=None)`
    #[allow(clippy::too_many_arguments)]
    pub fn add_pdf_pages<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        pdf: &[u8],
        file_name: &str,
        page_sizes: &[(f64, f64)],
        page_indices: Option<&[i64]>,
        template_type: Option<i64>,
        operation_id: &str,
        now_ms: Option<i64>,
    ) -> R<Val> {
        let snapshot = resolve_for(snapshot.into(), server_id)?;
        if !pdf.starts_with(b"%PDF-") {
            return Err(unsupported("Dosya PDF değil."));
        }
        now_check(now_ms, "Kayıt zamanı geçersiz.")?;
        let indices: Vec<i64> = match page_indices {
            Some(i) => i.to_vec(),
            None => (0..page_sizes.len() as i64).collect(),
        };
        self.validate_identity()?;
        let opaque = opaque_fingerprint(&snapshot, &[], None)?;
        let file_sha = util::sha256_hex(pdf);
        let sizes_val = Val::List(page_sizes.iter().map(|(w, h)| Val::List(vec![Val::Float(*w), Val::Float(*h)])).collect());
        let (output, replay) = self.operation(
            operation_id,
            vec![
                ("kind", Val::s("add_pdf_pages")),
                ("server_id", Val::s(server_id)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("baseline_commit", Val::opt_str(snapshot.commit_id.as_deref())),
                ("file_sha256", Val::s(&file_sha)),
                ("file_name", Val::s(file_name)),
                ("page_sizes", sizes_val),
                ("page_indices", Val::List(indices.iter().map(|i| Val::Int(*i)).collect())),
                ("template_type", template_type.map(Val::Int).unwrap_or(Val::Null)),
                ("now_ms", now_val(now_ms)),
                ("snapshot_sha256", sha(&snapshot.resource_xml)),
                ("opaque_sha256", Val::s(&opaque)),
            ],
        )?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let fresh = self.fresh_matching(server_id, &snapshot, snapshot.commit_id.as_deref(), Some(&opaque))?;
        let prepared = output.join("prepared.json");
        let plan = if prepared.exists() {
            load_json(&prepared, util::DEFAULT_LIMIT)?
        } else {
            let ids: Vec<Val> = (0..2 * indices.len()).map(|_| Val::s(self.random.uuid4())).collect();
            let plan = Val::dict(vec![("now_ms", Val::Int(now_ms.unwrap_or_else(|| self.now_ms()))), ("ids", Val::List(ids))]);
            json_once(&prepared, &plan)?;
            plan
        };
        let upload_file = output.join("upload.json");
        let upload = if upload_file.exists() {
            load_json(&upload_file, util::DEFAULT_LIMIT)?
        } else {
            let result = self.upload_content_file(pdf)?;
            let upload = Val::dict(vec![
                ("sha256", ditem(&result, "sha256")?.clone()),
                ("uploadToken", ditem(&result, "uploadToken")?.clone()),
                ("status", ditem(&result, "status")?.clone()),
            ]);
            json_once(&upload_file, &upload)?;
            upload
        };
        if !py_eq(ditem(&upload, "sha256")?, &Val::s(&file_sha)) {
            return Err(Fail::api("operation_mismatch", "Yüklenen dosya işlemle uyuşmuyor."));
        }
        let ids = match ditem(&plan, "ids")? {
            Val::List(items) => items.iter().map(|v| v.as_str().map(str::to_string)).collect(),
            other => return Err(Fail::type_(format!("'{}' object is not iterable", other.type_name()))),
        };
        let args = rs_ops::page_template::PdfArgs {
            file_sha256: Some(file_sha.clone()),
            upload_token: ditem(&upload, "uploadToken")?.as_str().map(str::to_string),
            file_name: Some(file_name.to_string()),
            page_sizes: page_sizes.to_vec(),
            now_ms: match ditem(&plan, "now_ms")? {
                Val::Int(v) => Some(*v as i128),
                _ => None,
            },
            template_type: template_type.map(|t| Some(t as i128)),
            page_indices: Some(indices.iter().map(|i| Some(*i as i128)).collect()),
            ids: Some(ids),
        };
        let edit = rs_ops::page_template::prepare_pdf_pages(&PureDeps::default(), &fresh, &args).map_err(|e| doc_to_unsupported(fail_of_ops(e)))?;
        let expected = Val::dict(vec![
            ("title", Val::s(&snapshot.title)),
            ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
            ("server_id", Val::s(server_id)),
            ("body_sha256", sha(snapshot.body.as_bytes())),
            ("content_fingerprint_version", Val::Int(2)),
            ("content_sha256", Val::s(editor::editor_content_fingerprint(&edit.full_xml)?)),
            ("opaque_sha256", Val::s(&opaque)),
            ("page_ids", Val::List(edit.page_ids.iter().map(Val::s).collect())),
        ]);
        json_once(&output.join("expected.json"), &expected)?;
        save_once(&output.join("draft.xml"), &edit.full_xml)?;
        let response = self.compare_request(&output, server_id, &edit.tree_xml)?;
        guards::compare_new_pages_guard(
            response.body.as_deref(),
            snapshot.commit_id.as_deref(),
            &opt_ids(&edit.page_ids),
            &opt_ids(&edit.layer_ids),
            Some(&file_sha),
        )
        .map_err(|e| guard_err(e, "Sunucunun istediği öğeler yeni sayfalarla uyuşmuyor."))?;
        let (payload, content_type) = multipart(self.random(), &edit.upload_xml);
        self.publish(&output, &format!("{}/update", self.url(server_id)?), payload, &content_type)
    }

    /// `CloudAPI.update_ink_clone(server_id, snapshot, source_stroke_id, new_stroke_id=..., dx, dy, operation_id=..., now_ms=None)`
    #[allow(clippy::too_many_arguments)]
    pub fn update_ink_clone<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        source_stroke_id: &str,
        new_stroke_id: &str,
        dx: f64,
        dy: f64,
        operation_id: &str,
        now_ms: Option<i64>,
    ) -> R<Val> {
        if [dx, dy].iter().any(|v| !v.is_finite() || v.abs() > 100000.0) {
            return Err(unsupported("Çizgi konumu geçersiz."));
        }
        let source = source_stroke_id.to_string();
        let new = new_stroke_id.to_string();
        self.update_ink_document(
            server_id,
            snapshot.into(),
            source_stroke_id,
            new_stroke_id,
            operation_id,
            now_ms,
            "update_ink_clone",
            vec![("dx", Val::Float(dx)), ("dy", Val::Float(dy))],
            &move |fresh: &Snapshot, tick: i64| {
                if fresh.commit_id.as_deref().is_none_or(str::is_empty) {
                    return Err(Fail::new(Kind::Ink, "committed snapshot required"));
                }
                let limits = rs_doc::mime::Limits::default();
                let parse = |raw: &[u8], ct: &str| rs_ink::core::prepare::payload_parts(raw, ct, &limits);
                let snap = rs_ink::core::prepare::SnapshotIn { resource_xml: &fresh.resource_xml, raw_body: &fresh.raw_body, content_type: &fresh.content_type };
                rs_ink::core::prepare::prepare_ink_document(
                    &snap,
                    Some(&source),
                    Some(&new),
                    rs_ink::core::prepare::Geometry::Translate(IVal::Float(dx), IVal::Float(dy)),
                    IVal::Int(tick as i128),
                    1,
                    None,
                    &parse,
                )
                .map_err(fail_of_ink)
            },
        )
    }

    /// `CloudAPI.append_ink_stroke(...)` — original v1 capture recipe.
    #[allow(clippy::too_many_arguments)]
    pub fn append_ink_stroke<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        source_stroke_id: &str,
        samples: &RawSamples,
        new_stroke_id: &str,
        operation_id: &str,
        now_ms: Option<i64>,
    ) -> R<Val> {
        self.append_ink(server_id, snapshot.into(), source_stroke_id, samples, new_stroke_id, operation_id, now_ms, InkRecipe::V1)
    }

    /// `CloudAPI.append_ink_stroke_v2(...)` — page-coordinate capture, cache release.
    #[allow(clippy::too_many_arguments)]
    pub fn append_ink_stroke_v2<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        source_stroke_id: &str,
        samples: &RawSamples,
        new_stroke_id: &str,
        operation_id: &str,
        now_ms: Option<i64>,
    ) -> R<Val> {
        self.append_ink(server_id, snapshot.into(), source_stroke_id, samples, new_stroke_id, operation_id, now_ms, InkRecipe::V2)
    }

    /// `CloudAPI.append_ink_stroke_v3(...)` — recipe 2 plus explicit colour and page-unit size.
    /// `pen_size` is the Python value (`Val::Int` or `Val::Float`).
    #[allow(clippy::too_many_arguments)]
    pub fn append_ink_stroke_v3<'a>(
        &self,
        server_id: &str,
        snapshot: impl Into<SnapIn<'a>>,
        source_stroke_id: &str,
        samples: &RawSamples,
        new_stroke_id: &str,
        operation_id: &str,
        color_argb: &Val,
        pen_size: &Val,
        now_ms: Option<i64>,
    ) -> R<Val> {
        self.append_ink(
            server_id,
            snapshot.into(),
            source_stroke_id,
            samples,
            new_stroke_id,
            operation_id,
            now_ms,
            InkRecipe::V3 { color_argb: color_argb.clone(), pen_size: pen_size.clone() },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn append_ink(
        &self,
        server_id: &str,
        snapshot: SnapIn<'_>,
        source_stroke_id: &str,
        samples: &RawSamples,
        new_stroke_id: &str,
        operation_id: &str,
        now_ms: Option<i64>,
        recipe: InkRecipe,
    ) -> R<Val> {
        let version: i64 = match recipe {
            InkRecipe::V1 => 1,
            InkRecipe::V2 => 2,
            InkRecipe::V3 { .. } => 3,
        };
        let frozen = samples.clone();
        let built = (|| -> R<(Vec<(&'static str, Val)>, Option<(u32, f64)>)> {
            if !(1..=MAX_POINTS).contains(&frozen.points.len()) {
                return Err(Fail::new(Kind::Ink, "invalid capture"));
            }
            let capture = rs_ink::core::capture::normalize_capture(&frozen, false).map_err(fail_of_ink)?;
            let mut parameters = vec![(
                "samples",
                Val::dict(vec![
                    ("schemaVersion", Val::Int(1)),
                    ("encoding", Val::s("native-q5-q12-v1")),
                    ("count", Val::Int(capture.samples.points.len() as i64)),
                    ("channelsBase64", Val::s(util::b64encode(&capture.channels))),
                ]),
            )];
            if version >= 2 {
                parameters.push(("recipeVersion", Val::Int(version)));
            }
            let mut style = None;
            if let InkRecipe::V3 { color_argb, pen_size } = &recipe {
                let color = match color_argb {
                    Val::Int(c) if (0..=0xffff_ffff).contains(c) && c >> 24 == 0xff => *c,
                    _ => return Err(Fail::new(Kind::Ink, "invalid style")),
                };
                let size = match pen_size {
                    Val::Int(i) => *i as f64,
                    Val::Float(f) => *f,
                    _ => return Err(Fail::new(Kind::Ink, "invalid style")),
                };
                if !size.is_finite() || !(rs_ink::core::coords::MIN_PEN_SIZE..=rs_ink::core::coords::MAX_PEN_SIZE).contains(&size) {
                    return Err(Fail::new(Kind::Ink, "invalid style"));
                }
                let size32 = size as f32 as f64;
                parameters.push(("style", Val::dict(vec![("colorARGB", Val::Int(color)), ("penSize", Val::Float(size32))])));
                style = Some((color as u32, size32));
            }
            let dumped = json::dumps(&Val::dict(parameters.clone()), &json::DEFAULT);
            if dumped.chars().count() > 1024 * 1024 {
                return Err(Fail::new(Kind::Ink, "capture intent bound"));
            }
            Ok((parameters, style))
        })();
        let (parameters, style) = match built {
            Ok(v) => v,
            Err(e) if e.kind == Kind::Ink || e.is_type() || e.is_value() || e.kind == Kind::Overflow => {
                return Err(unsupported("Çizgi noktaları desteklenen aralıkta değil."))
            }
            Err(e) => return Err(e),
        };
        let kind = ["append_ink_stroke", "append_ink_stroke_v2", "append_ink_stroke_v3"][version as usize - 1];
        let source = source_stroke_id.to_string();
        let new = new_stroke_id.to_string();
        let recipe_core = match (version, style) {
            (1, _) => rs_ink::core::prepare::AppendRecipe::V1,
            (2, _) => rs_ink::core::prepare::AppendRecipe::V2,
            (_, Some((color_argb, pen_size))) => rs_ink::core::prepare::AppendRecipe::V3 { color_argb, pen_size },
            _ => rs_ink::core::prepare::AppendRecipe::V2,
        };
        self.update_ink_document(server_id, snapshot, source_stroke_id, new_stroke_id, operation_id, now_ms, kind, parameters, &move |fresh: &Snapshot, tick: i64| {
            let snap = rs_ink::core::prepare::AppendSnapshot::from_doc(fresh);
            rs_ink::core::prepare::prepare_append(&snap, &source, &new, &frozen, tick, recipe_core, None).map_err(fail_of_ink)
        })
    }

    /// `CloudAPI._update_ink_document(...)`: the shared immutable ink transaction.
    #[allow(clippy::too_many_arguments)]
    fn update_ink_document(
        &self,
        server_id: &str,
        snapshot: SnapIn<'_>,
        source_stroke_id: &str,
        new_stroke_id: &str,
        operation_id: &str,
        now_ms: Option<i64>,
        kind: &str,
        parameters: Vec<(&str, Val)>,
        prepare: &dyn Fn(&Snapshot, i64) -> R<rs_ink::core::prepare::InkAppendEdit>,
    ) -> R<Val> {
        let snapshot = resolve_for(snapshot, server_id)?;
        check_uuid(source_stroke_id)?;
        check_uuid(new_stroke_id)?;
        now_check(now_ms, "Çizgi zamanı geçersiz.")?;
        self.validate_identity()?;
        let opaque = opaque_fingerprint(&snapshot, &[], None)?;
        let mut intent = vec![
            ("kind", Val::s(kind)),
            ("server_id", Val::s(server_id)),
            ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
            ("baseline_commit", Val::opt_str(snapshot.commit_id.as_deref())),
            ("source_stroke_id", Val::s(source_stroke_id)),
            ("new_stroke_id", Val::s(new_stroke_id)),
        ];
        intent.extend(parameters);
        intent.push(("now_ms", now_val(now_ms)));
        intent.push(("snapshot_sha256", sha(&snapshot.resource_xml)));
        intent.push(("opaque_sha256", Val::s(&opaque)));
        let (output, replay) = self.operation(operation_id, intent)?;
        if let Some(r) = replay {
            return Ok(r);
        }
        let fresh = self.fresh_matching(server_id, &snapshot, snapshot.commit_id.as_deref(), Some(&opaque))?;
        let prepared_at = self.prepared_time(&output, now_ms)?;
        let checked = (|| -> R<(rs_ink::core::prepare::InkAppendEdit, Vec<String>, Appended, Val)> {
            let edit = prepare(&fresh, prepared_at)?;
            let recipe: u8 = match kind {
                "append_ink_stroke_v2" => 2,
                "append_ink_stroke_v3" => 3,
                _ => 1,
            };
            let changed: Vec<String> = if recipe >= 2 { edit.content_file_ids.clone() } else { vec![] };
            if edit.recipe_version != recipe {
                return Err(Fail::new(Kind::Ink, "Çizgi sürümü uyuşmuyor."));
            }
            let requested: HashSet<&String> = edit.requested_ids.iter().collect();
            let mut wanted: HashSet<&String> = [&edit.page_id, &edit.layer_id].into_iter().collect();
            let new_owned = new_stroke_id.to_string();
            wanted.insert(&new_owned);
            wanted.extend(changed.iter());
            if requested != wanted || edit.binary_parts.len() != 1 {
                return Err(Fail::new(Kind::Ink, "Beklenmeyen çizgi değişikliği."));
            }
            let (name, filename, body) = edit.binary_parts[0].clone();
            if name != "strokeResource"
                || filename != format!("{}_{}", new_stroke_id, edit.stroke_modified_time)
                || body.is_empty()
                || body.len() > MAX_BODY_BYTES
            {
                return Err(Fail::new(Kind::Ink, "Geçersiz çizgi verisi."));
            }
            let part: Appended = (name, filename, body);
            let expected = Val::dict(vec![
                ("title", Val::s(&snapshot.title)),
                ("local_id", Val::opt_str(snapshot.local_id.as_deref())),
                ("server_id", Val::s(server_id)),
                ("body_sha256", sha(snapshot.body.as_bytes())),
                ("content_fingerprint_version", Val::Int(2)),
                ("content_sha256", Val::s(editor::editor_content_fingerprint(&edit.full_xml)?)),
                ("opaque_sha256", Val::s(opaque_fingerprint(&snapshot, std::slice::from_ref(&part), None)?)),
            ]);
            Ok((edit, changed, part, expected))
        })();
        let (edit, changed, part, expected) = checked.map_err(ink_to_unsupported)?;
        json_once(&output.join("expected.json"), &expected)?;
        save_once(&output.join("draft.xml"), &edit.full_xml)?;
        save_once(&output.join("stroke.bin"), &part.2)?;
        let response = self.compare_request(&output, server_id, &edit.tree_xml)?;
        guards::compare_ink_guard(
            response.body.as_deref(),
            snapshot.commit_id.as_deref(),
            Some(&edit.page_id),
            Some(&edit.layer_id),
            Some(new_stroke_id),
            &opt_ids(&changed),
        )
        .map_err(|e| guard_err(e, "Sunucunun istediği çizim içeriği düzenlemeyle uyuşmuyor."))?;
        let (payload, content_type) = ink_multipart(self.random(), &edit.upload_xml, std::slice::from_ref(&part))?;
        self.publish(&output, &format!("{}/update", self.url(server_id)?), payload, &content_type)
    }
}

/// Ink append recipe (`append_ink_stroke` / `_v2` / `_v3`).
#[derive(Clone)]
pub enum InkRecipe {
    V1,
    V2,
    /// Python values of `color_argb` / `pen_size` (validated like the oracle).
    V3 { color_argb: Val, pen_size: Val },
}

mod editor_edit {
    pub type Out = rs_editor::edit::EditOut;
}

fn expected_str<'a>(expected: &'a Val, key: &str) -> R<&'a Val> {
    ditem(expected, key)
}

fn neq(a: String, b: &Val) -> bool {
    !py_eq(&Val::s(a), b)
}

/// The kind-specific branch of `CloudAPI.reconcile` (raises ValueError-like
/// failures on mismatch; the caller maps them to `write_uncertain`).
pub(crate) fn verify_kind(api: &CloudApi, output: &Path, kind: &str, snapshot: &Snapshot, expected: &Val) -> R<()> {
    let mismatch = || Fail::value("readback mismatch");
    match kind {
        "update_title" => {
            if neq(util::sha256_hex(snapshot.body.as_bytes()), expected_str(expected, "body_sha256")?)
                || neq(editor::title_content_fingerprint(&snapshot.resource_xml)?, expected_str(expected, "content_sha256")?)
                || neq(opaque_fingerprint(snapshot, &[], None)?, expected_str(expected, "opaque_sha256")?)
            {
                return Err(mismatch());
            }
        }
        "update_ink_clone" | "append_ink_stroke" | "append_ink_stroke_v2" | "append_ink_stroke_v3" | "update_page_background" | "add_pdf_pages" => {
            let draft = util::read_bytes(&output.join("draft.xml"))?;
            if !py_eq(dget(expected, "content_fingerprint_version")?.unwrap_or(&Val::Null), &Val::Int(2))
                || neq(editor::editor_content_fingerprint(&draft)?, expected_str(expected, "content_sha256")?)
                || neq(editor::editor_content_fingerprint(&snapshot.resource_xml)?, expected_str(expected, "content_sha256")?)
                || neq(util::sha256_hex(snapshot.body.as_bytes()), expected_str(expected, "body_sha256")?)
                || neq(opaque_fingerprint(snapshot, &[], None)?, expected_str(expected, "opaque_sha256")?)
            {
                return Err(mismatch());
            }
        }
        "create_editor" | "update_editor" => {
            let (text_edit, model) = editor::projection(snapshot)?;
            let draft = util::read_bytes(&output.join("draft.xml"))?;
            let version = dget(expected, "content_fingerprint_version")?.cloned().unwrap_or(Val::Int(1));
            if py_eq(&version, &Val::Int(1)) {
                if neq(editor::title_content_fingerprint(&draft)?, expected_str(expected, "content_sha256")?) {
                    return Err(mismatch());
                }
            } else if py_eq(&version, &Val::Int(2)) {
                if neq(editor::editor_content_fingerprint(&draft)?, expected_str(expected, "content_sha256")?) {
                    return Err(mismatch());
                }
            } else {
                return Err(mismatch());
            }
            if !text_edit
                || neq(editor::editor_model_fingerprint(&model)?, expected_str(expected, "model_sha256")?)
                || neq(util::sha256_hex(snapshot.body.as_bytes()), expected_str(expected, "body_sha256")?)
                || editor::editor_content_fingerprint(&snapshot.resource_xml)? != editor::editor_content_fingerprint(&draft)?
                || (dget(expected, "opaque_sha256")?.is_some()
                    && neq(opaque_fingerprint(snapshot, &[], None)?, expected_str(expected, "opaque_sha256")?))
            {
                return Err(mismatch());
            }
        }
        "erase_ink_strokes_v1" | "move_ink_strokes_v1" => crate::ops::verify_erase_readback(output, snapshot, expected)?,
        _ => {
            if !py_eq(&Val::s(&snapshot.body), expected_str(expected, "body")?) {
                return Err(mismatch());
            }
        }
    }
    let _ = api;
    Ok(())
}

/// `ink_store.samples_from_points(points)` (the engine's stroke conversion):
/// a list of `{x, y, pressure, timeMs}` dicts → capture samples.
pub fn samples_from_points(points: &Val) -> R<RawSamples> {
    notes_host::areas::samples_from_points(points)
}
