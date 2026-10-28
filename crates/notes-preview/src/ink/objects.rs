//! Non-ink page objects for the local page composite (`page-reconstructed`):
//! images (the note's media), shapes and lines (ours and foreign, from their
//! saved path geometry), strokes the renderer does not admit (approximated as
//! plain polylines) and calm placeholders for anything else.
//!
//! Everything here is derived from parsed numbers and attribute values; no
//! note text ever enters an SVG fragment. Images are referenced by their
//! content-file SHA256 and fetched/decoded by the manager.
use crate::util::J;
use rs_ink::core::binary::{Record, StyleVal};
use rs_ink::core::prepare::Binding;
use rs_ops::xml::Element;
use std::collections::HashMap;

pub const NOTICE_HIDDEN: &str = "Bu sayfadaki bazı nesneler henüz gösterilemiyor.";
pub const NOTICE_INK: &str = "Bazı çizgiler yaklaşık gösteriliyor.";
pub const NOTICE_SHAPES: &str = "Şekiller yaklaşık gösteriliyor.";
pub const NOTICE_TEXT: &str = "Metin biçimi yaklaşık gösteriliyor.";
pub const NOTICE_BACKGROUND: &str = "Sayfanın arka plan resmi henüz gösterilmiyor.";
pub const NOTICE_IMAGES: &str = "Resimler indirildiğinde sayfa tamamlanır.";

/// One composite layer item.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Svg(String),
    Image { sha256: String, rect: [f64; 4], crop: Option<[f64; 4]>, flip_h: bool, flip_v: bool, degrees: f64, opacity: f64 },
}

impl Item {
    pub fn json(&self) -> J {
        match self {
            Item::Svg(s) => J::obj(vec![("svg", J::Str(s.clone()))]),
            Item::Image { sha256, rect, crop, flip_h, flip_v, degrees, opacity } => J::obj(vec![("image", J::obj(vec![
                ("sha256", J::Str(sha256.clone())),
                ("rect", J::List(rect.iter().map(|v| J::Float(*v)).collect())),
                ("crop", crop.map_or(J::Null, |c| J::List(c.iter().map(|v| J::Float(*v)).collect()))),
                ("flipH", J::Bool(*flip_h)),
                ("flipV", J::Bool(*flip_v)),
                ("degrees", J::Float(*degrees)),
                ("opacity", J::Float(*opacity)),
            ]))]),
        }
    }
}

/// `contentFileList`: content-file id (what a picture's `fillEffect imageId`
/// names) and bindNumber -> lowercase SHA256 of the file's bytes (its
/// `fileHash`, equal to the id; the `hash` attribute is the entry's own
/// metadata digest). Downloads are keyed and verified by that file hash.
pub fn content_files(root: &Element) -> HashMap<String, String> {
    let hex64 = |h: &str| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit());
    let mut out = HashMap::new();
    for list in root.children.iter().filter(|c| c.tag == "contentFileList") {
        for file in &list.children {
            let digest = file.find("fileHash").and_then(|f| f.text.as_deref()).map(str::trim).or(file.get("id"));
            let Some(digest) = digest.filter(|h| hex64(h)) else { continue };
            for key in [file.get("id"), file.get("bindNumber")].into_iter().flatten() {
                out.entry(key.to_string()).or_insert_with(|| digest.to_lowercase());
            }
        }
    }
    out
}

fn f(v: Option<&str>) -> Option<f64> {
    v.and_then(|s| s.trim().parse::<f64>().ok()).filter(|x| x.is_finite() && x.abs() < 1e7)
}

/// An `l t r b` frame element as numbers (None: absent, unreadable or inverted).
pub fn rect_of(e: Option<&Element>) -> Option<[f64; 4]> {
    let e = e?;
    let r = [f(e.get("l"))?, f(e.get("t"))?, f(e.get("r"))?, f(e.get("b"))?];
    (r[2] >= r[0] && r[3] >= r[1]).then_some(r)
}

fn num(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.to_string() }
}

/// Signed decimal ARGB -> (`#rrggbb`, opacity).
fn argb(value: Option<&str>) -> Option<(String, f64)> {
    let v: i64 = value?.trim().parse().ok()?;
    if !(-(1i64 << 31)..(1i64 << 32)).contains(&v) {
        return None;
    }
    let c = (v as u64 & 0xffff_ffff) as u32;
    Some((format!("#{:06x}", c & 0xff_ffff), (c >> 24) as f64 / 255.0))
}

