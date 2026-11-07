//! Vaults locked on this device: their notebooks, sync data and search index, encrypted at rest
//! under a key only the vault's root secret derives.
//!
//! ```text
//! <library>/locked-vaults/<vault id>/locked.json      format marker, vault id (plaintext)
//! <library>/locked-vaults/<vault id>/manifest.bin     sealed: what was locked, and secrets
//! <library>/locked-vaults/<vault id>/blobs/<n>        sealed file contents, in 1 MiB chunks
//! <library>/vault-locks/<vault id>.json               copy of the server's passphrase lock
//! ```
//!
//! **How it works.** Locking reads every file of the folders it is given (each under a label,
//! such as `notebook/<id>`), encrypts it into a new pack and commits the pack with one rename.
//! The caller then deletes the plaintext ([`remove_plaintext`]). Unlocking decrypts everything
//! into a staging folder first, so a wrong key or damaged pack changes nothing, then moves each
//! folder into place, and only then deletes the pack ([`finish_unlock`]).
//!
//! **Invariant: a vault with a committed pack is locked.** Any plaintext of it that exists is a
//! leftover from a lock interrupted before its deletions, or an unlock interrupted before its
//! commit; either way the pack holds the same content, so start-up deletes the plaintext
//! ([`remove_plaintext`] again). Partial packs and staging folders are deleted at start-up too
//! ([`clean_up`]): an uncommitted lock leaves the plaintext untouched.
//!
//! **Keys.** Contents are sealed with XChaCha20-Poly1305 under [`RootSecret::vault_local_key`].
//! Every chunk's associated data binds the vault id, the file number, the chunk number and
//! whether it is the last chunk, so chunks can't be reordered, swapped between files or
//! vaults, or cut off. The device keeps the local key in its secret store only while the vault
//! is unlocked. To unlock without a connection it keeps a copy of the vault's passphrase lock
//! ([`save_cached_lock`]), which is what the server stores anyway, and so open to the same
//! offline guessing: the passphrase rules in [`crate::vault`] are what protects it.
//!
//! What this does not cover: a vault's notebooks are plaintext while it is unlocked, deleted
//! files can stay readable on the disk underneath (no secure erase), and journal data that was
//! already damaged can't be attributed to a notebook and stays where it is.
//!
//! See `docs/formats/locked-vault.md` and `docs/security/threat-model.md`.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use penroam_storage::fsutil::{remove_dir_all_durable, rename_durable, sync_dir, write_atomic};
use penroam_storage::Library;
use penroam_sync_protocol::{base64url, VaultLock};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::crypto::{self, CryptoError, RootSecret, KEY_LEN};
use crate::engine::SyncEngine;
use crate::secrets::SecretStore;
use crate::transport::SyncTransport;
use crate::vault::{NotebookOwners, Scope};

/// Folder of the packs, inside the library.
pub const LOCKED_DIR: &str = "locked-vaults";
/// Folder of the cached passphrase locks, inside the library.
pub const LOCKS_DIR: &str = "vault-locks";
const MARKER: &str = "locked.json";
const MANIFEST: &str = "manifest.bin";
const FORMAT: &str = "penroam-locked-vault";
const VERSION: u32 = 1;
/// Plaintext bytes per sealed chunk.
pub const CHUNK: usize = 1 << 20;
const MANIFEST_AAD: &[u8] = b"penroam/locked-vault/manifest/v1";
const CHUNK_AAD: &[u8] = b"penroam/locked-vault/chunk/v1";
const PARTIAL: &str = ".partial";
const RESTORE: &str = ".restore";
const TRASH: &str = ".trash";

