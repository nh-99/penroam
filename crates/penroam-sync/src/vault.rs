//! Passphrase-locked vaults: notebooks whose keys the server never receives.
//!
//! A vault is a hidden second account of the same user (`vaults.v1`, protocol §12e), so it has
//! its own operation log, key log, blobs and checkpoints, and one more [`SyncEngine`] syncs it.
//! That separation is the design: a device that hasn't unlocked a vault simply doesn't run its
//! engine, so locked content can't stall ordinary sync, and a main-account checkpoint can't
//! contain vault content.
//!
//! * [`Scope`] says which notebooks and which sync folder an engine owns.
//! * [`NotebookOwners`] records which notebooks belong to a vault. **Invariant:** a notebook must
//!   be recorded here before it can be seen by the main engine, because the main engine uploads
//!   every notebook it isn't told to leave alone, under a key the server holds. Assignments are
//!   written under the library lock, and engines reload the file under that same lock. The file
//!   sits outside the sync folder so that disconnecting can't turn vault notebooks back into
//!   ordinary ones. A notebook whose vault has no engine here stays on this device, unsynced.
//! * [`new_lock`] and [`open_lock`] wrap a vault's root secret with a passphrase (Argon2id,
//!   XChaCha20-Poly1305). A passphrase is the only thing keeping the server out of a stored lock,
//!   since the server can try guesses offline. Hence [`MIN_PASSPHRASE_CHARS`] and the slow KDF.
//!
//! The lock's associated data can't name the vault, because the server assigns the id afterwards.
//! A server swapping two locks gains nothing: the opened root must verify the vault's key log
//! ([`SyncEngine::enroll_with_root`]), and it won't verify the wrong one.
//!
//! See `docs/security/threat-model.md`.
//!
//! [`SyncEngine`]: crate::engine::SyncEngine
//! [`SyncEngine::enroll_with_root`]: crate::engine::SyncEngine::enroll_with_root

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use penroam_storage::fsutil::write_atomic;
use penroam_sync_protocol::vault::KDF_ARGON2ID;
use penroam_sync_protocol::{base64url, VaultLock};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::crypto::{self, CryptoError, RootSecret, KEY_LEN};
use crate::store::SyncStore;

/// Shortest passphrase a vault accepts, in characters. Length is what resists offline guessing.
pub const MIN_PASSPHRASE_CHARS: usize = 12;
/// Argon2id memory cost of new locks, in KiB (64 MiB, affordable on an e-paper tablet).
const KDF_MEMORY_KIB: u32 = 64 * 1024;
/// Argon2id iterations of new locks.
const KDF_ITERATIONS: u32 = 3;
/// Argon2id parallelism of new locks.
const KDF_PARALLELISM: u32 = 1;
/// Associated data of a lock's ciphertext.
const LOCK_AAD: &[u8] = b"penroam/vault-lock/v1";
/// Associated data of an encrypted vault name.
const NAME_AAD: &[u8] = b"penroam/vault-name/v1";
/// File (in the library root) of [`NotebookOwners`].
const OWNERS_FILE: &str = "vault-notebooks.json";

/// Failures specific to vaults. Messages are safe to show to the user.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The passphrase doesn't open this lock (or the lock was altered: the two can't be told
    /// apart).
    #[error("that passphrase doesn't open this vault")]
    WrongPassphrase,
    /// A new passphrase is too short.
    #[error("use a passphrase of at least {MIN_PASSPHRASE_CHARS} characters")]
    WeakPassphrase,
    /// The stored lock isn't something this client will run.
    #[error("this vault's lock can't be used: {0}")]
    BadLock(String),
    /// The server gave a vault id that can't name a folder.
    #[error("the server sent an invalid vault id")]
    BadVaultId,
    /// A cryptographic primitive failed.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// Reading or writing the notebook owners file failed.
    #[error("vault files: {0}")]
    Io(#[from] io::Error),
}

/// Which notebooks an engine syncs and where it keeps its sync data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The signed-in account: every notebook not assigned to a vault.
    Account,
    /// One vault, by id: exactly the notebooks assigned to it.
    Vault(String),
}

impl Scope {
    /// A vault scope. The id comes from the server and becomes a folder name, so anything but
    /// letters, digits, `-` and `_` is refused.
    pub fn vault(id: &str) -> Result<Self, VaultError> {
        let ok = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if ok {
            Ok(Self::Vault(id.to_string()))
        } else {
            Err(VaultError::BadVaultId)
        }
    }