fn truthy(v: Option<&str>) -> bool {
    v.is_some_and(|s| s.eq_ignore_ascii_case("true") || s == "1")
}

/// Affine map from the frame the path was saved in to the current frame.
struct Frame {
    from: [f64; 4],
    to: [f64; 4],
}

impl Frame {
    fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let sx = if self.from[2] - self.from[0] > 1e-6 { (self.to[2] - self.to[0]) / (self.from[2] - self.from[0]) } else { 1.0 };
        let sy = if self.from[3] - self.from[1] > 1e-6 { (self.to[3] - self.to[1]) / (self.from[3] - self.from[1]) } else { 1.0 };
        (self.to[0] + (x - self.from[0]) * sx, self.to[1] + (y - self.from[1]) * sy)
    }
}

/// The saved `path/segmentList` as SVG path data (None: no usable path).
fn path_data(node: &Element, frame: &Frame, rect: [f64; 4]) -> Option<String> {
    let segments = node.find("path")?.find("segmentList")?;
    let mut d = String::new();
    let mut started = false;
    for s in &segments.children {
        let p = |kx: &str, ky: &str| -> Option<(f64, f64)> { Some(frame.map(f(s.get(kx))?, f(s.get(ky))?)) };
        let part = match s.get("type")? {
            "1" => {
                let (x, y) = p("x", "y")?;
                started = true;
                format!("M{} {}", num(x), num(y))
            }
            "2" => {
                let (x, y) = p("x", "y")?;
                format!("L{} {}", num(x), num(y))
            }
            "3" => {
                let (cx, cy) = p("x", "y")?;
                let (x, y) = p("x2", "y2")?;
                format!("Q{} {} {} {}", num(cx), num(cy), num(x), num(y))
            }
            "4" => {
                let (ax, ay) = p("x", "y")?;
                let (bx, by) = p("x1", "y1")?;
                let (x, y) = p("x2", "y2")?;
                format!("C{} {} {} {} {} {}", num(ax), num(ay), num(bx), num(by), num(x), num(y))
            }
            // Arc: approximated by its end point.
            "5" => {
                let (x, y) = p("x2", "y2").or_else(|| p("x", "y"))?;
                format!("L{} {}", num(x), num(y))
            }
            "6" => "Z".into(),
            "7" => {
                let [l, t, r, b] = rect;
                let (rx, ry, cx, cy) = ((r - l) / 2.0, (b - t) / 2.0, (l + r) / 2.0, (t + b) / 2.0);
                started = true;
                format!("M{} {}A{} {} 0 1 0 {} {}A{} {} 0 1 0 {} {}Z", num(cx - rx), num(cy), num(rx), num(ry), num(cx + rx), num(cy),
                        num(rx), num(ry), num(cx - rx), num(cy))
            }
            _ => return None,
        };
        if !started {
            return None;
        }
        d.push_str(&part);
        if d.len() > 256 * 1024 {
            return None;
        }
    }
    (!d.is_empty()).then_some(d)
}

fn transform(node: &Element, rect: [f64; 4]) -> String {
    let (cx, cy) = ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0);
    let mut t = String::new();
    if let Some(deg) = f(node.get("originalDegree")).filter(|d| d.abs() > 1e-9) {
        t.push_str(&format!("rotate({} {} {}) ", num(deg), num(cx), num(cy)));
    }
    let (h, v) = (truthy(node.get("isHorizontalFlipped")), truthy(node.get("isVerticalFlipped")));
    if h || v {
        t.push_str(&format!("translate({} {}) scale({} {}) translate({} {})", num(cx), num(cy), if h { -1 } else { 1 },
                            if v { -1 } else { 1 }, num(-cx), num(-cy)));
    }
    t.trim_end().to_string()
}

fn stroke_attrs(node: &Element, approximate: &mut bool) -> Option<String> {
    let color = node.find("lineColorEffect")?;
    let style = node.find("lineStyleEffect");
    let width = style.and_then(|s| f(s.get("width"))).unwrap_or(0.0);
    match color.get("colorType") {
        Some("2") => return None,
        Some("0") => {}
        _ => *approximate = true,
    }
    if width <= 0.0 {
        return None;
    }
    let (hex, alpha) = argb(color.get("solidColor"))?;
    let cap = match style.and_then(|s| s.get("capType")) { Some("1") => "round", Some("2") => "square", _ => "butt" };
    let join = match style.and_then(|s| s.get("joinType")) { Some("1") => "round", Some("2") => "bevel", _ => "miter" };
    let mut out = format!(" stroke=\"{hex}\" stroke-width=\"{}\" stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\"", num(width.min(1024.0)));
    if alpha < 1.0 {
        out.push_str(&format!(" stroke-opacity=\"{}\"", num(alpha)));
    }
    if style.and_then(|s| s.get("dashType")).is_some_and(|d| d != "0") {
        *approximate = true;
        out.push_str(&format!(" stroke-dasharray=\"{} {}\"", num(width * 3.0), num(width * 2.0)));
    }
    Some(out)
}

