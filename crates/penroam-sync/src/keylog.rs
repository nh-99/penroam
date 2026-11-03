//! The account key log: signed, hash-chained membership entries stored on
//! the server as opaque bytes.
//!
//! * Entry 0 (genesis) is signed by the account key derived from the
//!   recovery secret. It certifies the first device and grants epoch 1.
//! * `addDevice` certifies a new device and grants it every epoch key. It is
//!   signed by an active member device (pairing approval) or by the account
//!   key (enrollment with the recovery code).
//! * `removeDevice` deactivates a device and starts a new epoch whose key is
//!   wrapped only to the remaining devices and to the recovery key.
//!
//! Every entry names the SHA-256 of the previous entry's bytes, so the
//! server can withhold the tail of the log but cannot reorder, drop from the
//! middle, or inject entries. Clients pin the prefix they verified.
//!
//! [`make_entry`] builds and signs entries; [`verify_log`] replays a whole log
//! from genesis, checking every signature, signer authorization, chain link
//! and grant, and returns the resulting [`Membership`] (who is a member,
//! which epoch is current, and every wrapped epoch key). The engine
//! (`crate::engine`) re-verifies the log from scratch on every reconcile;
//! logs are small. Wire format: `docs/protocol/sync-protocol-v1.md` §14.

use crate::crypto::{self, CryptoError, DeviceKeys, EpochKey};
use ed25519_dalek::SigningKey;
use penroam_sync_protocol::base64url;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Signature domain for key-log entries (see [`crypto::sign`]).
const DOMAIN: &[u8] = b"penroam/keylog/v1\0";
/// `SignedEntry::signer` value meaning "signed by the account key" (derived
/// from the recovery secret) rather than by a member device.
pub const ACCOUNT_SIGNER: &str = "account";

/// Why a key log was rejected. Any of these means the log cannot be trusted;
/// the engine stops syncing and reports an integrity alert. Indexes are
/// positions in the log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyLogError {
    /// Not valid JSON/base64, wrong version, or an index out of place.
    #[error("key log entry {0} is malformed")]
    Malformed(u64),
    /// The signature does not verify, or the signer is not an active member.
    #[error("key log entry {0} has an invalid signature or an unauthorized signer")]
    BadSignature(u64),
    /// `prev` is not the hash of the previous entry (reordered or spliced log).
    #[error("key log entry {0} does not chain to the previous entry")]
    BrokenChain(u64),
    /// Genesis names a different account key than expected (from the
    /// recovery code or this device's pin).
    #[error("the key log does not belong to this account (account key mismatch)")]
    WrongAccount,
    /// The log is shorter than, or differs from, the prefix this device pinned.
    #[error("the server returned a key log that contradicts the one this device verified before")]
    Rewritten,
    /// Well-formed and signed, but breaks a membership rule (explained).
    #[error("key log entry {0} is not allowed: {1}")]
    Invalid(u64, &'static str),
    /// Key material inside an entry could not be decoded.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// A device certificate: binds a sync device id to its public keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCert {
    /// The sync device id (hyphenated UUID).
    pub device_id: String,
    /// Ed25519 public key, unpadded base64url.
    pub sign_pk: String,
    /// X25519 public key, unpadded base64url.
    pub dh_pk: String,
    /// User-chosen label (encrypted log, so not visible to the server).
    pub label: String,
}

impl DeviceCert {
    /// The certificate for `keys` with a user-chosen label.
    pub fn of(keys: &DeviceKeys, label: &str) -> Self {
        Self {
            device_id: keys.device_id.clone(),
            sign_pk: base64url::encode(&keys.sign_pk()),
            dh_pk: base64url::encode(&keys.dh_pk()),
            label: label.to_string(),
        }
    }
    /// Decoded Ed25519 public key.
    pub fn sign_pk(&self) -> Result<[u8; 32], CryptoError> {
        crypto::decode32(&self.sign_pk)
    }
    /// Decoded X25519 public key.
    pub fn dh_pk(&self) -> Result<[u8; 32], CryptoError> {
        crypto::decode32(&self.dh_pk)
    }
    /// The fingerprint shown in pairing codes and device lists.
    pub fn fingerprint(&self) -> Result<String, CryptoError> {
        Ok(crypto::fingerprint(&self.sign_pk()?, &self.dh_pk()?))
    }
}

