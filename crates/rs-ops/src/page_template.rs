//! Replacement for the Python module `page_template`: PDF-backed pages
//! (templates and inserted PDF pages), offline only.
use crate::bridge;
#[cfg(feature = "python")]
use crate::bridge::PyDeps;
use crate::bridge::RPy;
use crate::deps::*;
use crate::err::*;
use crate::page_style::{advance, next_tick, sections};
use crate::util::*;
use crate::xml::{self, Element};
use base64::Engine as _;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyList, PyTuple};
use std::any::Any;
use std::collections::HashSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_PDF_PAGES: usize = 200;

pub struct PdfPages {
    pub full_xml: Vec<u8>,
    pub tree_xml: Vec<u8>,
    pub upload_xml: Vec<u8>,
    pub page_ids: Vec<String>,
    pub layer_ids: Vec<String>,
    pub file_id: String,
    pub tick: i64,
}

fn parse_pdfinfo(stdout: &str) -> Vec<(f64, f64)> {
    // ^Page\s+\d+\s+size:\s+([\d.]+) x ([\d.]+) pts  (re.M)
    let mut sizes = Vec::new();
    for line in stdout.split('\n') {
        let rest = match line.strip_prefix("Page") {
            Some(r) => r,
            None => continue,
        };
        let parse = || -> Option<(f64, f64)> {
            let r = rest.strip_prefix(char::is_whitespace)?.trim_start();
            let digits = r.find(|c: char| !c.is_ascii_digit())?;
            if digits == 0 {
                return None;
            }
            let r = &r[digits..];
            let r = r.strip_prefix(char::is_whitespace)?.trim_start();
            let r = r.strip_prefix("size:")?;
            let r = r.strip_prefix(char::is_whitespace)?.trim_start();
            let wend = r.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
            let (w, r) = r.split_at(wend);
            let r = r.strip_prefix(" x ")?;
            let hend = r.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
            let (h, r) = r.split_at(hend);
            if w.is_empty() || h.is_empty() || !r.starts_with(" pts") {
                return None;
            }
            Some((w.parse().ok()?, h.parse().ok()?))
        };
        if let Some(s) = parse() {
            sizes.push(s);
        }
    }
    sizes
}

/// Page sizes in points via Poppler's `pdfinfo` (no rendering).
pub fn pdf_page_sizes(path: &str, timeout: Duration) -> R<Vec<(f64, f64)>> {
    let unreadable = || e_doc("PDF okunamadı.");
    let mut child = Command::new("pdfinfo")
        .args(["-f", "1", "-l", &(MAX_PDF_PAGES + 1).to_string(), path])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| unreadable())?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unreadable());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return Err(unreadable()),
        }
    };
    let out = out_reader.join().unwrap_or_default();
    let _ = err_reader.join();
    let sizes = parse_pdfinfo(&String::from_utf8_lossy(&out));
    if !status.success() || sizes.is_empty() {
        return Err(unreadable());
    }
    if sizes.len() > MAX_PDF_PAGES {
        return doc(&format!("En fazla {MAX_PDF_PAGES} PDF sayfası eklenebilir."));
    }
    Ok(sizes)
}

fn file_hash(identity: &str, bind: i64, name: &str, ref_count: usize, tick: i64) -> String {
    hex(&sha256(format!("{identity}{bind}{name}{ref_count}{tick}true").as_bytes()))
}

/// Arguments of `prepare_pdf_pages` after Python-level type inspection.
pub struct PdfArgs {
    pub file_sha256: Option<String>,
    pub upload_token: Option<String>,
    pub file_name: Option<String>,
    pub page_sizes: Vec<(f64, f64)>,
    pub now_ms: Option<i128>,
    /// None = not given; Some(None) = invalid type.
    pub template_type: Option<Option<i128>>,
    /// None = all pages; entries None = not an int.
    pub page_indices: Option<Vec<Option<i128>>>,
    pub ids: Option<Vec<Option<String>>>,
}

