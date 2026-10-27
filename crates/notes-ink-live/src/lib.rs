//! Live pen preview core (GPL-3.0, pinned sdocx; see Cargo.toml).
//!
//! The desktop UI draws the stroke under the pen with this crate built as
//! WebAssembly. A captured stroke is turned into the record the writer will
//! store (same Q5/Q12 channel quantization as `rs-ink` capture, the template
//! pen copied with the recipe 3 colour/size rules, or the recipe 4 FountainPen
//! of a first stroke) and then drawn by the saved-page pipeline: sdocx's
//! stroke SVG (with tools/ink-renderer's "calibrated" admission tweaks) and
//! the resvg rasterizer at the screen's device-pixel scale. No algorithm is
//! reimplemented here; only the writer's input preparation is mirrored.
//!
//! Input and output are private note geometry: never log them.
use sdocx::{
    BoundingBox, Color, Document, DocumentMetadata, InkPen2RenderProfile, Page, Point, RenderOptions, Stroke,
    StrokeRendering,
};
use serde::Deserialize;
use std::sync::Arc;

pub mod wasm;

pub const FOUNTAIN: &str = "com.samsung.android.sdk.pen.pen.preload.FountainPen";
pub const INKPEN2: &str = "com.samsung.android.sdk.pen.pen.preload.InkPen2";
pub const MARKER4: &str = "com.samsung.android.sdk.pen.pen.preload.Marker4";
/// Recipe 4 (rs-ops ink_first_stroke): FountainPen V16 profile, mouse tool.
pub const FIRST_SETTINGS: &str = "18;0;100;";
pub const FIRST_TOOL: u16 = 3;
pub const FIRST_TOLERANCE_RATIO: f64 = 0.25596845;
/// Host profile the page renderer passes for InkPen2 (notes-preview prepare.rs).
pub const INKPEN2_PROFILE: (u32, u32, bool) = (1440, 2304, false);
pub const MAX_POINTS: usize = 20_000;
/// Device-pixel budget of one raster (keeps a runaway zoom bounded).
pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub &'static str);
pub type R<T> = Result<T, Error>;

/// Pen of the stroke being drawn, as the UI sends it.
///
/// `rendering` is a template stroke's saved pen exactly as the page renderer
/// sees it (page units; `GET /api/notes/:id/ink-pens`). Without it the stroke
/// is a recipe 4 first stroke. `colorARGB`/`size` are the stroke's own; when
/// they differ from the template the writer's recipe 3 override is applied.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PenSpec {
    #[serde(default)]
    pub rendering: Option<StrokeRendering>,
    #[serde(rename = "colorARGB")]
    pub color_argb: u32,
    pub size: f32,
    pub page_width: u32,
    pub page_height: u32,
}

/// One captured sample in page units (what the UI stores and sends).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub x: f64,
    pub y: f64,
    pub pressure: f64,
    pub time_ms: i64,
}

/// A prepared pen: the template stroke with the writer's style applied.
#[derive(Debug, Clone)]
pub struct Pen {
    template: Stroke,
    options: RenderOptions,
    page: (u32, u32),
}

fn f32r(v: f64) -> R<f64> {
    let y = v as f32;
    if y.is_finite() { Ok(y as f64) } else { Err(Error("unrepresentable value")) }
}

fn first_rendering(color: u32, size: f32) -> R<StrokeRendering> {
    if color >> 24 != 0xff {
        return Err(Error("opaque colour required"));
    }
    let size = size as f64;
    if !(0.5..=128.0).contains(&size) {
        return Err(Error("pen size out of range"));
    }
    // ink_first_stroke: STROKE_PROPERTIES 0x0421 (compressed, millisecond
    // timestamps; binary_added/generated are stored inverted). The sdocx
    // types are non-exhaustive, so they are built through their serde form.
    rendering_json(FOUNTAIN, FIRST_SETTINGS, FIRST_TOOL, &["compressed", "millisecond_timestamps", "binary_added"], serde_json::json!({
        "color_argb": color, "pen_size": size as f32, "fixed_width": size as f32, "particle_density": 1,
        "initial_tolerance": f32r(size * FIRST_TOLERANCE_RATIO)? as f32,
    }))
}