/// An epoch key wrapped to one device (or to the recovery key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyGrant {
    /// Device id, or `"recovery"`.
    pub to: String,
    /// The epoch this key belongs to.
    pub epoch: u32,
    /// [`crypto::wrap_to`] output for the recipient, unpadded base64url.
    pub wrapped: String,
}

/// [`KeyGrant::to`] value for the grant wrapped to the recovery key.
pub const RECOVERY: &str = "recovery";

/// What an entry does. Who may sign each kind is enforced by [`verify_log`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EntryKind {
    #[serde(rename_all = "camelCase")]
    /// Entry 0 only, signed by the account key: creates the account.
    Genesis {
        /// Account Ed25519 public key (base64url).
        account_pk: String,
        /// Recovery X25519 public key (base64url).
        recovery_pk: String,
        /// The first device.
        device: DeviceCert,
        /// Epoch 1 wrapped to the first device and to the recovery key.
        grants: Vec<KeyGrant>,
    },
    #[serde(rename_all = "camelCase")]
    /// Certifies a new device; signed by an active member or the account key.
    AddDevice {
        /// The new device.
        device: DeviceCert,
        /// Existing epoch keys wrapped to the new device only.
        grants: Vec<KeyGrant>,
    },
    #[serde(rename_all = "camelCase")]
    /// Deactivates a device and starts a new epoch; signed by an active
    /// member or the account key.
    RemoveDevice {
        /// The device to deactivate.
        device_id: String,
        /// Must be the current epoch plus one.
        new_epoch: u32,
        /// The new epoch key wrapped to every remaining device and to recovery.
        grants: Vec<KeyGrant>,
        /// Highest sequence of the removed device the remover had received
        /// when it removed it. Clients refuse that device's operations above
        /// it, so a misbehaving server cannot deliver writes it made after
        /// removal. Absent in entries written before this rule existed (then
        /// no cutoff is enforced).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_seq: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// The signed content of an entry.
pub struct EntryBody {
    /// Entry format version (1).
    pub v: u32,
    /// Position in the log; must equal the entry's actual position.
    pub index: u64,
    /// Hex SHA-256 of the previous entry's bytes ("" for genesis).
    pub prev: String,
    /// What the entry does.
    pub entry: EntryKind,
}

/// Wire form of an entry (its JSON bytes are what the server stores).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedEntry {
    /// base64url of the exact signed body JSON bytes.
    pub body: String,
    /// Signing device id or `"account"`.
    pub signer: String,
    /// Ed25519 signature over `DOMAIN ‖ body`, unpadded base64url.
    pub sig: String,
}

/// Associated data for wrapping an epoch key: binds the wrapped bytes to the
/// epoch and recipient, so a grant cannot be replayed as another one.
pub fn grant_context(epoch: u32, to: &str) -> Vec<u8> {
    format!("penroam/epoch-grant/v1/{epoch}/{to}").into_bytes()
}

/// Wraps an epoch key to a recipient's X25519 public key.
pub fn grant(
    epoch: u32,
    key: &EpochKey,
    to: &str,
    dh_pk: &[u8; 32],
) -> Result<KeyGrant, CryptoError> {
    Ok(KeyGrant {
        to: to.to_string(),
        epoch,
        wrapped: base64url::encode(&crypto::wrap_to(dh_pk, &grant_context(epoch, to), &key.0)?),
    })
}

