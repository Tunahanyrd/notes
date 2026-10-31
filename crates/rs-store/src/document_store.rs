//! Durable content-addressed document files, independent of the notes schema.
//!
//! All processes using a store must use `with_write` around *both* publication
//! and the transaction which makes a reference visible (or removes one).
//! `doctor` and `collect` acquire the same lock before enumerating references.
//! A caller that bypasses this protocol invalidates the collection guarantee.
use crate::db::{Error, Result};
use crate::storage::private_directory;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

// Streaming zstd decoder window is capped at 8 MiB. The returned decoded
// buffer is separately capped at 256 MiB; neither limit depends on the frame.
const MAX_WINDOW_LOG: u32 = 23;
const MAX_SOURCE_SIZE: u64 = 256 * 1024 * 1024;
const MAX_STORED_SIZE: u64 = 260 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentRef {
    pub source_sha256: String,
    pub stored_sha256: String,
    pub source_size: u64,
    pub stored_size: u64,
}

/// File identity used only to invalidate a previously verified cache entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentFingerprint {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
}

/// Reference to an immutable, uncompressed ZIP container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawDocumentRef {
    pub sha256: String,
    pub size: u64,
}

pub struct DocumentStore {
    root: PathBuf,
    raw_root: PathBuf,
}

/// Counts unique identities; no document contents or paths are exposed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DoctorReport {
    pub referenced: usize,
    pub missing: usize,
    pub corrupt: usize,
    pub unreferenced: usize,
    pub temporary: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CollectionReport {
    pub removed: usize,
    pub temporary_removed: usize,
}

/// A publication lock held through the caller's reference transaction.
pub struct DocumentWriter<'a> {
    store: &'a DocumentStore,
}

impl DocumentWriter<'_> {
    pub fn put(&self, bytes: &[u8]) -> Result<DocumentRef> {
        self.put_reader(bytes)
    }

    pub fn put_reader(&self, source: impl Read) -> Result<DocumentRef> {
        self.store.put_reader_locked(source)
    }

    pub fn read(&self, reference: &DocumentRef) -> Result<Vec<u8>> {
        self.store.read(reference)
    }

    pub fn fingerprint(&self, reference: &DocumentRef) -> Result<DocumentFingerprint> {
        self.store.fingerprint(reference)
    }

    /// Publish a temporary container while the caller holds the reference lock.
    pub fn put_raw_file(&self, temp: &Path) -> Result<RawDocumentRef> {
        self.store.put_raw_file_locked(temp)
    }

    pub fn put_raw_bytes(&self, bytes: &[u8]) -> Result<RawDocumentRef> {
        let (path, mut file) = self.store.create_raw_temp()?;
        let temp = TempFile(path);
        file.write_all(bytes)?;
        drop(file);
        self.put_raw_file(&temp.0)
    }

    /// Build directly in a private temporary file and publish it under the
    /// same reference lock. The temporary file is removed on every error.
    pub fn put_raw_built<T>(&self, build: impl FnOnce(&mut File) -> Result<T>) -> Result<(RawDocumentRef, T)> {
        let (path, mut file) = self.store.create_raw_temp()?;
        let temp = TempFile(path);
        let result = build(&mut file)?;
        drop(file);
        let reference = self.put_raw_file(&temp.0)?;
        Ok((reference, result))
    }

    pub fn open_raw(&self, reference: &RawDocumentRef) -> Result<File> {
        self.store.open_raw(reference)
    }
}

struct HashWriter<W> {
    inner: W,
    hash: Sha256,
    size: u64,
}

