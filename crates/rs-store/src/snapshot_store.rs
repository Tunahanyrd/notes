//! Conversion between a Cloud snapshot and its immutable container reference.
//!
//! A caller publishing a reference must hold `DocumentStore::with_write`
//! through its SQLite commit. In particular, never collect between publication
//! and commit.
use crate::db::{Error, Result};
use crate::document_store::{DocumentStore, DocumentWriter, RawDocumentRef};
use crate::note_container::{self, PageInfo};
use crate::val::Val;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::collections::VecDeque;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// Account for both JSON strings and heap overhead; oversized snapshots are
// reconstructed on demand but are never retained in this cache.
const CACHE_BUDGET: usize = 64 * 1024 * 1024;
#[derive(Clone, PartialEq, Eq)]
struct CacheKey {
    directory: PathBuf,
    metadata_hash: String,
    device: u64,
    inode: u64,
    length: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
struct CacheEntry { key: CacheKey, value: Val, cost: usize }
static SNAPSHOTS: Mutex<VecDeque<CacheEntry>> = Mutex::new(VecDeque::new());

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn field<'a>(value: &'a Val, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Val::as_str)
        .ok_or_else(|| Error::Value(format!("invalid snapshot {name}")))
}

pub fn raw_reference(value: &Val) -> Result<RawDocumentRef> {
    let size = value
        .get("container_size")
        .and_then(Val::as_i64)
        .filter(|size| *size >= 0)
        .ok_or_else(|| Error::Value("invalid container size".into()))?;
    Ok(RawDocumentRef {
        sha256: field(value, "container_sha256")?.to_string(),
        size: size as u64,
    })
}

/// Build and verify a container before publication. A previous ZIP enables
/// raw-copying unchanged compressed entries; it is never modified.
pub fn store_snapshot_with_pages(
    writer: &DocumentWriter<'_>,
    snapshot: &Val,
    previous: Option<&[u8]>,
) -> Result<(Val, Vec<PageInfo>)> {
    let mut previous = previous.map(Cursor::new);
    store_snapshot_with_pages_from_reader(writer, snapshot, previous.as_mut().map(|p| p as &mut dyn note_container::ReadSeek))
}

/// As above, but copies unchanged ZIP entries directly from a seekable file.
pub fn store_snapshot_with_pages_from_reader(
    writer: &DocumentWriter<'_>, snapshot: &Val, previous: Option<&mut dyn note_container::ReadSeek>,
) -> Result<(Val, Vec<PageInfo>)> {
    if !matches!(snapshot, Val::Dict(_)) {
        return Err(Error::Value("invalid snapshot".into()));
    }
    let raw = STANDARD
        .decode(field(snapshot, "raw_body")?)
        .map_err(|_| Error::Value("invalid snapshot body encoding".into()))?;
    let resource = STANDARD
        .decode(field(snapshot, "resource_xml")?)
        .map_err(|_| Error::Value("invalid snapshot XML encoding".into()))?;
    let expected_hash = field(snapshot, "raw_sha256")?;
    if digest(&raw) != expected_hash {
        return Err(Error::Value("snapshot SHA-256 verification failed".into()));
    }
    let ct = field(snapshot, "content_type")?;
    let parse_started = std::time::Instant::now();
    let parsed = rs_doc::doc::parse_document_fields(&raw, ct, &rs_doc::doc::DocLimits::default(), &mut rs_doc::mime::NoHooks, &mut || Ok(None))
        .map_err(|_| Error::Value("snapshot parse failed".into()))?;
    if parsed.resource_xml != resource {
        return Err(Error::Value(
            "container round-trip verification failed".into(),
        ));
    }
    if std::env::var_os("NOTES_MIGRATION_PROFILE").is_some() { eprintln!("container-source input_bytes={} parse_ms={}", raw.len(), parse_started.elapsed().as_millis()); }
    let (reference, built) = writer.put_raw_built(|file| {
        note_container::build_with_snapshot_to(&raw, ct, previous, &resource,
            snapshot.get("local_id").and_then(Val::as_str), snapshot.get("commit_id").and_then(Val::as_str), file).map_err(Error::Value)
    })?;
    if built.raw_sha256 != expected_hash { return Err(Error::Value("container source SHA-256 mismatch".into())); }
    let Val::Dict(items) = snapshot else { unreachable!() };
    let mut removed = std::collections::HashSet::new();
    let mut metadata = Val::Dict(items.iter().filter(|(k,_)| !matches!(k.as_str(), "raw_body" | "resource_xml" | "context_sha256" | "manifest_sha256") || !removed.insert(k.as_str()))
        .map(|(k,v)| (k.clone(),v.clone())).collect());
    metadata.set("container_sha256", Val::s(&reference.sha256));
    metadata.set("container_size", Val::Int(reference.size as i64));
    metadata.set("manifest_sha256", Val::s(&built.manifest_sha256));
    if let Some(hash) = &built.context_sha256 {
        metadata.set("context_sha256", Val::s(hash));
    }
    Ok((metadata, built.pages))
}

