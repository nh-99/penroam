//! Account key copies and vaults (capabilities `account-key.v1` and `vaults.v1`, protocol 1.6).
//!
//! **Account key.** By default an account's notebooks sync with nothing more than a sign-in.
//! To allow that, the first device stores the account's root secret on the server, and every
//! later device fetches it after signing in. The server can therefore read those notebooks.
//!
//! **Vaults.** A vault holds notebooks whose keys the server never receives. On the wire a vault
//! is a hidden second account of the same user, with its own operation log, key log, blobs and
//! checkpoints, so nothing about ordinary sync changes inside it. The vault's root secret reaches
//! the server only inside a [`VaultLock`], wrapped with a key derived from the user's passphrase.
//! A vault never has an account key copy: a server that offers one for a vault account breaks
//! the feature's whole promise, so `GET` and `PUT` of the account key must refuse vault sessions.
//!
//! A passphrase can be guessed offline by whoever holds the lock, which includes the server. The
//! lock's strength is the passphrase's strength. See `docs/security/threat-model.md`, and
//! `docs/protocol/sync-protocol-v1.md` §12e for the routes.

use serde::{Deserialize, Serialize};

use crate::ids::AccountId;

/// The only key derivation function a [`VaultLock`] may name today.
pub const KDF_ARGON2ID: &str = "argon2id";
/// Largest Argon2id memory cost a client accepts, in KiB (1 GiB). A hostile lock must not be able
/// to exhaust a device's memory.
pub const MAX_KDF_MEMORY_KIB: u32 = 1 << 20;
/// Largest Argon2id iteration count a client accepts.
pub const MAX_KDF_ITERATIONS: u32 = 16;
/// Largest Argon2id parallelism a client accepts.
pub const MAX_KDF_PARALLELISM: u32 = 8;
/// Largest encoded [`VaultLock`] or vault name a server stores, in bytes.
pub const MAX_VAULT_FIELD_BYTES: usize = 4096;

/// The account's root secret, as stored on the server (`GET`/`PUT /v1/account/key`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyDocument {
    /// Base64url of the 32-byte root secret. Servers treat it as opaque, and must never log it.
    pub key: String,
}

impl std::fmt::Debug for AccountKeyDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountKeyDocument { key: <redacted> }")
    }
}

/// A vault's root secret wrapped with a key derived from its passphrase. Opaque to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultLock {
    /// Key derivation function. Always [`KDF_ARGON2ID`] today; clients refuse anything else.
    pub kdf: String,
    /// Argon2id memory cost in KiB.
    pub memory_kib: u32,
    /// Argon2id iterations.
    pub iterations: u32,
    /// Argon2id parallelism.
    pub parallelism: u32,
    /// Base64url of the random salt.
    pub salt: String,
    /// Base64url of `nonce ‖ ciphertext ‖ tag` holding the root secret.
    pub wrapped_root: String,
}

impl VaultLock {
    /// Checks the parameters a client is willing to run. Call before deriving a key, since the
    /// lock comes from the server.
    pub fn validate(&self) -> Result<(), String> {
        if self.kdf != KDF_ARGON2ID {
            return Err(format!("unknown key derivation function {:?}", self.kdf));
        }
        if self.memory_kib == 0 || self.memory_kib > MAX_KDF_MEMORY_KIB {
            return Err("memory cost out of range".into());
        }
        if self.iterations == 0 || self.iterations > MAX_KDF_ITERATIONS {
            return Err("iteration count out of range".into());
        }
        if self.parallelism == 0 || self.parallelism > MAX_KDF_PARALLELISM {
            return Err("parallelism out of range".into());
        }
        if self.salt.len() > MAX_VAULT_FIELD_BYTES
            || self.wrapped_root.len() > MAX_VAULT_FIELD_BYTES
        {
            return Err("field too long".into());
        }
        Ok(())
    }
}

/// `POST /v1/vaults`: creates a vault account for the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVaultRequest {
    /// The passphrase lock.
    pub lock: VaultLock,
    /// The vault's name, encrypted by the client. Opaque to the server.
    pub name: String,
}

/// One vault of an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRecord {
    /// The vault's own account id, which is also the vault's id.
    pub vault_id: AccountId,
    /// The passphrase lock.
    pub lock: VaultLock,
    /// The vault's name, encrypted by the client. Opaque to the server.
    pub name: String,
    /// Creation time (display only).
    pub created_at_unix_s: u64,
}

/// `GET /v1/vaults`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultListResponse {
    /// Every vault of the account, oldest first.
    pub vaults: Vec<VaultRecord>,
}

/// `PUT /v1/vaults/{id}`: replaces the lock after a passphrase change. It's stored as given, so
/// the client must already hold the root secret. A server could put an old lock back, which would
/// only bring back the old passphrase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateVaultLockRequest {
    /// The new passphrase lock.
    pub lock: VaultLock,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock() -> VaultLock {
        VaultLock {
            kdf: KDF_ARGON2ID.into(),
            memory_kib: 65536,
            iterations: 3,
            parallelism: 1,
            salt: "c2FsdA".into(),
            wrapped_root: "AAAA".into(),
        }
    }

    #[test]
    fn hostile_kdf_parameters_are_refused() {
        assert!(lock().validate().is_ok());
        for bad in [
            VaultLock {
                kdf: "pbkdf2".into(),
                ..lock()
            },
            VaultLock {
                memory_kib: 0,
                ..lock()
            },
            VaultLock {
                memory_kib: MAX_KDF_MEMORY_KIB + 1,
                ..lock()
            },
            VaultLock {
                iterations: 0,
                ..lock()
            },
            VaultLock {
                iterations: MAX_KDF_ITERATIONS + 1,
                ..lock()
            },
            VaultLock {
                parallelism: MAX_KDF_PARALLELISM + 1,
                ..lock()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_account_key_never_appears_in_debug_output() {
        let d = AccountKeyDocument {
            key: "secret-bytes".into(),
        };
        assert!(!format!("{d:?}").contains("secret-bytes"));
    }
}