struct HashReader<R> {
    inner: R,
    hash: Sha256,
    size: u64,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(bytes)?;
        self.hash.update(&bytes[..n]);
        self.size += n as u64;
        Ok(n)
    }
}
impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.hash.update(&bytes[..n]);
        self.size += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl DocumentStore {
    pub fn open(directory: &Path) -> Result<Self> {
        check_private_dir(directory)?;
        let root = directory.join("documents");
        private_directory(&root)?;
        File::open(directory)?.sync_all()?;
        let raw_root = directory.join("sdocx");
        private_directory(&raw_root)?;
        File::open(directory)?.sync_all()?;
        Ok(Self { root, raw_root })
    }

    fn path(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Value("invalid document SHA-256".into()));
        }
        Ok(self.root.join(&hash[..2]).join(&hash[2..4]).join(hash))
    }

    fn publication_lock(&self) -> Result<File> {
        check_private_dir(&self.root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join(".publish.lock"))?;
        let metadata = lock.metadata()?;
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(Error::Value(
                "document lock must be a private regular file owned by the user".into(),
            ));
        }
        // SAFETY: flock operates on this valid owned file descriptor; dropping
        // the File releases the advisory lock, including on error or panic.
        loop {
            if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        Ok(lock)
    }

    /// Hold the process-shared lock while publishing and committing references.
    /// Never nest this call, `doctor`, or `collect` for the same store.
    pub fn with_write<T>(&self, f: impl FnOnce(&DocumentWriter<'_>) -> Result<T>) -> Result<T> {
        let _lock = self.publication_lock()?;
        f(&DocumentWriter { store: self })
    }

    /// Hash source and compressed bytes while streaming to a private temp file.
    /// The file and each newly published directory entry are synced before return.
    fn put_reader_locked(&self, mut source: impl Read) -> Result<DocumentRef> {
        let temp = TempFile(self.root.join(format!(".{}.tmp", uuid::Uuid::new_v4())));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp.0)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let writer = HashWriter {
            inner: file,
            hash: Sha256::new(),
            size: 0,
        };
        let mut encoder = zstd::stream::write::Encoder::new(writer, 3)?;
        encoder.window_log(MAX_WINDOW_LOG)?;
        let mut source_hash = Sha256::new();
        let mut source_size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = source.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            source_size = source_size
                .checked_add(n as u64)
                .ok_or_else(|| Error::Value("document too large".into()))?;
            if source_size > MAX_SOURCE_SIZE {
                return Err(Error::Value("document exceeds decoded size limit".into()));
            }
            source_hash.update(&buffer[..n]);
            encoder.write_all(&buffer[..n])?;
        }
        let writer = encoder.finish()?;
        if writer.size > MAX_STORED_SIZE {
            return Err(Error::Value("document exceeds stored size limit".into()));
        }
        writer.inner.sync_all()?;
        let reference = DocumentRef {
            source_sha256: format!("{:x}", source_hash.finalize()),
            stored_sha256: format!("{:x}", writer.hash.finalize()),
            source_size,
            stored_size: writer.size,
        };
        drop(writer.inner);
        let path = self.path(&reference.source_sha256)?;
        let first = path.parent().unwrap().parent().unwrap();
        let second = path.parent().unwrap();
        private_directory(first)?;
        File::open(&self.root)?.sync_all()?;
        private_directory(second)?;
        File::open(first)?.sync_all()?;
        if path.exists() {
            // Existing compression may differ after a compressor upgrade.
            // Preserve immutable files already referenced by SQLite.
            let (stored_sha256, stored_size) = hash_file(&path)?;
            let existing = DocumentRef {
                stored_sha256,
                stored_size,
                ..reference
            };
            self.read(&existing)?;
            File::open(second)?.sync_all()?;
            return Ok(existing);
        }
        fs::rename(&temp.0, &path)?;
        File::open(second)?.sync_all()?;
        File::open(&self.root)?.sync_all()?;
        Ok(reference)
    }

    fn raw_path(&self, hash: &str) -> Result<PathBuf> {
        if !is_hex(hash, 64) {
            return Err(Error::Value("invalid container SHA-256".into()));
        }
        Ok(self
            .raw_root
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(format!("{hash}.sdocx")))
    }

    /// Create a private temporary file in the container filesystem. The caller
    /// owns its cleanup until it has successfully been published.
    pub fn create_raw_temp(&self) -> Result<(PathBuf, File)> {
        check_private_dir(&self.raw_root)?;
        let path = self.raw_root.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok((path, file))
    }

    fn put_raw_file_locked(&self, temp: &Path) -> Result<RawDocumentRef> {
        check_private_dir(&self.raw_root)?;
        if temp.parent() != Some(self.raw_root.as_path())
            || !temp
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_temp_name)
        {
            return Err(Error::Value("invalid container temporary path".into()));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(temp)?;
        let metadata = file.metadata()?;
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
            || metadata.len() > MAX_STORED_SIZE
        {
            return Err(Error::Value(
                "invalid container temporary file or size".into(),
            ));
        }
        file.sync_all()?;
        let (sha256, size) = hash_reader((&mut file).take(MAX_STORED_SIZE + 1))?;
        if size != metadata.len() || size > MAX_STORED_SIZE {
            return Err(Error::Value("container size verification failed".into()));
        }
        let reference = RawDocumentRef { sha256, size };
        let path = self.raw_path(&reference.sha256)?;
        let first = path.parent().unwrap().parent().unwrap();
        let second = path.parent().unwrap();
        private_directory(first)?;
        File::open(&self.raw_root)?.sync_all()?;
        private_directory(second)?;
        File::open(first)?.sync_all()?;
        if fs::symlink_metadata(&path).is_ok() {
            let existing = self.open_raw(&reference)?;
            let (hash, size) = hash_reader(existing)?;
            if hash != reference.sha256 || size != reference.size {
                return Err(Error::Value(
                    "existing container SHA-256 verification failed".into(),
                ));
            }
            fs::remove_file(temp)?;
            File::open(&self.raw_root)?.sync_all()?;
            File::open(second)?.sync_all()?;
            return Ok(reference);
        }
        drop(file);
        fs::rename(temp, &path)?;
        File::open(second)?.sync_all()?;
        File::open(&self.raw_root)?.sync_all()?;
        Ok(reference)
    }

    /// Open for random access, verifying identity and size but not hashing the
    /// whole file. Readers must verify individual entry hashes themselves.
    pub fn open_raw(&self, reference: &RawDocumentRef) -> Result<File> {
        #[cfg(feature = "read-tracing")]
        crate::read_trace::container(&self.raw_root, &reference.sha256);
        check_private_dir(&self.raw_root)?;
        let path = self.raw_path(&reference.sha256)?;
        check_private_dir(path.parent().unwrap().parent().unwrap())?;
        check_private_dir(path.parent().unwrap())?;
        if reference.size > MAX_STORED_SIZE {
            return Err(Error::Value("container exceeds stored size limit".into()));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != reference.size {
            return Err(Error::Value(
                "container stored size verification failed".into(),
            ));
        }
        Ok(file)
    }

    /// Verify full container hashes and count orphans while holding the same
    /// publication lock used for reference transactions.
    pub fn doctor_raw(
        &self,
        references: impl FnOnce() -> Result<Vec<RawDocumentRef>>,
    ) -> Result<DoctorReport> {
        let _lock = self.publication_lock()?;
        let mut unique: HashMap<String, RawDocumentRef> = HashMap::new();
        let mut conflicting = HashSet::new();
        for reference in references()? {
            if let Some(previous) = unique.get(&reference.sha256) {
                if previous != &reference {
                    conflicting.insert(reference.sha256.clone());
                }
            } else {
                unique.insert(reference.sha256.clone(), reference);
            }
        }
        let mut report = DoctorReport {
            referenced: unique.len(),
            ..DoctorReport::default()
        };
        for (hash, reference) in &unique {
            if conflicting.contains(hash) {
                report.corrupt += 1;
                continue;
            }
            let verified = self.open_raw(reference).and_then(|file| hash_reader(file));
            match verified {
                Ok((actual, size)) if actual == *hash && size == reference.size => {}
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => report.missing += 1,
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::PermissionDenied => {
                    return Err(e.into())
                }
                _ => report.corrupt += 1,
            }
        }
        let (files, temporary) = scan_files(&self.raw_root, ".sdocx")?;
        report.unreferenced = files
            .iter()
            .filter(|(hash, _)| !unique.contains_key(hash))
            .count();
        report.temporary = temporary.len();
        Ok(report)
    }

    /// Remove only unreferenced canonical containers older than grace.
    pub fn collect_raw(
        &self,
        grace: Duration,
        references: impl FnOnce() -> Result<Vec<RawDocumentRef>>,
    ) -> Result<CollectionReport> {
        let _lock = self.publication_lock()?;
        let mut marked = HashSet::new();
        for reference in references()? {
            self.raw_path(&reference.sha256)?;
            marked.insert(reference.sha256);
        }
        let (files, temporary) = scan_files(&self.raw_root, ".sdocx")?;
        let now = SystemTime::now();
        let mut report = CollectionReport::default();
        for (hash, path) in files {
            if !marked.contains(&hash) && old_enough(&path, now, grace)? {
                fs::remove_file(&path)?;
                File::open(path.parent().unwrap())?.sync_all()?;
                report.removed += 1;
            }
        }
        for path in temporary {
            if old_enough(&path, now, grace)? {
                fs::remove_file(&path)?;
                File::open(&self.raw_root)?.sync_all()?;
                report.temporary_removed += 1;
            }
        }
        Ok(report)
    }

    fn fingerprint(&self, reference: &DocumentRef) -> Result<DocumentFingerprint> {
        check_private_dir(&self.root)?;
        let path = self.path(&reference.source_sha256)?;
        check_private_dir(path.parent().unwrap().parent().unwrap())?;
        check_private_dir(path.parent().unwrap())?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let m = file.metadata()?;
        if !m.is_file() || m.len() != reference.stored_size {
            return Err(Error::Value(
                "document stored size verification failed".into(),
            ));
        }
        Ok(DocumentFingerprint {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
        })
    }

    /// Verify the stored bytes before decompression and the source bytes after.
    pub fn read(&self, reference: &DocumentRef) -> Result<Vec<u8>> {
        check_private_dir(&self.root)?;
        let path = self.path(&reference.source_sha256)?;
        check_private_dir(path.parent().unwrap().parent().unwrap())?;
        check_private_dir(path.parent().unwrap())?;
        if reference.source_size > MAX_SOURCE_SIZE {
            return Err(Error::Value("document exceeds decoded size limit".into()));
        }
        if reference.stored_size > MAX_STORED_SIZE {
            return Err(Error::Value("document exceeds stored size limit".into()));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != reference.stored_size {
            return Err(Error::Value(
                "document stored size verification failed".into(),
            ));
        }
        let (stored_hash, stored_size) = hash_reader((&mut file).take(reference.stored_size + 1))?;
        if stored_size != reference.stored_size || stored_hash != reference.stored_sha256 {
            return Err(Error::Value(
                "document stored SHA-256 verification failed".into(),
            ));
        }
        file.seek(SeekFrom::Start(0))?;
        // Hash again during decoding, so a file modified between the initial
        // verification and decompression cannot silently change stored bytes.
        let input = HashReader {
            inner: file.take(reference.stored_size + 1),
            hash: Sha256::new(),
            size: 0,
        };
        let mut decoder = zstd::stream::read::Decoder::new(input)?;
        decoder.window_log_max(MAX_WINDOW_LOG)?;
        let mut bytes = Vec::new();
        let mut limited = decoder.take(reference.source_size + 1);
        limited.read_to_end(&mut bytes)?;
        let input = limited.into_inner().finish().into_inner();
        if input.size != reference.stored_size
            || format!("{:x}", input.hash.finalize()) != reference.stored_sha256
        {
            return Err(Error::Value(
                "document stored SHA-256 verification failed".into(),
            ));
        }
        if bytes.len() as u64 != reference.source_size
            || format!("{:x}", Sha256::digest(&bytes)) != reference.source_sha256
        {
            return Err(Error::Value(
                "document source SHA-256 or size verification failed".into(),
            ));
        }
        Ok(bytes)
    }

    /// Enumerate references only after acquiring the lock, then verify each
    /// unique identity and count valid orphan files and unfinished temp files.
    pub fn doctor(
        &self,
        references: impl FnOnce() -> Result<Vec<DocumentRef>>,
    ) -> Result<DoctorReport> {
        let _lock = self.publication_lock()?;
        let mut unique: HashMap<String, DocumentRef> = HashMap::new();
        let mut conflicting = HashSet::new();
        for reference in references()? {
            if let Some(previous) = unique.get(&reference.source_sha256) {
                if previous != &reference {
                    conflicting.insert(reference.source_sha256.clone());
                }
            } else {
                unique.insert(reference.source_sha256.clone(), reference);
            }
        }
        let mut report = DoctorReport {
            referenced: unique.len(),
            ..DoctorReport::default()
        };
        for (hash, reference) in &unique {
            if conflicting.contains(hash) {
                report.corrupt += 1;
                continue;
            }
            match self.read(reference) {
                Ok(_) => {}
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => report.missing += 1,
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::PermissionDenied => {
                    return Err(e.into())
                }
                Err(_) => report.corrupt += 1,
            }
        }
        let (files, temporary) = self.files()?;
        report.unreferenced = files
            .iter()
            .filter(|(hash, _)| !unique.contains_key(hash))
            .count();
        report.temporary = temporary.len();
        Ok(report)
    }

    /// Mark under the lock, then remove only canonical files older than grace.
    /// An enumeration error or malformed reference aborts before any deletion.
    pub fn collect(
        &self,
        grace: Duration,
        references: impl FnOnce() -> Result<Vec<DocumentRef>>,
    ) -> Result<CollectionReport> {
        let _lock = self.publication_lock()?;
        let mut marked = HashSet::new();
        for reference in references()? {
            self.path(&reference.source_sha256)?;
            marked.insert(reference.source_sha256);
        }
        let (files, temporary) = self.files()?;
        let now = SystemTime::now();
        let mut report = CollectionReport::default();
        for (hash, path) in files {
            if !marked.contains(&hash) && old_enough(&path, now, grace)? {
                fs::remove_file(&path)?;
                File::open(path.parent().unwrap())?.sync_all()?;
                report.removed += 1;
            }
        }
        for path in temporary {
            if old_enough(&path, now, grace)? {
                fs::remove_file(&path)?;
                File::open(&self.root)?.sync_all()?;
                report.temporary_removed += 1;
            }
        }
        Ok(report)
    }

    // Do not follow symlinks, visit unrelated files, or remove noncanonical names.
    fn files(&self) -> Result<(Vec<(String, PathBuf)>, Vec<PathBuf>)> {
        scan_files(&self.root, "")
    }
}

fn scan_files(root: &Path, suffix: &str) -> Result<(Vec<(String, PathBuf)>, Vec<PathBuf>)> {
    check_private_dir(root)?;
    let mut files = Vec::new();
    let mut temporary = Vec::new();
    for first in fs::read_dir(root)? {
        let first = first?;
        let name = first.file_name().to_string_lossy().into_owned();
        let kind = first.file_type()?;
        if kind.is_file() && is_temp_name(&name) {
            temporary.push(first.path());
            continue;
        }
        if !kind.is_dir() || !is_hex(&name, 2) {
            continue;
        }
        check_private_dir(&first.path())?;
        for second in fs::read_dir(first.path())? {
            let second = second?;
            let sub = second.file_name().to_string_lossy().into_owned();
            if !second.file_type()?.is_dir() || !is_hex(&sub, 2) {
                continue;
            }
            check_private_dir(&second.path())?;
            for file in fs::read_dir(second.path())? {
                let file = file?;
                let filename = file.file_name().to_string_lossy().into_owned();
                let Some(hash) = filename.strip_suffix(suffix) else {
                    continue;
                };
                let hash = hash.to_owned();
                if file.file_type()?.is_file()
                    && is_hex(&hash, 64)
                    && hash.starts_with(&name)
                    && hash[2..4] == sub
                {
                    files.push((hash, file.path()));
                }
            }
        }
    }
    Ok((files, temporary))
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_temp_name(name: &str) -> bool {
    let Some(id) = name.strip_prefix('.').and_then(|s| s.strip_suffix(".tmp")) else {
        return false;
    };
    uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id)
}

fn check_private_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(Error::Value(
            "document directory must be private and owned by the user".into(),
        ));
    }
    Ok(())
}

