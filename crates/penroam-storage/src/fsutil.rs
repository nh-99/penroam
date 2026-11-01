//! Durable file primitives.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub const TMP_MARKER: &str = ".tmp-";

/// Writes `bytes` to `path` atomically: temp file in the same directory →
/// write → fsync → rename over the target → fsync the directory. Readers see
/// either the old or the new content, never a torn file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent dir"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!("{name}{TMP_MARKER}{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let res = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        sync_dir(dir)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Appends one record and fsyncs before returning.
pub fn append_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_data()
}

#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    // Windows: directory handles cannot be fsynced through std; NTFS
    // journals the rename metadata itself.
    Ok(())
}

pub fn is_storage_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull || e.kind() == io::ErrorKind::QuotaExceeded || e.raw_os_error() == Some(28)
}
