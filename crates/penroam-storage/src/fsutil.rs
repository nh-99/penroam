//! Durable file primitives, with crash-injection points.
//!
//! With the `fault-injection` feature, [`fault::arm`] makes the N-th
//! injection point fail. From then on every injection point fails, which
//! models the process dying at that boundary: bytes written before it stay
//! on disk, nothing after it happens. Appends can also stop half-way
//! through a record (torn write). Without the feature the points compile to
//! nothing.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub const TMP_MARKER: &str = ".tmp-";

#[cfg(feature = "fault-injection")]
pub mod fault {
    use std::cell::Cell;
    use std::io;

    thread_local! {
        static COUNTDOWN: Cell<Option<u64>> = const { Cell::new(None) };
        static DEAD: Cell<bool> = const { Cell::new(false) };
        static SEEN: Cell<u64> = const { Cell::new(0) };
        static LAST: Cell<&'static str> = const { Cell::new("") };
        static TRANSIENT: Cell<bool> = const { Cell::new(false) };
    }

    /// Like [`arm`], but the failure is a one-off "disk full" error and the
    /// process keeps running (no crash).
    pub fn arm_transient(n: u64) {
        arm(n);
        TRANSIENT.with(|t| t.set(true));
    }

    /// Fail at the `n`-th injection point from now (0 = the next one).
    pub fn arm(n: u64) {
        COUNTDOWN.with(|c| c.set(Some(n)));
        DEAD.with(|d| d.set(false));
        SEEN.with(|s| s.set(0));
    }

    /// Back to normal operation (the "restarted process").
    pub fn disarm() {
        COUNTDOWN.with(|c| c.set(None));
        DEAD.with(|d| d.set(false));
        TRANSIENT.with(|t| t.set(false));
    }

    /// Injection points passed since [`arm`].
    pub fn seen() -> u64 {
        SEEN.with(Cell::get)
    }

    /// Whether the simulated crash happened.
    pub fn crashed() -> bool {
        DEAD.with(Cell::get)
    }

    /// Name of the point that crashed (diagnostics).
    pub fn last() -> &'static str {
        LAST.with(Cell::get)
    }

    pub(crate) fn point(name: &'static str) -> io::Result<()> {
        if DEAD.with(Cell::get) {
            return Err(io::Error::other(format!("injected crash (dead) at {name}")));
        }
        let fire = COUNTDOWN.with(|c| match c.get() {
            Some(0) => {
                c.set(None);
                true
            }
            Some(n) => {
                c.set(Some(n - 1));
                false
            }
            None => false,
        });
        SEEN.with(|s| s.set(s.get() + 1));
        if fire {
            LAST.with(|l| l.set(name));
            if TRANSIENT.with(|t| t.replace(false)) {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    format!("injected disk full at {name}"),
                ));
            }
            DEAD.with(|d| d.set(true));
            return Err(io::Error::other(format!("injected crash at {name}")));
        }
        Ok(())
    }
}

#[inline(always)]
pub(crate) fn fp(_name: &'static str) -> io::Result<()> {
    #[cfg(feature = "fault-injection")]
    {
        fault::point(_name)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        Ok(())
    }
}

/// Writes `bytes` to `path` atomically: temp file in the same directory →
/// write → fsync → rename over the target → fsync the directory. Readers see
/// either the old or the new content, never a torn file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent dir"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(
        "{name}{TMP_MARKER}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let res = (|| {
        fp("atomic.create")?;
        let mut f = File::create(&tmp)?;
        fp("atomic.write")?;
        f.write_all(bytes)?;
        fp("atomic.fsync")?;
        f.sync_all()?;
        drop(f);
        fp("atomic.rename")?;
        fs::rename(&tmp, path)?;
        fp("atomic.dirsync")?;
        sync_dir(dir)
    })();
    if res.is_err() && !crashed() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

#[cfg(feature = "fault-injection")]
fn crashed() -> bool {
    fault::crashed()
}
#[cfg(not(feature = "fault-injection"))]
fn crashed() -> bool {
    false
}

/// Appends `bytes` at the end of an open file and fsyncs before returning.
/// On failure the file is truncated back to its previous length (so a
/// failed append, e.g. on a full disk, never leaves a partial record that a
/// later append would follow).
pub fn append_durable(f: &mut File, len_before: u64, bytes: &[u8]) -> io::Result<()> {
    let res = (|| {
        fp("append.write")?;
        #[cfg(feature = "fault-injection")]
        if let Err(e) = fault::point("append.torn") {
            // the write stops in the middle: a prefix reaches the disk
            let _ = f.write_all(&bytes[..bytes.len() / 2]);
            let _ = f.sync_data();
            return Err(e);
        }
        f.write_all(bytes)?;
        fp("append.fsync")?;
        f.sync_data()
    })();
    if res.is_err() && !crashed() {
        let _ = f.set_len(len_before);
        let _ = f.sync_data();
    }
    res
}

/// Creates a new empty file durably (file + directory entry).
pub fn create_durable(path: &Path, header: &[u8]) -> io::Result<File> {
    fp("create.file")?;
    let mut f = OpenOptions::new()
        .create_new(true)
        .read(true)
        .append(true)
        .open(path)?;
    fp("create.header")?;
    f.write_all(header)?;
    f.sync_all()?;
    fp("create.dirsync")?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))?;
    Ok(f)
}

/// Truncates a file to `len` durably.
pub fn truncate_durable(path: &Path, len: u64) -> io::Result<()> {
    fp("truncate")?;
    let f = OpenOptions::new().write(true).open(path)?;
    f.set_len(len)?;
    f.sync_all()
}

/// Renames durably (rename + directory fsync).
pub fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    fp("rename")?;
    fs::rename(from, to)?;
    fp("rename.dirsync")?;
    sync_dir(to.parent().unwrap_or(Path::new(".")))
}

/// Removes a file (missing is fine).
pub fn remove_durable(path: &Path) -> io::Result<()> {
    fp("remove")?;
    match fs::remove_file(path) {
        Ok(()) => sync_dir(path.parent().unwrap_or(Path::new("."))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes a directory tree (crash-injection aware).
pub fn remove_dir_all_durable(path: &Path) -> io::Result<()> {
    fp("remove.dir")?;
    match fs::remove_dir_all(path) {
        Ok(()) => sync_dir(path.parent().unwrap_or(Path::new("."))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
    e.kind() == io::ErrorKind::StorageFull
        || e.kind() == io::ErrorKind::QuotaExceeded
        || e.raw_os_error() == Some(28)
}
