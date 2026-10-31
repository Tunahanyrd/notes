//! Atomic operation-result publication for optional schema 7 container storage.
//! The caller must also teach garbage collection to enumerate these references
//! under the same DocumentStore lock before enabling this path.
use crate::util::{load_json, save_json, R};
use notes_host::{Fail, Val};
use rs_store::document_store::DocumentStore;
use rs_store::snapshot_store::{load_snapshot, store_snapshot};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const LIMIT: usize = 96 * 1024 * 1024;

/// Publish the immutable receipt only after the referenced container is durable.
/// A hard link publishes a complete, fsynced temp file without replacing an
/// existing receipt. The lock spans publication and the directory fsync.
pub(crate) fn save_result(directory: Option<&Path>, output: &Path, result: &Val) -> R<()> {
    save_result_inner(directory, output, result, || Ok(()))
}

fn save_result_inner(
    directory: Option<&Path>,
    output: &Path,
    result: &Val,
    before_link: impl FnOnce() -> R<()>,
) -> R<()> {
    let Some(directory) = directory else {
        return save_json(&output.join("result.json"), result);
    };
    let Some(snapshot) = result.get("snapshot") else {
        return save_json(&output.join("result.json"), result);
    };
    let store = DocumentStore::open(directory).map_err(Fail::from)?;
    store
        .with_write(|writer| {
            let reference = store_snapshot(writer, snapshot, None)?;
            let mut receipt = result.clone();
            receipt.set("snapshot", reference);
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes)
                .map_err(|_| rs_store::db::Error::Value("receipt randomness unavailable".into()))?;
            let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let temp = output.join(format!(".result-{name}.tmp"));
            let target = output.join("result.json");
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            let saved = (|| -> R<()> {
                file.write_all(&crate::json::receipt_bytes(&receipt))?;
                file.sync_all()?;
                before_link()?;
                fs::hard_link(&temp, &target)?;
                fs::File::open(output)?.sync_all()?;
                Ok(())
            })();
            let _ = fs::remove_file(&temp);
            saved.map_err(rs_store::db::Error::from)
        })
        .map_err(Fail::from)
}

/// Legacy embedded snapshots stay readable. A reference is always resolved
/// while holding the publication lock, so collection cannot race the read.
pub(crate) fn load_result(directory: Option<&Path>, output: &Path) -> R<Val> {
    let path = output.join("result.json");
    let Some(directory) = directory else {
        let result = load_json(&path, LIMIT)?;
        if result
            .get("snapshot")
            .and_then(|s| s.get("container_sha256"))
            .is_some()
        {
            return Err(Fail::value(
                "container store is required for operation receipt",
            ));
        }
        return Ok(result);
    };
    let store = DocumentStore::open(directory).map_err(Fail::from)?;
    store
        .with_write(|_| {
            let mut result = load_json(&path, LIMIT).map_err(rs_store::db::Error::from)?;
            if let Some(reference) = result
                .get("snapshot")
                .filter(|s| s.get("container_sha256").is_some())
                .cloned()
            {
                result.set("snapshot", load_snapshot(&store, &reference)?);
            }
            Ok(result)
        })
        .map_err(Fail::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).unwrap();
            let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("cloud-receipt-{name}"));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn result() -> Val {
        let raw = b"<noteResource/>";
        let xml = rs_doc::doc::parse_document(raw, "application/xml", None)
            .unwrap()
            .resource_xml;
        let snapshot = Val::dict(vec![
            ("schemaVersion", Val::Int(1)),
            ("content_type", Val::s("application/xml")),
            ("raw_body", Val::s(STANDARD.encode(raw))),
            ("resource_xml", Val::s(STANDARD.encode(xml))),
            ("raw_sha256", Val::s(format!("{:x}", Sha256::digest(raw)))),
            ("title", Val::s("synthetic")),
        ]);
        Val::dict(vec![("snapshot", snapshot), ("verified", Val::Bool(true))])
    }

    #[test]
    fn receipt_roundtrip_is_immutable_and_fails_closed_when_container_missing() {
        let temp = Temp::new();
        let output = temp.0.join("op");
        fs::create_dir(&output).unwrap();
        let original = result();
        save_result(Some(&temp.0), &output, &original).unwrap();
        let receipt = load_json(&output.join("result.json"), LIMIT).unwrap();
        let reference = receipt.get("snapshot").unwrap();
        assert!(reference.get("raw_body").is_none());
        assert!(reference.get("resource_xml").is_none());
        let hash = reference.get("container_sha256").unwrap().as_str().unwrap();
        assert!(rs_store::val::py_eq(
            &load_result(Some(&temp.0), &output).unwrap(),
            &original
        ));
        assert!(load_result(None, &output).is_err());
        assert!(save_result(Some(&temp.0), &output, &original).is_err());
        assert!(rs_store::val::py_eq(
            &load_result(Some(&temp.0), &output).unwrap(),
            &original
        ));
        fs::remove_file(
            temp.0
                .join("sdocx")
                .join(&hash[..2])
                .join(&hash[2..4])
                .join(format!("{hash}.sdocx")),
        )
        .unwrap();
        assert!(load_result(Some(&temp.0), &output).is_err());
    }

    #[test]
    fn legacy_embedded_receipt_replays_with_or_without_store() {
        let temp = Temp::new();
        let output = temp.0.join("legacy");
        fs::create_dir(&output).unwrap();
        let original = result();
        save_result(None, &output, &original).unwrap();
        assert!(rs_store::val::py_eq(
            &load_result(None, &output).unwrap(),
            &original
        ));
        assert!(rs_store::val::py_eq(
            &load_result(Some(&temp.0), &output).unwrap(),
            &original
        ));
    }

    #[test]
    fn interrupted_receipt_publication_can_retry_without_partial_result() {
        let temp = Temp::new();
        let output = temp.0.join("op");
        fs::create_dir(&output).unwrap();
        let original = result();
        assert!(
            save_result_inner(Some(&temp.0), &output, &original, || Err(Fail::value(
                "injected crash"
            )))
            .is_err()
        );
        assert!(!output.join("result.json").exists());
        let store = DocumentStore::open(&temp.0).unwrap();
        assert_eq!(store.doctor_raw(|| Ok(vec![])).unwrap().unreferenced, 1);
        save_result(Some(&temp.0), &output, &original).unwrap();
        assert!(rs_store::val::py_eq(
            &load_result(Some(&temp.0), &output).unwrap(),
            &original
        ));
    }

    #[test]
    fn collector_waits_until_receipt_is_published() {
        use std::sync::mpsc;
        use std::time::Duration;
        let temp = Temp::new();
        let output = temp.0.join("op");
        fs::create_dir(&output).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let root = temp.0.clone();
        let out = output.clone();
        let writer = std::thread::spawn(move || {
            save_result_inner(Some(&root), &out, &result(), || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
        });
        entered_rx.recv().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let root = temp.0.clone();
        let out = output.clone();
        let collector = std::thread::spawn(move || {
            let store = DocumentStore::open(&root).unwrap();
            started_tx.send(()).unwrap();
            let report = store
                .doctor_raw(|| {
                    let receipt = load_json(&out.join("result.json"), LIMIT)
                        .map_err(rs_store::db::Error::from)?;
                    Ok(vec![rs_store::snapshot_store::raw_reference(
                        receipt.get("snapshot").unwrap(),
                    )?])
                })
                .unwrap();
            done_tx.send(report).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        let report = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        collector.join().unwrap();
        assert_eq!(
            (
                report.referenced,
                report.missing,
                report.corrupt,
                report.unreferenced
            ),
            (1, 0, 0, 0)
        );
    }
}
