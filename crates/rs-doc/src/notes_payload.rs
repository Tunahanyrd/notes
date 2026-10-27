//! Python bindings replacing `tools.notes_payload`.
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::io::Write as _;

#[cfg(feature = "python")]
use pyo3::exceptions::PyTypeError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyDict, PyList, PyModule, PyString, PyTuple};

use crate::mime::{self, Part};
use crate::payload;
#[cfg(feature = "python")]
use crate::pyapi::{self, IntoPy, PayloadError, PyHeaders, PyHooks};
use crate::util::{self, Json};

/// Generates a frozen dataclass-like pyclass whose fields are Python objects.
macro_rules! value_class {
    ($rust:ident, $pyname:literal, $module:literal, [$($field:ident),+] $(, default $dfield:ident)? ; { $($extra:tt)* }) => {
        #[cfg(feature = "python")]
        #[pyclass(module = $module, name = $pyname, frozen, subclass)]
        pub struct $rust {
            $(#[pyo3(get)] pub $field: Py<PyAny>,)+
            $(#[pyo3(get)] pub $dfield: Py<PyAny>,)?
        }
        impl $rust {
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($field),)+ $(stringify!($dfield))?];
            #[cfg(feature = "python")]
            pub fn values(&self, py: Python<'_>) -> Vec<Py<PyAny>> {
                vec![$(self.$field.clone_ref(py),)+ $(self.$dfield.clone_ref(py))?]
            }
        }
        #[cfg(feature = "python")]
        #[pymethods]
        impl $rust {
            #[cfg(feature = "python")]
            #[new]
            #[pyo3(signature = ($($field,)+ $($dfield = None)?))]
            fn __new__($($field: Py<PyAny>,)+ $($dfield: Option<Py<PyAny>>)?) -> Self {
                Python::attach(|_py| $rust { $($field,)+ $($dfield: $dfield.unwrap_or_else(|| _py.None()))? })
            }
            #[cfg(feature = "python")]
            #[classattr]
            fn __dataclass_fields__(py: Python<'_>) -> PyResult<Py<PyDict>> {
                #[allow(unused_mut)]
                let mut defaults: Vec<(&str, Py<PyAny>)> = Vec::new();
                $(defaults.push((stringify!($dfield), py.None()));)?
                crate::pyapi::dataclass_fields(py, Self::FIELDS, &defaults)
            }
            #[cfg(feature = "python")]
            fn __eq__(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
                let py = slf.py();
                let mine = slf.get().values(py);
                crate::pyapi::fields_eq(py, slf.get_type().as_any(), other, mine, || {
                    let o = other.cast::<Self>()?;
                    Ok(o.get().values(py))
                })
            }
            #[cfg(feature = "python")]
            fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
                pyo3::types::PyTuple::new(py, self.values(py))?.hash()
            }
            $($extra)*
        }
    };
}
pub(crate) use value_class;

#[cfg(feature = "python")]
value_class!(PyPayloadPart, "PayloadPart", "tools.notes_payload", [index, name, filename, content_type, body]; {
    #[cfg(feature = "python")]
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("PayloadPart(index={}, bytes={}, private=True)", self.index.bind(py).str()?, self.body.bind(py).len()?))
    }
});
#[cfg(feature = "python")]
value_class!(PyDownloadUrl, "DownloadUrl", "tools.notes_payload", [id, url]; {
    fn __repr__(&self) -> &'static str { "DownloadUrl([PRIVATE])" }
});
#[cfg(feature = "python")]
value_class!(PyContentFile, "ContentFile", "tools.notes_payload", [file_hash, source_id, metadata_hash, name, name_encoded, bind_number]; {
    fn __repr__(&self) -> &'static str { "ContentFile([PRIVATE])" }
});
#[cfg(feature = "python")]
value_class!(PyTextPreview, "TextPreview", "tools.notes_payload", [context, text]; {
    #[cfg(feature = "python")]
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("TextPreview(context={}, characters={}, private=True)", self.context.bind(py).str()?, self.text.bind(py).len()?))
    }
});
#[cfg(feature = "python")]
value_class!(PyPayloadResult, "PayloadResult", "tools.notes_payload",
    [body, content_type, parts, xml_documents, xml_roots, resource_ids, download_urls, content_files, text_previews, locked]; {
    #[cfg(feature = "python")]
    /// Private logical-text preview; does not render layout or handwriting.
    #[getter]
    fn text(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let texts: Vec<Bound<'_, PyAny>> = self.text_previews.bind(py).try_iter()?
            .map(|p| p.and_then(|p| p.getattr("text"))).collect::<PyResult<_>>()?;
        Ok(PyString::new(py, "\n").call_method1("join", (PyList::new(py, texts)?,))?.unbind())
    }
    #[cfg(feature = "python")]
    fn safe_summary(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        safe_summary(slf.as_any())
    }
    #[cfg(feature = "python")]
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("PayloadResult(parts={}, private=True)", self.parts.bind(py).len()?))
    }
});

