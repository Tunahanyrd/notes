use super::*;

fn line(n: usize, pressure: impl Fn(f64) -> f64, step_ms: i64) -> Vec<Sample> {
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            Sample { x: 100.0 + 400.0 * t, y: 200.0 + 30.0 * (t * 6.0).sin(), pressure: pressure(t), time_ms: i as i64 * step_ms }
        })
        .collect()
}

fn first(size: f32) -> Pen {
    Pen::new(PenSpec { rendering: None, color_argb: 0xff252525, size, page_width: 800, page_height: 600 }).unwrap()
}

/// Opaque-ish column extent (alpha >= 128) of the raster at page x.
fn thickness(r: &Raster, scale: f64, x: f64) -> u32 {
    let col = (x * scale) as i32 - r.x;
    assert!(col >= 0 && (col as u32) < r.width);
    (0..r.height).filter(|row| r.rgba[((row * r.width + col as u32) * 4 + 3) as usize] >= 128).count() as u32
}

#[test]
fn quantize_matches_rs_ink_capture_channels() {
    // Expected values from rs-ink/Python normalize_capture for this input
    // (recorded with samsung_notes.ink_capture.normalize_capture).
    let samples = [
        Sample { x: 10.3, y: 20.7, pressure: 0.31, time_ms: 0 },
        Sample { x: 11.01, y: 20.2, pressure: 0.4567, time_ms: 7 },
        Sample { x: 13.77, y: 19.95, pressure: 0.45, time_ms: 15 },
        Sample { x: 13.77, y: 22.5, pressure: 0.9, time_ms: 31 },
    ];
    let q = quantize(&samples).unwrap();
    let expected = include!("quantize_expected.txt");
    assert_eq!(q.len(), expected.len());
    for (s, e) in q.iter().zip(expected.iter()) {
        assert_eq!((s.x, s.y, s.pressure, s.time_ms), *e);
    }
}

#[test]
fn quantize_rejects_what_the_writer_rejects() {
    let s = |t: i64, p: f64| Sample { x: 1.0, y: 1.0, pressure: p, time_ms: t };
    assert!(quantize(&[s(5, 0.5)]).is_err());
    assert!(quantize(&[s(0, 0.5), s(70_000, 0.5)]).is_err());
    assert!(quantize(&[s(0, 1.5)]).is_err());
    assert!(quantize(&[]).is_err());
}

/// Ink mass (sum of alpha) of the raster column at page x.
fn mass(r: &Raster, scale: f64, x: f64) -> u32 {
    let col = (x * scale) as i32 - r.x;
    assert!(col >= 0 && (col as u32) < r.width);
    (0..r.height).map(|row| r.rgba[((row * r.width + col as u32) * 4 + 3) as usize] as u32).sum()
}

#[test]
fn stylus_template_width_follows_pressure() {
    // Straight horizontal line, pressure ramping 0.05 -> 0.95, stylus tool.
    let samples: Vec<Sample> = (0..60)
        .map(|i| { let t = i as f64 / 59.0; Sample { x: 100.0 + 400.0 * t, y: 200.0, pressure: 0.05 + 0.9 * t, time_ms: i * 8 } })
        .collect();
    for settings in ["14;", "18;0;100;"] {
        let spec = PenSpec { rendering: Some(template(settings, FOUNTAIN, 6.0, None)), color_argb: 0xff2462a0, size: 6.0, page_width: 800, page_height: 600 };
        let r = Pen::new(spec).unwrap().raster(&samples, 2.0).unwrap();
        let (light, heavy) = (mass(&r, 2.0, 150.0), mass(&r, 2.0, 470.0));
        assert!(heavy * 10 > light * 15, "{settings}: pressure must widen the stroke ({light} vs {heavy})");
    }
}

#[test]
fn first_stroke_uses_the_mouse_fountain_profile() {
    // Recipe 4 writes tool type 3 (mouse): Samsung's Fountain ignores pressure
    // there, so the preview must not invent pressure width either.
    let samples: Vec<Sample> = (0..60)
        .map(|i| { let t = i as f64 / 59.0; Sample { x: 100.0 + 400.0 * t, y: 200.0, pressure: 0.05 + 0.9 * t, time_ms: i * 8 } })
        .collect();
    let r = first(6.0).raster(&samples, 2.0).unwrap();
    let (light, heavy) = (mass(&r, 2.0, 150.0), mass(&r, 2.0, 470.0));
    assert!(light > 0 && heavy > 0);
    assert!(heavy * 10 < light * 13, "{light} vs {heavy}");
    let stroke = first(6.0).stroke(&samples).unwrap();
    let rendering = stroke.rendering.unwrap();
    assert_eq!((rendering.tool_type_raw, rendering.advanced_settings.as_deref()), (FIRST_TOOL, Some(FIRST_SETTINGS)));
    assert_eq!(rendering.style.initial_tolerance, Some((6.0f64 * FIRST_TOLERANCE_RATIO) as f32));
}