pub fn store_snapshot(
    writer: &DocumentWriter<'_>,
    snapshot: &Val,
    previous: Option<&[u8]>,
) -> Result<Val> {
    Ok(store_snapshot_with_pages(writer, snapshot, previous)?.0)
}

/// Reconstruct only on demand and validate the original snapshot digest.
pub fn load_snapshot(store: &DocumentStore, metadata: &Val) -> Result<Val> {
    let reference = raw_reference(metadata)?;
    let file = store.open_raw(&reference)?;
    load_snapshot_from_file(metadata, file)
}

/// Reconstruct from a file opened while the publication lock was held. On
/// Unix the descriptor remains valid even if GC unlinks the path afterwards.
pub fn load_snapshot_from_file(metadata: &Val, file: std::fs::File) -> Result<Val> {
    let reference = raw_reference(metadata)?;
    if file.metadata()?.len() != reference.size { return Err(Error::Value("container size mismatch".into())); }
    let rebuilt = note_container::reconstruct(file).map_err(Error::Value)?;
    if digest(&rebuilt.raw_body) != field(metadata, "raw_sha256")?
        || rebuilt.content_type != field(metadata, "content_type")?
    {
        return Err(Error::Value(
            "reconstructed snapshot verification failed".into(),
        ));
    }
    let mut snapshot = metadata.clone();
    snapshot.remove("container_sha256");
    snapshot.remove("container_size");
    snapshot.remove("context_sha256");
    snapshot.remove("manifest_sha256");
    snapshot.set("raw_body", Val::s(STANDARD.encode(&rebuilt.raw_body)));
    let parsed = rs_doc::doc::parse_document_fields(&rebuilt.raw_body, &rebuilt.content_type, &rs_doc::doc::DocLimits::default(), &mut rs_doc::mime::NoHooks, &mut || Ok(None))
        .map_err(|_| Error::Value("snapshot parse failed".into()))?;
    snapshot.set(
        "resource_xml",
        Val::s(STANDARD.encode(parsed.resource_xml)),
    );
    Ok(snapshot)
}

/// Resolve lazily. The caller holds the publication lock from reference
/// lookup through this call, so GC cannot remove the file in between.
pub fn load_snapshot_cached(store: &DocumentStore, directory: &Path, metadata: &Val) -> Result<Val> {
    let reference = raw_reference(metadata)?;
    let file = store.open_raw(&reference)?;
    load_snapshot_cached_from_file(directory, metadata, file)
}