fn old_enough(path: &Path, now: SystemTime, grace: Duration) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    Ok(metadata.file_type().is_file()
        && now
            .duration_since(metadata.modified()?)
            .is_ok_and(|age| age >= grace))
}

fn hash_reader(mut reader: impl Read) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut bytes = [0; 64 * 1024];
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
        size += n as u64;
    }
    Ok((format!("{:x}", hash.finalize()), size))
}
fn hash_file(path: &Path) -> Result<(String, u64)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_STORED_SIZE {
        return Err(Error::Value("invalid stored document file or size".into()));
    }
    hash_reader(file.take(MAX_STORED_SIZE + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("notes-document-store-{}", uuid::Uuid::new_v4()));
            private_directory(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn put(store: &DocumentStore, bytes: &[u8]) -> DocumentRef {
        store.with_write(|writer| writer.put(bytes)).unwrap()
    }

    fn put_raw(store: &DocumentStore, bytes: &[u8]) -> RawDocumentRef {
        let (path, mut file) = store.create_raw_temp().unwrap();
        file.write_all(bytes).unwrap();
        drop(file);
        store
            .with_write(|writer| writer.put_raw_file(&path))
            .unwrap()
    }

    #[test]
    fn streamed_raw_temp_is_removed_when_builder_fails() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        assert!(store.with_write(|writer| writer.put_raw_built(|file| {
            file.write_all(b"partial")?;
            Err::<(), _>(Error::Value("injected failure".into()))
        })).is_err());
        assert!(fs::read_dir(temp.0.join("sdocx")).unwrap().next().is_none());
    }

    #[test]
    fn raw_files_are_private_deduplicated_and_random_accessible() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let reference = put_raw(&store, b"synthetic container");
        assert_eq!(reference, put_raw(&store, b"synthetic container"));
        let mut file = store.open_raw(&reference).unwrap();
        file.seek(SeekFrom::Start(10)).unwrap();
        let mut rest = Vec::new();
        file.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"container");
        let path = store.raw_path(&reference.sha256).unwrap();
        assert_eq!(
            path.file_name().unwrap(),
            format!("{}.sdocx", reference.sha256).as_str()
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(store.doctor_raw(|| Ok(vec![reference])).unwrap().corrupt, 0);
    }

    #[test]
    fn raw_doctor_detects_corruption_and_collection_respects_references() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let good = put_raw(&store, b"good");
        let corrupt = put_raw(&store, b"bad!");
        let missing = put_raw(&store, b"missing");
        let orphan = put_raw(&store, b"orphan");
        fs::write(store.raw_path(&corrupt.sha256).unwrap(), b"evil").unwrap();
        fs::remove_file(store.raw_path(&missing.sha256).unwrap()).unwrap();
        let report = store
            .doctor_raw(|| Ok(vec![good.clone(), corrupt, missing]))
            .unwrap();
        assert_eq!(
            (
                report.referenced,
                report.corrupt,
                report.missing,
                report.unreferenced
            ),
            (3, 1, 1, 1)
        );
        let mut invalid = good.clone();
        invalid.sha256 = "../invalid".into();
        assert!(store
            .collect_raw(Duration::ZERO, || Ok(vec![invalid]))
            .is_err());
        assert!(store.raw_path(&orphan.sha256).unwrap().exists());
        assert_eq!(
            store
                .collect_raw(Duration::ZERO, || Ok(vec![good.clone()]))
                .unwrap()
                .removed,
            2
        );
        assert!(store.open_raw(&good).is_ok());
        assert!(!store.raw_path(&orphan.sha256).unwrap().exists());
    }

    #[test]
    fn raw_collection_waits_for_reference_commit() {
        use std::sync::{mpsc, Arc, Mutex};
        let temp = Temp::new();
        let writer_store = DocumentStore::open(&temp.0).unwrap();
        let collector = DocumentStore::open(&temp.0).unwrap();
        let references = Arc::new(Mutex::new(Vec::new()));
        let writer_refs = references.clone();
        let (published_tx, published_rx) = mpsc::channel();
        let (commit_tx, commit_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            writer_store
                .with_write(|guard| {
                    let reference = guard.put_raw_bytes(b"in flight container")?;
                    published_tx.send(()).unwrap();
                    commit_rx.recv().unwrap();
                    writer_refs.lock().unwrap().push(reference);
                    Ok(())
                })
                .unwrap();
        });
        published_rx.recv().unwrap();
        let (mark_tx, mark_rx) = mpsc::channel();
        let collector_refs = references.clone();
        let collector = std::thread::spawn(move || {
            collector
                .collect_raw(Duration::ZERO, || {
                    mark_tx.send(()).unwrap();
                    Ok(collector_refs.lock().unwrap().clone())
                })
                .unwrap()
        });
        assert!(mark_rx.recv_timeout(Duration::from_millis(150)).is_err());
        commit_tx.send(()).unwrap();
        writer.join().unwrap();
        assert_eq!(collector.join().unwrap().removed, 0);
        mark_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn raw_open_rejects_symlinks_and_size_mismatches() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let reference = put_raw(&store, b"safe");
        let mut wrong = reference.clone();
        wrong.size += 1;
        assert!(store.open_raw(&wrong).is_err());
        let path = store.raw_path(&reference.sha256).unwrap();
        let moved = path.with_extension("saved");
        fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(store.open_raw(&reference).is_err());
        assert_eq!(store.doctor_raw(|| Ok(vec![reference])).unwrap().corrupt, 1);
    }

    #[test]
    fn compressed_files_are_private_verified_and_deduplicated() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let bytes = vec![42; 1024 * 1024];
        let reference = store
            .with_write(|writer| writer.put_reader(&bytes[..]))
            .unwrap();
        assert_eq!(put(&store, &bytes), reference);
        assert_eq!(store.read(&reference).unwrap(), bytes);
        assert!(reference.stored_size < reference.source_size);
        assert_ne!(reference.source_sha256, reference.stored_sha256);
        let path = store.path(&reference.source_sha256).unwrap();
        assert_eq!(
            path.parent().unwrap().file_name().unwrap(),
            &reference.source_sha256[2..4]
        );
        assert_eq!(
            path.parent()
                .unwrap()
                .parent()
                .unwrap()
                .file_name()
                .unwrap(),
            &reference.source_sha256[..2]
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(store.root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn detects_both_hash_failures_and_invalid_references() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let reference = put(&store, b"synthetic document");
        let mut wrong_source = reference.clone();
        wrong_source.source_size += 1;
        assert!(store.read(&wrong_source).is_err());
        let path = store.path(&reference.source_sha256).unwrap();
        fs::write(path, vec![0; reference.stored_size as usize]).unwrap();
        assert!(store
            .read(&reference)
            .unwrap_err()
            .to_string()
            .contains("verification"));
        assert!(store
            .with_write(|writer| writer.put(b"synthetic document"))
            .is_err());
        let mut invalid = reference;
        invalid.source_sha256 = "../invalid".into();
        assert!(store.read(&invalid).is_err());
    }

    #[test]
    fn rejects_different_decoded_bytes_with_valid_stored_hash() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let mut reference = put(&store, b"original");
        let other = zstd::stream::encode_all(&b"replaced"[..], 3).unwrap();
        fs::write(store.path(&reference.source_sha256).unwrap(), &other).unwrap();
        reference.stored_size = other.len() as u64;
        reference.stored_sha256 = format!("{:x}", Sha256::digest(&other));
        assert!(store
            .read(&reference)
            .unwrap_err()
            .to_string()
            .contains("source SHA-256"));
    }

    #[test]
    fn hash_matches_sha256_known_vector() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        assert_eq!(
            put(&store, b"abc").source_sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rejects_frames_exceeding_decoder_window() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let source = vec![42; 17 * 1024 * 1024];
        let mut reference = put(&store, &source);
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        encoder.window_log(24).unwrap();
        encoder.write_all(&source).unwrap();
        let oversized = encoder.finish().unwrap();
        fs::write(store.path(&reference.source_sha256).unwrap(), &oversized).unwrap();
        reference.stored_size = oversized.len() as u64;
        reference.stored_sha256 = format!("{:x}", Sha256::digest(&oversized));
        let error = store.read(&reference).unwrap_err().to_string();
        assert!(error.contains("memory"), "unexpected error: {error}");
    }

    fn age(path: &Path, seconds: u64) {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(
            fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(seconds)),
        )
        .unwrap();
    }

    #[test]
    fn doctor_counts_missing_corrupt_duplicates_orphans_and_temporaries() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let good = put(&store, b"good");
        let missing = put(&store, b"missing");
        let corrupt = put(&store, b"corrupt");
        let orphan = put(&store, b"orphan");
        fs::remove_file(store.path(&missing.source_sha256).unwrap()).unwrap();
        fs::write(store.path(&corrupt.source_sha256).unwrap(), b"broken").unwrap();
        let temporary = store.root.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temporary, b"interrupted").unwrap();
        assert!(store.path(&orphan.source_sha256).unwrap().exists());
        let report = store
            .doctor(|| Ok(vec![good.clone(), good.clone(), missing, corrupt]))
            .unwrap();
        assert_eq!(
            report,
            DoctorReport {
                referenced: 3,
                missing: 1,
                corrupt: 1,
                unreferenced: 1,
                temporary: 1
            }
        );
        let mut inconsistent = good.clone();
        inconsistent.stored_size += 1;
        let report = store.doctor(|| Ok(vec![good, inconsistent])).unwrap();
        assert_eq!(report.referenced, 1);
        assert_eq!(report.corrupt, 1);
    }

    #[test]
    fn collection_respects_references_grace_and_invalid_mark_failures() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let kept = put(&store, b"kept");
        let old = put(&store, b"old");
        let young = put(&store, b"young");
        let old_path = store.path(&old.source_sha256).unwrap();
        age(&old_path, 120);
        let temporary = store.root.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temporary, b"incomplete").unwrap();
        age(&temporary, 120);
        let unrelated = store.root.join("do-not-remove.tmp");
        fs::write(&unrelated, b"unrelated").unwrap();

        let mut invalid = kept.clone();
        invalid.source_sha256 = "invalid".into();
        assert!(store.collect(Duration::ZERO, || Ok(vec![invalid])).is_err());
        assert!(old_path.exists());
        assert!(store
            .collect(Duration::ZERO, || Err(Error::Value(
                "reference query failed".into()
            )))
            .is_err());
        assert!(old_path.exists());

        let report = store
            .collect(Duration::from_secs(60), || Ok(vec![kept.clone()]))
            .unwrap();
        assert_eq!(
            report,
            CollectionReport {
                removed: 1,
                temporary_removed: 1
            }
        );
        assert!(store.read(&kept).is_ok());
        assert!(store.read(&young).is_ok());
        assert!(!old_path.exists());
        assert!(!temporary.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn collection_waits_until_publication_and_reference_commit() {
        use std::sync::{mpsc, Arc, Mutex};
        let temp = Temp::new();
        let writer_store = DocumentStore::open(&temp.0).unwrap();
        let collector = DocumentStore::open(&temp.0).unwrap();
        let refs = Arc::new(Mutex::new(Vec::new()));
        let writer_refs = refs.clone();
        let (published_tx, published_rx) = mpsc::channel();
        let (commit_tx, commit_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            writer_store
                .with_write(|guard| {
                    let reference = guard.put(b"in flight")?;
                    published_tx.send(()).unwrap();
                    commit_rx.recv().unwrap();
                    writer_refs.lock().unwrap().push(reference);
                    Ok(())
                })
                .unwrap();
        });
        published_rx.recv().unwrap();
        let (mark_tx, mark_rx) = mpsc::channel();
        let collector_refs = refs.clone();
        let collect = std::thread::spawn(move || {
            collector
                .collect(Duration::ZERO, || {
                    mark_tx.send(()).unwrap();
                    Ok(collector_refs.lock().unwrap().clone())
                })
                .unwrap()
        });
        assert!(mark_rx.recv_timeout(Duration::from_millis(150)).is_err());
        commit_tx.send(()).unwrap();
        writer.join().unwrap();
        assert_eq!(collect.join().unwrap().removed, 0);
        mark_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            DocumentStore::open(&temp.0)
                .unwrap()
                .read(&refs.lock().unwrap()[0])
                .unwrap(),
            b"in flight"
        );
    }

    #[test]
    fn failing_input_cleans_up_temp_and_never_publishes() {
        struct Failing(bool);
        impl Read for Failing {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("synthetic read failure"));
                }
                self.0 = true;
                buf[..4].copy_from_slice(b"data");
                Ok(4)
            }
        }
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        assert!(store
            .with_write(|writer| writer.put_reader(Failing(false)))
            .is_err());
        assert_eq!(
            store.doctor(|| Ok(vec![])).unwrap(),
            DoctorReport::default()
        );
    }

    #[test]
    fn failed_reference_transaction_leaves_collectable_orphan() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let result: Result<()> = store.with_write(|writer| {
            writer.put(b"uncommitted")?;
            Err(Error::Value("transaction failed".into()))
        });
        assert!(result.is_err());
        assert_eq!(store.doctor(|| Ok(vec![])).unwrap().unreferenced, 1);
        assert_eq!(
            store
                .collect(Duration::ZERO, || Ok(vec![]))
                .unwrap()
                .removed,
            1
        );
    }

    #[test]
    fn decoded_output_cannot_exceed_recorded_size() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let mut reference = put(&store, &vec![42; 100_000]);
        reference.source_size = 16;
        assert!(store.read(&reference).is_err());
    }

    #[test]
    fn subprocess_lock_helper() {
        let Some(directory) = std::env::var_os("NOTES_DOCSTORE_TEST_CHILD") else {
            return;
        };
        let directory = PathBuf::from(directory);
        let store = DocumentStore::open(&directory).unwrap();
        store
            .with_write(|writer| {
                if std::env::var_os("NOTES_DOCSTORE_TEST_PUBLISH").is_some() {
                    writer.put(b"crash before reference commit")?;
                }
                fs::write(directory.join("ready"), b"ready")?;
                let until = std::time::Instant::now() + Duration::from_secs(15);
                while !directory.join("release").exists() {
                    if std::time::Instant::now() >= until {
                        return Err(Error::Value("test lock timed out".into()));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn collector_observes_locks_held_by_another_process() {
        use std::sync::mpsc;
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "document_store::tests::subprocess_lock_helper"])
            .env("NOTES_DOCSTORE_TEST_CHILD", &temp.0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !temp.0.join("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !temp.0.join("ready").exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("subprocess did not acquire the lock");
        }
        let (marked_tx, marked_rx) = mpsc::channel();
        let collector = std::thread::spawn(move || {
            store
                .collect(Duration::ZERO, || {
                    marked_tx.send(()).unwrap();
                    Ok(vec![])
                })
                .unwrap()
        });
        let blocked = marked_rx.recv_timeout(Duration::from_millis(150)).is_err();
        fs::write(temp.0.join("release"), b"release").unwrap();
        let status = child.wait().unwrap();
        let report = collector.join().unwrap();
        assert!(blocked);
        assert!(status.success());
        assert_eq!(report.removed, 0);
        marked_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn crashed_writer_releases_lock_and_leaves_safe_orphan() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "document_store::tests::subprocess_lock_helper"])
            .env("NOTES_DOCSTORE_TEST_CHILD", &temp.0)
            .env("NOTES_DOCSTORE_TEST_PUBLISH", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !temp.0.join("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let was_ready = temp.0.join("ready").exists();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(was_ready, "writer did not reach publication");
        assert_eq!(store.doctor(|| Ok(vec![])).unwrap().unreferenced, 1);
        assert_eq!(
            store
                .collect(Duration::ZERO, || Ok(vec![]))
                .unwrap()
                .removed,
            1
        );
    }

    #[test]
    fn rejects_symlinked_fanout_and_oversized_reference() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let reference = put(&store, b"protected");
        let path = store.path(&reference.source_sha256).unwrap();
        let second = path.parent().unwrap();
        let moved = second.with_extension("moved");
        fs::rename(second, &moved).unwrap();
        symlink(&moved, second).unwrap();
        assert!(store.read(&reference).is_err());
        assert_eq!(
            store
                .doctor(|| Ok(vec![reference.clone()]))
                .unwrap()
                .corrupt,
            1
        );
        fs::remove_file(second).unwrap();
        fs::rename(moved, second).unwrap();
        let mut oversized = reference;
        oversized.source_size = MAX_SOURCE_SIZE + 1;
        assert!(store
            .read(&oversized)
            .unwrap_err()
            .to_string()
            .contains("limit"));
    }

    #[test]
    fn refuses_store_directory_that_is_no_longer_private() {
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        let reference = put(&store, b"private");
        fs::set_permissions(&store.root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(store.read(&reference).is_err());
        assert!(store.with_write(|writer| writer.put(b"other")).is_err());
        assert!(store.collect(Duration::ZERO, || Ok(vec![])).is_err());
        fs::set_permissions(&store.root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(store.read(&reference).unwrap(), b"private");
    }

    #[test]
    fn open_requires_an_existing_private_parent() {
        let temp = Temp::new();
        assert!(DocumentStore::open(&temp.0.join("missing")).is_err());
        let public = temp.0.join("public");
        fs::create_dir(&public).unwrap();
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(DocumentStore::open(&public).is_err());
        assert!(!public.join("documents").exists());
    }

    #[test]
    fn refuses_untrusted_lock_file() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let store = DocumentStore::open(&temp.0).unwrap();
        put(&store, b"create lock");
        let lock = store.root.join(".publish.lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.doctor(|| Ok(vec![])).is_err());
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
        let moved = store.root.join("saved-lock");
        fs::rename(&lock, &moved).unwrap();
        symlink(&moved, &lock).unwrap();
        assert!(store.doctor(|| Ok(vec![])).is_err());
        fs::remove_file(&lock).unwrap();
        fs::rename(&moved, &lock).unwrap();
        assert!(store.doctor(|| Ok(vec![])).is_ok());
    }
}
