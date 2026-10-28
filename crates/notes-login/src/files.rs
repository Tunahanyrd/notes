//! Private file helpers with the exact semantics of `tools/notes_login.py`
//! (`private_write`, `load_json`, `save_json`), `storage.private_directory`
//! and `notes_login.ensure_state`.
use crate::error::{Error, Result};
use crate::json::{self, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

/// `os.open(path, O_CREAT|O_EXCL|O_WRONLY, 0o600)`, write, flush, fsync.
/// Never overwrites and never follows an existing symlink.
///
/// The file appears complete or not at all: the data goes to a private
/// temporary file in the same directory, which is then hard-linked to `path`
/// (fails with `AlreadyExists` like `O_EXCL`). Writing `path` directly let a
/// poller see it between create and write and read it empty: the sign-in
/// waits for `callback.json` with `exists()` and then parses it.
pub fn private_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|e| io::Error::other(e.to_string()))?;
    let suffix: String = suffix.iter().map(|b| format!("{:02x}", b)).collect();
    let temp = path.with_file_name(format!(".{}.{}.tmp", name.to_string_lossy(), suffix));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        fs::hard_link(&temp, path)
    })();
    let _ = fs::remove_file(&temp);
    result
}

/// `path.open('rb').read(limit + 1)`
pub fn read_limited(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let f = fs::File::open(path)?;
    let mut raw = Vec::new();
    f.take(limit as u64 + 1).read_to_end(&mut raw)?;
    Ok(raw)
}

/// `notes_login.load_json(path, limit)`
pub fn load_json(path: &Path, limit: usize) -> Result<Value> {
    let raw = read_limited(path, limit)?;
    if raw.len() > limit {
        return Err(Error::value("private input exceeds size limit"));
    }
    Ok(json::loads(&raw)?)
}

/// `load_json(path)` with the default 128 KiB bound.
pub fn load(path: &Path) -> Result<Value> {
    load_json(path, 128 * 1024)
}

/// `notes_login.save_json`: `private_write(json.dumps(data, indent=2) + '\n')`.
pub fn save_json(path: &Path, value: &Value) -> io::Result<()> {
    private_write(path, &json::indent2_bytes(value))
}

pub fn getuid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// `Path.mkdir(parents=True, exist_ok=True, mode=0o700)`: missing parents get
/// the default mode (0o777 & ~umask), the leaf 0o700 (& ~umask).
pub fn mkdir_parents_private(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
                other => other,
            }
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// `Path.mkdir(mode=0o700)` (no parents, must not exist).
pub fn mkdir_private(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// `Path.mkdir(mode=0o700, exist_ok=True)`
pub fn mkdir_private_exist_ok(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        other => other,
    }
}

fn private_owned(path: &Path) -> io::Result<bool> {
    let link = fs::symlink_metadata(path)?;
    if link.file_type().is_symlink() {
        return Ok(false);
    }
    let meta = fs::metadata(path)?;
    Ok(meta.uid() == getuid() && meta.mode() & 0o077 == 0)
}

/// `notes_login.ensure_state()` for the given state directory.
pub fn ensure_state(state: &Path) -> Result<()> {
    mkdir_parents_private(state)?;
    if !private_owned(state)? {
        return Err(Error::value("state directory must be owned by this user and private"));
    }
    Ok(())
}

/// `samsung_notes.storage.private_directory(path)`
pub fn private_directory(path: &Path) -> Result<()> {
    mkdir_parents_private(path)?;
    if !private_owned(path)? {
        return Err(Error::value("application directory must be private and owned by the user"));
    }
    Ok(())
}

/// Python `str(path)` for a path that must be valid UTF-8 (JSON receipts).
pub fn path_text(path: &Path) -> Result<String> {
    path.to_str().map(str::to_string).ok_or_else(|| Error::value("non-UTF-8 private path is not supported"))
}

#[cfg(test)]
mod atomic_write_tests {
    use super::private_write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn private_write_is_exclusive_private_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("notes-login-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("callback.json");
        let _ = std::fs::remove_file(&path);
        private_write(&path, b"{\"ok\": true}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"ok\": true}");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(private_write(&path, b"x").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("callback.json")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_poller_never_sees_the_file_before_its_content() {
        let dir = std::env::temp_dir().join(format!("notes-login-poll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = vec![b'7'; 1 << 20];
        for round in 0..20 {
            let path = dir.join(format!("callback-{round}.json"));
            let (reader_path, expected) = (path.clone(), data.len());
            let reader = std::thread::spawn(move || loop {
                if reader_path.exists() {
                    return std::fs::read(&reader_path).unwrap().len() == expected;
                }
                std::hint::spin_loop();
            });
            private_write(&path, &data).unwrap();
            assert!(reader.join().unwrap(), "round {round}: read a partial file");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