/// Serializes and signs an entry; returns the bytes to store.
pub fn make_entry(
    index: u64,
    prev_hash: &str,
    entry: EntryKind,
    signer: &str,
    key: &SigningKey,
) -> Vec<u8> {
    let body = serde_json::to_vec(&EntryBody {
        v: 1,
        index,
        prev: prev_hash.to_string(),
        entry,
    })
    .expect("entry serializes");
    let sig = crypto::sign(key, DOMAIN, &body);
    serde_json::to_vec(&SignedEntry {
        body: base64url::encode(&body),
        signer: signer.to_string(),
        sig: base64url::encode(&sig),
    })
    .expect("entry serializes")
}

/// A device as recorded in the verified log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its certificate.
    pub cert: DeviceCert,
    /// False once a `removeDevice` entry deactivated it.
    pub active: bool,
    /// Index of the entry that added it.
    pub added_at: u64,
    /// Who signed that entry (device id or [`ACCOUNT_SIGNER`]).
    pub added_by: String,
    /// For a removed device: its last accepted sequence (see
    /// [`EntryKind::RemoveDevice`]); operations above it are refused.
    pub removed_after: Option<u64>,
}

impl Member {
    /// Whether an operation with `device_seq` from this device may be applied:
    /// always for active devices; for removed ones only up to the recorded
    /// cutoff (or, for old entries without a cutoff, always).
    pub fn may_write(&self, device_seq: u64) -> bool {
        self.active || self.removed_after.is_none_or(|last| device_seq <= last)
    }
}

/// The verified state of a key log.
#[derive(Debug, Clone, Default)]
pub struct Membership {
    /// Account Ed25519 public key from genesis.
    pub account_pk: [u8; 32],
    /// Recovery X25519 public key from genesis.
    pub recovery_pk: [u8; 32],
    /// Every device ever added, by id (removed ones have `active == false`).
    pub devices: BTreeMap<String, Member>,
    /// The epoch new writes use.
    pub current_epoch: u32,
    /// All grants, by epoch.
    pub grants: BTreeMap<u32, Vec<KeyGrant>>,
    /// Number of verified entries.
    pub len: u64,
    /// Hex SHA-256 of the last verified entry (what a [`Pin`] records).
    pub head: String,
}

impl Membership {
    /// Whether `device` is a current member.
    pub fn is_active(&self, device: &str) -> bool {
        self.devices.get(device).is_some_and(|m| m.active)
    }

    /// Current members.
    pub fn active_devices(&self) -> impl Iterator<Item = &Member> {
        self.devices.values().filter(|m| m.active)
    }

    /// Every epoch key granted to `keys`' device. Grants that fail to unwrap
    /// are skipped; a missing current epoch is reported by the engine.
    pub fn epoch_keys_for(&self, keys: &DeviceKeys) -> BTreeMap<u32, EpochKey> {
        let mut out = BTreeMap::new();
        for (epoch, gs) in &self.grants {
            for g in gs.iter().filter(|g| g.to == keys.device_id) {
                if let Ok(w) = base64url::decode(&g.wrapped) {
                    if let Ok(k) = crypto::unwrap_with(&keys.dh, &grant_context(*epoch, &g.to), &w)
                    {
                        if let Ok(k) = <[u8; 32]>::try_from(k.as_slice()) {
                            out.insert(*epoch, EpochKey(k));
                        }
                    }
                }
            }
        }
        out
    }

    /// Every epoch key, recovered with the recovery secret.
    pub fn epoch_keys_with_recovery(
        &self,
        dh: &x25519_dalek::StaticSecret,
    ) -> BTreeMap<u32, EpochKey> {
        let mut out = BTreeMap::new();
        for (epoch, gs) in &self.grants {
            for g in gs.iter().filter(|g| g.to == RECOVERY) {
                if let Some(k) = base64url::decode(&g.wrapped)
                    .ok()
                    .and_then(|w| {
                        crypto::unwrap_with(dh, &grant_context(*epoch, RECOVERY), &w).ok()
                    })
                    .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok())
                {
                    out.insert(*epoch, EpochKey(k));
                }
            }
        }
        out
    }
}

