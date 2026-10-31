//! Real loopback HTTP, real engine and real inventory worker; synthetic data only.
mod common;

use notes_engine::server::AppServer;
use notes_preview::{inventory_worker::compute_static, PREVIEW_INVENTORY_RECIPE};
use rs_store::{
    preview_inventory::InventoryOutcome, snapshot_migration::migrate_v7, storage::StoreCore,
    val::Val,
};
use std::{
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    time::{Duration, Instant},
};

fn raw_request(server: &AppServer, method: &str, path: &str, body: Option<&str>) -> (u16, Vec<u8>) {
    let mut socket = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let body = body.unwrap_or("");
    write!(socket, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer synthetic-local-secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", server.port, body.len()).unwrap();
    let mut reply = Vec::new();
    socket.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&reply[..split]).unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, reply[split + 4..].to_vec())
}
fn request(server: &AppServer, method: &str, path: &str, body: Option<&str>) -> (u16, Val) {
    let (status, body) = raw_request(server, method, path, body);
    (
        status,
        rs_store::val::loads(std::str::from_utf8(&body).unwrap()).unwrap(),
    )
}

#[test]
fn terminal_failure_requires_post_then_worker_publishes_ready() {
    let temp = common::TempDir::new("preview-retry-http");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("conformance/golden/library/main/library.sqlite3");
    let database = temp.path().join("library.sqlite3");
    std::fs::copy(source, &database).unwrap();
    migrate_v7(
        temp.path(),
        &database,
        &notes_host::areas::MigrationProjectionVerifier,
    )
    .unwrap();
    let store = StoreCore::open(temp.path()).unwrap();
    let key = store
        .pending_preview_inventories(PREVIEW_INVENTORY_RECIPE, 128)
        .unwrap()
        .into_iter()
        .find(|key| {
            let snapshot = store.preview_inventory_source(key).unwrap().unwrap();
            compute_static(&snapshot).is_ok()
        })
        .expect("synthetic fixture must contain a supported note");
    assert!(store
        .publish_preview_inventory(&key, &InventoryOutcome::Failed)
        .unwrap());
    let engine = common::offline_engine(temp.path());
    let server = AppServer::bind(
        0,
        engine.clone(),
        "synthetic-local-secret",
        temp.path(),
        Some(temp.path().join("fonts")),
    )
    .unwrap();
    let accept = server.spawn();
    let path = format!("/api/notes/{}/pages", key.note_id);

    let (status, failure) = request(&server, "GET", &path, None);
    assert_eq!(status, 503, "{failure:?}");
    assert_eq!(
        failure
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Val::as_str),
        Some("preview_inventory_failed")
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap(),
        Some(InventoryOutcome::Failed)
    );
    let (status, pending) = request(&server, "POST", &path, Some("{}"));
    assert_eq!(status, 200, "{pending:?}");
    assert_eq!(pending.get("pending"), Some(&Val::Bool(true)));

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match store
            .preview_inventory(&key.note_id, PREVIEW_INVENTORY_RECIPE)
            .unwrap()
        {
            Some(InventoryOutcome::Ready(_)) => break,
            Some(other) => panic!("retry did not produce Ready: {other:?}"),
            None => {}
        }
        assert!(Instant::now() < deadline, "worker retry timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (status, ready) = request(&server, "GET", &path, None);
    assert_eq!(status, 200, "{ready:?}");
    assert_ne!(ready.get("pending"), Some(&Val::Bool(true)));
    assert!(
        matches!(ready.get("pages"), Some(Val::List(_))),
        "{ready:?}"
    );
    server.close();
    accept.join().unwrap();
    engine.close();
}