    /// This scope's sync folder inside a library.
    pub fn dir(&self, library_root: &Path) -> PathBuf {
        let base = SyncStore::dir_for(library_root);
        match self {
            Self::Account => base,
            Self::Vault(id) => base.join("vaults").join(id),
        }
    }

    /// The name of a secret in this scope, so a vault's keys never collide with the account's.
    pub fn secret_name(&self, base: &str) -> String {
        match self {
            Self::Account => base.to_string(),
            Self::Vault(id) => format!("vault-{id}-{base}"),
        }
    }

    /// Whether a notebook owned by `owner` (`None` = no vault) belongs to this scope.
    pub fn contains(&self, owner: Option<&str>) -> bool {
        match self {
            Self::Account => owner.is_none(),
            Self::Vault(id) => owner == Some(id.as_str()),
        }
    }
}

/// Vault ids of the vaults with a sync folder in this library.
pub fn local_vaults(library_root: &Path) -> Vec<String> {
    let dir = SyncStore::dir_for(library_root).join("vaults");
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("state.json").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    out.sort();
    out
}

/// Which vault each notebook belongs to, and the names of vaults opened here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotebookOwners {
    /// Notebook → vault id. Absent means an ordinary notebook.
    #[serde(default)]
    pub notebooks: BTreeMap<Uuid, String>,
    /// Vault id → name, as decrypted on this device.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

impl NotebookOwners {
    /// Reads the file; a missing file is an empty map. A damaged file is an error, never empty:
    /// treating it as empty would upload vault notebooks to the main account.
    pub fn load(library_root: &Path) -> io::Result<Self> {
        match std::fs::read(library_root.join(OWNERS_FILE)) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Writes the file atomically. Call with the library lock held.
    pub fn save(&self, library_root: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_atomic(&library_root.join(OWNERS_FILE), &bytes)
            .map_err(|e| io::Error::other(e.to_string()))
    }

    /// The vault a notebook belongs to.
    pub fn owner(&self, notebook: &Uuid) -> Option<&str> {
        self.notebooks.get(notebook).map(String::as_str)
    }
}

fn check_new_passphrase(passphrase: &str) -> Result<(), VaultError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(VaultError::WeakPassphrase);
    }
    Ok(())
}

/// The wrapping key. The passphrase is NFC-normalized first: the same word typed on two systems
/// can arrive as different code points, which would otherwise lock the user out.
fn derive_key(
    passphrase: &str,
    salt: &[u8],
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    let params = Params::new(memory_kib, iterations, parallelism, Some(KEY_LEN))
        .map_err(|e| VaultError::BadLock(e.to_string()))?;
    let text = Zeroizing::new(passphrase.nfc().collect::<String>());
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(text.as_bytes(), salt, key.as_mut())
        .map_err(|e| VaultError::BadLock(e.to_string()))?;
    Ok(key)
}

/// Wraps `root` with `passphrase`. Slow on purpose: run it off any UI thread.
pub fn new_lock(root: &RootSecret, passphrase: &str) -> Result<VaultLock, VaultError> {
    check_new_passphrase(passphrase)?;
    let salt: [u8; 16] = crypto::random_bytes()?;
    let key = derive_key(
        passphrase,
        &salt,
        KDF_MEMORY_KIB,
        KDF_ITERATIONS,
        KDF_PARALLELISM,
    )?;
    let secret = Zeroizing::new(root.to_bytes());
    let wrapped = crypto::seal(&key, &salt, LOCK_AAD, secret.as_ref());
    Ok(VaultLock {
        kdf: KDF_ARGON2ID.into(),
        memory_kib: KDF_MEMORY_KIB,
        iterations: KDF_ITERATIONS,
        parallelism: KDF_PARALLELISM,
        salt: base64url::encode(&salt),
        wrapped_root: base64url::encode(&wrapped),
    })
}

/// Opens a lock from the server with `passphrase`. The parameters are checked first, so a hostile
/// lock can't make the device allocate without limit.
pub fn open_lock(lock: &VaultLock, passphrase: &str) -> Result<RootSecret, VaultError> {
    lock.validate().map_err(VaultError::BadLock)?;
    let salt = base64url::decode(&lock.salt)
        .map_err(|_| VaultError::BadLock("salt is not base64url".into()))?;
    let wrapped = base64url::decode(&lock.wrapped_root)
        .map_err(|_| VaultError::BadLock("wrapped key is not base64url".into()))?;
    let key = derive_key(
        passphrase,
        &salt,
        lock.memory_kib,
        lock.iterations,
        lock.parallelism,
    )?;
    let bytes = Zeroizing::new(
        crypto::open(&key, LOCK_AAD, &wrapped).map_err(|_| VaultError::WrongPassphrase)?,
    );
    let secret: [u8; KEY_LEN] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::WrongPassphrase)?;
    Ok(RootSecret::from_bytes(secret))
}