fn is_sha(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub fn prepare_pdf_pages(deps: &dyn Deps, snapshot: &dyn Any, a: &PdfArgs) -> R<PdfPages> {
    let snapshot = deps.convert(snapshot, "samsung_notes.page_template")?;
    let snapshot: &dyn Any = &*snapshot;
    let sha = match a.file_sha256.as_deref() {
        Some(s) if is_sha(s) => s.to_string(),
        _ => return doc("PDF özeti geçersiz."),
    };
    let token = match a.upload_token.as_deref() {
        Some(t) if t.len() <= 128 && t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') => t.to_string(),
        _ => return doc("Yükleme belirteci geçersiz."),
    };
    let file_name = match a.file_name.as_deref() {
        Some(f) if !f.is_empty() && f.len() <= 255 && !f.chars().any(|c| (c as u32) < 0x20 || c == '/' || c == '\\') => f.to_string(),
        _ => return doc("Dosya adı geçersiz."),
    };
    let template_type = match a.template_type {
        None => None,
        Some(Some(t)) if (0..1i128 << 31).contains(&t) => Some(t as i64),
        Some(_) => return doc("Şablon türü geçersiz."),
    };
    let sizes = &a.page_sizes;
    let indices: Vec<Option<i128>> = match &a.page_indices {
        None => (0..sizes.len() as i128).map(Some).collect(),
        Some(v) => v.clone(),
    };
    if sizes.is_empty()
        || indices.is_empty()
        || indices.len() > MAX_PDF_PAGES
        || indices.iter().any(|i| !i.is_some_and(|i| 0 <= i && i < sizes.len() as i128))
    {
        return doc("PDF sayfa seçimi geçersiz.");
    }
    let indices: Vec<usize> = indices.into_iter().map(|i| i.unwrap() as usize).collect();
    if sizes.iter().any(|(w, h)| !(0.0 < *w && *w < 20000.0 && 0.0 < *h && *h < 20000.0)) {
        return doc("PDF sayfa boyutu desteklenmiyor.");
    }
    if let Some(r) = deps.verified(snapshot)? {
        return doc(&r);
    }
    let mut root = xml::parse_bytes(&deps.resource_xml(snapshot)?).map_err(|m| e_parse(&m))?;
    let (ai, ti, pi) = sections(&root)?;
    if root.children[ai].get("formatVersion") != Some("4000") || root.children[ti].get("ver") != Some("4000") {
        return doc("Bu notun biçim sürümü desteklenmiyor.");
    }
    let attrs = &root.children[ai];
    if attrs.get_or("pageMode", "0") != "0" || attrs.get_or("pageHorizontalPadding", "0") != "0" {
        return doc("Bu notun sayfa düzeni desteklenmiyor.");
    }
    let now_ms = advance(&root, a.now_ms)?;
    let tpi = one_idx(&root.children[ti], "pagelist")?;
    if root.children[pi].is_empty() || ids(&root.children[pi].children) != ids(&root.children[ti].children[tpi].children) {
        return doc("Sayfa ağacı notla uyuşmuyor.");
    }
    // Indices after a possible contentFileList insertion right after noteTree.
    let (mut ai, mut ti, mut pi) = (ai, ti, pi);
    if root.find("contentFileList").is_none() {
        root.children.insert(ti + 1, Element::new("contentFileList", &[]));
        let shift = |i: usize| if i > ti { i + 1 } else { i };
        ai = shift(ai);
        pi = shift(pi);
        ti = shift(ti);
    }
    let fi = one_idx(&root, "contentfilelist")?;
    let tree_files: Vec<usize> =
        root.children[ti].children.iter().enumerate().filter(|(_, n)| n.tag == "contentFileList").map(|(i, _)| i).collect();
    if tree_files.len() > 1 || (!root.children[fi].is_empty() && tree_files.is_empty()) {
        return doc("Dosya ağacı notla uyuşmuyor.");
    }
    if root.children[fi].children.iter().any(|f| f.get("id") == Some(sha.as_str())) {
        return doc("Bu PDF notta zaten var; aynı dosyayı ikinci kez eklemek desteklenmiyor.");
    }
    let binds: Vec<i64> = root.children[fi].children.iter().map(|f| integer(f.get("bindNumber"))).collect::<R<_>>()?;
    let attrs = &root.children[ai];
    let width = integer(attrs.get("width"))?;
    let padding = integer(Some(attrs.get_or("pageVerticalPadding", "0")))?;
    if !(0 < width && width < 20000) {
        return doc("Not genişliği desteklenmiyor.");
    }
    if root.children[pi].children.iter().any(|p| p.get("width") != Some(width.to_string().as_str())) {
        return doc("Farklı genişlikte sayfaları olan notlara henüz PDF sayfası eklenemiyor.");
    }
    let base = next_tick(&root, &root.children[ai])?;
    let tick = base + 2 * indices.len() as i128;
    let revision = integer(root.children[ai].get("fileRevision"))? as i128 + 1;
    if tick >= I63 || revision >= 1 << 31 {
        return doc("Not sürümü taşması.");
    }
    let (base, tick) = (base as i64, tick as i64);
    let bind = binds.iter().max().map(|b| b + 1).unwrap_or(0);
    let bound_name = format!("{bind}@{file_name}");
    let name = base64::engine::general_purpose::STANDARD.encode(bound_name.as_bytes());
    let fresh: Vec<Option<String>> = match &a.ids {
        Some(v) => v.clone(),
        None => (0..2 * indices.len()).map(|_| Some(uuid4())).collect(),
    };
    if fresh.len() != 2 * indices.len() || fresh.iter().collect::<HashSet<_>>().len() != fresh.len() {
        return doc("Kimlikler geçersiz.");
    }
    let bg = root.children[pi].children.last().unwrap().get_or("bgColor", "-197380").to_string();
    let (mut new_pages, mut new_tree_pages, mut page_ids, mut layer_ids) = (vec![], vec![], vec![], vec![]);
    for (n, &index) in indices.iter().enumerate() {
        let (pw, ph) = sizes[index];
        let height = (width as f64 * ph / pw).round_ties_even() as i64;
        let (page_id, layer_id) = (fresh[2 * n].clone(), fresh[2 * n + 1].clone());
        let (layer_t, page_t) = (base + 2 * n as i64, base + 2 * n as i64 + 1);
        let layer_raw = parent_hash(layer_id.as_deref(), layer_t, &[])?;
        let layer_h = hex(&layer_raw);
        let page_h = hex(&parent_hash(page_id.as_deref(), page_t, &[layer_raw])?);
        let (page_id, layer_id) = (page_id.unwrap(), layer_id.unwrap());
        let (w, h, pt, lt) = (width.to_string(), height.to_string(), page_t.to_string(), layer_t.to_string());
        let mut attrs_list: Vec<(&str, &str)> = vec![
            ("bgColor", &bg),
            ("bgWidth", &w),
            ("createdTime", "0"),
            ("currentLayerIndex", "0"),
            ("formatVersion", "4000"),
            ("hash", &page_h),
            ("height", &h),
            ("id", &page_id),
            ("isTextOnly", "false"),
            ("minFormatVersion", "4000"),
            ("modifiedTime", &pt),
            ("offsetX", "0"),
            ("offsetY", "0"),
            ("orientation", "0"),
        ];
        let tt = template_type.map(|t| t.to_string());
        if let Some(t) = &tt {
            attrs_list.push(("templateType", t));
        }
        attrs_list.push(("width", &w));
        let mut page = Element::new("page", &attrs_list);
        let idx = index.to_string();
        page.sub_element("pdfDataList", &[])
            .sub_element("pdfData", &[("hash", &sha), ("pageIndex", &idx)])
            .sub_element("pdfRect", &[("b", &h), ("l", "0"), ("r", &w), ("t", "0")]);
        page.sub_element("layerList", &[])
            .sub_element(
                "layer",
                &[
                    ("createdTime", "0"),
                    ("flagEventForwardable", "true"),
                    ("hash", &layer_h),
                    ("id", &layer_id),
                    ("layerNumber", "0"),
                    ("lockState", "false"),
                    ("modifiedTime", &lt),
                    ("thumbnail", "-1"),
                    ("visible", "true"),
                ],
            )
            .sub_element("objectList", &[]);
        let mut node = Element::new("page", &[("h", &page_h), ("id", &page_id), ("t", &pt)]);
        node.sub_element("layerList", &[]).sub_element("layer", &[("h", &layer_h), ("id", &layer_id), ("t", &lt)]);
        new_pages.push(page);
        new_tree_pages.push(node);
        page_ids.push(page_id);
        layer_ids.push(layer_id);
    }
    let ref_count = indices.len();
    let file_h = file_hash(&sha, bind, &bound_name, ref_count, tick);
    let (bs, ts, rc) = (bind.to_string(), tick.to_string(), ref_count.to_string());
    let mut entry = Element::new(
        "contentFile",
        &[
            ("bindNumber", &bs),
            ("createdTime", "0"),
            ("hash", &file_h),
            ("id", &sha),
            ("isFileAttached", "true"),
            ("modifiedTime", &ts),
            ("name", &name),
            ("refCount", &rc),
            ("uploadToken", &token),
        ],
    );
    entry.sub_element("fileHash", &[]).text = Some(sha.clone());
    root.children[pi].children.extend(new_pages);
    root.children[ti].children[tpi].children.extend(new_tree_pages);
    root.children[fi].children.push(entry);
    let tf = match tree_files.first() {
        Some(i) => *i,
        None => {
            // Server order (live readback): noteTree = pageList, contentFileList, bodyText.
            root.children[ti].children.insert(tpi + 1, Element::new("contentFileList", &[]));
            tpi + 1
        }
    };
    root.children[ti].children[tf].sub_element("contentFile", &[("h", &file_h), ("id", &sha), ("t", &ts)]);
    let heights: Vec<i64> = root.children[pi].children.iter().map(|p| integer(p.get("height"))).collect::<R<_>>()?;
    let total: i128 = heights.iter().map(|h| *h as i128).sum::<i128>() + padding as i128 * (heights.len() as i128 - 1);
    let attrs = &mut root.children[ai];
    attrs.set("height", &total.to_string());
    attrs.set("fileRevision", &revision.to_string());
    attrs.set("modifiedTime", &ts);
    attrs.set("displayModifiedTime", &now_ms.to_string());
    root.set("syncModifiedTime", &now_ms.to_string());
    let mut upload = upload_copy(&root);
    one_mut(&mut upload, "contentfilelist")?.children.retain(|n| n.get("id") == Some(sha.as_str()));
    one_mut(&mut upload, "pagelist")?.children.retain(|p| p.get("id").is_some_and(|i| page_ids.iter().any(|k| k == i)));
    Ok(PdfPages {
        full_xml: xml::xml_bytes(&root),
        tree_xml: xml::xml_bytes(&root.children[ti]),
        upload_xml: xml::xml_bytes(&upload),
        page_ids,
        layer_ids,
        file_id: sha,
        tick,
    })
}

// ------------------------------------------------------------------ Python

#[cfg(feature = "python")]
crate::record_class!(PyPdfPagesEdit, "PdfPagesEdit", "samsung_notes.page_template",
    [full_xml, tree_xml, upload_xml, page_ids, layer_ids, file_id, tick]);

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "pdf_page_sizes", signature = (path, *, timeout=None))]
fn py_pdf_page_sizes<'py>(path: &Bound<'py, PyAny>, timeout: Option<f64>) -> PyResult<Bound<'py, PyList>> {
    let py = path.py();
    let p = path.str()?.to_string();
    let t = Duration::from_secs_f64(timeout.unwrap_or(20.0).max(0.0));
    let sizes = py.detach(|| pdf_page_sizes(&p, t)).py(py)?;
    PyList::new(py, sizes)
}

