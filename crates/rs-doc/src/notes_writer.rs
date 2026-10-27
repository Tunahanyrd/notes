//! Python bindings replacing `tools.notes_writer`.
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;

#[cfg(feature = "python")]
use pyo3::exceptions::{PySystemExit, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyList, PyModule, PyString, PyTuple};

use crate::pyapi::{self, IntoPy};
use crate::util::{self, Json};
use crate::writer::{self, ResearchNote};

#[cfg(feature = "python")]
fn verr(msg: &'static str) -> PyErr {
    PyValueError::new_err(msg)
}

#[cfg(feature = "python")]
/// `_uuid(value)`
fn uuid_arg(v: &Bound<'_, PyAny>) -> PyResult<String> {
    let s = v.cast::<PyString>().map_err(|_| verr("canonical lower-case UUID required"))?;
    let s = s.to_str().map_err(|_| verr("badly formed hexadecimal UUID string"))?;
    if !util::uuid_parses(s) {
        return Err(verr("badly formed hexadecimal UUID string"));
    }
    if !util::is_canonical_uuid(s) {
        return Err(verr("canonical lower-case UUID required"));
    }
    Ok(s.to_string())
}

#[cfg(feature = "python")]
/// `_timestamp(value)`
fn timestamp_arg(v: &Bound<'_, PyAny>) -> PyResult<i64> {
    match pyapi::exact_int(v) {
        Some(n) => writer::check_timestamp(n).map_err(|e| pyapi::to_pyerr(v.py(), e)),
        None => Err(verr("positive signed-64-bit millisecond timestamp required")),
    }
}

#[cfg(feature = "python")]
/// `_text(value)`
fn text_arg(v: &Bound<'_, PyAny>) -> PyResult<String> {
    let s = v.cast::<PyString>().map_err(|_| verr("nonempty plain text required"))?;
    if s.len()? == 0 {
        return Err(verr("nonempty plain text required"));
    }
    let s = s.to_str().map_err(|_| verr("unsupported text character"))?;
    writer::check_text(s).py(v.py())?;
    Ok(s.to_string())
}

#[cfg(feature = "python")]
/// q1.c.t(): SHA256(UTF8(id + decimal time)); not a content checksum.
#[pyfunction]
fn object_hash<'py>(py: Python<'py>, object_id: &Bound<'py, PyAny>, modified_ms: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let id = uuid_arg(object_id)?;
    let ms = timestamp_arg(modified_ms)?;
    Ok(PyBytes::new(py, &writer::object_hash(&id, ms as i128).py(py)?))
}

#[cfg(feature = "python")]
/// n1.u: child raw hashes in order, then own id/time hash, then SHA256.
#[pyfunction]
fn parent_hash<'py>(py: Python<'py>, parent_id: &Bound<'py, PyAny>, modified_ms: &Bound<'py, PyAny>, child_hashes: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let mut children: Vec<[u8; 32]> = Vec::new();
    let mut bad = false;
    for h in child_hashes.try_iter()? {
        let h = h?;
        match h.cast::<PyBytes>() {
            Ok(b) if b.as_bytes().len() == 32 => children.push(b.as_bytes().try_into().unwrap()),
            _ => bad = true,
        }
    }
    if bad {
        return Err(verr("child hashes must be raw SHA256 bytes"));
    }
    let id = uuid_arg(parent_id)?;
    let ms = timestamp_arg(modified_ms)?;
    Ok(PyBytes::new(py, &writer::parent_hash(&id, ms as i128, &children).py(py)?))
}

#[cfg(feature = "python")]
/// q1.n.d(): annotation-free rich text is Base64(Base64(UTF8(text))).
#[pyfunction]
fn encode_plain_text(value: &Bound<'_, PyAny>) -> PyResult<String> {
    let s = text_arg(value)?;
    Ok(writer::double_b64(&s))
}

#[cfg(feature = "python")]
#[pyclass(module = "tools.notes_writer", name = "ResearchNote", frozen, subclass)]
pub struct PyResearchNote {
    pub inner: ResearchNote,
}

const NOTE_FIELDS: [&str; 10] =
    ["note_id", "title_id", "body_id", "page_id", "layer_id", "created_ms", "modified_ms", "title", "body", "revision"];

