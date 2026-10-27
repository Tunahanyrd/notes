//! The `save_page_op(op='shape')` dry run shared by `PyHost` and the pure
//! host: `cloud_shapes.shape_intent` + `page_shapes.prepare_add_shape` over
//! JSON values, on any rs-ops `Deps`.
use crate::val::Val;
use _ops::err::{Error, Kind, R};
use _ops::page_shapes::{self, Fill, Shape, StyleIn};
use std::any::Any;

/// `type(v) is int` → value (saturated to the i128 range).
fn exact_int(v: &Val) -> Option<i128> {
    match v {
        Val::Int(i) => Some(*i as i128),
        Val::BigInt(s) => Some(match s.trim().parse::<i128>() {
            Ok(i) => i,
            Err(_) if s.trim_start().starts_with('-') => i128::MIN,
            Err(_) => i128::MAX,
        }),
        _ => None,
    }
}

/// `type(v) in (int, float)` → f64.
fn int_or_float(v: &Val) -> Option<f64> {
    match v {
        Val::Float(f) => Some(*f),
        other => exact_int(other).map(|i| i as f64),
    }
}

/// The normalized shape as `cloud_shapes.shape_intent` returns it.
pub fn shape_val(s: &Shape) -> Val {
    Val::dict(vec![
        ("kind", Val::s(s.kind.as_str())),
        ("geometry", Val::List(s.geometry.iter().map(|v| Val::Float(*v)).collect())),
        ("colorARGB", Val::Int(s.color_argb as i64)),
        ("width", Val::Float(s.width)),
        ("fillARGB", s.fill_argb.map(|f| Val::Int(f as i64)).unwrap_or(Val::Null)),
    ])
}

/// `cloud_shapes.shape_intent(shape)`; a failure is the ShapeError message
/// (UnsupportedDocumentError in Python).
pub fn shape_intent(shape: &Val) -> Result<Shape, String> {
    let allowed = ["kind", "geometry", "colorARGB", "width", "fillARGB"];
    let Val::Dict(items) = shape else {
        return Err("Şekil biçimi geçersiz.".into());
    };
    if items.iter().any(|(k, _)| !allowed.contains(&k.as_str())) {
        return Err("Şekil biçimi geçersiz.".into());
    }
    let kind = shape.get("kind").and_then(|v| v.as_str());
    let geometry: Option<Vec<Option<f64>>> = match shape.get("geometry") {
        Some(Val::List(xs)) => Some(xs.iter().map(int_or_float).collect()),
        _ => None,
    };
    let keys: Vec<String> = ["colorARGB", "width", "fillARGB"].iter().filter(|k| shape.get(k).is_some()).map(|k| k.to_string()).collect();
    let style = StyleIn {
        is_dict: true,
        keys,
        color: shape.get("colorARGB").and_then(exact_int),
        width: shape.get("width").and_then(int_or_float),
        fill: match shape.get("fillARGB") {
            None | Some(Val::Null) => Fill::Absent,
            Some(v) => match exact_int(v) {
                Some(i) => Fill::Int(i),
                None => Fill::Bad,
            },
        },
    };
    page_shapes::normalize_shape(kind, geometry.as_deref(), &style).map_err(|e| e.msg)
}

fn style_of(s: &Shape) -> StyleIn {
    StyleIn {
        is_dict: true,
        keys: vec!["colorARGB".into(), "width".into(), "fillARGB".into()],
        color: Some(s.color_argb as i128),
        width: Some(s.width),
        fill: s.fill_argb.map(|f| Fill::Int(f as i128)).unwrap_or(Fill::Absent),
    }
}

/// storage.save_page_op's `op == 'shape'` block: returns the spec to store
/// (`{'pageId', 'shape': <normalized>}`). Errors carry the Python class:
/// Doc (shape_intent/prepare refusal), Key (spec keys), Type, others as raised.
pub fn dry_run(deps: &dyn _ops::deps::Deps, document: &dyn Any, spec: &Val) -> R<Val> {
    let raw = match spec {
        Val::Dict(_) => spec.get("shape").ok_or_else(|| Error::new(Kind::Key, "'shape'"))?,
        Val::List(_) => return Err(Error::new(Kind::Type, "list indices must be integers or slices, not str")),
        Val::Str(_) => return Err(Error::new(Kind::Type, "string indices must be integers, not 'str'")),
        other => return Err(Error::new(Kind::Type, format!("'{}' object is not subscriptable", other.type_name()))),
    };
    let shape = shape_intent(raw).map_err(|m| Error::new(Kind::Doc, m))?;
    let Val::Dict(items) = spec else { unreachable!() };
    let keys: std::collections::BTreeSet<&str> = items.iter().map(|(k, _)| k.as_str()).collect();
    if keys != ["pageId", "shape"].into_iter().collect() {
        return Err(Error::new(Kind::Key, "'spec'"));
    }
    let page = spec.get("pageId").unwrap();
    page_shapes::prepare_add_shape(
        deps,
        document,
        page.as_str(),
        Some(shape.kind.as_str()),
        Some(&shape.geometry.map(Some)),
        &style_of(&shape),
        Some(crate::storage::DRY_RUN_MS as i128),
        None,
    )?;
    Ok(Val::dict(vec![("pageId", page.clone()), ("shape", shape_val(&shape))]))
}
