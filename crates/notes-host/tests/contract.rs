//! The `&dyn Any` contract of PureDeps and the PureHost fault classification
//! (synthetic inputs only).
use std::any::Any;

use notes_host::areas;
use notes_host::deps::{raw_samples_from_points, PureDeps};
use notes_host::host::{exact_int, PureHost};
use notes_host::{Kind, Val};
use rs_ops::deps::Deps;
use rs_store::host::Host;

fn fixture_doc(name: &str) -> Val {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cases.json");
    let fx = rs_store::val::loads(&std::fs::read_to_string(path).unwrap()).unwrap();
    fx.get("docs").unwrap().get(name).unwrap().clone()
}

#[test]
fn snapshot_arguments() {
    let deps = PureDeps::default();
    // Non-dict values are "not a DocumentSnapshot".
    for v in [Val::Null, Val::Int(5), Val::List(vec![]), Val::s("x")] {
        assert!(deps.snapshot(&v as &dyn Any).unwrap().is_none());
    }
    // A dict goes through from_dict (its integrity failure is a DocumentError).
    let bad = Val::dict(vec![("schemaVersion", Val::Int(1)), ("raw_sha256", Val::s("0"))]);
    match deps.snapshot(&bad as &dyn Any) {
        Err(e) => assert_eq!(e.kind, rs_ops::err::Kind::Doc),
        Ok(_) => panic!("integrity failure expected"),
    }
    let doc = fixture_doc("ink");
    let snap = deps.snapshot(&doc as &dyn Any).unwrap().unwrap();
    let parsed = notes_host::snapshot::from_dict(&doc).unwrap();
    assert_eq!(snap.resource_xml, parsed.resource_xml);
    // The handle is the parsed DocumentSnapshot, accepted again as an argument.
    let handle = snap.handle.as_deref().unwrap();
    assert!(handle.downcast_ref::<notes_host::snapshot::Snapshot>().is_some());
    assert_eq!(deps.verified(handle).unwrap(), None);
    assert_eq!(deps.verified(&parsed as &dyn Any).unwrap(), None);
    // Unsupported argument types are a TypeError, never a panic.
    assert_eq!(deps.convert(&42u8 as &dyn Any, "m").err().unwrap().kind, rs_ops::err::Kind::Type);
}

#[test]
fn samples_from_points_mirrors_python_passes() {
    let pts = rs_store::val::loads(r#"[{"x": 1, "y": 2.5, "pressure": 0.5, "timeMs": 0}, {"x": 3.0, "y": 4.0, "pressure": 1, "timeMs": 7}]"#).unwrap();
    let raw = raw_samples_from_points(&pts).unwrap();
    assert_eq!(raw.points.len(), 2);
    assert_eq!(raw.timestamps.len(), 2);
    // KeyError from the first pass (x/y) wins over a missing pressure.
    let bad = rs_store::val::loads(r#"[{"y": 1, "timeMs": 0}]"#).unwrap();
    let e = raw_samples_from_points(&bad).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (Kind::Key, "'x'"));
    let e = raw_samples_from_points(&Val::Int(1)).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (Kind::Type, "'int' object is not iterable"));
}

#[test]
fn faults_are_classified_like_python_except_clauses() {
    let h = PureHost::new();
    let doc = fixture_doc("locked");
    // Locked note: DocumentError (page style), PageOpsError (a DocumentError) for page ops.
    let f = h.prepare_page_background(&doc, 0xFFF5DDDD).unwrap_err();
    assert!(f.document && f.value && !f.ink && !f.type_);
    let f = h.prepare_add_page(&doc, &Val::Int(0)).unwrap_err();
    assert!(f.document && f.value);
    // Non-iterable page ids: the snapshot load runs first, then TypeError.
    let ok_doc = fixture_doc("three_pages");
    let f = h.prepare_delete_page(&ok_doc, &Val::Int(5)).unwrap_err();
    assert!(f.type_ && !f.document);
    assert_eq!(f.message, "'int' object is not iterable");
    let f = h.prepare_delete_page(&doc, &Val::Int(5)).unwrap_err();
    assert!(f.document, "locked load error wins over the TypeError");
    // Collaborator failures travel as Fail inside db::Error.
    let fail = notes_host::Fail::from(f.error);
    assert_eq!(fail.kind, Kind::PageOps);
}

#[test]
fn exact_int_follows_type_is_int() {
    assert_eq!(exact_int(&Val::Int(3)), Some(3));
    assert_eq!(exact_int(&Val::Bool(true)), None);
    assert_eq!(exact_int(&Val::Float(1.0)), None);
    assert_eq!(exact_int(&Val::BigInt("-1180591620717411303424".into())), Some(-1180591620717411303424));
    assert_eq!(exact_int(&Val::BigInt("9".repeat(45))), Some(i128::MAX));
}

#[test]
fn null_snapshot_states() {
    let bg = areas::page_background_state(None).unwrap();
    assert_eq!(rs_store::val::dumps(&bg, false, false).unwrap(), r#"{"backgroundArgb": null, "editable": false, "reason": "Notun sayfa bilgisi henüz hazır değil."}"#);
    let ops = areas::page_ops_state(Some(&Val::Null)).unwrap();
    assert_eq!(rs_store::val::dumps(&ops, false, false).unwrap(), r#"{"editable": false, "reason": "Notun sayfa bilgisi henüz hazır değil.", "pages": []}"#);
    assert_eq!(areas::PROJECTION_RECIPE, "ink-projection-v6:highlighter");
}