/// Cache-aware reconstruction from a descriptor acquired under the
/// publication lock. The descriptor remains valid after the lock is dropped.
pub fn load_snapshot_cached_from_file(directory: &Path, metadata: &Val, file: std::fs::File) -> Result<Val> {
    let reference = raw_reference(metadata)?;
    let stat = file.metadata()?;
    if stat.len() != reference.size { return Err(Error::Value("container size mismatch".into())); }
    let encoded = crate::db::dumps_utf8(metadata)?;
    let key = CacheKey {
        directory: directory.to_path_buf(), metadata_hash: digest(encoded.as_bytes()),
        device: stat.dev(), inode: stat.ino(), length: stat.len(),
        mtime: (stat.mtime(), stat.mtime_nsec()), ctime: (stat.ctime(), stat.ctime_nsec()),
    };
    {
        let mut cache = SNAPSHOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = cache.iter().position(|entry| entry.key == key) {
            let entry = cache.remove(index).unwrap();
            let value = entry.value.clone();
            cache.push_back(entry);
            return Ok(value);
        }
    }
    let value = load_snapshot_from_file(metadata, file.try_clone()?)?;
    let after = file.metadata()?;
    // GC may unlink an open immutable container after the lock is released;
    // unlink changes ctime, while the descriptor and verified bytes remain
    // valid. A content change still changes size/mtime or fails the digest.
    let unlinked = stat.nlink() > 0 && after.nlink() == 0;
    if stat.dev() != after.dev() || stat.ino() != after.ino() || stat.len() != after.len()
        || stat.mtime() != after.mtime() || stat.mtime_nsec() != after.mtime_nsec()
        || (!unlinked && (stat.ctime() != after.ctime() || stat.ctime_nsec() != after.ctime_nsec())) {
        return Err(Error::Value("container changed while reading".into()));
    }
    let cost = snapshot_heap_cost(&value).saturating_add(std::mem::size_of::<CacheEntry>())
        .saturating_add(key.directory.as_os_str().len()).saturating_add(key.metadata_hash.capacity());
    if cost <= CACHE_BUDGET {
        let mut cache = SNAPSHOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache_insert(&mut cache, key, &value, cost, CACHE_BUDGET);
    }
    Ok(value)
}

fn cache_insert(cache: &mut VecDeque<CacheEntry>, key: CacheKey, value: &Val, cost: usize, budget: usize) {
    if cost > budget { return; }
    cache.retain(|old| old.key != key);
    while cache.iter().fold(cost, |n, e| n.saturating_add(e.cost)) > budget {
        cache.pop_front();
    }
    cache.push_back(CacheEntry { key, value: value.clone(), cost });
}

/// Conservative retained allocation cost, without serializing a second copy
/// of a potentially very large reconstructed snapshot.
pub fn snapshot_heap_cost(value: &Val) -> usize {
    use std::mem::size_of;
    match value {
        Val::Str(s) | Val::BigInt(s) => s.capacity(),
        Val::Bytes(b) => b.capacity(),
        Val::List(v) => v.iter().fold(v.capacity().saturating_mul(size_of::<Val>()), |n, value| n.saturating_add(snapshot_heap_cost(value))),
        Val::Dict(v) => v.iter().fold(v.capacity().saturating_mul(size_of::<(String, Val)>()), |n, (key, value)| n.saturating_add(key.capacity()).saturating_add(snapshot_heap_cost(value))),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_cache_accounts_allocations_and_evicts_by_bytes() {
        fn entry(id: &str, size: usize) -> CacheEntry {
            let value = Val::Dict(vec![("payload".into(), Val::Str("x".repeat(size)))]);
            let cost = snapshot_heap_cost(&value) + std::mem::size_of::<CacheEntry>();
            CacheEntry { key: CacheKey { directory: PathBuf::from("/synthetic"), metadata_hash: id.into(), device: 1, inode: 1, length: 1, mtime: (1,0), ctime: (1,0) }, value, cost }
        }
        let mut cache = VecDeque::new();
        let a = entry("a", 2000); let b = entry("b", 2000);
        let budget = a.cost + b.cost - 1;
        cache_insert(&mut cache, a.key, &a.value, a.cost, budget);
        cache_insert(&mut cache, b.key, &b.value, b.cost, budget);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].key.metadata_hash, "b");
        let oversized = entry("oversized", budget + 1);
        cache_insert(&mut cache, oversized.key, &oversized.value, oversized.cost, budget);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].key.metadata_hash, "b");
    }
}