/// A previously verified prefix (index count + last hash) to pin against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
///
/// Stored in the sync state after each successful verification. A later log
/// must contain this prefix unchanged ([`KeyLogError::Rewritten`] otherwise),
/// which stops a server from rolling membership back or swapping history.
pub struct Pin {
    /// Account public key, base64url.
    pub account_pk: String,
    /// Number of entries verified.
    pub len: u64,
    /// Hash of the last verified entry.
    pub head: String,
}

/// Verifies a complete key log (entries 0..n as stored bytes).
/// `expected_account` (from the recovery code or a pin) must match genesis.
///
/// With neither `expected_account` nor `pin` (a new device that is pairing),
/// the account key is trusted on first use; the pairing comparison code is
/// what detects a substituted log in that case.
pub fn verify_log(
    entries: &[Vec<u8>],
    expected_account: Option<&[u8; 32]>,
    pin: Option<&Pin>,
) -> Result<Membership, KeyLogError> {
    let mut m = Membership::default();
    let mut prev = String::new();
    for (i, raw) in entries.iter().enumerate() {
        let i = i as u64;
        let se: SignedEntry = serde_json::from_slice(raw).map_err(|_| KeyLogError::Malformed(i))?;
        let body_bytes = base64url::decode(&se.body).map_err(|_| KeyLogError::Malformed(i))?;
        let sig = base64url::decode(&se.sig).map_err(|_| KeyLogError::Malformed(i))?;
        let body: EntryBody =
            serde_json::from_slice(&body_bytes).map_err(|_| KeyLogError::Malformed(i))?;
        if body.v != 1 || body.index != i {
            return Err(KeyLogError::Malformed(i));
        }
        if body.prev != prev {
            return Err(KeyLogError::BrokenChain(i));
        }
        // signer authorization: only genesis is self-certifying (it introduces
        // the account key); every later entry must be signed by the account key
        // or by a device that is active *at this point* in the log, so a removed
        // device can never sign again
        let signer_pk = if se.signer == ACCOUNT_SIGNER {
            match (&body.entry, i) {
                (EntryKind::Genesis { account_pk, .. }, 0) => crypto::decode32(account_pk)?,
                (EntryKind::Genesis { .. }, _) => {
                    return Err(KeyLogError::Invalid(i, "second genesis"))
                }
                _ if i == 0 => return Err(KeyLogError::Invalid(i, "log must start with genesis")),
                _ => m.account_pk,
            }
        } else {
            if i == 0 {
                return Err(KeyLogError::Invalid(
                    i,
                    "genesis must be signed by the account key",
                ));
            }
            let member = m
                .devices
                .get(&se.signer)
                .filter(|d| d.active)
                .ok_or(KeyLogError::BadSignature(i))?;
            member.cert.sign_pk()?
        };
        crypto::verify(&signer_pk, DOMAIN, &body_bytes, &sig)
            .map_err(|_| KeyLogError::BadSignature(i))?;
        match body.entry {
            EntryKind::Genesis {
                account_pk,
                recovery_pk,
                device,
                grants,
            } => {
                m.account_pk = crypto::decode32(&account_pk)?;
                m.recovery_pk = crypto::decode32(&recovery_pk)?;
                if let Some(exp) = expected_account {
                    if *exp != m.account_pk {
                        return Err(KeyLogError::WrongAccount);
                    }
                }
                check_grants(i, &grants, 1, &[device.device_id.as_str(), RECOVERY])?;
                m.current_epoch = 1;
                m.grants.entry(1).or_default().extend(grants);
                m.devices.insert(
                    device.device_id.clone(),
                    Member {
                        cert: device,
                        active: true,
                        added_at: 0,
                        added_by: ACCOUNT_SIGNER.into(),
                        removed_after: None,
                    },
                );
            }
            EntryKind::AddDevice { device, grants } => {
                if m.devices.contains_key(&device.device_id) {
                    return Err(KeyLogError::Invalid(i, "device id already used"));
                }
                device.sign_pk()?;
                device.dh_pk()?;
                for g in &grants {
                    if g.to != device.device_id || g.epoch == 0 || g.epoch > m.current_epoch {
                        return Err(KeyLogError::Invalid(
                            i,
                            "grant for another device or unknown epoch",
                        ));
                    }
                }
                for g in grants {
                    m.grants.entry(g.epoch).or_default().push(g);
                }
                m.devices.insert(
                    device.device_id.clone(),
                    Member {
                        cert: device,
                        active: true,
                        added_at: i,
                        added_by: se.signer.clone(),
                        removed_after: None,
                    },
                );
            }
            EntryKind::RemoveDevice {
                device_id,
                new_epoch,
                grants,
                last_seq,
            } => {
                if !m.is_active(&device_id) {
                    return Err(KeyLogError::Invalid(i, "device is not an active member"));
                }
                if new_epoch != m.current_epoch + 1 {
                    return Err(KeyLogError::Invalid(i, "epoch must increase by one"));
                }
                let removed = m.devices.get_mut(&device_id).unwrap();
                removed.active = false;
                removed.removed_after = last_seq;
                let remaining: Vec<String> = m
                    .active_devices()
                    .map(|d| d.cert.device_id.clone())
                    .collect();
                let mut allowed: Vec<&str> = remaining.iter().map(String::as_str).collect();
                allowed.push(RECOVERY);
                check_grants(i, &grants, new_epoch, &allowed)?;
                m.current_epoch = new_epoch;
                m.grants.entry(new_epoch).or_default().extend(grants);
            }
        }
        // the next entry must name the hash of this entry's exact stored bytes
        prev = crypto::sha256_hex(raw);
        m.len = i + 1;
        m.head = prev.clone();
        if let Some(p) = pin {
            if p.len == m.len && p.head != m.head {
                return Err(KeyLogError::Rewritten);
            }
        }
    }
    if let Some(p) = pin {
        if m.len < p.len {
            return Err(KeyLogError::Rewritten);
        }
        if base64url::encode(&m.account_pk) != p.account_pk {
            return Err(KeyLogError::WrongAccount);
        }
    }
    Ok(m)
}