const PROPERTY_NAMES: [&str; 14] = ["compressed", "replay_only", "stylus_channels", "eraser", "fixed_width", "millisecond_timestamps",
    "top_layer_pen", "alpha_lock", "binary_added", "generated", "fixed_opacity", "rainbow_effect", "straighten", "reveal_mode"];

/// A `StrokeRendering` from pen name, settings, tool, the set property names
/// and a partial style object (other style fields absent).
pub fn rendering_json(pen: &str, settings: &str, tool: u16, set: &[&str], style: serde_json::Value) -> R<StrokeRendering> {
    let mut properties = serde_json::Map::new();
    for name in PROPERTY_NAMES {
        properties.insert(name.into(), serde_json::Value::Bool(set.contains(&name)));
    }
    let mut style = match style {
        serde_json::Value::Object(map) => map,
        _ => return Err(Error("invalid style")),
    };
    style.insert("trailing_data".into(), serde_json::json!([]));
    style.insert("first_unparsed_field".into(), serde_json::Value::Null);
    let value = serde_json::json!({"metadata": null, "pen_name": pen, "advanced_settings": settings, "tool_type_raw": tool,
                                   "properties": properties, "style": style});
    serde_json::from_value(value).map_err(|_| Error("invalid rendering"))
}

impl Pen {
    pub fn from_json(text: &str) -> R<Pen> {
        let spec: PenSpec = serde_json::from_str(text).map_err(|_| Error("invalid pen"))?;
        Pen::new(spec)
    }

    pub fn new(spec: PenSpec) -> R<Pen> {
        let (w, h) = (spec.page_width, spec.page_height);
        if w == 0 || h == 0 || w > 32768 || h > 32768 || !spec.size.is_finite() || spec.size <= 0.0 {
            return Err(Error("invalid page or size"));
        }
        let mut rendering = match spec.rendering {
            None => first_rendering(spec.color_argb, spec.size)?,
            Some(mut r) => {
                let basic = matches!(r.pen_name.as_deref(), Some(FOUNTAIN) | Some(INKPEN2));
                if !basic || r.metadata.is_some() || r.properties.eraser {
                    return Err(Error("unsupported template pen"));
                }
                // The writer drops the stylus axes (ink_editor capture rebuild).
                r.properties.stylus_channels = false;
                let old = r.style.pen_size.ok_or(Error("template size required"))? as f64;
                let size = f32r(spec.size as f64)?;
                let color = r.style.color_argb.ok_or(Error("template colour required"))?;
                if color != spec.color_argb || old != size {
                    // Recipe 3 (rs-ink prepare_ink_document).
                    if spec.color_argb >> 24 != 0xff || !(old > 0.0) {
                        return Err(Error("unsupported style override"));
                    }
                    let factor = size / old;
                    r.style.color_argb = Some(spec.color_argb);
                    r.style.pen_size = Some(size as f32);
                    if let Some(fixed) = r.style.fixed_width {
                        r.style.fixed_width = Some(if fixed as f64 == old { size as f32 } else { f32r(fixed as f64 * factor)? as f32 });
                    }
                    if let Some(tolerance) = r.style.initial_tolerance {
                        r.style.initial_tolerance = Some(f32r(tolerance as f64 * factor)? as f32);
                    }
                }
                r
            }
        };
        let mut options = RenderOptions::default();
        // tools/ink-renderer "calibrated" admission: InkPen2 9; strokes take
        // the relaxed V9 route; Marker4 mouse input takes the stylus path.
        if rendering.pen_name.as_deref() == Some(INKPEN2) {
            if rendering.advanced_settings.as_deref() == Some("9;") {
                rendering.properties.millisecond_timestamps = false;
                rendering.style.initial_tolerance = Some(0.0);
            }
            let (screen_width, screen_height, enhanced_antialias) = INKPEN2_PROFILE;
            options.inkpen2_profile = Some(InkPen2RenderProfile { screen_width, screen_height, enhanced_antialias });
        }
        if rendering.pen_name.as_deref() == Some(MARKER4) && rendering.tool_type_raw == 3 {
            rendering.tool_type_raw = 2;
        }
        let argb = rendering.style.color_argb.ok_or(Error("colour required"))?;
        let pen_width = rendering.style.pen_size.ok_or(Error("size required"))?;
        let template = Stroke {
            rendering: Some(rendering), bbox: BoundingBox::default(), points: Vec::new(), pressures: Vec::new(),
            timestamps: Vec::new(), tilts: Vec::new(), orientations: Vec::new(),
            color: Some(Color { r: (argb >> 16) as u8, g: (argb >> 8) as u8, b: argb as u8 }), pen_width,
        };
        Ok(Pen { template, options, page: (w, h) })
    }

