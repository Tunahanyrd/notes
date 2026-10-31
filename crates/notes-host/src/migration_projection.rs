//! Bounded-lifetime projection hashing for offline schema migration.
use crate::areas::{editor_projection_val, MigrationProjectionVerifier};
use rs_store::db::{Error, Result};
use rs_store::snapshot_migration::{projection_value_hash, ProjectionHasher, ProjectionVerifier};
use rs_store::val::Val;
use rs_ops::ink_projection::Projection;

/// Produces the same baseline as the existing verifier while releasing each
/// editor/ink projection before computing the next one.
pub struct StreamingMigrationProjectionVerifier;
impl ProjectionVerifier for StreamingMigrationProjectionVerifier {
    fn recipe(&self) -> String { MigrationProjectionVerifier.recipe() }

    fn accepts_snapshot_identity(&self) -> bool { true }

    fn project(&self, snapshot: &Val) -> Result<(Val, Val)> {
        MigrationProjectionVerifier.project(snapshot)
    }

    fn project_hashes(&self, snapshot: &Val) -> Result<(String, String)> {
        let editor = {
            let value = editor_projection_val(snapshot)
                .map_err(|_| Error::Value("editor projection failed during migration".into()))?;
            projection_value_hash(&value)?
        };
        trim_heap();
        let mut hash = ProjectionHasher::new();
        hash.literal(b"[")?;
        for (i, points) in [false, true].into_iter().enumerate() {
            if i != 0 { hash.literal(b", ")?; }
            let value = rs_ops::ink_projection::project_ink_for_migration(&crate::PureDeps::default(), snapshot, points)
                .map_err(|_| Error::Value("ink projection failed during migration".into()))?;
            ink_projection(&mut hash, &value)?;
            drop(value);
            trim_heap();
        }
        hash.literal(b"]")?;
        Ok((editor, hash.finish()?))
    }
}

fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim takes no pointers and is thread-safe.
    unsafe { libc::malloc_trim(0); }
}

