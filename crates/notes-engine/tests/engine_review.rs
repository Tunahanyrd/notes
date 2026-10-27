//! Port of tests/test_engine_review.py plus outbox decisions of engine.py
//! (`_push` except clauses) with a fake Cloud.
mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::*;
use notes_engine::sessions::{NoLogin, Sessions};
use notes_host::snapshot;
use notes_host::{Fail, Val};

#[test]
fn loaded_note_fetch_failure_is_retried_after_cursor_advance() {
    let t = TempDir::new("retry");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let identity = engine.store.apply_document(&engine.host, &Val::s("server-1"), &snapshot::to_dict(&document("A", "commit-1", "server-1")), None).unwrap();
    *api.current.lock().unwrap() = ("New remote text".into(), "commit-2".into());
    *api.fail_once.lock().unwrap() = true;
    assert!(!engine.sync_once());
    assert_eq!(engine.store.meta("last_synced_at").unwrap(), None);
    assert_eq!(engine.store.get(&identity).unwrap().get("body"), Some(&Val::s("A")));
    assert_eq!(engine.store.meta("cursor").unwrap(), Some(Val::s("after-page")));
    assert!(engine.sync_once());
    assert_eq!(engine.store.get(&identity).unwrap().get("body"), Some(&Val::s("New remote text")));
    assert!(api.reads.lock().unwrap().len() >= 2);
    engine.close();
}

#[test]
fn offline_edit_reverted_to_cloud_base_does_not_become_failed_job() {
    let t = TempDir::new("revert");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let identity = engine.store.apply_document(&engine.host, &Val::s("server-1"), &snapshot::to_dict(&document("A", "commit-1", "server-1")), None).unwrap();
    let row = engine.store.get(&identity).unwrap();
    let edited = engine.store.save(&engine.host, &identity, &Val::s("Title"), &Val::s("B"), row.get("revision").unwrap()).unwrap();
    engine.store.save(&engine.host, &identity, &Val::s("Title"), &Val::s("A"), edited.get("revision").unwrap()).unwrap();
    let cloud: Arc<dyn notes_engine::cloud::Cloud> = api.clone();
    engine.push(&cloud).unwrap();
    assert_eq!(engine.store.get(&identity).unwrap().get("syncState"), Some(&Val::s("synced")));
    assert_eq!(api.writes.load(Ordering::SeqCst), 0);
    engine.close();
}

#[test]
fn disconnect_during_page_stops_remaining_remote_fetches() {
    let t = TempDir::new("disconnect");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    // Downloads run on DOWNLOAD_WORKERS threads: a read already past its
    // interruption check when the first read disconnects may still finish,
    // but no worker starts another one and the cursor must not advance.
    let workers = notes_engine::engine::DOWNLOAD_WORKERS;
    *api.change_count.lock().unwrap() = workers * 3;
    let sessions = engine.sessions.clone();
    *api.disconnect_on_read.lock().unwrap() = Some(Box::new(move || {
        sessions.disconnect().unwrap();
    }));
    let cloud: Arc<dyn notes_engine::cloud::Cloud> = api.clone();
    assert!(!engine.pull(&cloud).unwrap());
    let reads = api.reads.lock().unwrap().len();
    assert!((1..=workers).contains(&reads), "{} reads after disconnect", reads);
    assert_ne!(engine.store.meta("cursor").unwrap(), Some(Val::s("after-page")));
    engine.close();
}

#[test]
fn duplicate_remote_snapshot_does_not_invalidate_editor_revision() {
    let t = TempDir::new("dup");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let doc = snapshot::to_dict(&document("A", "commit-1", "server-1"));
    let identity = engine.store.apply_document(&engine.host, &Val::s("server-1"), &doc, None).unwrap();
    let before = engine.store.get(&identity).unwrap();
    engine.store.apply_document(&engine.host, &Val::s("server-1"), &doc, None).unwrap();
    assert_eq!(engine.store.get(&identity).unwrap().get("revision"), before.get("revision"));
    engine.close();
}