fn arrow(from: (f64, f64), to: (f64, f64), width: f64, hex: &str) -> String {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = dx.hypot(dy);
    if len < 1e-6 {
        return String::new();
    }
    let size = (width * 4.0).max(8.0).min(len);
    let (ux, uy) = (dx / len, dy / len);
    let (bx, by) = (to.0 - ux * size, to.1 - uy * size);
    let (px, py) = (-uy * size * 0.5, ux * size * 0.5);
    format!("<path d=\"M{} {}L{} {}L{} {}Z\" fill=\"{hex}\"/>", num(to.0), num(to.1), num(bx + px), num(by + py), num(bx - px), num(by - py))
}

/// Result of converting one layer object.
pub struct Converted {
    pub items: Vec<Item>,
    pub notices: Vec<&'static str>,
}

/// A shape, line, image or text-box frame (fill and border; the text itself
/// goes to the renderer). None: not a shape-family object.
pub fn shape_family(node: &Element, files: &HashMap<String, String>) -> Option<Converted> {
    let kind = node.get("type")?;
    if !matches!(kind, "shape" | "line" | "image" | "textBox") {
        return None;
    }
    let mut out = Converted { items: Vec::new(), notices: Vec::new() };
    let Some(rect) = rect_of(node.find("rect")) else {
        out.notices.push(NOTICE_HIDDEN);
        return Some(out);
    };
    let saved = if kind == "line" { node.find("originalDrawnRect") } else { node.find("drawnRect") };
    let frame = Frame { from: rect_of(saved).unwrap_or(rect), to: rect };
    let mut approximate = false;
    let fill = node.find("fillEffect");
    // Image fill (pictures are ObjectShape with an image fill).
    if fill.and_then(|e| e.get("type")) == Some("image") {
        let fill = fill.unwrap();
        let sha = fill.get("imageId").and_then(|id| files.get(id));
        match sha {
            Some(sha) => {
                let transparency = f(fill.get("transparency")).unwrap_or(0.0).clamp(0.0, 1.0);
                out.items.push(Item::Image {
                    sha256: sha.clone(), rect, crop: rect_of(node.find("cropRect")),
                    flip_h: truthy(node.get("isHorizontalFlipped")), flip_v: truthy(node.get("isVerticalFlipped")),
                    degrees: f(node.get("originalDegree")).unwrap_or(0.0), opacity: 1.0 - transparency,
                });
            }
            None => {
                out.notices.push(NOTICE_HIDDEN);
                out.items.push(Item::Svg(rs_render::compose::placeholder_svg(rect)));
            }
        }
    }
    let transform = transform(node, rect);
    let group = |body: String| if transform.is_empty() { body } else { format!("<g transform=\"{transform}\">{body}</g>") };
    let mut fill_attr = "none".to_string();
    if let Some(fill) = fill.filter(|e| e.get("type") != Some("image")) {
        match (fill.get("type"), argb(fill.get("solidColor"))) {
            (Some("color"), Some((hex, alpha))) => {
                fill_attr = if alpha < 1.0 { format!("{hex}\" fill-opacity=\"{}", num(alpha)) } else { hex };
                if fill.get("colorType").is_some_and(|c| c != "0") {
                    approximate = true;
                }
            }
            _ => approximate = true,
        }
    }
    let stroke = stroke_attrs(node, &mut approximate);
    let path = match kind {
        "line" => {
            let b = node.find("beginPos").and_then(|p| Some(frame.map(f(p.get("x"))?, f(p.get("y"))?)));
            let e = node.find("endPos").and_then(|p| Some(frame.map(f(p.get("x"))?, f(p.get("y"))?)));
            match (b, e, node.get("connectorType")) {
                (Some(b), Some(e), Some("0") | None) => Some(format!("M{} {}L{} {}", num(b.0), num(b.1), num(e.0), num(e.1))),
                _ => {
                    approximate = true;
                    path_data(node, &frame, rect)
                }
            }
        }
        _ => path_data(node, &frame, rect).or_else(|| (kind != "image").then(|| {
            // No usable saved path: its frame stands in for the outline.
            if node.find("path").is_some() {
                approximate = true;
            }
            format!("M{} {}H{}V{}H{}Z", num(rect[0]), num(rect[1]), num(rect[2]), num(rect[3]), num(rect[0]))
        })),
    };
    if let Some(d) = path {
        let paints_fill = fill_attr != "none";
        if paints_fill || stroke.is_some() {
            let mut body = format!("<path d=\"{d}\" fill=\"{fill_attr}\"{}/>", stroke.clone().unwrap_or_default());
            // Arrow heads on lines.
            if kind == "line" {
                if let (Some(style), Some((hex, _))) = (node.find("lineStyleEffect"), node.find("lineColorEffect").and_then(|c| argb(c.get("solidColor")))) {
                    let width = f(style.get("width")).unwrap_or(1.0);
                    let b = node.find("beginPos").and_then(|p| Some(frame.map(f(p.get("x"))?, f(p.get("y"))?)));
                    let e = node.find("endPos").and_then(|p| Some(frame.map(f(p.get("x"))?, f(p.get("y"))?)));
                    if let (Some(b), Some(e)) = (b, e) {
                        if style.get("endArrowType").is_some_and(|t| t != "0") {
                            body.push_str(&arrow(b, e, width, &hex));
                        }
                        if style.get("beginArrowType").is_some_and(|t| t != "0") {
                            body.push_str(&arrow(e, b, width, &hex));
                        }
                        if style.get("endArrowType").is_some_and(|t| t != "0" && t != "1") || style.get("beginArrowType").is_some_and(|t| t != "0" && t != "1") {
                            approximate = true;
                        }
                    }
                }
            }
            out.items.push(Item::Svg(group(body)));
        }
    } else if kind != "image" {
        approximate = true;
    }
    // Text inside a shape/line/picture (not a text box) is not laid out here.
    if kind != "textBox" && node.iter().any(|n| n.text.as_deref().is_some_and(|t| !t.trim().is_empty())) {
        out.notices.push(NOTICE_HIDDEN);
    }
    if approximate {
        out.notices.push(NOTICE_SHAPES);
    }
    Some(out)
}