#[cfg(feature = "python")]
impl PyResearchNote {
    #[cfg(feature = "python")]
    fn values(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let n = &self.inner;
        let s = |v: &str| PyString::new(py, v).into_any().unbind();
        Ok(vec![
            s(&n.note_id), s(&n.title_id), s(&n.body_id), s(&n.page_id), s(&n.layer_id),
            n.created_ms.into_pyobject(py)?.into_any().unbind(),
            n.modified_ms.into_pyobject(py)?.into_any().unbind(),
            s(&n.title), s(&n.body),
            n.revision.into_pyobject(py)?.into_any().unbind(),
        ])
    }
}

#[cfg(feature = "python")]
pub fn build_note(
    ids: [&Bound<'_, PyAny>; 5],
    created: &Bound<'_, PyAny>,
    modified: &Bound<'_, PyAny>,
    title: &Bound<'_, PyAny>,
    body: &Bound<'_, PyAny>,
    revision: &Bound<'_, PyAny>,
) -> PyResult<ResearchNote> {
    let mut parsed = Vec::with_capacity(5);
    for id in ids {
        parsed.push(uuid_arg(id)?);
    }
    for (i, a) in parsed.iter().enumerate() {
        if parsed[i + 1..].contains(a) {
            return Err(verr("research-note identifiers must be distinct"));
        }
    }
    let c = timestamp_arg(created)?;
    let m = timestamp_arg(modified)?;
    let rev = pyapi::exact_int(revision);
    if m < c || !matches!(rev, Some(r) if (1..=i32::MAX as i128).contains(&r)) {
        return Err(verr("invalid research-note revision"));
    }
    let t = text_arg(title)?;
    let b = text_arg(body)?;
    ResearchNote::new([&parsed[0], &parsed[1], &parsed[2], &parsed[3], &parsed[4]], c as i128, m as i128, &t, &b, rev.unwrap()).py(title.py())
}

#[cfg(feature = "python")]
#[pymethods]
impl PyResearchNote {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (note_id, title_id, body_id, page_id, layer_id, created_ms, modified_ms, title, body, revision=None))]
    #[allow(clippy::too_many_arguments)]
    fn __new__(
        py: Python<'_>,
        note_id: &Bound<'_, PyAny>,
        title_id: &Bound<'_, PyAny>,
        body_id: &Bound<'_, PyAny>,
        page_id: &Bound<'_, PyAny>,
        layer_id: &Bound<'_, PyAny>,
        created_ms: &Bound<'_, PyAny>,
        modified_ms: &Bound<'_, PyAny>,
        title: &Bound<'_, PyAny>,
        body: &Bound<'_, PyAny>,
        revision: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let one = 1i64.into_pyobject(py)?.into_any();
        let revision = revision.cloned().unwrap_or(one);
        Ok(PyResearchNote { inner: build_note([note_id, title_id, body_id, page_id, layer_id], created_ms, modified_ms, title, body, &revision)? })
    }
    #[cfg(feature = "python")]
    #[classattr]
    fn __dataclass_fields__(py: Python<'_>) -> PyResult<Py<PyDict>> {
        let one = 1i64.into_pyobject(py)?.into_any().unbind();
        pyapi::dataclass_fields(py, &NOTE_FIELDS, &[("revision", one)])
    }
    #[cfg(feature = "python")]
    #[getter] fn note_id(&self) -> &str { &self.inner.note_id }
    #[getter] fn title_id(&self) -> &str { &self.inner.title_id }
    #[getter] fn body_id(&self) -> &str { &self.inner.body_id }
    #[getter] fn page_id(&self) -> &str { &self.inner.page_id }
    #[getter] fn layer_id(&self) -> &str { &self.inner.layer_id }
    #[getter] fn created_ms(&self) -> i64 { self.inner.created_ms }
    #[getter] fn modified_ms(&self) -> i64 { self.inner.modified_ms }
    #[getter] fn title(&self) -> &str { &self.inner.title }
    #[getter] fn body(&self) -> &str { &self.inner.body }
    #[getter] fn revision(&self) -> i64 { self.inner.revision }
    fn __repr__(&self) -> String {
        format!("ResearchNote(revision={}, content=PRIVATE, offline=True)", self.inner.revision)
    }
    #[cfg(feature = "python")]
    fn __eq__(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let mine = slf.get().values(py)?;
        pyapi::fields_eq(py, slf.get_type().as_any(), other, mine, || other.cast::<Self>()?.get().values(py))
    }
    #[cfg(feature = "python")]
    fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
        PyTuple::new(py, self.values(py)?)?.hash()
    }
    #[cfg(feature = "python")]
    fn hashes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let h = self.inner.hashes();
        let d = PyDict::new(py);
        d.set_item("title", h.title)?;
        d.set_item("body", h.body)?;
        d.set_item("layer", h.layer)?;
        d.set_item("page", h.page)?;
        Ok(d)
    }
    #[cfg(feature = "python")]
    fn tree_element<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        py.import("xml.etree.ElementTree")?.getattr("fromstring")?.call1((PyBytes::new(py, &self.inner.tree_xml()),))
    }
    #[cfg(feature = "python")]
    fn tree_xml<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.tree_xml())
    }
    #[cfg(feature = "python")]
    fn xml<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.xml())
    }
}