#[test]
fn other_account_rejected_before_session_switch() {
    use std::os::unix::fs::PermissionsExt;
    let t = TempDir::new("account");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let research = t.path().join("synthetic-research");
    let mut paths = vec![];
    for name in ["account-a", "account-b"] {
        let path = research.join("sessions").join(name);
        std::fs::create_dir_all(path.join("cloud")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(path.join("cloud/cloud-token.json"), "{}").unwrap();
        std::fs::write(path.join("notes-account-token.json"), format!("{{\"clientId\": \"synthetic-notes\", \"response\": {{\"userId\": \"{}\"}}}}", name)).unwrap();
        paths.push(path);
    }
    let manager = Sessions::with_research(engine.store.clone(), true, None, Arc::new(NoLogin), research.clone()).unwrap();
    manager.adopt(&paths[0]).unwrap();
    let error = manager.adopt(&paths[1]).unwrap_err();
    assert!(error.is_store());
    assert_eq!(error.code, "different_account");
    assert_eq!(manager.path(), Some(std::fs::canonicalize(&paths[0]).unwrap()));
    engine.close();
}

fn pending_edit(engine: &notes_engine::engine::Engine) -> String {
    let identity = engine.store.apply_document(&engine.host, &Val::s("server-1"), &snapshot::to_dict(&document("A", "commit-1", "server-1")), None).unwrap();
    let row = engine.store.get(&identity).unwrap();
    engine.store.save(&engine.host, &identity, &Val::s("Title"), &Val::s("B"), row.get("revision").unwrap()).unwrap();
    identity
}

#[test]
fn verified_update_completes_job_and_applies_snapshot() {
    let t = TempDir::new("verified");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let identity = pending_edit(&engine);
    let cloud: Arc<dyn notes_engine::cloud::Cloud> = api.clone();
    engine.push(&cloud).unwrap();
    assert_eq!(api.writes.load(Ordering::SeqCst), 1);
    let note = engine.store.get(&identity).unwrap();
    assert_eq!(note.get("syncState"), Some(&Val::s("synced")));
    assert_eq!(note.get("body"), Some(&Val::s("B")));
    engine.close();
}

#[test]
fn ambiguous_write_becomes_uncertain_and_is_not_retried() {
    let t = TempDir::new("ambiguous");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let identity = pending_edit(&engine);
    *api.update_error.lock().unwrap() = Some(Fail::ambiguous("write_uncertain", "Bulut kaydı henüz doğrulanamadı."));
    let cloud: Arc<dyn notes_engine::cloud::Cloud> = api.clone();
    engine.push(&cloud).unwrap();
    assert_eq!(engine.store.get(&identity).unwrap().get("syncState"), Some(&Val::s("uncertain")));
    // A second push must not publish the uncertain job again.
    engine.push(&cloud).unwrap();
    assert_eq!(api.writes.load(Ordering::SeqCst), 0);
    engine.close();
}

#[test]
fn unavailable_cloud_requeues_job_and_raises() {
    let t = TempDir::new("requeue");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let identity = pending_edit(&engine);
    *api.update_error.lock().unwrap() = Some(Fail::api("cloud_unavailable", "Samsung Cloud'a ulaşılamadı."));
    let cloud: Arc<dyn notes_engine::cloud::Cloud> = api.clone();
    let error = engine.push(&cloud).unwrap_err();
    assert_eq!(error.code, "cloud_unavailable");
    assert_eq!(engine.store.get(&identity).unwrap().get("syncState"), Some(&Val::s("pending")));
    // The queued job is published by the next push.
    engine.push(&cloud).unwrap();
    assert_eq!(api.writes.load(Ordering::SeqCst), 1);
    engine.close();
}

#[test]
fn conflict_marks_job_and_refreshes_remote() {
    let t = TempDir::new("conflict");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let identity = pending_edit(&engine);
    *api.current.lock().unwrap() = ("Remote".into(), "commit-9".into());
    *api.update_error.lock().unwrap() = Some(Fail::conflict("Not başka bir cihazda değişti; yerel metin korunuyor."));
    let cloud: Arc<dyn notes_engine::cloud::Cloud> = api.clone();
    engine.push(&cloud).unwrap();
    let note = engine.store.get(&identity).unwrap();
    assert_eq!(note.get("syncState"), Some(&Val::s("conflict")));
    assert_eq!(*api.reads.lock().unwrap(), vec!["server-1".to_string()]);
    assert_eq!(api.writes.load(Ordering::SeqCst), 0);
    engine.close();
}

#[test]
fn sync_status_reports_offline_error_when_cloud_unreachable() {
    let t = TempDir::new("status");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    pending_edit(&engine);
    *api.update_error.lock().unwrap() = Some(Fail::api("cloud_unavailable", "Samsung Cloud'a ulaşılamadı."));
    assert!(!engine.sync_once());
    let status = engine.status().unwrap();
    let sync = status.get("sync").unwrap();
    assert_eq!(sync.get("state"), Some(&Val::s("offline")));
    assert_eq!(sync.get("error"), Some(&Val::s("Samsung Cloud'a ulaşılamadı.")));
    assert_eq!(status.get("connected"), Some(&Val::Bool(true)));
    engine.close();
}