/// Why locking or unlocking failed.
#[derive(Debug, thiserror::Error)]
pub enum LockerError {
    /// The key doesn't open this pack, or the pack was altered (the two can't be told apart).
    #[error("that key doesn't open this vault's locked copy")]
    WrongKey,
    /// The pack is missing parts or isn't one this build understands.
    #[error("the locked copy of this vault is damaged: {0}")]
    Damaged(String),
    /// A file-system operation failed.
    #[error("locked vault files: {0}")]
    Io(#[from] io::Error),
    /// The library or the vault's sync data refused a step.
    #[error("{0}")]
    Library(String),
    /// This device doesn't hold the vault's local key: it was unlocked before devices kept one.
    /// Locking then needs the passphrase once.
    #[error("enter the vault's passphrase to lock it on this device")]
    NeedsKey,
}

/// A vault's local key, wiped from memory when dropped.
pub type LocalKey = Zeroizing<[u8; KEY_LEN]>;

/// What to lock: folders by label, and secrets to keep inside the pack.
#[derive(Default)]
pub struct Contents {
    /// `(label, folder)`. Labels name a folder to [`unlock`]'s caller; a missing folder is
    /// skipped.
    pub folders: Vec<(String, PathBuf)>,
    /// `(name, value)` secrets that must not stay in the secret store while locked.
    pub secrets: Vec<(String, String)>,
}

/// What [`unlock`] put back.
#[derive(Debug)]
pub struct Unlocked {
    /// The labels of the folders now in place.
    pub labels: Vec<String>,
    /// The secrets the pack held, for the caller to store again.
    pub secrets: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    format: String,
    version: u32,
    vault_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    vault_id: String,
    folders: Vec<FolderEntry>,
    secrets: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FolderEntry {
    label: String,
    /// Sub-folders, relative, so empty ones come back too.
    dirs: Vec<String>,
    files: Vec<FileEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    path: String,
    size: u64,
    blob: u64,
}

fn locked_root(library: &Path) -> PathBuf {
    library.join(LOCKED_DIR)
}

fn pack_dir(library: &Path, vault: &str) -> PathBuf {
    locked_root(library).join(vault)
}

fn checked(vault: &str) -> Result<&str, LockerError> {
    // the same rule as a vault's sync folder: the id becomes a folder name
    Scope::vault(vault)
        .map(|_| vault)
        .map_err(|e| LockerError::Damaged(e.to_string()))
}

/// Whether `vault` has a committed pack here, so it is locked on this device.
pub fn is_locked(library: &Path, vault: &str) -> bool {
    pack_dir(library, vault).join(MARKER).is_file()
}

/// Vault ids with a committed pack, sorted.
pub fn locked_vaults(library: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(locked_root(library))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| Scope::vault(n).is_ok() && is_locked(library, n))
        .collect();
    out.sort();
    out
}

/// Deletes partial packs, unlock staging folders and unlocked packs being removed: whatever an
/// interrupted lock or unlock left behind. Run it at start-up, before the library opens.
pub fn clean_up(library: &Path) -> io::Result<()> {
    for e in fs::read_dir(locked_root(library))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(PARTIAL) || name.ends_with(RESTORE) || name.ends_with(TRASH) {
            remove_dir_all_durable(&e.path())?;
        }
    }
    Ok(())
}

fn chunk_aad(vault: &str, blob: u64, index: u64, last: bool) -> Vec<u8> {
    let mut a = CHUNK_AAD.to_vec();
    a.push(0);
    a.extend_from_slice(vault.as_bytes());
    a.push(0);
    a.extend_from_slice(&blob.to_le_bytes());
    a.extend_from_slice(&index.to_le_bytes());
    a.push(u8::from(last));
    a
}

fn manifest_aad(vault: &str) -> Vec<u8> {
    let mut a = MANIFEST_AAD.to_vec();
    a.push(0);
    a.extend_from_slice(vault.as_bytes());
    a
}

/// Sub-folders, and regular files with their full paths, of one folder.
type Tree = (Vec<String>, Vec<(String, PathBuf)>);

/// Every sub-folder and regular file under `dir`, relative and with `/` separators, sorted.
/// Symbolic links and other special files are left out: nothing in a library is one.
fn walk(dir: &Path) -> io::Result<Tree> {
    fn rel(base: &Path, p: &Path) -> Option<String> {
        let parts: Option<Vec<&str>> = p
            .strip_prefix(base)
            .ok()?
            .components()
            .map(|c| c.as_os_str().to_str())
            .collect();
        Some(parts?.join("/"))
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)? {
            let e = e?;
            let t = e.file_type()?;
            let p = e.path();
            let Some(r) = rel(dir, &p) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("a file name in {} isn't valid Unicode", d.display()),
                ));
            };
            if t.is_dir() {
                dirs.push(r);
                stack.push(p);
            } else if t.is_file() {
                files.push((r, p));
            }
        }
    }
    dirs.sort();
    files.sort();
    Ok((dirs, files))
}