    pub fn page(&self) -> (u32, u32) {
        self.page
    }

    /// The record the writer stores for these samples, as the renderer reads it.
    pub fn stroke(&self, samples: &[Sample]) -> R<Stroke> {
        let q = quantize(samples)?;
        let mut stroke = self.template.clone();
        let half = stroke.pen_width as f64 * 0.5;
        let extent = f32r(f32r(half)? + 4.0)?;
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for s in &q {
            x0 = x0.min(s.x);
            y0 = y0.min(s.y);
            x1 = x1.max(s.x);
            y1 = y1.max(s.y);
        }
        stroke.bbox = BoundingBox { x_min: f32r(x0 - extent)?, y_min: f32r(y0 - extent)?, x_max: f32r(x1 + extent)?, y_max: f32r(y1 + extent)? };
        stroke.points = q.iter().map(|s| Point { x: s.x, y: s.y }).collect();
        stroke.pressures = q.iter().map(|s| s.pressure).collect();
        stroke.timestamps = q.iter().map(|s| s.time_ms).collect();
        Ok(stroke)
    }

    /// Page SVG of the stroke alone, without the page background.
    pub fn svg(&self, samples: &[Sample]) -> R<String> {
        let doc = self.document(self.stroke(samples)?);
        let layout = sdocx::layout_document(&doc);
        let fonts = sdocx::fonts::FontBook::new(Arc::new(fontdb::Database::new()));
        let out = sdocx::render_layout_page_svg_with_fonts(&doc, &layout, 0, &self.options, &fonts).ok_or(Error("render unavailable"))?;
        Ok(strip_background(out.svg))
    }

    /// One white page holding just this stroke (the page renderer's shape).
    pub fn document(&self, stroke: Stroke) -> Document {
        let (w, h) = self.page;
        let page = Page {
            uuid: "page".into(), width: w, height: h, content_bbox: BoundingBox::default(),
            background_color: Some(Color { r: 255, g: 255, b: 255 }), template: None, background: Default::default(),
            objects: vec![stroke.into()],
        };
        let metadata = DocumentMetadata {
            page_ids: vec!["page".into()], page_dimensions: Some((w, h)), dark_mode_compatibility: Some(false),
            ..Default::default()
        };
        Document { pages: vec![page], metadata }
    }

    /// Rasterize the stroke at `scale` device pixels per page unit.
    pub fn raster(&self, samples: &[Sample], scale: f64) -> R<Raster> {
        if samples.len() < 2 {
            return Ok(Raster::default());
        }
        let svg = self.svg(samples)?;
        raster_svg(&svg, self.page, scale)
    }
}

/// sdocx emits the page colour as the first element; the live layer is
/// transparent and composited over the page image like the saved stroke.
pub fn strip_background(svg: String) -> String {
    let Some(open) = svg.find('>') else { return svg };
    let rest = &svg[open + 1..];
    let trimmed = rest.trim_start();
    if trimmed.starts_with("<rect ") {
        if let Some(end) = trimmed.find("/>") {
            let skip = rest.len() - trimmed.len() + end + 2;
            return format!("{}{}", &svg[..open + 1], &rest[skip..]);
        }
    }
    svg
}