#[cfg(feature = "python")]
fn now_arg(now_ms: Option<&Bound<'_, PyAny>>) -> PyResult<i64> {
    match now_ms.filter(|v| !v.is_none()) {
        None => Ok(writer::now_ms()),
        Some(v) => timestamp_arg(v),
    }
}

#[cfg(feature = "python")]
/// Create fresh local IDs; does not accept a downloaded note or remote ID.
#[pyfunction]
#[pyo3(signature = (title, body, *, now_ms=None))]
fn new_plain_text_note(py: Python<'_>, title: &Bound<'_, PyAny>, body: &Bound<'_, PyAny>, now_ms: Option<&Bound<'_, PyAny>>) -> PyResult<PyResearchNote> {
    let now = now_arg(now_ms)?;
    let t = text_arg(title)?;
    let b = text_arg(body)?;
    Ok(PyResearchNote { inner: writer::new_plain_text_note(&t, &b, now).py(py)? })
}

#[cfg(feature = "python")]
/// Prepare a local draft revision only; caller must verify compare commitId.
#[pyfunction]
#[pyo3(signature = (note, title, body, *, now_ms=None))]
fn revise_research_note(py: Python<'_>, note: &Bound<'_, PyAny>, title: &Bound<'_, PyAny>, body: &Bound<'_, PyAny>, now_ms: Option<&Bound<'_, PyAny>>) -> PyResult<PyResearchNote> {
    let note = note.cast::<PyResearchNote>().map_err(|_| verr("locally generated research note required"))?;
    let now = now_arg(now_ms)?;
    let current = &note.get().inner;
    if now <= current.modified_ms {
        return Err(verr("revision timestamp must increase"));
    }
    let t = text_arg(title)?;
    let b = text_arg(body)?;
    Ok(PyResearchNote { inner: writer::revise_research_note(current, &t, &b, now).py(py)? })
}

#[cfg(feature = "python")]
/// Return (gzip bytes, Content-Type); one XML part and no binary resources.
#[pyfunction]
#[pyo3(signature = (note, *, boundary=None))]
fn create_multipart<'py>(py: Python<'py>, note: &Bound<'py, PyAny>, boundary: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyTuple>> {
    let note = note.cast::<PyResearchNote>().map_err(|_| verr("locally generated research note required"))?;
    let chosen: Option<String> = match boundary {
        Some(b) if b.is_truthy()? => {
            let s = b.cast::<PyString>().map_err(|_| verr("invalid multipart boundary"))?;
            Some(s.to_str().map_err(|_| verr("invalid multipart boundary"))?.to_string())
        }
        _ => None,
    };
    let (data, ct) = writer::create_multipart(&note.get().inner, chosen.as_deref()).py(py)?;
    PyTuple::new(py, [PyBytes::new(py, &data).into_any(), PyString::new(py, &ct).into_any()])
}

const USAGE: &str = "[-h] --title TITLE --body BODY --output OUTPUT";

#[cfg(feature = "python")]
fn usage_error(py: Python<'_>, prog: &str, message: &str) -> PyResult<()> {
    let sys = py.import("sys")?;
    let stderr = sys.getattr("stderr")?;
    stderr.call_method1("write", (format!("usage: {prog} {USAGE}\n{prog}: error: {message}\n"),))?;
    Err(PySystemExit::new_err(2))
}

