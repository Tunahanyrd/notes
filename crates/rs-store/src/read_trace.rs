//! Opt-in test instrumentation; absent from production builds.
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
thread_local! { static CONTAINER: RefCell<Option<(PathBuf, String)>> = const { RefCell::new(None) }; }

struct Observer {
    id: u64,
    names: HashSet<String>,
    container: Option<(PathBuf, String)>,
    seen: Vec<String>,
}
static OBSERVERS: Mutex<Vec<Observer>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A scoped observer records only the supplied unique fixture entry names.
pub struct Session(u64);
impl Session {
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        OBSERVERS.lock().unwrap().push(Observer {
            id,
            names: names.into_iter().collect(),
            container: None,
            seen: Vec::new(),
        });
        Self(id)
    }
    /// Trace every entry for a container in one store, including global entries.
    /// Identical containers in another store must not contaminate this trace.
    pub fn for_container(directory: &Path, container: &str) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        OBSERVERS.lock().unwrap().push(Observer { id, names: HashSet::new(), container: Some((directory.join("sdocx"), container.into())), seen: Vec::new() });
        Self(id)
    }
    pub fn finish(self) -> Vec<String> {
        let mut guard = OBSERVERS.lock().unwrap();
        guard
            .iter()
            .position(|o| o.id == self.0)
            .map(|i| guard.remove(i).seen)
            .unwrap_or_default()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        OBSERVERS.lock().unwrap().retain(|o| o.id != self.0);
    }
}
pub(crate) fn entry(name: &str) {
    let container = CONTAINER.with(|current| current.borrow().clone());
    for observer in OBSERVERS.lock().unwrap().iter_mut() {
        if observer.names.contains(name) || observer.container.as_ref().is_some_and(|expected| Some(expected) == container.as_ref()) {
            observer.seen.push(name.to_owned());
        }
    }
}
pub(crate) fn container(directory: &Path, hash: &str) { CONTAINER.with(|current| *current.borrow_mut() = Some((directory.to_path_buf(), hash.into()))); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_hash_in_another_store_does_not_pollute_trace() {
        let a = Path::new("/synthetic/trace-store-a");
        let b = Path::new("/synthetic/trace-store-b");
        let trace = Session::for_container(a, "same-hash");
        std::thread::spawn(move || {
            container(&b.join("sdocx"), "same-hash");
            entry("cloud/note.xml");
        }).join().unwrap();
        container(&a.join("sdocx"), "same-hash");
        entry("page.page");
        assert_eq!(trace.finish(), ["page.page"]);
    }
}