/// Device-pixel RGBA (straight alpha, ready for `putImageData`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Raster {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn raster_svg(svg: &str, page: (u32, u32), scale: f64) -> R<Raster> {
    if !scale.is_finite() || !(0.01..=64.0).contains(&scale) {
        return Err(Error("invalid scale"));
    }
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).map_err(|_| Error("invalid svg"))?;
    if !tree.root().has_children() {
        return Ok(Raster::default());
    }
    let bounds = tree.root().abs_layer_bounding_box();
    let (pw, ph) = ((page.0 as f64 * scale).ceil(), (page.1 as f64 * scale).ceil());
    let x0 = (bounds.left() as f64 * scale).floor().clamp(0.0, pw);
    let y0 = (bounds.top() as f64 * scale).floor().clamp(0.0, ph);
    let x1 = (bounds.right() as f64 * scale).ceil().clamp(0.0, pw);
    let y1 = (bounds.bottom() as f64 * scale).ceil().clamp(0.0, ph);
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    if w == 0 || h == 0 {
        return Ok(Raster::default());
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(Error("raster budget exceeded"));
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or(Error("raster unavailable"))?;
    let transform = resvg::tiny_skia::Transform::from_row(scale as f32, 0.0, 0.0, scale as f32, -x0 as f32, -y0 as f32);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(Raster { x: x0 as i32, y: y0 as i32, width: w, height: h, rgba })
}

/// Captured samples as the writer stores them: rs-ink `normalize_capture`
/// (f32 inputs, Q5 position deltas with the 1/64 bias, Q12 pressure deltas of
/// the original values, float32 accumulation on decode).
pub fn quantize(samples: &[Sample]) -> R<Vec<Sample>> {
    let n = samples.len();
    if !(1..=MAX_POINTS).contains(&n) {
        return Err(Error("capture channel count bound"));
    }
    let mut out = Vec::with_capacity(n);
    let first = samples[0];
    if first.time_ms != 0 {
        return Err(Error("capture timestamp must begin at zero"));
    }
    let mut previous = (f32r(first.x)?, f32r(first.y)?);
    let pressures = samples.iter().map(|s| f32r(s.pressure)).collect::<R<Vec<f64>>>()?;
    if pressures.iter().any(|p| !(0.0..=1.0).contains(p)) {
        return Err(Error("capture pressure must be 0..1"));
    }
    let mut pressure = pressures[0];
    out.push(Sample { x: previous.0, y: previous.1, pressure, time_ms: 0 });
    for i in 1..n {
        let s = samples[i];
        if !(0..=65535).contains(&(s.time_ms - samples[i - 1].time_ms)) {
            return Err(Error("capture timestamp gap bound"));
        }
        let point = (f32r(s.x)?, f32r(s.y)?);
        let mut restored = [0f64; 2];
        for (k, (current, prior)) in [(point.0, previous.0), (point.1, previous.1)].into_iter().enumerate() {
            let bias = if current > prior { 1.0 / 64.0 } else { -1.0 / 64.0 };
            let biased = f32r(f32r(current - prior)? + bias)?;
            let magnitude = (biased.abs() * 32.0).trunc();
            if magnitude > 32767.0 {
                return Err(Error("capture coordinate delta bound"));
            }
            let m = magnitude as u16 as f64 / 32.0;
            restored[k] = f32r(prior + if biased < 0.0 { -m } else { m })?;
        }
        previous = (restored[0], restored[1]);
        let delta = f32r(pressures[i] - pressures[i - 1])?;
        let magnitude = (delta.abs() * 4096.0).trunc();
        if magnitude > 32767.0 {
            return Err(Error("capture float delta bound"));
        }
        let m = magnitude / 4096.0;
        pressure = f32r(pressure + if delta < 0.0 { -m } else { m })?;
        out.push(Sample { x: previous.0, y: previous.1, pressure, time_ms: s.time_ms });
    }
    if out.iter().any(|s| !(0.0..=1.0).contains(&s.pressure)) {
        return Err(Error("quantized capture pressure leaves 0..1"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