/// What the user typed to open a vault.
pub enum VaultKey<'a> {
    /// The vault's passphrase.
    Passphrase(&'a str),
    /// The recovery code shown when the vault was created.
    RecoveryCode(&'a str),
}

/// The root secret of a vault. A recovery code isn't checked against the lock: it proves itself
/// when the vault's key log verifies against it.
pub fn root_for(lock: &VaultLock, key: &VaultKey<'_>) -> Result<RootSecret, VaultError> {
    match key {
        VaultKey::Passphrase(p) => open_lock(lock, p),
        VaultKey::RecoveryCode(c) => Ok(RootSecret::parse_recovery_code(c)?),
    }
}

/// Encrypts a vault's name for the server, under a key only the vault's root can derive.
pub fn seal_name(root: &RootSecret, name: &str) -> String {
    let key = root.vault_name_key();
    base64url::encode(&crypto::seal(&key, b"name", NAME_AAD, name.as_bytes()))
}

/// Decrypts [`seal_name`] output.
pub fn open_name(root: &RootSecret, sealed: &str) -> Result<String, VaultError> {
    let key = root.vault_name_key();
    let bytes = base64url::decode(sealed)
        .map_err(|_| VaultError::Crypto(CryptoError::Malformed("vault name")))?;
    let plain = crypto::open(&key, NAME_AAD, &bytes)?;
    String::from_utf8(plain).map_err(|_| VaultError::Crypto(CryptoError::Malformed("vault name")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "correct horse battery";

    #[test]
    fn a_lock_opens_only_with_its_passphrase() {
        let root = RootSecret::generate().unwrap();
        let lock = new_lock(&root, GOOD).unwrap();
        assert_eq!(open_lock(&lock, GOOD).unwrap().to_bytes(), root.to_bytes());
        assert!(matches!(
            open_lock(&lock, "correct horse battern"),
            Err(VaultError::WrongPassphrase)
        ));
    }

    #[test]
    fn composed_and_decomposed_input_open_the_same_lock() {
        let root = RootSecret::generate().unwrap();
        let lock = new_lock(&root, "caf\u{e9} au lait pour moi").unwrap();
        assert!(open_lock(&lock, "cafe\u{301} au lait pour moi").is_ok());
    }

    #[test]
    fn short_passphrases_are_refused() {
        let root = RootSecret::generate().unwrap();
        assert!(matches!(
            new_lock(&root, "too short"),
            Err(VaultError::WeakPassphrase)
        ));
    }

    #[test]
    fn a_tampered_lock_does_not_open() {
        let root = RootSecret::generate().unwrap();
        let mut lock = new_lock(&root, GOOD).unwrap();
        lock.salt = base64url::encode(&[7u8; 16]);
        assert!(open_lock(&lock, GOOD).is_err());
    }

    #[test]
    fn a_hostile_memory_cost_is_refused_before_any_work() {
        let root = RootSecret::generate().unwrap();
        let mut lock = new_lock(&root, GOOD).unwrap();
        lock.memory_kib = u32::MAX;
        assert!(matches!(
            open_lock(&lock, GOOD),
            Err(VaultError::BadLock(_))
        ));
    }

    #[test]
    fn names_are_readable_only_with_the_root() {
        let root = RootSecret::generate().unwrap();
        let other = RootSecret::generate().unwrap();
        let sealed = seal_name(&root, "Journal");
        assert!(!sealed.contains("Journal"));
        assert_eq!(open_name(&root, &sealed).unwrap(), "Journal");
        assert!(open_name(&other, &sealed).is_err());
    }

    #[test]
    fn server_ids_cannot_escape_the_sync_folder() {
        for bad in ["", "..", "../x", "a/b", "a\\b", "a.b"] {
            assert!(Scope::vault(bad).is_err(), "{bad:?}");
        }
        assert!(Scope::vault("v-0123abc").is_ok());
    }

    #[test]
    fn scopes_never_share_a_notebook() {
        let v = Scope::vault("v1").unwrap();
        assert!(Scope::Account.contains(None));
        assert!(!Scope::Account.contains(Some("v1")));
        assert!(v.contains(Some("v1")));
        assert!(!v.contains(None));
        assert!(!v.contains(Some("v2")));
    }
}