#[cfg(feature = "python")]
fn opt_str(py: Python<'_>, v: &Option<String>) -> Py<PyAny> {
    match v {
        Some(s) => PyString::new(py, s).into_any().unbind(),
        None => py.None(),
    }
}

#[cfg(feature = "python")]
fn part_object(py: Python<'_>, part: &Part, body: Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let obj = PyPayloadPart {
        index: part.index.into_pyobject(py)?.into_any().unbind(),
        name: opt_str(py, &part.name),
        filename: opt_str(py, &part.filename),
        content_type: PyString::new(py, &part.content_type).into_any().unbind(),
        body: body.unbind(),
    };
    Ok(Py::new(py, obj)?.into_any())
}

#[cfg(feature = "python")]
/// Validate `max_parts` like `_parts` and resolve the default.
fn max_parts_arg(py: Python<'_>, max_parts: Option<&Bound<'_, PyAny>>, limits: &mime::Limits) -> PyResult<i64> {
    let Some(v) = max_parts.filter(|v| !v.is_none()) else { return Ok(limits.max_parts) };
    match pyapi::exact_int(v) {
        Some(n) if n >= 1 && n <= limits.max_readonly_parts as i128 => Ok(n as i64),
        _ => Err(pyapi::to_pyerr(py, crate::Error::Payload("invalid MIME part bound"))),
    }
}

#[cfg(feature = "python")]
fn content_type_str<'a>(py: Python<'_>, v: &'a Bound<'_, PyAny>) -> PyResult<&'a str> {
    match v.cast::<PyString>() {
        Ok(s) => s.to_str().map_err(|_| PayloadError::new_err("invalid content type")),
        Err(_) => Err(pyapi::to_pyerr(py, crate::Error::Payload("invalid content type"))),
    }
}

#[cfg(feature = "python")]
/// Rust `_parts` producing core parts plus Python part objects.
pub(crate) fn rust_parts<'py>(
    py: Python<'py>,
    body: &Bound<'py, PyAny>,
    content_type: &Bound<'py, PyAny>,
    max_parts: i64,
    limits: &mime::Limits,
) -> PyResult<(Vec<Part>, Vec<Py<PyAny>>)> {
    let raw = body.cast::<PyBytes>().map_err(|_| PyTypeError::new_err("expected bytes payload"))?;
    let ct = content_type_str(py, content_type)?;
    let mut hooks = PyHooks::new(py)?;
    let parts = mime::parts(raw.as_bytes(), ct, max_parts, limits, &mut hooks).py(py)?;
    let mut objs = Vec::with_capacity(parts.len());
    for p in &parts {
        let b = if p.body.start == 0 && p.body.end == raw.as_bytes().len() {
            body.clone()
        } else {
            PyBytes::new(py, &raw.as_bytes()[p.body.clone()]).into_any()
        };
        objs.push(part_object(py, p, b)?);
    }
    Ok((parts, objs))
}