#[test]
fn larger_pen_size_draws_wider() {
    let samples = line(40, |_| 0.6, 8);
    let scale = 2.0;
    let thin = thickness(&first(2.0).raster(&samples, scale).unwrap(), scale, 300.0);
    let wide = thickness(&first(12.0).raster(&samples, scale).unwrap(), scale, 300.0);
    assert!(wide > thin * 2, "{thin} vs {wide}");
}

#[test]
fn raster_is_deterministic_and_tracks_scale() {
    let samples = line(30, |t| 0.3 + 0.5 * t, 10);
    let pen = first(4.0);
    let a = pen.raster(&samples, 1.5).unwrap();
    let b = pen.raster(&samples, 1.5).unwrap();
    assert_eq!(a, b);
    let c = pen.raster(&samples, 3.0).unwrap();
    assert!(c.width >= a.width * 2 - 2 && c.width <= a.width * 2 + 2);
    assert_eq!(c.x, (c.x as f64) as i32);
}

#[test]
fn svg_has_no_page_background() {
    let svg = first(4.0).svg(&line(10, |_| 0.5, 8)).unwrap();
    assert!(!svg.contains("fill=\"#ffffff\" height=\"600\""), "{}", &svg[..svg.len().min(300)]);
    assert!(svg.starts_with("<svg"));
}

#[test]
fn colour_reaches_the_raster() {
    let pen = Pen::new(PenSpec { rendering: None, color_argb: 0xffd03030, size: 8.0, page_width: 800, page_height: 600 }).unwrap();
    let r = pen.raster(&line(30, |_| 0.8, 8), 1.0).unwrap();
    let solid = r.rgba.chunks_exact(4).find(|p| p[3] == 255).expect("an opaque pixel");
    // The renderer's own colour handling decides the exact value; it stays red.
    assert!(solid[0] >= 0xc8 && solid[1] <= 0x40 && solid[2] <= 0x40, "{solid:?}");
}

fn template(settings: &str, pen: &str, size: f32, tolerance: Option<f32>) -> StrokeRendering {
    let mut style = serde_json::json!({"color_argb": 0xff2462a0u32, "pen_size": size});
    if let Some(t) = tolerance {
        style["initial_tolerance"] = serde_json::json!(t);
    }
    if pen == INKPEN2 {
        for k in ["stroke_type_raw", "line_type_raw", "color_type_raw"] {
            style[k] = serde_json::json!(0);
        }
    }
    rendering_json(pen, settings, 2, &["compressed", "stylus_channels", "millisecond_timestamps", "binary_added"], style).unwrap()
}

#[test]
fn template_override_follows_recipe_3() {
    let spec = PenSpec { rendering: Some(template("14;", FOUNTAIN, 4.0, Some(1.0))), color_argb: 0xff10a010, size: 8.0, page_width: 800, page_height: 600 };
    let pen = Pen::new(spec).unwrap();
    let r = pen.template.rendering.as_ref().unwrap();
    assert_eq!(r.style.color_argb, Some(0xff10a010));
    assert_eq!(r.style.pen_size, Some(8.0));
    assert_eq!(r.style.initial_tolerance, Some(2.0));
    assert!(!r.properties.stylus_channels, "the writer drops stylus axes");
    assert_eq!(pen.template.pen_width, 8.0);
    // Same colour and size: the template style is copied verbatim.
    let same = Pen::new(PenSpec { rendering: Some(template("14;", FOUNTAIN, 4.0, Some(1.0))), color_argb: 0xff2462a0, size: 4.0, page_width: 800, page_height: 600 }).unwrap();
    assert_eq!(same.template.rendering.as_ref().unwrap().style.initial_tolerance, Some(1.0));
}