fn style_f(v: Option<&StyleVal>) -> Option<f64> {
    match v? {
        StyleVal::I(i) => Some(*i as f64),
        StyleVal::F(x) => Some(*x),
        StyleVal::G(_) => None,
    }
}

/// A stroke the renderer does not draw, approximated as a round-capped
/// polyline in its saved colour and pen width (page units).
pub fn stroke_polyline(record: &Record, binding: &Binding, width: i64, height: i64) -> Option<String> {
    use rs_ink::core::num::Val;
    let scale = rs_ink::core::coords::page_coordinate_scale(&record.frames[0], Val::Int(width as i128), Val::Int(height as i128), Val::Int(0)).ok()?;
    let points = &record.samples.points;
    if points.is_empty() || points.len() > 65536 {
        return None;
    }
    let color = match binding.style.get("colorARGB")? {
        StyleVal::I(v) => *v,
        StyleVal::F(v) => *v as i64,
        StyleVal::G(_) => return None,
    };
    let c = (color as u64 & 0xffff_ffff) as u32;
    let pen = style_f(binding.style.get("penSize")).unwrap_or(2.0) * scale;
    let pen = if pen.is_finite() { pen.clamp(0.5, 256.0) } else { 2.0 };
    let mut alpha = (c >> 24) as f64 / 255.0;
    if binding.pen_name.ends_with("Marker4") || binding.pen_name.contains("Highlighter") {
        alpha = alpha.min(0.45);
    }
    let mut d = String::with_capacity(points.len() * 14);
    for (i, (x, y)) in points.iter().enumerate() {
        let (x, y) = (x * scale, y * scale);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        d.push(if i == 0 { 'M' } else { 'L' });
        d.push_str(&num(x));
        d.push(' ');
        d.push_str(&num(y));
    }
    if points.len() == 1 {
        d.push_str("l0.01 0");
    }
    let opacity = if alpha < 1.0 { format!(" stroke-opacity=\"{}\"", num(alpha.max(0.05))) } else { String::new() };
    Some(format!("<path d=\"{d}\" fill=\"none\" stroke=\"#{:06x}\" stroke-width=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"{opacity}/>",
                 c & 0xff_ffff, num(pen)))
}