#[cfg(feature = "python")]
/// Parts from a (patched) Python `_parts`: rebuild core parts over a joined buffer.
pub(crate) fn parts_from_python(objs: &Bound<'_, PyAny>) -> PyResult<(Vec<Part>, Vec<u8>, Vec<Py<PyAny>>)> {
    let mut parts = Vec::new();
    let mut buf = Vec::new();
    let mut keep = Vec::new();
    for (i, obj) in objs.try_iter()?.enumerate() {
        let obj = obj?;
        let body = obj.getattr("body")?;
        let bytes = pyapi::bytes_of(&body)?;
        let start = buf.len();
        buf.extend_from_slice(&bytes);
        let s = |name: &str| -> PyResult<Option<String>> {
            let v = obj.getattr(name)?;
            if v.is_none() { Ok(None) } else { Ok(Some(v.extract()?)) }
        };
        parts.push(Part {
            index: obj.getattr("index")?.extract().unwrap_or(i),
            name: s("name")?,
            filename: s("filename")?,
            content_type: obj.getattr("content_type")?.extract()?,
            body: start..buf.len(),
        });
        keep.push(obj.unbind());
    }
    Ok((parts, buf, keep))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "_parts", signature = (body, content_type, *, max_parts=None))]
fn parts_py<'py>(py: Python<'py>, body: &Bound<'py, PyAny>, content_type: &Bound<'py, PyAny>, max_parts: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyList>> {
    let limits = pyapi::payload_limits(py)?;
    let max = max_parts_arg(py, max_parts, &limits)?;
    let (_, objs) = rust_parts(py, body, content_type, max, &limits)?;
    PyList::new(py, objs)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "_headers")]
fn headers_py(py: Python<'_>, raw: &Bound<'_, PyAny>) -> PyResult<PyHeaders> {
    let limits = pyapi::payload_limits(py)?;
    let raw = pyapi::bytes_of(raw)?;
    Ok(PyHeaders { inner: mime::parse_headers(&raw, &limits).py(py)? })
}

#[cfg(feature = "python")]
/// Build the Python `PayloadResult` from a core result.
pub(crate) fn result_object<'py>(
    py: Python<'py>,
    body: &Bound<'py, PyAny>,
    content_type: &Bound<'py, PyAny>,
    result: &payload::PayloadResult,
    part_objs: Vec<Py<PyAny>>,
) -> PyResult<Bound<'py, PyPayloadResult>> {
    let docs: Vec<Py<PyAny>> = result
        .xml_parts
        .iter()
        .map(|&i| part_objs[i].bind(py).getattr("body").map(Bound::unbind))
        .collect::<PyResult<_>>()?;
    let strs = |v: &[String]| PyTuple::new(py, v.iter().map(|s| PyString::new(py, s)));
    let urls: Vec<Py<PyAny>> = result
        .download_urls
        .iter()
        .map(|u| Py::new(py, PyDownloadUrl { id: PyString::new(py, &u.id).into_any().unbind(), url: PyString::new(py, &u.url).into_any().unbind() }).map(Py::into_any))
        .collect::<PyResult<_>>()?;
    let files: Vec<Py<PyAny>> = result
        .content_files
        .iter()
        .map(|f| {
            Py::new(py, PyContentFile {
                file_hash: PyString::new(py, &f.file_hash).into_any().unbind(),
                source_id: opt_str(py, &f.source_id),
                metadata_hash: opt_str(py, &f.metadata_hash),
                name: opt_str(py, &f.name),
                name_encoded: opt_str(py, &f.name_encoded),
                bind_number: opt_str(py, &f.bind_number),
            })
            .map(Py::into_any)
        })
        .collect::<PyResult<_>>()?;
    let previews: Vec<Py<PyAny>> = result
        .text_previews
        .iter()
        .map(|p| Py::new(py, PyTextPreview { context: PyString::new(py, p.context).into_any().unbind(), text: PyString::new(py, &p.text).into_any().unbind() }).map(Py::into_any))
        .collect::<PyResult<_>>()?;
    Bound::new(py, PyPayloadResult {
        body: body.clone().unbind(),
        content_type: content_type.clone().unbind(),
        parts: PyTuple::new(py, part_objs)?.into_any().unbind(),
        xml_documents: PyTuple::new(py, docs)?.into_any().unbind(),
        xml_roots: strs(&result.xml_roots)?.into_any().unbind(),
        resource_ids: strs(&result.resource_ids)?.into_any().unbind(),
        download_urls: PyTuple::new(py, urls)?.into_any().unbind(),
        content_files: PyTuple::new(py, files)?.into_any().unbind(),
        text_previews: PyTuple::new(py, previews)?.into_any().unbind(),
        locked: PyBool::new(py, result.locked).to_owned().into_any().unbind(),
    })
}

