//! Shared immutable media, addressed by the Cloud content SHA-256.
//!
//! Files are published under the document-store lock and are never renamed or
//! replaced after publication. Read paths verify both the file and the digest.
use crate::db::{Error, Result};
use crate::document_store::DocumentStore;
use crate::storage::private_directory;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_MEDIA_BYTES: u64 = 128 * 1024 * 1024;

pub struct MediaStore {
    root: PathBuf,
    documents: DocumentStore,
}

fn check_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(Error::Value(
            "media directory must be private and owned by the user".into(),
        ));
    }
    Ok(())
}
fn check_hash(hash: &str) -> Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Value("invalid media SHA-256".into()));
    }
    Ok(())
}
fn digest<R: Read>(reader: &mut R) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| Error::Value("media too large".into()))?;
        if size > MAX_MEDIA_BYTES {
            return Err(Error::Value("media exceeds size limit".into()));
        }
        hash.update(&buf[..n]);
    }
    Ok((format!("{:x}", hash.finalize()), size))
}
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl MediaStore {
    pub fn open(directory: &Path) -> Result<Self> {
        check_directory(directory)?;
        let documents = DocumentStore::open(directory)?;
        let root = directory.join("media");
        private_directory(&root)?;
        File::open(directory)?.sync_all()?;
        Ok(Self { root, documents })
    }
    fn path(&self, hash: &str) -> Result<PathBuf> {
        check_hash(hash)?;
        Ok(self.root.join(&hash[..2]).join(&hash[2..4]).join(hash))
    }
    fn open_file(&self, hash: &str, limit: u64) -> Result<Option<File>> {
        let path = self.path(hash)?;
        check_directory(&self.root)?;
        for parent in [
            path.parent().unwrap().parent().unwrap(),
            path.parent().unwrap(),
        ] {
            match check_directory(parent) {
                Ok(()) => {}
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let meta = file.metadata()?;
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        if !meta.is_file()
            || meta.uid() != uid
            || meta.mode() & 0o077 != 0
            || meta.len() > limit.min(MAX_MEDIA_BYTES)
        {
            return Err(Error::Value("invalid media file or size".into()));
        }
        Ok(Some(file))
    }
    fn open_verified(&self, hash: &str) -> Result<Option<File>> {
        let Some(mut file) = self.open_file(hash, MAX_MEDIA_BYTES)? else {
            return Ok(None);
        };
        let metadata = file.metadata()?;
        let (actual, size) = digest(&mut file)?;
        if actual != hash || size != metadata.len() {
            return Err(Error::Value("media SHA-256 verification failed".into()));
        }
        file.seek(SeekFrom::Start(0))?;
        Ok(Some(file))
    }
    /// Returns a verified canonical path, or None when absent.
    pub fn path_verified(&self, hash: &str) -> Result<Option<PathBuf>> {
        Ok(self
            .open_verified(hash)?
            .map(|_| self.path(hash).expect("validated hash")))
    }
    pub fn read(&self, hash: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        let Some(file) = self.open_file(hash, max_bytes)? else {
            return Ok(None);
        };
        if file.metadata()?.len() > max_bytes {
            return Err(Error::Value("media exceeds consumer size limit".into()));
        }
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes || format!("{:x}", Sha256::digest(&bytes)) != hash {
            return Err(Error::Value("media verification failed".into()));
        }
        Ok(Some(bytes))
    }
    pub fn put(&self, hash: &str, bytes: &[u8]) -> Result<PathBuf> {
        self.put_reader(hash, bytes)
    }
    pub fn put_file(&self, hash: &str, source: &Path) -> Result<PathBuf> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(source)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Value("media source must be a regular file".into()));
        }
        self.put_reader(hash, file)
    }
    fn put_reader(&self, hash: &str, mut source: impl Read) -> Result<PathBuf> {
        let path = self.path(hash)?;
        self.documents.with_write(|_| {
            check_directory(&self.root)?;
            let temp = Temp(self.root.join(format!(".{}.tmp", uuid::Uuid::new_v4())));
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp.0)?;
            let mut hasher = Sha256::new();
            let mut size = 0u64;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = source.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                size = size
                    .checked_add(n as u64)
                    .ok_or_else(|| Error::Value("media too large".into()))?;
                if size > MAX_MEDIA_BYTES {
                    return Err(Error::Value("media exceeds size limit".into()));
                }
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n])?;
            }
            if format!("{:x}", hasher.finalize()) != hash {
                return Err(Error::Value("media SHA-256 mismatch".into()));
            }
            out.sync_all()?;
            let first = path.parent().unwrap().parent().unwrap();
            let second = path.parent().unwrap();
            private_directory(first)?;
            check_directory(first)?;
            File::open(&self.root)?.sync_all()?;
            private_directory(second)?;
            check_directory(second)?;
            File::open(first)?.sync_all()?;
            match fs::hard_link(&temp.0, &path) {
                Ok(()) => {
                    File::open(second)?.sync_all()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.open_verified(hash)?
                        .ok_or_else(|| Error::Value("media disappeared".into()))?;
                }
                Err(e) => return Err(e.into()),
            }
            Ok(path.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("media-store-test-{}", uuid::Uuid::new_v4()));
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

    #[test]
    fn shared_media_deduplicates_and_rejects_invalid_or_corrupt_bytes() {
        let temp = Temp::new();
        let media = MediaStore::open(&temp.0).unwrap();
        let bytes = b"synthetic image bytes";
        let hash = format!("{:x}", Sha256::digest(bytes));
        let path = media.put(&hash, bytes).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        assert_eq!(media.put(&hash, bytes).unwrap(), path);
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(media.read(&hash, 1024).unwrap(), Some(bytes.to_vec()));
        assert!(media.read(&hash, 4).is_err());
        assert!(media.put(&hash, b"wrong").is_err());
        assert!(media.path_verified("../invalid").is_err());
        fs::write(&path, b"modified by test").unwrap();
        assert!(media
            .read(&hash, 4)
            .unwrap_err()
            .to_string()
            .contains("size"));
        assert!(media.read(&hash, 1024).is_err());
        assert!(media.put(&hash, bytes).is_err());
    }

    #[test]
    fn no_symlink_source_or_hash_directory_is_followed() {
        let temp = Temp::new();
        let media = MediaStore::open(&temp.0).unwrap();
        let hash = format!("{:x}", Sha256::digest(b"a"));
        let source = temp.0.join("source");
        fs::write(&source, b"a").unwrap();
        let link = temp.0.join("link");
        symlink(&source, &link).unwrap();
        assert!(media.put_file(&hash, &link).is_err());
        let external = temp.0.join("other");
        fs::create_dir(&external).unwrap();
        symlink(&external, temp.0.join("media").join(&hash[..2])).unwrap();
        assert!(media.put(&hash, b"a").is_err());
        assert!(media.path_verified(&hash).is_err());
    }
}