fn traced_fixture() -> Val {
    let original = rs_store::val::loads(include_str!(
        "../../notes-host/tests/fixtures/ink_objects_base.json"
    ))
    .unwrap();
    let source = notes_host::snapshot::from_dict(&original).unwrap();
    let resource = rs_doc::xml::fromstring(&source.resource_xml).unwrap();
    let pages = resource
        .children
        .iter()
        .find(|e| e.local() == "pagelist")
        .unwrap();
    let mut raw = source.raw_body.clone();
    for (index, page) in pages.children.iter().enumerate() {
        let old = page.get("id").unwrap().as_bytes();
        let new = format!(
            "20000000-0000-4000-8000-{:012x}",
            std::process::id() as u64 * 100 + index as u64
        );
        let mut replaced = Vec::new();
        let mut cursor = 0;
        while let Some(at) = raw[cursor..].windows(old.len()).position(|w| w == old) {
            let at = cursor + at;
            replaced.extend_from_slice(&raw[cursor..at]);
            replaced.extend_from_slice(new.as_bytes());
            cursor = at + old.len();
        }
        replaced.extend_from_slice(&raw[cursor..]);
        raw = replaced;
    }
    let limits = rs_doc::mime::Limits::default();
    let parts = rs_doc::mime::parts(
        &raw,
        &source.content_type,
        limits.max_readonly_parts,
        &limits,
        &mut rs_doc::mime::NoHooks,
    )
    .unwrap();
    let mut canonical = Vec::new();
    for part in parts {
        let original = &raw[part.body];
        let name = part.name.as_deref().unwrap();
        let body = if name == "noteResource" {
            let mut document = rs_ops::xml::parse_bytes(original).unwrap();
            let note = document
                .children
                .iter_mut()
                .find(|n| n.local() == "noteresource")
                .unwrap();
            note.children
                .iter_mut()
                .find(|n| n.local() == "attributes")
                .unwrap()
                .set("height", "4629");
            let tree = note
                .children
                .iter()
                .find(|n| n.local() == "notetree")
                .unwrap()
                .children
                .iter()
                .find(|n| n.local() == "pagelist")
                .unwrap()
                .children[0]
                .clone();
            let page = note
                .children
                .iter()
                .find(|n| n.local() == "pagelist")
                .unwrap()
                .children[0]
                .clone();
            fn clear_objects(element: &mut rs_ops::xml::Element) {
                element.children.retain(|c| c.tag != "object");
                for child in &mut element.children {
                    clear_objects(child);
                }
            }
            for (id, layer) in [
                (
                    "00000000-0000-4000-8000-000000000101",
                    "00000000-0000-4000-8000-000000000111",
                ),
                (
                    "00000000-0000-4000-8000-000000000102",
                    "00000000-0000-4000-8000-000000000112",
                ),
            ] {
                let mut tree_page = tree.clone();
                let mut page = page.clone();
                for node in [&mut tree_page, &mut page] {
                    node.set("id", id);
                    clear_objects(node);
                    if let Some(list) = node.children.iter_mut().find(|n| n.tag == "layerList") {
                        if let Some(item) = list.children.iter_mut().find(|n| n.tag == "layer") {
                            item.set("id", layer);
                        }
                    }
                }
                note.children
                    .iter_mut()
                    .find(|n| n.local() == "notetree")
                    .unwrap()
                    .children
                    .iter_mut()
                    .find(|n| n.local() == "pagelist")
                    .unwrap()
                    .children
                    .push(tree_page);
                note.children
                    .iter_mut()
                    .find(|n| n.local() == "pagelist")
                    .unwrap()
                    .children
                    .push(page);
            }
            rs_ops::xml::xml_bytes(&document)
        } else {
            original.to_vec()
        };
        let filename = part
            .filename
            .as_deref()
            .map(|s| format!("; filename=\"{s}\""))
            .unwrap_or_default();
        let content_type = if part.filename.is_some() {
            "application/octet-stream"
        } else {
            "application/xml"
        };
        canonical.extend_from_slice(format!("--RenderTest\r\nContent-Disposition: form-data; name=\"{name}\"{filename}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
        canonical.extend_from_slice(&body);
        canonical.extend_from_slice(b"\r\n");
    }
    canonical.extend_from_slice(b"--RenderTest--\r\n");
    let snapshot = notes_host::snapshot::parse_document(
        &canonical,
        "multipart/form-data;boundary=RenderTest",
        source.server_id.as_deref(),
    )
    .unwrap();
    notes_host::snapshot::to_dict(&snapshot)
}

#[test]
fn http_preview_reads_only_selected_page_entries() {
    use rs_store::db::Sql;
    let temp = common::TempDir::new("preview-two-entries-http");
    let store = StoreCore::open(temp.path()).unwrap();
    let fixture = traced_fixture();
    let note = store.create(&Val::s("Synthetic"), &Val::s("")).unwrap();
    let id = note.get("id").unwrap().as_str().unwrap().to_string();
    let document = rs_store::val::dumps(&fixture, true, true).unwrap();
    store
        .with_db(|db| {
            db.exec(
                "UPDATE notes SET base_document=?,loaded=1,deleted_remote=0 WHERE id=?",
                &rs_store::p![document, &id],
            )
            .map(|_| ())
        })
        .unwrap();
    drop(store);
    migrate_v7(
        temp.path(),
        &temp.path().join("library.sqlite3"),
        &notes_host::areas::MigrationProjectionVerifier,
    )
    .unwrap();
    let store = StoreCore::open(temp.path()).unwrap();
    let key = store
        .pending_preview_inventories(PREVIEW_INVENTORY_RECIPE, 128)
        .unwrap()
        .into_iter()
        .find(|k| k.note_id == id)
        .unwrap();
    let engine = common::offline_engine(temp.path());
    let server = AppServer::bind(
        0,
        engine.clone(),
        "synthetic-local-secret",
        temp.path(),
        Some(temp.path().join("fonts")),
    )
    .unwrap();
    let accept = server.spawn();
    let pages_path = format!("/api/notes/{id}/pages");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (status, value) = request(&server, "GET", &pages_path, None);
        assert_eq!(status, 200, "{value:?}");
        if value.get("pending") != Some(&Val::Bool(true)) {
            break;
        }
        assert!(Instant::now() < deadline, "inventory timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        store
            .preview_inventory(&id, PREVIEW_INVENTORY_RECIPE)
            .unwrap(),
        Some(InventoryOutcome::Ready(_))
    ));
    assert!(
        store
            .preview_render_blob(&id, PREVIEW_INVENTORY_RECIPE)
            .unwrap()
            .is_some(),
        "fixture needs admitted projection"
    );
    let page_ids = store
        .with_db(|db| {
            let rows = db.query(
                "SELECT page_id FROM note_pages WHERE note_id=? ORDER BY idx",
                &rs_store::p![&id],
            )?;
            Ok(rows
                .iter()
                .map(|row| row.text("page_id").unwrap().to_owned())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert!(page_ids.len() >= 3, "fixture must exercise multiple pages");
    let page_id = &page_ids[0];
    let trace = rs_store::read_trace::Session::for_container(temp.path(), &key.container_sha256);
    let (status, manifest) = request(&server, "GET", &pages_path, None);
    assert_eq!(status, 200, "{manifest:?}");
    let preview_path = format!("/api/notes/{id}/preview?page=0");
    loop {
        let (status, body) = raw_request(&server, "GET", &preview_path, None);
        if status == 200 {
            assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"));
            break;
        }
        assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
        assert!(Instant::now() < deadline, "preview timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    server.close();
    accept.join().unwrap();
    engine.close();
    let entries = trace.finish();
    let page = format!("{page_id}.page");
    let xml = format!("cloud/pages/{page_id}.xml");
    assert!(
        entries.contains(&page) && entries.contains(&xml),
        "{entries:?}"
    );
    assert!(
        entries.iter().all(|name| name == &page || name == &xml),
        "{entries:?}"
    );
}