/// Placeholder for an object of an unknown kind that has a frame.
pub fn unknown_object(node: &Element) -> Converted {
    let mut out = Converted { items: Vec::new(), notices: vec![NOTICE_HIDDEN] };
    if let Some(rect) = rect_of(node.find("rect")).filter(|r| r[2] - r[0] > 1.0 && r[3] - r[1] > 1.0) {
        out.items.push(Item::Svg(rs_render::compose::placeholder_svg(rect)));
    }
    out
}

/// The layer objects of a page in paint order (layers, then each objectList).
pub fn layer_objects(page: &Element) -> Vec<&Element> {
    let mut out = Vec::new();
    for list in page.children.iter().filter(|c| c.tag == "layerList") {
        for layer in list.children.iter().filter(|c| c.tag == "layer") {
            for objects in layer.children.iter().filter(|c| c.tag == "objectList") {
                out.extend(objects.children.iter().filter(|o| o.tag == "object"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(xml: &str) -> Element {
        rs_ops::xml::parse_str(xml).unwrap()
    }

    #[test]
    fn foreign_rectangle_with_fill_and_dash() {
        let node = el(r#"<object type="shape" shapeType="4" originalDegree="0.0"><rect l="10" t="10" r="50" b="30"/>
            <lineColorEffect colorType="0" solidColor="-16776961"/><lineStyleEffect width="2" dashType="1"/>
            <path><segmentList><segment type="1" x="10" y="10"/><segment type="2" x="50" y="10"/><segment type="2" x="50" y="30"/><segment type="6"/></segmentList></path>
            <fillEffect type="color" solidColor="-65536" colorType="0"/></object>"#);
        let c = shape_family(&node, &HashMap::new()).unwrap();
        let Item::Svg(svg) = &c.items[0] else { panic!() };
        assert!(svg.contains("M10 10L50 10L50 30Z") && svg.contains("fill=\"#ff0000\"") && svg.contains("stroke=\"#0000ff\""));
        assert!(c.notices.contains(&NOTICE_SHAPES));
    }

    #[test]
    fn moved_shape_maps_saved_path_into_current_frame() {
        let node = el(r#"<object type="shape" shapeType="4"><rect l="110" t="10" r="150" b="30"/>
            <lineColorEffect colorType="0" solidColor="-16777216"/><lineStyleEffect width="1" dashType="0"/>
            <path><segmentList><segment type="1" x="10" y="10"/><segment type="2" x="50" y="30"/></segmentList></path>
            <drawnRect l="10" t="10" r="50" b="30"/></object>"#);
        let Item::Svg(svg) = &shape_family(&node, &HashMap::new()).unwrap().items[0] else { panic!() };
        assert!(svg.contains("M110 10L150 30"), "{svg}");
    }

    #[test]
    fn image_resolves_content_file_id_to_its_hash() {
        let hash = "a".repeat(64);
        let node = el(r#"<object type="image" isHorizontalFlipped="true"><rect l="0" t="0" r="100" b="50"/>
            <lineColorEffect colorType="2" solidColor="0"/><lineStyleEffect width="0"/>
            <fillEffect type="image" imageId="3" transparency="0.25"/><cropRect l="1" t="2" r="3" b="4"/></object>"#);
        let files = content_files(&el(&format!(r#"<noteResource><contentFileList><contentFile id="3" bindNumber="7" hash="{}"><fileHash>{hash}</fileHash></contentFile></contentFileList></noteResource>"#, "b".repeat(64))));
        let c = shape_family(&node, &files).unwrap();
        assert_eq!(c.items, vec![Item::Image { sha256: hash, rect: [0.0, 0.0, 100.0, 50.0], crop: Some([1.0, 2.0, 3.0, 4.0]),
            flip_h: true, flip_v: false, degrees: 0.0, opacity: 0.75 }]);
        let missing = shape_family(&node, &HashMap::new()).unwrap();
        assert!(missing.notices.contains(&NOTICE_HIDDEN));
    }

    #[test]
    fn line_with_arrow() {
        let node = el(r#"<object type="line" connectorType="0"><rect l="0" t="0" r="100" b="100"/>
            <lineColorEffect colorType="0" solidColor="-16777216"/><lineStyleEffect width="2" endArrowType="1" beginArrowType="0" dashType="0"/>
            <beginPos x="0" y="0"/><endPos x="100" y="100"/></object>"#);
        let Item::Svg(svg) = &shape_family(&node, &HashMap::new()).unwrap().items[0] else { panic!() };
        assert!(svg.contains("M0 0L100 100") && svg.matches("<path").count() == 2);
    }
}