/// Emit the canonical host projection directly from the DTO, one point at a
/// time. This matches `areas::project_ink` key order and optional fields but
/// does not create an additional Val tree for every point in a large note.
fn ink_projection(hash: &mut ProjectionHasher, projection: &Projection) -> Result<()> {
    fn value(hash: &mut ProjectionHasher, value: Val) -> Result<()> { hash.value(&value) }
    fn field(hash: &mut ProjectionHasher, name: &[u8], data: Val) -> Result<()> { hash.literal(name)?; value(hash, data) }
    hash.literal(b"{\"schemaVersion\": 1, \"pages\": [")?;
    for (i, page) in projection.pages.iter().enumerate() {
        if i != 0 { hash.literal(b", ")?; }
        field(hash, b"{\"id\": ", Val::s(&page.id))?;
        field(hash, b", \"index\": ", Val::Int(page.index as i64))?;
        field(hash, b", \"width\": ", Val::Int(page.width))?;
        field(hash, b", \"height\": ", Val::Int(page.height))?;
        field(hash, b", \"background\": ", if page.pdf { Val::s("pdf") } else { Val::Null })?;
        hash.literal(b", \"strokes\": [")?;
        for (j, stroke) in page.strokes.iter().enumerate() {
            if j != 0 { hash.literal(b", ")?; }
            field(hash, b"{\"id\": ", Val::s(&stroke.id))?;
            field(hash, b", \"kind\": ", Val::s(stroke.kind))?;
            if stroke.template || stroke.kind == "highlighter" {
                field(hash, b", \"colorARGB\": ", Val::Int(stroke.color))?;
                field(hash, b", \"size\": ", Val::Float(stroke.size))?;
            }
            field(hash, b", \"template\": ", Val::Bool(stroke.template))?;
            field(hash, b", \"layerId\": ", Val::s(&stroke.layer_id))?;
            field(hash, b", \"erasable\": ", Val::Bool(stroke.erasable))?;
            if let Some(bounds) = stroke.bounds {
                hash.literal(b", \"bounds\": [")?;
                for (k, coordinate) in bounds.iter().enumerate() { if k != 0 { hash.literal(b", ")?; } value(hash, Val::Float(*coordinate))?; }
                hash.literal(b"]")?;
            }
            if let Some(points) = &stroke.points {
                hash.literal(b", \"points\": [")?;
                for (k, (x, y, pressure, time)) in points.iter().enumerate() {
                    if k != 0 { hash.literal(b", ")?; }
                    field(hash, b"{\"x\": ", Val::Float(*x))?;
                    field(hash, b", \"y\": ", Val::Float(*y))?;
                    field(hash, b", \"pressure\": ", Val::Float(*pressure))?;
                    field(hash, b", \"timeMs\": ", Val::Int(*time))?;
                    hash.literal(b"}")?;
                }
                hash.literal(b"]")?;
            }
            hash.literal(b"}")?;
        }
        hash.literal(b"], \"ink\": {")?;
        field(hash, b"\"appendInk\": ", Val::Bool(page.ink.append_ink))?;
        field(hash, b", \"eraseInk\": ", Val::Bool(page.ink.erase_ink))?;
        field(hash, b", \"firstStroke\": ", Val::Bool(page.ink.first_stroke))?;
        field(hash, b", \"highlighter\": ", Val::Bool(page.ink.highlighter))?;
        field(hash, b", \"reason\": ", Val::opt_str(page.ink.reason.as_deref()))?;
        hash.literal(b"}}")?;
    }
    hash.literal(b"], \"capabilities\": {")?;
    field(hash, b"\"appendInk\": ", Val::Bool(projection.append_ink))?;
    field(hash, b", \"eraseInk\": ", Val::Bool(projection.erase_ink))?;
    field(hash, b", \"firstStroke\": ", Val::Bool(projection.first_stroke))?;
    field(hash, b", \"highlighter\": ", Val::Bool(projection.highlighter))?;
    field(hash, b"}, \"reason\": ", Val::opt_str(projection.reason.as_deref()))?;
    hash.literal(b", \"rendering\": {\"nativeEquivalent\": false, \"mode\": \"geometry-only\"}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_hashes_equal_legacy_recipe_and_projection() {
        let corpus = rs_store::val::loads(include_str!("../../notes-preview/tests/fixtures/ink_cases.json")).unwrap();
        let mut cases = vec![Val::Null, Val::dict(vec![])];
        if let Val::Dict(entries) = corpus {
            for (_, item) in entries { if let Some(snapshot) = item.get("snapshot") { cases.push(snapshot.clone()); } }
        }
        for fixture in [include_str!("../../notes-preview/tests/fixtures/objects_note.json"), include_str!("../../notes-preview/tests/fixtures/floating_note.json")] {
            let item = rs_store::val::loads(fixture).unwrap();
            cases.push(item.get("snapshot").unwrap().clone());
        }
        let old = MigrationProjectionVerifier;
        let new = StreamingMigrationProjectionVerifier;
        assert_eq!(old.recipe(), new.recipe());
        for snapshot in cases {
            assert_eq!(old.project_hashes(&snapshot).unwrap(), new.project_hashes(&snapshot).unwrap());
        }
    }

    #[test]
    fn streamed_dto_keeps_optional_fields_and_float_format() {
        use rs_ops::ink_projection::{PageDto, PageInk, Projection, StrokeDto};
        let dto = Projection {
            pages: vec![PageDto {
                id: "page\"ç".into(), index: 1, width: 10, height: 20, pdf: true,
                strokes: vec![StrokeDto { id: "stroke".into(), template: false, kind: "highlighter", color: -12, size: -0.0, layer_id: "layer".into(), erasable: true,
                    bounds: Some([0.0, -0.0, 1.25, f64::INFINITY]), points: Some(vec![(1.5, -0.0, f64::NAN, 42)]) }],
                ink: PageInk { append_ink: true, erase_ink: false, first_stroke: true, highlighter: false, reason: Some("sentetik\nsebep".into()) },
            }], append_ink: true, erase_ink: false, first_stroke: true, highlighter: false, reason: None,
        };
        let expected = Val::dict(vec![
            ("schemaVersion", Val::Int(1)),
            ("pages", Val::List(vec![Val::dict(vec![
                ("id", Val::s("page\"ç")), ("index", Val::Int(1)), ("width", Val::Int(10)), ("height", Val::Int(20)), ("background", Val::s("pdf")),
                ("strokes", Val::List(vec![Val::dict(vec![
                    ("id", Val::s("stroke")), ("kind", Val::s("highlighter")), ("colorARGB", Val::Int(-12)), ("size", Val::Float(-0.0)),
                    ("template", Val::Bool(false)), ("layerId", Val::s("layer")), ("erasable", Val::Bool(true)),
                    ("bounds", Val::List(vec![Val::Float(0.0), Val::Float(-0.0), Val::Float(1.25), Val::Float(f64::INFINITY)])),
                    ("points", Val::List(vec![Val::dict(vec![("x", Val::Float(1.5)), ("y", Val::Float(-0.0)), ("pressure", Val::Float(f64::NAN)), ("timeMs", Val::Int(42))])])),
                ])])),
                ("ink", Val::dict(vec![("appendInk", Val::Bool(true)), ("eraseInk", Val::Bool(false)), ("firstStroke", Val::Bool(true)), ("highlighter", Val::Bool(false)), ("reason", Val::s("sentetik\nsebep"))])),
            ])])),
            ("capabilities", Val::dict(vec![("appendInk", Val::Bool(true)), ("eraseInk", Val::Bool(false)), ("firstStroke", Val::Bool(true)), ("highlighter", Val::Bool(false))])),
            ("reason", Val::Null),
            ("rendering", Val::dict(vec![("nativeEquivalent", Val::Bool(false)), ("mode", Val::s("geometry-only"))])),
        ]);
        let mut hash = ProjectionHasher::new();
        ink_projection(&mut hash, &dto).unwrap();
        assert_eq!(hash.finish().unwrap(), projection_value_hash(&expected).unwrap());
    }
}