#[cfg(feature = "python")]
/// Offline CLI (argparse-compatible subset of the original).
#[pyfunction]
#[pyo3(signature = (argv=None))]
fn main(py: Python<'_>, argv: Option<&Bound<'_, PyAny>>) -> PyResult<i32> {
    let sys = py.import("sys")?;
    let args: Vec<String> = match argv.filter(|a| !a.is_none()) {
        Some(a) => a.extract()?,
        None => {
            let full: Vec<String> = sys.getattr("argv")?.extract()?;
            full.into_iter().skip(1).collect()
        }
    };
    let prog: String = {
        let argv0: Vec<String> = sys.getattr("argv")?.extract().unwrap_or_default();
        let a0 = argv0.first().cloned().unwrap_or_default();
        std::path::Path::new(&a0).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
    };
    const OPTS: [&str; 3] = ["--title", "--body", "--output"];
    let mut values: [Option<String>; 3] = [None, None, None];
    let mut i = 0;
    let mut extra: Vec<String> = Vec::new();
    while i < args.len() {
        let arg = &args[i];
        if arg == "-h" || (arg.starts_with("--h") && "--help".starts_with(arg.as_str())) {
            let stdout = sys.getattr("stdout")?;
            stdout.call_method1("write", (format!(
                "usage: {prog} {USAGE}\n\noptions:\n  -h, --help       show this help message and exit\n  --title TITLE\n  --body BODY\n  --output OUTPUT  new offline XML file; never overwritten\n"
            ),))?;
            return Err(PySystemExit::new_err(0));
        }
        if arg.starts_with("--") && arg.len() > 2 {
            let (name, inline) = match arg.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (arg.clone(), None),
            };
            let matches: Vec<usize> = (0..3).filter(|&k| OPTS[k].starts_with(name.as_str())).collect();
            if matches.len() > 1 {
                let names: Vec<&str> = matches.iter().map(|&k| OPTS[k]).collect();
                return usage_error(py, &prog, &format!("ambiguous option: {name} could match {}", names.join(", "))).map(|_| 0);
            }
            if let Some(&k) = matches.first() {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        match args.get(i + 1) {
                            Some(v) if !v.starts_with('-') || v == "-" => {
                                i += 1;
                                v.clone()
                            }
                            _ => return usage_error(py, &prog, &format!("argument {}: expected one argument", OPTS[k])).map(|_| 0),
                        }
                    }
                };
                values[k] = Some(value);
                i += 1;
                continue;
            }
        }
        extra.push(arg.clone());
        i += 1;
    }
    let missing: Vec<&str> = (0..3).filter(|&k| values[k].is_none()).map(|k| OPTS[k]).collect();
    if !missing.is_empty() {
        return usage_error(py, &prog, &format!("the following arguments are required: {}", missing.join(", "))).map(|_| 0);
    }
    if !extra.is_empty() {
        return usage_error(py, &prog, &format!("unrecognized arguments: {}", extra.join(" "))).map(|_| 0);
    }
    let [Some(title), Some(body), Some(output)] = values else { unreachable!() };
    for t in [&title, &body] {
        writer::check_text(t).py(py)?;
    }
    let note = writer::new_plain_text_note(&title, &body, writer::now_ms()).py(py)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&output).map_err(PyErr::from)?;
    let xml = note.xml();
    file.write_all(&xml).map_err(PyErr::from)?;
    drop(file);
    let out = Json::Object(vec![
        ("offline".into(), Json::Bool(true)),
        ("kind".into(), Json::Str("experimental-research-note".into())),
        ("bytes".into(), Json::Int(xml.len() as i128)),
        ("noteId".into(), Json::Str(note.note_id.clone())),
        ("createdMs".into(), Json::Int(note.created_ms as i128)),
        ("networkRequests".into(), Json::Int(0)),
        ("serverValidated".into(), Json::Bool(false)),
    ]);
    py.import("builtins")?.getattr("print")?.call1((util::json_dumps(&out, None, true),))?;
    Ok(0)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(object_hash, m)?)?;
    m.add_function(wrap_pyfunction!(parent_hash, m)?)?;
    m.add_function(wrap_pyfunction!(encode_plain_text, m)?)?;
    m.add_class::<PyResearchNote>()?;
    m.add_function(wrap_pyfunction!(new_plain_text_note, m)?)?;
    m.add_function(wrap_pyfunction!(revise_research_note, m)?)?;
    m.add_function(wrap_pyfunction!(create_multipart, m)?)?;
    m.add_function(wrap_pyfunction!(main, m)?)?;
    let _ = PyList::empty(m.py());
    rs_common::export(m, &[
        "object_hash", "parent_hash", "encode_plain_text", "ResearchNote",
        "new_plain_text_note", "revise_research_note", "create_multipart", "main",
    ])
}