#[cfg(feature = "python")]
/// Core of `parse_payload` returning both the Python object and the core result.
pub(crate) fn parse_payload_full<'py>(
    py: Python<'py>,
    body: &Bound<'py, PyAny>,
    content_type: &Bound<'py, PyAny>,
    max_parts: Option<&Bound<'py, PyAny>>,
) -> PyResult<(Bound<'py, PyPayloadResult>, payload::PayloadResult)> {
    let limits = pyapi::payload_limits(py)?;
    let ok = match body.cast::<PyBytes>() {
        Ok(b) => !b.as_bytes().is_empty() && b.as_bytes().len() <= limits.max_body_bytes,
        Err(_) => false,
    };
    if !ok {
        return Err(PayloadError::new_err("payload is empty or exceeds size limit"));
    }
    let (result, objs) = match pyapi::patched_any(py, "_parts", &pyapi::BUILTIN_PARTS)? {
        None => {
            let max = max_parts_arg(py, max_parts, &limits)?;
            let (parts, objs) = rust_parts(py, body, content_type, max, &limits)?;
            let raw = body.cast::<PyBytes>()?.as_bytes();
            (payload::payload_from_parts(raw, parts, &limits).py(py)?, objs)
        }
        Some(f) => {
            let kw = PyDict::new(py);
            kw.set_item("max_parts", max_parts.map(|v| v.clone().unbind()).unwrap_or_else(|| py.None()))?;
            let returned = f.call((body, content_type), Some(&kw))?;
            let (parts, buf, objs) = parts_from_python(&returned)?;
            (payload::payload_from_parts(&buf, parts, &limits).py(py)?, objs)
        }
    };
    Ok((result_object(py, body, content_type, &result, objs)?, result))
}

#[cfg(feature = "python")]
/// Parse an HTTP-decoded private payload, preserving all original bytes.
#[pyfunction]
#[pyo3(signature = (body, content_type, *, max_parts=None))]
fn parse_payload<'py>(py: Python<'py>, body: &Bound<'py, PyAny>, content_type: &Bound<'py, PyAny>, max_parts: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyPayloadResult>> {
    Ok(parse_payload_full(py, body, content_type, max_parts)?.0)
}

