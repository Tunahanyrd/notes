//! Data types shared with the doc/ink areas and the dependency interface the
//! offline document operations need from them. `bridge::PyDeps` implements it
//! by calling the (not yet stable) Python modules; once rs-doc/rs-ink are
//! STABLE a pure-Rust implementation replaces it without touching the logic.
use crate::err::R;
use crate::xml::Element;
use std::any::Any;

pub type Handle = Box<dyn Any + Send + Sync>;

pub struct Snapshot {
    pub raw_body: Vec<u8>,
    pub content_type: String,
    pub resource_xml: Vec<u8>,
    pub local_id: Option<String>,
    pub commit_id: Option<String>,
    pub handle: Option<Handle>,
}

pub struct Part {
    pub name: Option<String>,
    pub filename: Option<String>,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub struct Payload {
    pub locked: bool,
    pub xml_documents: Vec<Vec<u8>>,
    pub parts: Vec<Part>,
    pub handle: Option<Handle>,
}

#[derive(Clone)]
pub struct Frame {
    pub raw: Vec<u8>,
    pub kind: i64,
    pub properties: u64,
    pub fields: u64,
    pub fixed_start: usize,
    pub flexible_start: usize,
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Samples {
    pub points: Vec<(f64, f64)>,
    pub pressures: Vec<f64>,
    pub timestamps: Vec<i64>,
    pub tilts: Vec<f64>,
    pub orientations: Vec<f64>,
}

pub struct Record {
    pub raw: Vec<u8>,
    pub frames: Vec<Frame>,
    pub samples: Samples,
    pub compressed: bool,
    pub stylus: bool,
    pub tool_type: i64,
    pub handle: Option<Handle>,
}

impl Record {
    /// ASCII UUID stored in the base frame (`raw[fixed_start+6:fixed_start+42]`).
    pub fn identity(&self) -> R<String> {
        let b = &self.frames[0];
        let slice = b.raw.get(b.fixed_start + 6..(b.fixed_start + 42).min(b.raw.len())).unwrap_or(&[]);
        if !slice.is_ascii() {
            return Err(crate::err::Error::new(crate::err::Kind::Unicode, "'ascii' codec can't decode bytes"));
        }
        Ok(String::from_utf8(slice.to_vec()).unwrap())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Num {
    I(i64),
    F(f64),
}

impl Num {
    pub fn as_f64(&self) -> f64 {
        match self {
            Num::I(i) => *i as f64,
            Num::F(f) => *f,
        }
    }
}

impl PartialEq for Num {
    fn eq(&self, other: &Num) -> bool {
        match (self, other) {
            (Num::I(a), Num::I(b)) => a == b,
            _ => self.as_f64() == other.as_f64(),
        }
    }
}

pub struct Binding {
    pub object_id: String,
    pub modified_time: i64,
    pub bounds: [f64; 4],
    pub pen_name: String,
    pub advanced_settings: Option<String>,
    pub style: Vec<(String, Num)>,
    pub handle: Option<Handle>,
}

impl Binding {
    pub fn style_get(&self, key: &str) -> Option<Num> {
        self.style.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }
}

pub struct Capture {
    pub samples: Samples,
    pub channels: Vec<u8>,
    pub stylus: bool,
    pub handle: Option<Handle>,
}

pub struct BinaryPart {
    pub name: String,
    pub filename: String,
    pub body: Vec<u8>,
}

pub trait Deps {
    /// Resolve a caller-supplied snapshot argument: dict → from_dict;
    /// `Ok(None)` when it is not a DocumentSnapshot.
    fn snapshot(&self, arg: &dyn Any) -> R<Option<Snapshot>>;
    /// `parse_payload(raw, content_type, max_parts=MAX_READONLY_PARTS)`.
    fn parse_payload(&self, raw: &[u8], content_type: &str) -> R<Payload>;
    /// `if isinstance(arg, dict): arg = <module>.DocumentSnapshot.from_dict(arg)`.
    fn convert(&self, arg: &dyn Any, module: &str) -> R<Handle>;
    /// `ink_projection.verified_resource(snapshot)` resolved at call time:
    /// `Some(reason)` when it returns a str.
    fn verified(&self, snapshot: &dyn Any) -> R<Option<String>>;
    /// `snapshot.resource_xml` (duck-typed).
    fn resource_xml(&self, snapshot: &dyn Any) -> R<Vec<u8>>;
    fn decode_stroke(&self, body: &[u8]) -> R<Record>;
    fn bind(&self, record: &Record, object: &Element, strings: Option<&Element>) -> R<Binding>;
    fn coordinate_scale(&self, record: &Record, width: i64, height: i64) -> R<f64>;
    fn owner_extent(&self, record: &Record) -> R<(i64, i64)>;
    fn read_style(&self, record: &Record) -> R<Vec<(String, Num)>>;
    fn decode_point_channels(&self, data: &[u8], count: usize, compressed: bool, stylus: bool) -> R<Samples>;
    /// `ink_cache.release_page_cache(root, <pageList page with page_id>, tick=tick)`.
    fn release_page_cache(&self, root: &mut Element, page_id: &str, tick: i64) -> R<Vec<String>>;
    /// Discarded `prepare_ink_append_v2` dry run with one centre sample.
    fn append_probe(&self, snapshot: &Snapshot, source: &str, new_id: &str, x: f64, y: f64, now_ms: i64) -> R<()>;
    /// Storage migration may override this with an equivalent bounded probe.
    fn append_probe_for_migration(&self, snapshot: &Snapshot, source: &str, new_id: &str, x: f64, y: f64, now_ms: i64) -> R<()> {
        self.append_probe(snapshot, source, new_id, x, y, now_ms)
    }
    /// Discarded `prepare_first_stroke` dry run (one centre sample, black, size 1).
    fn first_stroke_probe(&self, snapshot: &Snapshot, page_id: &str, x: f64, y: f64, now_ms: i64) -> R<()>;
    /// Discarded `prepare_highlighter_stroke` dry run (one centre sample, DEFAULT_COLOR, size 1).
    fn highlighter_probe(&self, snapshot: &Snapshot, page_id: &str, x: f64, y: f64, now_ms: i64) -> R<()>;
    fn basic_pens(&self) -> R<Vec<String>>;
    /// `normalize_capture(samples, stylus=False)`; `samples` is the caller's object.
    fn normalize_capture(&self, samples: &dyn Any) -> R<Capture>;
    fn drawn_bounds(&self, samples: &Samples, pen_name: &str, pen_size: f64) -> R<[f64; 4]>;
}
