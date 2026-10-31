//! The publication verifier must make exactly the same decision as the host.
use notes_host::{snapshot, PureDeps};
use rs_ops::deps::Snapshot;
use rs_ops::ink_projection::{self, Verified};

fn outcome(result: rs_ops::err::R<Verified>) -> String {
    match result {
        Ok(Verified::Reason(r)) => format!("reason:{r}"),
        Err(e) => format!("error:{:?}:{}", e.kind, e.msg),
        Ok(Verified::Ok(payload, root)) => {
            let parts: Vec<_> = payload
                .parts
                .iter()
                .map(|p| (&p.name, &p.filename, &p.content_type, &p.body))
                .collect();
            format!(
                "ok:{:?}:{:?}:{:?}:{:?}",
                payload.locked,
                payload.xml_documents,
                parts,
                rs_ops::xml::xml_bytes(&root)
            )
        }
    }
}
fn compare(raw: &[u8], ct: &str, resource: &[u8], local: Option<&str>, commit: Option<&str>) {
    let snapshot = Snapshot {
        raw_body: raw.to_vec(),
        content_type: ct.into(),
        resource_xml: resource.to_vec(),
        local_id: local.map(str::to_string),
        commit_id: commit.map(str::to_string),
        handle: None,
    };
    let existing = outcome(ink_projection::verify(&PureDeps::new(), &snapshot));
    let publish = outcome(ink_projection::verify_raw_snapshot(
        raw, ct, resource, local, commit,
    ));
    assert_eq!(existing, publish);
    let full = ink_projection::verify_raw_snapshot(raw, ct, resource, local, commit);
    let streaming = ink_projection::verify_raw_snapshot_admission(raw, ct, resource, local, commit);
    match (full, streaming) {
        (Ok(Verified::Ok(_, _)), Ok(true)) | (Ok(Verified::Reason(_)), Ok(false)) => {},
        (Err(a), Err(b)) => assert_eq!((a.kind, a.msg), (b.kind, b.msg)),
        _ => panic!("streaming and full resource admission differ"),
    }
}
fn mutate(raw: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let at = raw.windows(old.len()).position(|x| x == old).unwrap();
    let mut out = raw.to_vec();
    out.splice(at..at + old.len(), new.iter().copied());
    out
}
#[test]
fn verifier_matches_host_for_valid_locked_namespace_identity_commit_and_parser_errors() {
    let value = rs_store::val::loads(include_str!("fixtures/ink_objects_base.json")).unwrap();
    let snap = snapshot::from_dict(&value).unwrap();
    let (raw, ct, resource) = (
        &snap.raw_body,
        snap.content_type.as_str(),
        &snap.resource_xml,
    );
    let (local, commit) = (snap.local_id.as_deref(), snap.commit_id.as_deref());
    compare(raw, ct, resource, local, commit);
    compare(
        raw,
        ct,
        resource,
        Some("00000000-0000-4000-8000-000000000001"),
        commit,
    );
    compare(raw, ct, resource, local, Some("other-commit"));
    compare(raw, ct, b"wrong resource", local, commit);
    compare(raw, "invalid-content-type", resource, local, commit);
    compare(b"not xml", "application/xml", resource, local, commit);
    for modified in [
        mutate(
            raw,
            b"<object type=\"stroke\"",
            b"<object isLocked=\"true\" type=\"stroke\"",
        ),
        mutate(
            raw,
            b"</objectList>",
            b"<x:extra xmlns:x=\"urn:test\"/></objectList>",
        ),
    ] {
        let changed = rs_doc::doc::parse_document(&modified, ct, None).unwrap();
        compare(
            &modified,
            ct,
            &changed.resource_xml,
            changed.local_id.as_deref(),
            changed.commit_id.as_deref(),
        );
    }
}

#[test]
fn shared_stroke_binding_rejects_malformed_duplicate_and_unknown_records() {
    let value = rs_store::val::loads(include_str!("fixtures/ink_objects_base.json")).unwrap();
    let snap = snapshot::from_dict(&value).unwrap();
    let limits = rs_doc::mime::Limits::default();
    let parsed = rs_doc::payload::parse_payload(
        &snap.raw_body,
        &snap.content_type,
        Some(limits.max_readonly_parts),
        &limits,
        &mut rs_doc::mime::NoHooks,
    )
    .unwrap();
    let parts: Vec<Vec<u8>> = parsed
        .parts
        .iter()
        .filter(|p| p.name.as_deref() == Some("strokeResource"))
        .map(|p| snap.raw_body[p.body.clone()].to_vec())
        .collect();
    assert_eq!(parts.len(), 1);
    let decoded = rs_ink::core::records::decode_records(&snap.resource_xml, &parts).unwrap();
    assert_eq!(decoded.len(), 1);
    assert!(rs_ink::core::records::decode_records(
        &snap.resource_xml,
        &[parts[0].clone(), parts[0].clone()]
    )
    .is_err());
    let mut invalid = parts[0].clone();
    invalid[8] = 1;
    assert!(rs_ink::core::records::decode_records(&snap.resource_xml, &[invalid]).is_err());
    let xml = std::str::from_utf8(&snap.resource_xml)
        .unwrap()
        .replace(
            "00000000-0000-4000-8000-000000000010",
            "00000000-0000-4000-8000-000000000011",
        )
        .into_bytes();
    assert!(rs_ink::core::records::decode_records(&xml, &parts).is_err());
    assert!(rs_ink::core::records::decode_records(&snap.resource_xml, &[vec![255; 3]]).is_err());
}