#[cfg(feature = "python")]
/// Private logical text; outer Base64 plus annotation-delimited Base64 chunks.
#[pyfunction]
fn decode_rich_text(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<String> {
    let limits = pyapi::payload_limits(py)?;
    let s = value.cast::<PyString>().map_err(|_| PyTypeError::new_err("expected string or bytes-like object"))?;
    match s.to_str() {
        Ok(text) => payload::decode_rich_text(text, &limits).py(py),
        Err(_) => {
            let count = s.len()?;
            if count > ((limits.max_text_bytes + 2) / 3) * 4 {
                Err(PayloadError::new_err("encoded text exceeds limit"))
            } else {
                Err(PayloadError::new_err("invalid Base64 UTF-8 text"))
            }
        }
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn safe_summary(result: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let py = result.py();
    let d = PyDict::new(py);
    d.set_item("bodyBytes", result.getattr("body")?.len()?)?;
    d.set_item("partCount", result.getattr("parts")?.len()?)?;
    d.set_item("xmlDocumentCount", result.getattr("xml_documents")?.len()?)?;
    d.set_item("downloadUrlCount", result.getattr("download_urls")?.len()?)?;
    d.set_item("contentFileCount", result.getattr("content_files")?.len()?)?;
    let previews = result.getattr("text_previews")?;
    d.set_item("textPreviewCount", previews.len()?)?;
    let mut chars = 0usize;
    for p in previews.try_iter()? {
        chars += p?.getattr("text")?.len()?;
    }
    d.set_item("textPreviewCharacters", chars)?;
    d.set_item("locked", result.getattr("locked")?)?;
    d.set_item("sdocxExportProduced", false)?;
    Ok(d.into_any().unbind())
}

fn private_write_rust(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc_o_nofollow())
        .open(path)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()
}

fn libc_o_nofollow() -> i32 {
    0o400000 // O_NOFOLLOW on Linux
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(name = "_private_write")]
fn private_write(path: std::path::PathBuf, data: &Bound<'_, PyAny>) -> PyResult<()> {
    let data = pyapi::bytes_of(data)?;
    private_write_rust(&path, &data).map_err(PyErr::from)
}

#[cfg(feature = "python")]
fn json_of(v: &Bound<'_, PyAny>) -> PyResult<Json> {
    if v.is_none() {
        return Ok(Json::Null);
    }
    if let Ok(b) = v.cast::<PyBool>() {
        return Ok(Json::Bool(b.is_true()));
    }
    if let Ok(s) = v.cast::<PyString>() {
        return Ok(Json::Str(s.to_str()?.to_string()));
    }
    if let Ok(i) = v.extract::<i128>() {
        return Ok(Json::Int(i));
    }
    if let Ok(d) = v.cast::<PyDict>() {
        let mut items = Vec::new();
        for (k, val) in d.iter() {
            items.push((k.extract::<String>()?, json_of(&val)?));
        }
        return Ok(Json::Object(items));
    }
    let mut items = Vec::new();
    for item in v.try_iter()? {
        items.push(json_of(&item?)?);
    }
    Ok(Json::List(items))
}

#[cfg(feature = "python")]
/// Create a new private research cache. Partial failure leaves no cache.json.
#[pyfunction]
fn cache_to_directory(py: Python<'_>, result: &Bound<'_, PyAny>, directory: std::path::PathBuf) -> PyResult<Py<PyAny>> {
    std::fs::DirBuilder::new().mode(0o700).create(&directory).map_err(PyErr::from)?;
    let patched = pyapi::patched_any(py, "_private_write", &pyapi::BUILTIN_PRIVATE_WRITE)?;
    let pathlib = py.import("pathlib")?.getattr("Path")?;
    let write = |name: String, data: &[u8]| -> PyResult<()> {
        let path = directory.join(name);
        match &patched {
            None => private_write_rust(&path, data).map_err(PyErr::from),
            Some(f) => f.call1((pathlib.call1((path,))?, PyBytes::new(py, data))).map(|_| ()),
        }
    };
    let body = result.getattr("body")?;
    write("raw-body.bin".into(), &pyapi::bytes_of(&body)?)?;
    let ct: String = result.getattr("content_type")?.extract()?;
    if !ct.is_ascii() {
        return Err(pyo3::exceptions::PyUnicodeEncodeError::new_err("'ascii' codec can't encode characters"));
    }
    write("content-type.txt".into(), ct.as_bytes())?;
    let parts = result.getattr("parts")?;
    let mut part_json = Vec::new();
    for p in parts.try_iter()? {
        let p = p?;
        let index: i64 = p.getattr("index")?.extract()?;
        let pb = p.getattr("body")?;
        let pb = pyapi::bytes_of(&pb)?;
        let name = if index < 0 { format!("part-{:04}.bin", index) } else { format!("part-{index:04}.bin") };
        write(name, &pb)?;
        part_json.push(Json::Object(vec![
            ("index".into(), Json::Int(index as i128)),
            ("name".into(), json_of(&p.getattr("name")?)?),
            ("filename".into(), json_of(&p.getattr("filename")?)?),
            ("contentType".into(), json_of(&p.getattr("content_type")?)?),
            ("sha256".into(), Json::Str(util::hex(&util::sha256(&pb)))),
        ]));
    }
    for (i, raw) in result.getattr("xml_documents")?.try_iter()?.enumerate() {
        let raw = raw?;
        write(format!("xml-{i:04}.xml"), &pyapi::bytes_of(&raw)?)?;
    }
    let text: String = result.getattr("text")?.extract()?;
    write("preview.txt".into(), text.as_bytes())?;
    let mut urls = Vec::new();
    for u in result.getattr("download_urls")?.try_iter()? {
        let u = u?;
        urls.push(Json::Object(vec![("id".into(), json_of(&u.getattr("id")?)?), ("url".into(), json_of(&u.getattr("url")?)?)]));
    }
    let mut files = Vec::new();
    for f in result.getattr("content_files")?.try_iter()? {
        let f = f?;
        let mut items = Vec::new();
        for (key, attr) in [("fileHash", "file_hash"), ("sourceId", "source_id"), ("metadataHash", "metadata_hash"), ("name", "name"), ("nameEncoded", "name_encoded"), ("bindNumber", "bind_number")] {
            items.push((key.to_string(), json_of(&f.getattr(attr)?)?));
        }
        files.push(Json::Object(items));
    }
    let mut previews = Vec::new();
    for p in result.getattr("text_previews")?.try_iter()? {
        let p = p?;
        previews.push(Json::Object(vec![("context".into(), json_of(&p.getattr("context")?)?), ("text".into(), json_of(&p.getattr("text")?)?)]));
    }
    let details = Json::Object(vec![
        ("xmlRoots".into(), json_of(&result.getattr("xml_roots")?)?),
        ("resourceIds".into(), json_of(&result.getattr("resource_ids")?)?),
        ("downloadUrls".into(), Json::List(urls)),
        ("contentFiles".into(), Json::List(files)),
        ("parts".into(), Json::List(part_json)),
        ("textPreviews".into(), Json::List(previews)),
    ]);
    write("details.json".into(), util::json_dumps(&details, Some(2), false).as_bytes())?;
    let summary = safe_summary(result)?;
    let manifest = Json::Object(vec![
        ("schemaVersion".into(), Json::Int(1)),
        ("kind".into(), Json::Str("SamsungNotesResearchCache".into())),
        ("complete".into(), Json::Bool(true)),
        ("bodySha256".into(), Json::Str(util::hex(&util::sha256(&pyapi::bytes_of(&body)?)))),
        ("summary".into(), json_of(summary.bind(py))?),
    ]);
    write("cache.json".into(), util::json_dumps(&manifest, Some(2), true).as_bytes())?;
    Ok(summary)
}

const SAMSUNG_PART_HEADERS: &str = r#"Content-Disposition: form-data; name="([A-Za-z0-9_.-]{1,128})"(?:; filename="([A-Za-z0-9_.-]{1,256})")?\r\nContent-Type: (application/(?:octet-stream|xml))\r\nContent-Length: [0-9]{1,10}"#;

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("PayloadError", py.get_type::<PayloadError>())?;
    py.get_type::<PayloadError>().setattr("__module__", "tools.notes_payload")?;
    m.add_class::<PyPayloadPart>()?;
    m.add_class::<PyDownloadUrl>()?;
    m.add_class::<PyContentFile>()?;
    m.add_class::<PyTextPreview>()?;
    m.add_class::<PyPayloadResult>()?;
    m.add_function(wrap_pyfunction!(safe_summary, m)?)?;
    m.add_function(wrap_pyfunction!(decode_rich_text, m)?)?;
    m.add_function(wrap_pyfunction!(parse_payload, m)?)?;
    m.add_function(wrap_pyfunction!(cache_to_directory, m)?)?;
    m.add_function(wrap_pyfunction!(parts_py, m)?)?;
    m.add_function(wrap_pyfunction!(headers_py, m)?)?;
    m.add_function(wrap_pyfunction!(private_write, m)?)?;
    let pattern = py.import("re")?.getattr("compile")?.call1((PyBytes::new(py, SAMSUNG_PART_HEADERS.as_bytes()),))?;
    m.add("_SAMSUNG_PART_HEADERS", &pattern)?;
    let _ = pyapi::BUILTIN_PARTS.set(py, m.getattr("_parts")?.unbind());
    let _ = pyapi::BUILTIN_HEADERS.set(py, m.getattr("_headers")?.unbind());
    let _ = pyapi::BUILTIN_FAST.set(py, pattern.unbind());
    let _ = pyapi::BUILTIN_PRIVATE_WRITE.set(py, m.getattr("_private_write")?.unbind());
    rs_common::export(m, &[
        "PayloadError", "PayloadPart", "DownloadUrl", "ContentFile", "TextPreview", "PayloadResult",
        "safe_summary", "decode_rich_text", "parse_payload", "cache_to_directory",
        "_parts", "_headers", "_private_write", "_SAMSUNG_PART_HEADERS",
    ])
}