#[test]
fn every_admitted_basic_pen_rasterizes() {
    let samples = line(50, |t| 0.2 + 0.7 * (t * std::f64::consts::PI).sin(), 8);
    for (settings, name, tolerance) in [("14;", FOUNTAIN, None), ("18;0;100;", FOUNTAIN, None), ("9;", INKPEN2, Some(0.33)), ("8;", INKPEN2, Some(0.33))] {
        let spec = PenSpec { rendering: Some(template(settings, name, 6.0, tolerance)), color_argb: 0xff2462a0, size: 6.0, page_width: 800, page_height: 600 };
        let r = Pen::new(spec).unwrap().raster(&samples, 1.25).unwrap();
        assert!(r.width > 100 && r.rgba.chunks_exact(4).any(|p| p[3] > 200), "{settings} {name}");
    }
}

#[test]
fn unsupported_pens_are_refused() {
    let eraser = rendering_json(FOUNTAIN, "14;", 2, &["compressed", "eraser"], serde_json::json!({"color_argb": 0xff2462a0u32, "pen_size": 4.0})).unwrap();
    assert!(Pen::new(PenSpec { rendering: Some(eraser), color_argb: 0xff2462a0, size: 4.0, page_width: 800, page_height: 600 }).is_err());
    let other = template("7;", MARKER4, 4.0, None);
    assert!(Pen::new(PenSpec { rendering: Some(other), color_argb: 0xff2462a0, size: 4.0, page_width: 800, page_height: 600 }).is_err());
    assert!(Pen::new(PenSpec { rendering: None, color_argb: 0x80252525, size: 4.0, page_width: 800, page_height: 600 }).is_err());
    assert!(Pen::from_json("{\"colorARGB\":1}").is_err());
}

#[test]
fn single_sample_draws_nothing_like_the_page_renderer() {
    let r = first(4.0).raster(&[Sample { x: 5.0, y: 5.0, pressure: 0.5, time_ms: 0 }], 1.0).unwrap();
    assert_eq!(r, Raster::default());
}


/// Phase timings (native): `cargo test --release bench_phases -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_phases() {
    let pen = Pen::new(PenSpec { rendering: None, color_argb: 0xff252525, size: 4.0, page_width: 1000, page_height: 1400 }).unwrap();
    for n in [20usize, 100, 300] {
        let samples: Vec<Sample> = (0..n).map(|i| { let t = i as f64 / (n - 1) as f64; Sample { x: 100.0 + 700.0 * t, y: 300.0 + 150.0 * (t * 12.0).sin(), pressure: 0.3 + 0.5 * t, time_ms: i as i64 * 8 } }).collect();
        let t = std::time::Instant::now();
        let svg = pen.svg(&samples).unwrap();
        let a = t.elapsed();
        let t = std::time::Instant::now();
        let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
        let b = t.elapsed();
        let t = std::time::Instant::now();
        let r = raster_svg(&svg, (1000, 1400), 1.5).unwrap();
        let c = t.elapsed();
        eprintln!("n={n} svg={a:?} ({} bytes, {} elements) parse={b:?} parse+raster={c:?} {}x{}", svg.len(), svg.matches('<').count(), r.width, r.height);
        drop(tree);
    }
}


#[test]
#[ignore]
fn bench_prepare() {
    let pen = Pen::new(PenSpec { rendering: Some(template("18;0;100;", FOUNTAIN, 4.0, None)), color_argb: 0xff2462a0, size: 4.0, page_width: 1000, page_height: 1400 }).unwrap();
    let all: Vec<Sample> = (0..300).map(|i| { let t = i as f64 / 299.0; Sample { x: 100.0 + 700.0 * t, y: 300.0 + 150.0 * (t * 12.0).sin(), pressure: 0.3 + 0.5 * (t * 7.0).sin().abs(), time_ms: i as i64 * 8 } }).collect();
    let doc = pen.document(pen.stroke(&all).unwrap());
    let theme = sdocx::RenderTheme::resolve(&doc.pages[0], &doc.metadata, pen.options.color_mode);
    let stroke = doc.pages[0].strokes().next().unwrap();
    let t = std::time::Instant::now();
    let p = sdocx::prepare_stroke_for_page_with_theme(stroke, theme, &pen.options);
    eprintln!("prepare {:?} points {} radii {:?} ends {:?} profile {:?}", t.elapsed(), p.points.len(), p.dot_radii.as_ref().map(|r| r.len()), p.sample_ends.as_ref().map(|e| e.len()), p.profile);
    let t = std::time::Instant::now();
    let svg = pen.svg(&all).unwrap();
    eprintln!("svg {:?} {}", t.elapsed(), svg.len());
}