/// Grants of a new epoch: only to `allowed` recipients, for `epoch`, and always
/// including the recovery key, so no epoch can become unrecoverable.
fn check_grants(
    i: u64,
    grants: &[KeyGrant],
    epoch: u32,
    allowed: &[&str],
) -> Result<(), KeyLogError> {
    for g in grants {
        if g.epoch != epoch || !allowed.contains(&g.to.as_str()) {
            return Err(KeyLogError::Invalid(
                i,
                "grant for an unexpected recipient or epoch",
            ));
        }
    }
    if !grants.iter().any(|g| g.to == RECOVERY) {
        return Err(KeyLogError::Invalid(i, "every epoch must be recoverable"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::RootSecret;

    fn genesis(root: &RootSecret, dev: &DeviceKeys, k1: &EpochKey) -> Vec<u8> {
        let rec_pk = x25519_dalek::PublicKey::from(&root.recovery_dh()).to_bytes();
        let entry = EntryKind::Genesis {
            account_pk: base64url::encode(&root.account_signing_key().verifying_key().to_bytes()),
            recovery_pk: base64url::encode(&rec_pk),
            device: DeviceCert::of(dev, "laptop"),
            grants: vec![
                grant(1, k1, &dev.device_id, &dev.dh_pk()).unwrap(),
                grant(1, k1, RECOVERY, &rec_pk).unwrap(),
            ],
        };
        make_entry(0, "", entry, ACCOUNT_SIGNER, &root.account_signing_key())
    }

    #[test]
    fn genesis_add_remove_and_keys() {
        let root = RootSecret::generate().unwrap();
        let a = DeviceKeys::generate("dev-a".into()).unwrap();
        let b = DeviceKeys::generate("dev-b".into()).unwrap();
        let k1 = EpochKey::generate().unwrap();
        let g = genesis(&root, &a, &k1);
        let add = make_entry(
            1,
            &crypto::sha256_hex(&g),
            EntryKind::AddDevice {
                device: DeviceCert::of(&b, "tablet"),
                grants: vec![grant(1, &k1, "dev-b", &b.dh_pk()).unwrap()],
            },
            "dev-a",
            &a.signing,
        );
        let log = vec![g.clone(), add.clone()];
        let acct = root.account_signing_key().verifying_key().to_bytes();
        let m = verify_log(&log, Some(&acct), None).unwrap();
        assert!(m.is_active("dev-b"));
        assert_eq!(m.epoch_keys_for(&b)[&1].0, k1.0);
        assert_eq!(m.epoch_keys_with_recovery(&root.recovery_dh())[&1].0, k1.0);
        // removal: new epoch for remaining devices only
        let k2 = EpochKey::generate().unwrap();
        let rec_pk = m.recovery_pk;
        let rm = make_entry(
            2,
            &crypto::sha256_hex(&add),
            EntryKind::RemoveDevice {
                device_id: "dev-b".into(),
                new_epoch: 2,
                grants: vec![
                    grant(2, &k2, "dev-a", &a.dh_pk()).unwrap(),
                    grant(2, &k2, RECOVERY, &rec_pk).unwrap(),
                ],
                last_seq: Some(7),
            },
            "dev-a",
            &a.signing,
        );
        let log3 = vec![g.clone(), add.clone(), rm.clone()];
        let m = verify_log(&log3, Some(&acct), None).unwrap();
        assert!(!m.is_active("dev-b"));
        assert!(!m.epoch_keys_for(&b).contains_key(&2));
        assert_eq!(m.epoch_keys_for(&a)[&2].0, k2.0);
        // a removed device can no longer sign entries
        let c = DeviceKeys::generate("dev-c".into()).unwrap();
        let sneaky = make_entry(
            3,
            &crypto::sha256_hex(&rm),
            EntryKind::AddDevice {
                device: DeviceCert::of(&c, "evil"),
                grants: vec![],
            },
            "dev-b",
            &b.signing,
        );
        assert_eq!(
            verify_log(&[g.clone(), add.clone(), rm, sneaky], Some(&acct), None).unwrap_err(),
            KeyLogError::BadSignature(3)
        );
    }

    #[test]
    fn server_cannot_inject_or_rewrite() {
        let root = RootSecret::generate().unwrap();
        let a = DeviceKeys::generate("dev-a".into()).unwrap();
        let k1 = EpochKey::generate().unwrap();
        let g = genesis(&root, &a, &k1);
        let acct = root.account_signing_key().verifying_key().to_bytes();
        // forged genesis from another "account"
        let other = RootSecret::generate().unwrap();
        let fake = genesis(&other, &a, &k1);
        assert_eq!(
            verify_log(std::slice::from_ref(&fake), Some(&acct), None).unwrap_err(),
            KeyLogError::WrongAccount
        );
        // device added with a key signed by nobody in the log
        let mallory = DeviceKeys::generate("mallory".into()).unwrap();
        let inj = make_entry(
            1,
            &crypto::sha256_hex(&g),
            EntryKind::AddDevice {
                device: DeviceCert::of(&mallory, "x"),
                grants: vec![],
            },
            "mallory",
            &mallory.signing,
        );
        assert!(verify_log(&[g.clone(), inj], Some(&acct), None).is_err());
        // pinned prefix: shorter log or different head is rejected
        let m = verify_log(std::slice::from_ref(&g), Some(&acct), None).unwrap();
        let pin = Pin {
            account_pk: base64url::encode(&acct),
            len: 1,
            head: m.head.clone(),
        };
        assert_eq!(
            verify_log(&[], None, Some(&pin)).unwrap_err(),
            KeyLogError::Rewritten
        );
        let g2 = genesis(&root, &a, &EpochKey::generate().unwrap());
        assert_eq!(
            verify_log(&[g2], None, Some(&pin)).unwrap_err(),
            KeyLogError::Rewritten
        );
    }
}