/// Seals one file into `out`, chunk by chunk. Returns the plaintext size.
fn seal_file(
    key: &[u8; KEY_LEN],
    vault: &str,
    blob: u64,
    src: &Path,
    out: &Path,
) -> io::Result<u64> {
    let mut r = BufReader::new(File::open(src)?);
    let mut w = File::create(out)?;
    let mut buf = vec![0u8; CHUNK];
    let mut next = vec![0u8; CHUNK];
    let mut have = read_full(&mut r, &mut buf)?;
    let mut index = 0u64;
    let mut size = 0u64;
    loop {
        // look ahead one chunk to know whether this one is the last
        let more = if have == CHUNK {
            read_full(&mut r, &mut next)?
        } else {
            0
        };
        let last = more == 0;
        let aad = chunk_aad(vault, blob, index, last);
        w.write_all(&crypto::seal(key, &aad, &aad, &buf[..have]))?;
        size += have as u64;
        if last {
            break;
        }
        std::mem::swap(&mut buf, &mut next);
        have = more;
        index += 1;
    }
    w.sync_all()?;
    Ok(size)
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Opens one sealed file into `out`. `size` comes from the (authenticated) manifest, so the
/// chunk boundaries are known and a truncated or extended blob fails.
fn open_file(
    key: &[u8; KEY_LEN],
    vault: &str,
    blob: u64,
    size: u64,
    src: &Path,
    out: &Path,
) -> Result<(), LockerError> {
    let chunks = if size == 0 {
        1
    } else {
        size.div_ceil(CHUNK as u64)
    };
    let mut r = BufReader::new(File::open(src).map_err(|e| LockerError::Damaged(e.to_string()))?);
    let mut w = File::create(out)?;
    let mut buf = vec![0u8; crypto::sealed_len(CHUNK)];
    for index in 0..chunks {
        let last = index + 1 == chunks;
        let plain = if last {
            size - index * CHUNK as u64
        } else {
            CHUNK as u64
        } as usize;
        let n = crypto::sealed_len(plain);
        r.read_exact(&mut buf[..n])
            .map_err(|_| LockerError::Damaged(format!("file {blob} is cut short")))?;
        let aad = chunk_aad(vault, blob, index, last);
        let pt =
            Zeroizing::new(crypto::open(key, &aad, &buf[..n]).map_err(|_| LockerError::WrongKey)?);
        w.write_all(&pt)?;
    }
    let mut extra = [0u8; 1];
    if r.read(&mut extra)? != 0 {
        return Err(LockerError::Damaged(format!("file {blob} has extra data")));
    }
    w.sync_all()?;
    Ok(())
}

/// Encrypts `contents` into a pack for `vault` and commits it. Plaintext is left in place: the
/// caller deletes it with [`remove_plaintext`] once this returns. On error nothing is
/// committed and the vault stays unlocked.
///
/// The folders must be quiescent: nothing may write to them until they are deleted.
pub fn lock(
    library: &Path,
    vault: &str,
    key: &[u8; KEY_LEN],
    contents: &Contents,
) -> Result<(), LockerError> {
    let vault = checked(vault)?;
    let final_dir = pack_dir(library, vault);
    if is_locked(library, vault) {
        return Err(LockerError::Damaged("this vault is already locked".into()));
    }
    let partial = locked_root(library).join(format!("{vault}{PARTIAL}"));
    if partial.exists() {
        fs::remove_dir_all(&partial)?;
    }
    let blobs = partial.join("blobs");
    fs::create_dir_all(&blobs)?;
    let result = (|| {
        let mut manifest = Manifest {
            vault_id: vault.to_string(),
            folders: Vec::new(),
            secrets: contents.secrets.clone(),
        };
        let mut blob = 0u64;
        for (label, dir) in &contents.folders {
            if !dir.is_dir() {
                continue;
            }
            let (dirs, files) = walk(dir)?;
            let mut entry = FolderEntry {
                label: label.clone(),
                dirs,
                files: Vec::new(),
            };
            for (rel, path) in files {
                let size = seal_file(key, vault, blob, &path, &blobs.join(blob.to_string()))?;
                entry.files.push(FileEntry {
                    path: rel,
                    size,
                    blob,
                });
                blob += 1;
            }
            manifest.folders.push(entry);
        }
        sync_dir(&blobs)?;
        let json = Zeroizing::new(serde_json::to_vec(&manifest).map_err(io::Error::other)?);
        let aad = manifest_aad(vault);
        write_atomic(
            &partial.join(MANIFEST),
            &crypto::seal(key, &aad, &aad, &json),
        )?;
        let marker = Marker {
            format: FORMAT.into(),
            version: VERSION,
            vault_id: vault.to_string(),
        };
        write_atomic(
            &partial.join(MARKER),
            &serde_json::to_vec_pretty(&marker).map_err(io::Error::other)?,
        )?;
        // the commit point: from here on the vault is locked
        rename_durable(&partial, &final_dir)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    result.map_err(LockerError::from)
}

fn read_manifest(
    library: &Path,
    vault: &str,
    key: &[u8; KEY_LEN],
) -> Result<Manifest, LockerError> {
    let dir = pack_dir(library, vault);
    let marker: Marker = fs::read(dir.join(MARKER))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| LockerError::Damaged("its marker can't be read".into()))?;
    if marker.format != FORMAT || marker.vault_id != vault {
        return Err(LockerError::Damaged(
            "its marker names something else".into(),
        ));
    }
    if marker.version != VERSION {
        return Err(LockerError::Damaged(format!(
            "it was written by a newer Penroam (version {}); please update",
            marker.version
        )));
    }
    let sealed = fs::read(dir.join(MANIFEST)).map_err(|e| LockerError::Damaged(e.to_string()))?;
    let json = Zeroizing::new(
        crypto::open(key, &manifest_aad(vault), &sealed).map_err(|_| LockerError::WrongKey)?,
    );
    let m: Manifest =
        serde_json::from_slice(&json).map_err(|e| LockerError::Damaged(e.to_string()))?;
    if m.vault_id != vault {
        return Err(LockerError::Damaged("it belongs to another vault".into()));
    }
    Ok(m)
}

/// Checks that `key` opens the pack of `vault`, without changing anything.
pub fn check_key(library: &Path, vault: &str, key: &[u8; KEY_LEN]) -> Result<(), LockerError> {
    read_manifest(library, checked(vault)?, key).map(|_| ())
}

/// A relative path from a manifest, refused if it could leave its folder.
fn safe_rel(rel: &str) -> Result<PathBuf, LockerError> {
    let mut p = PathBuf::new();
    for part in rel.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('\\')
            || part.contains(':')
        {
            return Err(LockerError::Damaged(format!(
                "it names an unsafe path: {rel}"
            )));
        }
        p.push(part);
    }
    Ok(p)
}

/// Moves a folder into place: a rename, or a copy and delete when the two are on different
/// file systems (the search index lives in the cache folder).
fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if to.exists() {
        // leftover plaintext of a locked vault: the pack is the authority
        fs::remove_dir_all(to)?;
    }
    match rename_durable(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_dir(from, to)?;
            sync_dir(to.parent().unwrap_or(Path::new(".")))?;
            fs::remove_dir_all(from)
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let t = e.file_type()?;
        let dest = to.join(e.file_name());
        if t.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else if t.is_file() {
            fs::copy(e.path(), &dest)?;
            File::open(&dest)?.sync_all()?;
        }
    }
    sync_dir(to)
}

