//! A panic while syncing (e.g. on a hostile Cloud answer) must not end
//! syncing for the rest of the session: before the fix it escaped
//! `sync_once`, killed the worker thread and poisoned `sync_lock`, so every
//! later `sync_once` returned false without syncing until restart.
mod common;

use std::panic::{catch_unwind, AssertUnwindSafe};

use common::*;
use notes_host::snapshot;
use notes_host::Val;

#[test]
fn panic_during_sync_is_an_error_and_the_next_sync_runs() {
    let t = TempDir::new("sync-panic");
    let api = ReviewCloud::new();
    let engine = review_engine(t.path(), &api);
    let identity = engine.store.apply_document(&engine.host, &Val::s("server-1"), &snapshot::to_dict(&document("A", "commit-1", "server-1")), None).unwrap();
    *api.current.lock().unwrap() = ("New remote text".into(), "commit-2".into());
    *api.disconnect_on_read.lock().unwrap() = Some(Box::new(|| panic!("synthetic hostile answer")));
    let first = catch_unwind(AssertUnwindSafe(|| engine.sync_once()));
    assert_eq!(first.ok(), Some(false), "the panic must be reported as a failed sync, not escape");
    let status = engine.status().unwrap();
    assert_eq!(status.get("sync").and_then(|s| s.get("state")).or(status.get("state")), Some(&Val::s("error")));
    // The fake's own hook mutex was poisoned by the panic; reset it.
    *api.disconnect_on_read.lock().unwrap_or_else(|e| e.into_inner()) = None;
    api.disconnect_on_read.clear_poison();
    assert!(engine.sync_once(), "sync must run again after a panic");
    assert_eq!(engine.store.get(&identity).unwrap().get("body"), Some(&Val::s("New remote text")));
    engine.close();
}