#[pyfunction]
#[pyo3(name = "prepare_pdf_pages",
       signature = (snapshot, *, file_sha256, upload_token, file_name, page_sizes, now_ms,
                    template_type=None, page_indices=None, ids=None))]
#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn py_prepare_pdf_pages<'py>(
    snapshot: &Bound<'py, PyAny>,
    file_sha256: &Bound<'py, PyAny>,
    upload_token: &Bound<'py, PyAny>,
    file_name: &Bound<'py, PyAny>,
    page_sizes: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    template_type: Option<Bound<'py, PyAny>>,
    page_indices: Option<Bound<'py, PyAny>>,
    ids: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    #[cfg(feature = "python")]
    use crate::ink_erase::exact_int;
    #[cfg(feature = "python")]
    use crate::ink_erase::opt_str;
    let py = snapshot.py();
    let deps = PyDeps { py };
    let mut sizes = Vec::new();
    for item in page_sizes.try_iter()? {
        let item = item?;
        let pair: Vec<Bound<PyAny>> = item.try_iter()?.collect::<PyResult<_>>()?;
        if pair.len() != 2 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!("expected 2 values to unpack (got {})", pair.len())));
        }
        sizes.push((pair[0].extract::<f64>()?, pair[1].extract::<f64>()?));
    }
    let list = |o: &Bound<'py, PyAny>| -> PyResult<Vec<Bound<'py, PyAny>>> { o.try_iter()?.collect() };
    let args = PdfArgs {
        file_sha256: opt_str(file_sha256),
        upload_token: opt_str(upload_token),
        file_name: opt_str(file_name),
        page_sizes: sizes,
        now_ms: exact_int(now_ms),
        template_type: template_type.filter(|t| !t.is_none()).map(|t| exact_int(&t)),
        page_indices: match page_indices.filter(|p| !p.is_none()) {
            Some(p) => Some(list(&p)?.iter().map(exact_int).collect()),
            None => None,
        },
        ids: match ids.filter(|p| !p.is_none()) {
            Some(p) => Some(list(&p)?.iter().map(opt_str).collect()),
            None => None,
        },
    };
    let e = prepare_pdf_pages(&deps, &bridge::opaque(snapshot), &args).py(py)?;
    let obj = PyPdfPagesEdit {
        full_xml: PyBytes::new(py, &e.full_xml).into_any().unbind(),
        tree_xml: PyBytes::new(py, &e.tree_xml).into_any().unbind(),
        upload_xml: PyBytes::new(py, &e.upload_xml).into_any().unbind(),
        page_ids: PyTuple::new(py, &e.page_ids)?.into_any().unbind(),
        layer_ids: PyTuple::new(py, &e.layer_ids)?.into_any().unbind(),
        file_id: e.file_id.into_pyobject(py)?.into_any().unbind(),
        tick: e.tick.into_pyobject(py)?.into_any().unbind(),
    };
    Ok(Bound::new(py, obj)?.into_any())
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPdfPagesEdit>()?;
    m.add_function(wrap_pyfunction!(py_pdf_page_sizes, m)?)?;
    m.add_function(wrap_pyfunction!(py_prepare_pdf_pages, m)?)?;
    rs_common::export(m, &["PdfPagesEdit", "pdf_page_sizes", "prepare_pdf_pages"])
}