/// Decrypts the pack of `vault` and moves every folder to where `place` says its label goes
/// (`None` skips one). The pack stays: the caller stores the returned secrets, then calls
/// [`finish_unlock`], which is the commit point. Until then a restart treats the vault as still
/// locked and deletes what was moved into place.
///
/// Everything is decrypted and verified into a staging folder before the first move, so a wrong
/// key or a damaged pack changes nothing outside the staging folder.
pub fn unlock(
    library: &Path,
    vault: &str,
    key: &[u8; KEY_LEN],
    place: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Unlocked, LockerError> {
    let vault = checked(vault)?;
    let manifest = read_manifest(library, vault, key)?;
    let dir = pack_dir(library, vault);
    let staging = locked_root(library).join(format!("{vault}{RESTORE}"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result: Result<Vec<String>, LockerError> = (|| {
        let mut targets = Vec::new();
        for (i, f) in manifest.folders.iter().enumerate() {
            let Some(target) = place(&f.label) else {
                continue;
            };
            let base = staging.join(i.to_string());
            fs::create_dir_all(&base)?;
            for d in &f.dirs {
                fs::create_dir_all(base.join(safe_rel(d)?))?;
            }
            for file in &f.files {
                let out = base.join(safe_rel(&file.path)?);
                if let Some(p) = out.parent() {
                    fs::create_dir_all(p)?;
                }
                let src = dir.join("blobs").join(file.blob.to_string());
                open_file(key, vault, file.blob, file.size, &src, &out)?;
            }
            targets.push((f.label.clone(), base, target));
        }
        let mut labels = Vec::new();
        for (label, base, target) in targets {
            move_dir(&base, &target)?;
            labels.push(label);
        }
        Ok(labels)
    })();
    let _ = fs::remove_dir_all(&staging);
    Ok(Unlocked {
        labels: result?,
        secrets: manifest.secrets,
    })
}

/// Commits an unlock: deletes the pack, so the vault counts as unlocked from here on.
pub fn finish_unlock(library: &Path, vault: &str) -> io::Result<()> {
    let dir = pack_dir(library, vault);
    if !dir.exists() {
        return Ok(());
    }
    let trash = locked_root(library).join(format!("{vault}{TRASH}"));
    if trash.exists() {
        fs::remove_dir_all(&trash)?;
    }
    // the rename is the commit; a crash before the deletion leaves trash for `clean_up`
    rename_durable(&dir, &trash)?;
    remove_dir_all_durable(&trash)
}

/// Deletes plaintext a locked vault must not leave behind: each folder, and each secret through
/// `forget`. Safe to repeat; run it after [`lock`], and at start-up for every
/// [`locked_vaults`] entry.
pub fn remove_plaintext(
    folders: &[PathBuf],
    secrets: &[String],
    mut forget: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    let mut first_err = None;
    for f in folders {
        if let Err(e) = remove_dir_all_durable(f) {
            first_err.get_or_insert(e);
        }
    }
    for s in secrets {
        if let Err(e) = forget(s) {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

fn lock_path(library: &Path, vault: &str) -> PathBuf {
    library.join(LOCKS_DIR).join(format!("{vault}.json"))
}

/// Keeps a copy of the server's passphrase lock of `vault`, for unlocking without a connection.
pub fn save_cached_lock(library: &Path, vault: &str, lock: &VaultLock) -> Result<(), LockerError> {
    let vault = checked(vault)?;
    let path = lock_path(library, vault);
    let bytes = serde_json::to_vec_pretty(lock).map_err(io::Error::other)?;
    if fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    write_atomic(&path, &bytes)?;
    Ok(())
}

/// The cached passphrase lock of `vault`, if this device has one.
pub fn cached_lock(library: &Path, vault: &str) -> Option<VaultLock> {
    checked(vault).ok()?;
    let raw = fs::read(lock_path(library, vault)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Deletes the cached lock of `vault` (the vault was deleted).
pub fn forget_cached_lock(library: &Path, vault: &str) -> io::Result<()> {
    if checked(vault).is_err() {
        return Ok(());
    }
    match fs::remove_file(lock_path(library, vault)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Deletes the pack of `vault` without opening it (the vault was deleted on the server).
pub fn discard(library: &Path, vault: &str) -> io::Result<()> {
    if checked(vault).is_err() {
        return Ok(());
    }
    finish_unlock(library, vault)
}

// ------------------------------------------------------- vaults in a library --

/// Base name of the secret holding a vault's local key while it is unlocked here.
pub const LOCAL_KEY_SECRET: &str = "local-key";
/// Label of a vault's sync folder in a pack.
pub const SYNC_LABEL: &str = "sync";
/// Label prefix of a notebook folder in a pack.
pub const NOTEBOOK_LABEL: &str = "notebook/";

fn local_key_name(vault: &str) -> Result<String, LockerError> {
    Ok(Scope::vault(vault)
        .map_err(|e| LockerError::Damaged(e.to_string()))?
        .secret_name(LOCAL_KEY_SECRET))
}

/// Keeps the local key of `vault`, derived from its root, while the vault is unlocked here.
pub fn store_local_key(
    secrets: &dyn SecretStore,
    vault: &str,
    root: &RootSecret,
) -> Result<(), LockerError> {
    let key = Zeroizing::new(root.vault_local_key());
    secrets.set(&local_key_name(vault)?, &base64url::encode(key.as_ref()))?;
    Ok(())
}

/// The local key of `vault`, if this device keeps one.
pub fn local_key(secrets: &dyn SecretStore, vault: &str) -> Result<Option<LocalKey>, LockerError> {
    let Some(raw) = secrets.get(&local_key_name(vault)?)? else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(
        base64url::decode(&raw).map_err(|_| LockerError::Damaged("stored local key".into()))?,
    );
    let key: [u8; KEY_LEN] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| LockerError::Damaged("stored local key".into()))?;
    Ok(Some(Zeroizing::new(key)))
}

/// The local key a root derives, for opening a pack.
pub fn key_from_root(root: &RootSecret) -> LocalKey {
    Zeroizing::new(root.vault_local_key())
}

/// The folders a locked `vault` must not leave in plaintext: its notebooks (by the owners
/// file) and its sync folder. The caller adds its own (a search index).
pub fn vault_folders(library: &Path, vault: &str) -> Result<Vec<(String, PathBuf)>, LockerError> {
    let scope = Scope::vault(vault).map_err(|e| LockerError::Damaged(e.to_string()))?;
    let owners = NotebookOwners::load(library)?;
    let mut out: Vec<(String, PathBuf)> = owners
        .notebooks
        .iter()
        .filter(|(_, v)| v.as_str() == vault)
        .map(|(nb, _)| {
            (
                format!("{NOTEBOOK_LABEL}{nb}"),
                library.join("notebooks").join(nb.to_string()),
            )
        })
        .collect();
    out.push((SYNC_LABEL.into(), scope.dir(library)));
    Ok(out)
}

/// Secret names a locked `vault` must not leave in the secret store.
pub fn vault_secret_names(vault: &str, device_secret: &str) -> Result<Vec<String>, LockerError> {
    Ok(vec![device_secret.to_string(), local_key_name(vault)?])
}

/// A failed [`lock_vault`]: why, and the engine back when the vault is still unlocked.
pub type LockFailed<T> = (LockerError, Option<Box<SyncEngine<T>>>);

/// Locks `vault` on this device: copies the journal into the vault's operation log, takes its
/// notebooks out of `lib`, encrypts them, the sync folder and `extra` folders into a pack,
/// then deletes the plaintext and the vault's secrets. The engine is consumed: a locked vault
/// doesn't sync. Hold the library lock throughout.
///
/// On an error before the pack is committed, the notebooks go back into the library and the
/// engine is handed back, so nothing changed. After the commit, a failed deletion is
/// reported but the vault is locked; start-up finishes the deletion.
pub fn lock_vault<T: SyncTransport>(
    lib: &mut Library,
    mut engine: SyncEngine<T>,
    extra: Vec<(String, PathBuf)>,
    key: Option<&[u8; KEY_LEN]>,
) -> Result<(), LockFailed<T>> {
    let library = lib.root().to_path_buf();
    let Scope::Vault(vault) = engine.scope().clone() else {
        return Err((
            LockerError::Library("not a vault".into()),
            Some(Box::new(engine)),
        ));
    };
    let stored = match local_key(engine.secret_store(), &vault) {
        Ok(k) => k,
        Err(e) => return Err((e, Some(Box::new(engine)))),
    };
    let key: LocalKey = match (key, stored) {
        (Some(k), _) => Zeroizing::new(*k),
        (None, Some(k)) => k,
        (None, None) => return Err((LockerError::NeedsKey, Some(Box::new(engine)))),
    };
    if let Err(e) = engine.ingest_now(lib) {
        return Err((LockerError::Library(e.to_string()), Some(Box::new(engine))));
    }
    let mut folders = match vault_folders(&library, &vault) {
        Ok(f) => f,
        Err(e) => return Err((e, Some(Box::new(engine)))),
    };
    let ids: Vec<Uuid> = folders
        .iter()
        .filter_map(|(l, _)| l.strip_prefix(NOTEBOOK_LABEL)?.parse().ok())
        .collect();
    if let Err(e) = lib.detach_notebooks(&ids.iter().copied().collect()) {
        lib.attach_notebooks(&ids);
        return Err((LockerError::Library(e.to_string()), Some(Box::new(engine))));
    }
    folders.extend(extra);
    let device_secret = engine.device_secret_name();
    let device_keys = match engine.secret_store().get(&device_secret) {
        Ok(Some(k)) => k,
        Ok(None) => {
            lib.attach_notebooks(&ids);
            return Err((
                LockerError::Library("this device's keys for the vault are missing".into()),
                Some(Box::new(engine)),
            ));
        }
        Err(e) => {
            lib.attach_notebooks(&ids);
            return Err((e.into(), Some(Box::new(engine))));
        }
    };
    let contents = Contents {
        folders: folders.clone(),
        secrets: vec![(device_secret.clone(), device_keys)],
    };
    if let Err(e) = lock(&library, &vault, &key, &contents) {
        lib.attach_notebooks(&ids);
        return Err((e, Some(Box::new(engine))));
    }
    let names = vault_secret_names(&vault, &device_secret).map_err(|e| (e, None))?;
    let paths: Vec<PathBuf> = folders.into_iter().map(|(_, p)| p).collect();
    let secrets = engine.secret_store();
    let r = remove_plaintext(&paths, &names, |n| secrets.delete(n));
    drop(engine);
    r.map_err(|e| (e.into(), None))
}

/// Locks a vault that has no engine here because it isn't synced with the cloud: takes its
/// notebooks out of `lib`, encrypts them and `extra` folders under the local key from
/// `secrets`, then deletes the plaintext and the key. Hold the library lock throughout.
///
/// Fails with [`LockerError::NeedsKey`] when `secrets` has no local key and none is given. On
/// an error before the pack is committed the notebooks go back into the library.
pub fn lock_local(
    lib: &mut Library,
    vault: &str,
    secrets: &dyn SecretStore,
    extra: Vec<(String, PathBuf)>,
    key: Option<&[u8; KEY_LEN]>,
) -> Result<(), LockerError> {
    let library = lib.root().to_path_buf();
    let key: LocalKey = match (key, local_key(secrets, vault)?) {
        (Some(k), _) => Zeroizing::new(*k),
        (None, Some(k)) => k,
        (None, None) => return Err(LockerError::NeedsKey),
    };
    // no sync folder: nothing syncs this vault, so there is no operation log to keep
    let mut folders: Vec<(String, PathBuf)> = vault_folders(&library, vault)?
        .into_iter()
        .filter(|(l, _)| l != SYNC_LABEL)
        .collect();
    let ids: Vec<Uuid> = folders
        .iter()
        .filter_map(|(l, _)| l.strip_prefix(NOTEBOOK_LABEL)?.parse().ok())
        .collect();
    if let Err(e) = lib.detach_notebooks(&ids.iter().copied().collect()) {
        lib.attach_notebooks(&ids);
        return Err(LockerError::Library(e.to_string()));
    }
    folders.extend(extra);
    let contents = Contents {
        folders: folders.clone(),
        secrets: Vec::new(),
    };
    if let Err(e) = lock(&library, vault, &key, &contents) {
        lib.attach_notebooks(&ids);
        return Err(e);
    }
    let paths: Vec<PathBuf> = folders.into_iter().map(|(_, p)| p).collect();
    let names = vec![local_key_name(vault)?];
    remove_plaintext(&paths, &names, |n| secrets.delete(n))?;
    Ok(())
}

/// Unlocks `vault` on this device from its local `key`: puts its folders back (`extra` places
/// labels other than notebooks and the sync folder), restores its secrets into `secrets`,
/// commits, and puts its notebooks back into `lib`. Hold the library lock throughout, and start
/// the vault's engine afterwards with [`SyncEngine::resume_after_lock`].
///
/// A wrong key or damaged pack changes nothing. Returns the notebooks that came back.
pub fn unlock_vault(
    lib: &mut Library,
    vault: &str,
    key: &[u8; KEY_LEN],
    secrets: &dyn SecretStore,
    extra: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Vec<Uuid>, LockerError> {
    let library = lib.root().to_path_buf();
    let scope = Scope::vault(vault).map_err(|e| LockerError::Damaged(e.to_string()))?;
    let sync_dir = scope.dir(&library);
    let notebooks = library.join("notebooks");
    let u = unlock(&library, vault, key, |label| {
        if label == SYNC_LABEL {
            Some(sync_dir.clone())
        } else if let Some(nb) = label.strip_prefix(NOTEBOOK_LABEL) {
            // only a notebook id may become a folder name
            nb.parse::<Uuid>()
                .ok()
                .map(|id| notebooks.join(id.to_string()))
        } else {
            extra(label)
        }
    })?;
    for (name, value) in &u.secrets {
        secrets.set(name, value)?;
    }
    secrets.set(&local_key_name(vault)?, &base64url::encode(key))?;
    finish_unlock(&library, vault)?;
    let ids: Vec<Uuid> = u
        .labels
        .iter()
        .filter_map(|l| l.strip_prefix(NOTEBOOK_LABEL)?.parse().ok())
        .collect();
    Ok(lib.attach_notebooks(&ids))
}

/// Finishes what interrupted locks and unlocks left: removes partial packs and staging folders,
/// and deletes any plaintext and secrets of vaults that are locked. `extra` lists the caller's
/// own folders of a vault (a search index). Run it at start-up, before the library opens.
pub fn recover(
    library: &Path,
    secrets: &dyn SecretStore,
    extra: impl Fn(&str) -> Vec<PathBuf>,
) -> Result<(), LockerError> {
    clean_up(library)?;
    for vault in locked_vaults(library) {
        let scope = Scope::vault(&vault).map_err(|e| LockerError::Damaged(e.to_string()))?;
        let mut paths: Vec<PathBuf> = vault_folders(library, &vault)?
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        paths.extend(extra(&vault));
        let names = vault_secret_names(&vault, &scope.secret_name(crate::engine::DEVICE_SECRET))?;
        remove_plaintext(&paths, &names, |n| secrets.delete(n))?;
    }
    Ok(())
}

impl From<CryptoError> for LockerError {
    fn from(_: CryptoError) -> Self {
        LockerError::WrongKey
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> LocalKey {
        Zeroizing::new([b; KEY_LEN])
    }

    fn tree(root: &Path) {
        fs::create_dir_all(root.join("pages")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("notebook.json"), b"{\"title\":\"secret\"}").unwrap();
        fs::write(root.join("pages/a.prp"), vec![7u8; CHUNK * 2 + 5]).unwrap();
        fs::write(root.join("pages/exact.prp"), vec![9u8; CHUNK]).unwrap();
        fs::write(root.join("pages/empty.prp"), b"").unwrap();
    }

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        let (dirs, files) = walk(root).unwrap();
        let mut v: Vec<(String, Vec<u8>)> = dirs.into_iter().map(|d| (d, vec![])).collect();
        v.extend(files.into_iter().map(|(r, p)| (r, fs::read(p).unwrap())));
        v
    }

    fn contents(lib: &Path) -> Contents {
        Contents {
            folders: vec![
                ("notebook/n1".into(), lib.join("notebooks/n1")),
                ("missing".into(), lib.join("nothing-here")),
            ],
            secrets: vec![("vault-v1-keys".into(), "device keys".into())],
        }
    }

    fn place(lib: &Path) -> impl Fn(&str) -> Option<PathBuf> + '_ {
        move |label| {
            label
                .strip_prefix("notebook/")
                .map(|n| lib.join("notebooks").join(n))
        }
    }

    #[test]
    fn a_locked_vault_holds_no_plaintext_and_comes_back_whole() {
        let d = tempfile::tempdir().unwrap();
        let lib = d.path();
        tree(&lib.join("notebooks/n1"));
        let before = snapshot(&lib.join("notebooks/n1"));
        lock(lib, "v1", &key(1), &contents(lib)).unwrap();
        assert!(is_locked(lib, "v1"));
        assert_eq!(locked_vaults(lib), vec!["v1".to_string()]);
        remove_plaintext(&[lib.join("notebooks/n1")], &[], |_| Ok(())).unwrap();
        // nothing readable is left in the pack
        for (_, p) in walk(&lib.join(LOCKED_DIR)).unwrap().1 {
            let b = fs::read(p).unwrap();
            assert!(!b.windows(6).any(|w| w == b"secret"));
            assert!(!b.windows(64).any(|w| w.iter().all(|x| *x == 7)));
        }
        assert!(matches!(
            unlock(lib, "v1", &key(2), place(lib)),
            Err(LockerError::WrongKey)
        ));
        assert!(!lib.join("notebooks/n1").exists());
        let u = unlock(lib, "v1", &key(1), place(lib)).unwrap();
        assert_eq!(u.labels, vec!["notebook/n1".to_string()]);
        assert_eq!(
            u.secrets,
            vec![("vault-v1-keys".to_string(), "device keys".to_string())]
        );
        assert!(is_locked(lib, "v1"), "unlocked only once committed");
        finish_unlock(lib, "v1").unwrap();
        assert!(!is_locked(lib, "v1"));
        assert_eq!(snapshot(&lib.join("notebooks/n1")), before);
        clean_up(lib).unwrap();
        assert!(fs::read_dir(lib.join(LOCKED_DIR)).unwrap().next().is_none());
    }

    #[test]
    fn a_tampered_pack_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let lib = d.path();
        tree(&lib.join("notebooks/n1"));
        lock(lib, "v1", &key(1), &contents(lib)).unwrap();
        remove_plaintext(&[lib.join("notebooks/n1")], &[], |_| Ok(())).unwrap();
        let blobs = lib.join(LOCKED_DIR).join("v1/blobs");
        let (_, files) = walk(&blobs).unwrap();
        // swap two files' contents, then cut one short
        let (a, b) = (&files[0].1, &files[1].1);
        let (ba, bb) = (fs::read(a).unwrap(), fs::read(b).unwrap());
        fs::write(a, &bb).unwrap();
        fs::write(b, &ba).unwrap();
        assert!(unlock(lib, "v1", &key(1), place(lib)).is_err());
        assert!(!lib.join("notebooks/n1").exists());
        fs::write(a, &ba).unwrap();
        fs::write(b, &bb[..bb.len() - 1]).unwrap();
        assert!(unlock(lib, "v1", &key(1), place(lib)).is_err());
        assert!(!lib.join("notebooks/n1").exists());
        fs::write(b, &bb).unwrap();
        // a pack copied under another vault's name doesn't open there
        fs::rename(
            lib.join(LOCKED_DIR).join("v1"),
            lib.join(LOCKED_DIR).join("v2"),
        )
        .unwrap();
        let m = lib.join(LOCKED_DIR).join("v2").join(MARKER);
        fs::write(&m, fs::read_to_string(&m).unwrap().replace("v1", "v2")).unwrap();
        assert!(unlock(lib, "v2", &key(1), place(lib)).is_err());
    }

    #[test]
    fn interrupted_work_is_cleaned_up() {
        let d = tempfile::tempdir().unwrap();
        let lib = d.path();
        fs::create_dir_all(lib.join(LOCKED_DIR).join("v1.partial/blobs")).unwrap();
        fs::create_dir_all(lib.join(LOCKED_DIR).join("v1.restore/0")).unwrap();
        fs::create_dir_all(lib.join(LOCKED_DIR).join("v1.trash")).unwrap();
        assert!(locked_vaults(lib).is_empty());
        clean_up(lib).unwrap();
        assert!(fs::read_dir(lib.join(LOCKED_DIR)).unwrap().next().is_none());
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for p in ["../x", "a/../../x", "", "a//b", "C:\\x", "a\\b"] {
            assert!(safe_rel(p).is_err(), "{p}");
        }
        assert!(safe_rel("pages/a.prp").is_ok());
    }
}
