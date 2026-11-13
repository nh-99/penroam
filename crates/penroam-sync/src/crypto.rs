//! Cryptographic building blocks for sync. **Not independently reviewed**
//! (`security_review_pending`): see `docs/security/threat-model.md`. The exact
//! constructions are specified in `docs/protocol/sync-protocol-v1.md` §13–§15,
//! and `tests/vectors.rs` pins their output byte for byte.
//!
//! Key hierarchy, from the top:
//!
//! * [`RootSecret`] — 32 random bytes. An account stores it on the server so that
//!   signing in is enough to add a device (`account-key.v1`). A vault's reaches
//!   the server only inside a passphrase lock ([`crate::vault`]), and the user
//!   sees it once as the vault's recovery code. It derives the *account signing
//!   key* (signs the key-log genesis and enrollments) and the *recovery
//!   key-agreement key* (every epoch key is also wrapped to it). Devices never
//!   store it.
//! * [`EpochKey`] — a random content key per epoch, shared with member devices
//!   through the key log ([`crate::keylog`]). Per-notebook payload keys, blob
//!   keys, routing aliases and blob ids are derived from it with HKDF/HMAC, so
//!   the server never learns notebook ids.
//! * [`DeviceKeys`] — each sync device's Ed25519 signing key and X25519
//!   key-agreement key, kept only in the platform secret store
//!   ([`crate::secrets`]).
//!
//! Primitives (all from audited RustCrypto / dalek crates):
//! * Ed25519 signatures (`ed25519-dalek`) for device envelopes and key-log entries.
//! * X25519 (`x25519-dalek`) + HKDF-SHA256 + XChaCha20-Poly1305 to wrap keys to a device.
//! * XChaCha20-Poly1305 for operation payloads and blobs; associated data is the
//!   protocol's canonical AAD bytes.
//! * HKDF-SHA256 with distinct `info` labels for every derived key.
//!
//! Nonces for payloads are derived (HMAC of the key, the operation id and the
//! plaintext digest), so re-sealing the same operation after a lost response
//! yields the same ciphertext — the server's idempotency check depends on it —
//! while two different plaintexts never share a nonce.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use penroam_sync_protocol::base64url;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Length of every symmetric key and of the root secret, in bytes.
pub const KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 nonce length (prefixed to every ciphertext).
pub const NONCE_LEN: usize = 24;
/// Poly1305 authentication tag length (suffixed to every ciphertext).
pub const TAG_LEN: usize = 16;

/// Failures of the primitives in this module. Messages are safe to show to the
/// user; they never contain key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The recovery code has a wrong prefix, length, character or checksum.
    #[error("the recovery code is not valid (check for typos)")]
    BadRecoveryCode,
    /// AEAD decryption failed: the ciphertext or its associated data was
    /// altered, or the key is wrong. Never distinguish the two to callers.
    #[error("authentication failed: the data was altered or the key is wrong")]
    Decrypt,
    /// An Ed25519 signature did not verify (or was not 64 bytes).
    #[error("signature verification failed")]
    BadSignature,
    /// Key material could not be decoded (wrong length or encoding).
    #[error("malformed key material: {0}")]
    Malformed(&'static str),
    /// The OS random number generator returned an error. Nothing is
    /// generated from a weaker source.
    #[error("the operating system random generator failed")]
    Random,
}

/// `N` bytes from the operating system CSPRNG (`getrandom`).
pub fn random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).map_err(|_| CryptoError::Random)?;
    Ok(b)
}

/// SHA-256 digest.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// SHA-256 digest as 64 lowercase hex characters (the protocol's hash format).
pub fn sha256_hex(data: &[u8]) -> String {
    penroam_sync_protocol::hex::encode(&sha256(data))
}

/// HKDF-SHA256 expanded to 32 bytes. Every caller uses a distinct `info`
/// label so derived keys for different purposes are independent.
fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, &mut out)
        .expect("32 bytes is a valid HKDF length");
    out
}

/// HMAC-SHA256 over length-prefixed parts. The `u64` length prefix keeps the
/// encoding unambiguous: `("ab", "c")` and `("a", "bc")` give different MACs.
fn hmac32(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    for p in parts {
        m.update(&(p.len() as u64).to_le_bytes());
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

// ------------------------------------------------------------ root secret --

/// The root secret of an account or a vault. Devices do not keep it. Everything
/// at that level derives from it.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RootSecret([u8; KEY_LEN]);

/// Crockford base32 alphabet (no I, L, O, U).
const B32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_PREFIX: &str = "PRK1";

impl RootSecret {
    /// A new random root secret (account creation).
    pub fn generate() -> Result<Self, CryptoError> {
        Ok(Self(random_bytes()?))
    }

    /// Wraps existing bytes (tests and vectors; real code parses a recovery code).
    pub fn from_bytes(b: [u8; KEY_LEN]) -> Self {
        Self(b)
    }

    /// `PRK1-XXXXX-XXXXX-…`: 32 secret bytes + 2 checksum bytes in Crockford
    /// base32, groups of five.
    pub fn recovery_code(&self) -> String {
        let mut data = self.0.to_vec();
        data.extend_from_slice(
            &sha256(&[b"penroam/recovery-check/v1".as_slice(), &self.0].concat())[..2],
        );
        let mut bits = 0u32;
        let mut nbits = 0;
        let mut chars = String::new();
        for b in data {
            bits = (bits << 8) | b as u32;
            nbits += 8;
            while nbits >= 5 {
                nbits -= 5;
                chars.push(B32[((bits >> nbits) & 31) as usize] as char);
            }
        }
        if nbits > 0 {
            chars.push(B32[((bits << (5 - nbits)) & 31) as usize] as char);
        }
        let groups: Vec<String> = chars
            .as_bytes()
            .chunks(5)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        format!("{CODE_PREFIX}-{}", groups.join("-"))
    }

    /// The secret bytes, for storing on the server (`account-key.v1`) or wrapping in a passphrase
    /// lock. Never log the result.
    pub fn to_bytes(&self) -> [u8; KEY_LEN] {
        self.0
    }

    /// Parses a recovery code typed by a person: case, spaces and dashes are
    /// ignored; `I`/`L` read as 1 and `O` as 0.
    pub fn parse_recovery_code(code: &str) -> Result<Self, CryptoError> {
        let clean: String = code
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let body = clean
            .strip_prefix(CODE_PREFIX)
            .ok_or(CryptoError::BadRecoveryCode)?;
        let mut bits = 0u32;
        let mut nbits = 0;
        let mut out = Vec::new();
        for c in body.chars() {
            let c = match c {
                'I' | 'L' => '1',
                'O' => '0',
                c => c,
            };
            let v = B32
                .iter()
                .position(|b| *b as char == c)
                .ok_or(CryptoError::BadRecoveryCode)? as u32;
            bits = (bits << 5) | v;
            nbits += 5;
            if nbits >= 8 {
                nbits -= 8;
                out.push((bits >> nbits) as u8);
            }
        }
        if out.len() != KEY_LEN + 2 {
            return Err(CryptoError::BadRecoveryCode);
        }
        let secret: [u8; KEY_LEN] = out[..KEY_LEN].try_into().unwrap();
        let check = &sha256(&[b"penroam/recovery-check/v1".as_slice(), &secret].concat())[..2];
        if check != &out[KEY_LEN..] {
            return Err(CryptoError::BadRecoveryCode);
        }
        Ok(Self(secret))
    }

    /// The account signing key: signs the key-log genesis and recovery
    /// enrollments.
    pub fn account_signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&hkdf32(&self.0, b"penroam", b"penroam/account-signing/v1"))
    }

    /// The recovery key-agreement key: every epoch key is also wrapped to its
    /// public half, so the recovery code can always restore access.
    pub fn recovery_dh(&self) -> x25519_dalek::StaticSecret {
        x25519_dalek::StaticSecret::from(hkdf32(&self.0, b"penroam", b"penroam/recovery-dh/v1"))
    }

    /// The key a vault's name is encrypted under ([`crate::vault::seal_name`]).
    pub fn vault_name_key(&self) -> [u8; KEY_LEN] {
        hkdf32(&self.0, b"penroam", b"penroam/vault-name/v1")
    }

    /// The key a shared notebook space's sealed title is encrypted under
    /// ([`crate::shared::seal_meta`]). Only members, who hold the space's root, can derive it.
    pub fn space_meta_key(&self) -> [u8; KEY_LEN] {
        hkdf32(&self.0, b"penroam", b"penroam/space-meta/v1")
    }

    /// The key a vault's notebooks are encrypted under on a device while the vault is locked
    /// there ([`crate::locker`]). Never sent anywhere.
    pub fn vault_local_key(&self) -> [u8; KEY_LEN] {
        hkdf32(&self.0, b"penroam", b"penroam/vault-local/v1")
    }
}

// ----------------------------------------------------------- device keys --

/// A sync device's long-term keys. Stored only in the platform secret store.
///
/// The sync device is created at enrollment and is distinct from the local
/// library's device id; its id appears in envelope headers and key-log
/// certificates.
pub struct DeviceKeys {
    /// Hyphenated lowercase UUID identifying this sync device.
    pub device_id: String,
    /// Signs envelopes and this device's key-log entries.
    pub signing: SigningKey,
    /// Receives epoch keys wrapped to this device.
    pub dh: x25519_dalek::StaticSecret,
}

impl DeviceKeys {
    /// Fresh random keys for a new sync device.
    pub fn generate(device_id: String) -> Result<Self, CryptoError> {
        Ok(Self {
            device_id,
            signing: SigningKey::from_bytes(&random_bytes()?),
            dh: x25519_dalek::StaticSecret::from(random_bytes::<32>()?),
        })
    }

    /// Ed25519 public key (what other devices verify signatures with).
    pub fn sign_pk(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// X25519 public key (what epoch keys are wrapped to).
    pub fn dh_pk(&self) -> [u8; 32] {
        x25519_dalek::PublicKey::from(&self.dh).to_bytes()
    }

    /// Secret-store encoding: `device_id \n b64(sign) \n b64(dh)`.
    pub fn to_secret_string(&self) -> String {
        format!(
            "{}\n{}\n{}",
            self.device_id,
            base64url::encode(&self.signing.to_bytes()),
            base64url::encode(&self.dh.to_bytes())
        )
    }

    /// Parses [`DeviceKeys::to_secret_string`] output.
    pub fn from_secret_string(s: &str) -> Result<Self, CryptoError> {
        let mut it = s.lines();
        let id = it
            .next()
            .ok_or(CryptoError::Malformed("device id"))?
            .to_string();
        let sk: [u8; 32] = decode32(it.next().unwrap_or(""))?;
        let dh: [u8; 32] = decode32(it.next().unwrap_or(""))?;
        Ok(Self {
            device_id: id,
            signing: SigningKey::from_bytes(&sk),
            dh: x25519_dalek::StaticSecret::from(dh),
        })
    }

    /// Human-comparable fingerprint of this device's public keys.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.sign_pk(), &self.dh_pk())
    }
}

/// Decodes unpadded base64url that must be exactly 32 bytes (public keys).
pub fn decode32(s: &str) -> Result<[u8; 32], CryptoError> {
    base64url::decode(s)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or(CryptoError::Malformed("expected 32 bytes of base64url"))
}

/// `XXXX-XXXX-XXXX-XXXX` (hex, 64 bits) of a device's two public keys.
pub fn fingerprint(sign_pk: &[u8; 32], dh_pk: &[u8; 32]) -> String {
    let h = sha256(&[b"penroam/device-fp/v1".as_slice(), sign_pk, dh_pk].concat());
    let hex = penroam_sync_protocol::hex::encode(&h[..8]).to_uppercase();
    format!(
        "{}-{}-{}-{}",
        &hex[0..4],
        &hex[4..8],
        &hex[8..12],
        &hex[12..16]
    )
}

/// Ed25519 signature over `domain ‖ msg`. The domain prefix keeps a signature
/// made for one purpose (key-log entry) from being valid for another
/// (envelope); envelopes pass an empty domain because their canonical bytes
/// already start with their own domain string.
pub fn sign(key: &SigningKey, domain: &[u8], msg: &[u8]) -> [u8; 64] {
    key.sign(&[domain, msg].concat()).to_bytes()
}

/// Verifies a [`sign`] signature; [`CryptoError::BadSignature`] on any mismatch.
pub fn verify(pk: &[u8; 32], domain: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), CryptoError> {
    let vk = VerifyingKey::from_bytes(pk).map_err(|_| CryptoError::Malformed("public key"))?;
    let sig = ed25519_dalek::Signature::from_slice(sig).map_err(|_| CryptoError::BadSignature)?;
    vk.verify(&[domain, msg].concat(), &sig)
        .map_err(|_| CryptoError::BadSignature)
}

// ------------------------------------------------------------ content keys --

/// A content epoch key (random, shared with member devices through the key log).
///
/// A new epoch starts whenever a device is removed, so the removed device
/// cannot read *new* writes. Old epochs stay readable to every device that
/// had them.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct EpochKey(pub [u8; KEY_LEN]);

impl EpochKey {
    /// A new random epoch key.
    pub fn generate() -> Result<Self, CryptoError> {
        Ok(Self(random_bytes()?))
    }

    /// Opaque notebook routing alias the server sees instead of the notebook id.
    pub fn routing_id(&self, notebook: Uuid) -> String {
        let k = hkdf32(&self.0, b"penroam", b"penroam/routing/v1");
        let h = hmac32(&k, &[notebook.as_bytes()]);
        format!("nb-{}", base64url::encode(&h[..16]))
    }

    /// Per-notebook payload key (domain-separated by the routing alias).
    pub fn notebook_key(&self, routing_id: &str) -> [u8; KEY_LEN] {
        hkdf32(
            &self.0,
            routing_id.as_bytes(),
            b"penroam/notebook-content/v1",
        )
    }

    /// Key for checkpoint blobs (a device's whole operation history).
    pub fn checkpoint_key(&self) -> [u8; KEY_LEN] {
        hkdf32(&self.0, b"penroam", b"penroam/checkpoint/v1")
    }

    /// Key for encrypted blobs (assets) of a notebook.
    pub fn blob_key(&self, routing_id: &str) -> [u8; KEY_LEN] {
        hkdf32(&self.0, routing_id.as_bytes(), b"penroam/blob/v1")
    }

    /// Opaque blob id for an asset (stable for a given asset, key and notebook).
    pub fn blob_id(&self, routing_id: &str, asset_sha256: &str) -> String {
        let k = hkdf32(&self.0, b"penroam", b"penroam/blob-id/v1");
        let h = hmac32(&k, &[routing_id.as_bytes(), asset_sha256.as_bytes()]);
        format!("b-{}", base64url::encode(&h[..20]))
    }
}

/// Length of [`seal`]'s output for a plaintext of `n` bytes.
pub fn sealed_len(n: usize) -> usize {
    NONCE_LEN + n + TAG_LEN
}

/// Encrypts with a nonce derived from `(key, nonce_context, plaintext)`.
/// Output: `nonce ‖ ciphertext ‖ tag`.
pub fn seal(key: &[u8; KEY_LEN], nonce_context: &[u8], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let n = hmac32(
        key,
        &[b"penroam/nonce/v1", nonce_context, &sha256(plaintext)],
    );
    let nonce = XNonce::from_slice(&n[..NONCE_LEN]);
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("encryption of in-memory data cannot fail");
    let mut out = Vec::with_capacity(sealed_len(plaintext.len()));
    out.extend_from_slice(&n[..NONCE_LEN]);
    out.extend_from_slice(&ct);
    out
}

/// Decrypts [`seal`] output. Returns [`CryptoError::Decrypt`] if the data,
/// the associated data or the key do not match; nothing partial is returned.
pub fn open(key: &[u8; KEY_LEN], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if data.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError::Decrypt);
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| CryptoError::Decrypt)
}

/// Wraps `secret` to a recipient X25519 public key (ephemeral-static ECDH).
/// Output: `ephemeral_pk ‖ nonce ‖ ciphertext ‖ tag`.
pub fn wrap_to(
    recipient: &[u8; 32],
    context: &[u8],
    secret: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let eph = x25519_dalek::StaticSecret::from(random_bytes::<32>()?);
    let eph_pk = x25519_dalek::PublicKey::from(&eph).to_bytes();
    let shared = eph.diffie_hellman(&x25519_dalek::PublicKey::from(*recipient));
    let key = hkdf32(
        shared.as_bytes(),
        &[eph_pk.as_slice(), recipient].concat(),
        &[b"penroam/wrap/v1".as_slice(), context].concat(),
    );
    let nonce = random_bytes::<NONCE_LEN>()?;
    let ct = XChaCha20Poly1305::new((&key).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: secret,
                aad: context,
            },
        )
        .expect("in-memory encryption");
    Ok([eph_pk.as_slice(), &nonce, &ct].concat())
}

/// Reverses [`wrap_to`] with the recipient's X25519 secret. The `context` must
/// be the same bytes used when wrapping (it is bound as associated data).
pub fn unwrap_with(
    dh: &x25519_dalek::StaticSecret,
    context: &[u8],
    data: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if data.len() < 32 + NONCE_LEN + TAG_LEN {
        return Err(CryptoError::Decrypt);
    }
    let eph_pk: [u8; 32] = data[..32].try_into().unwrap();
    let me = x25519_dalek::PublicKey::from(dh).to_bytes();
    let shared = dh.diffie_hellman(&x25519_dalek::PublicKey::from(eph_pk));
    let key = hkdf32(
        shared.as_bytes(),
        &[eph_pk.as_slice(), &me].concat(),
        &[b"penroam/wrap/v1".as_slice(), context].concat(),
    );
    let (nonce, ct) = data[32..].split_at(NONCE_LEN);
    XChaCha20Poly1305::new((&key).into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: context,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_recovery_codes_are_fully_redacted_by_the_log_scrubber() {
        use penroam_sync_protocol::redact::{scrub_line, Scrubbed};
        for _ in 0..50 {
            let code = RootSecret::generate().unwrap().recovery_code();
            let line = format!("user typed {code} into the box");
            let Scrubbed::Redacted(out) = scrub_line(&line, &[]) else {
                panic!("not redacted: {line}");
            };
            assert_eq!(out, "user typed PRK1-[redacted] into the box");
            // also when typed lower-case with spaces
            let typed = code.to_lowercase().replace('-', " ");
            assert!(
                matches!(scrub_line(&typed, &[]), Scrubbed::Redacted(o) if !o.contains(&typed[5..15]))
            );
        }
    }

    #[test]
    fn recovery_code_round_trip_and_typos() {
        let r = RootSecret::from_bytes([7; 32]);
        let code = r.recovery_code();
        assert!(code.starts_with("PRK1-"));
        let back = RootSecret::parse_recovery_code(&code.to_lowercase().replace('-', " ")).unwrap();
        assert_eq!(back.0, r.0);
        // one changed character is caught by the checksum
        let mut bad: Vec<char> = code.chars().collect();
        let i = bad.len() - 3;
        bad[i] = if bad[i] == 'A' { 'B' } else { 'A' };
        assert!(RootSecret::parse_recovery_code(&bad.into_iter().collect::<String>()).is_err());
        assert!(RootSecret::parse_recovery_code("PRK1-AAAA").is_err());
    }

    #[test]
    fn seal_is_deterministic_per_plaintext_and_authenticated() {
        let k = [3u8; 32];
        let a = seal(&k, b"op1", b"aad", b"hello");
        assert_eq!(a, seal(&k, b"op1", b"aad", b"hello"));
        assert_ne!(
            a[..NONCE_LEN],
            seal(&k, b"op1", b"aad", b"hellO")[..NONCE_LEN]
        );
        assert_eq!(open(&k, b"aad", &a).unwrap(), b"hello");
        assert_eq!(open(&k, b"aaX", &a), Err(CryptoError::Decrypt));
        let mut t = a.clone();
        t[30] ^= 1;
        assert_eq!(open(&k, b"aad", &t), Err(CryptoError::Decrypt));
        assert_eq!(a.len(), sealed_len(5));
    }

    #[test]
    fn wrap_unwrap() {
        let d = DeviceKeys::generate("d".into()).unwrap();
        let w = wrap_to(&d.dh_pk(), b"ctx", b"secret").unwrap();
        assert_eq!(unwrap_with(&d.dh, b"ctx", &w).unwrap(), b"secret");
        assert!(unwrap_with(&d.dh, b"other", &w).is_err());
        let e = DeviceKeys::generate("e".into()).unwrap();
        assert!(unwrap_with(&e.dh, b"ctx", &w).is_err());
        let back = DeviceKeys::from_secret_string(&d.to_secret_string()).unwrap();
        assert_eq!(back.sign_pk(), d.sign_pk());
        assert_eq!(back.dh_pk(), d.dh_pk());
    }

    #[test]
    fn signatures() {
        let d = DeviceKeys::generate("d".into()).unwrap();
        let s = sign(&d.signing, b"dom", b"msg");
        verify(&d.sign_pk(), b"dom", b"msg", &s).unwrap();
        assert!(verify(&d.sign_pk(), b"dom", b"msX", &s).is_err());
        assert!(verify(&d.sign_pk(), b"doX", b"msg", &s).is_err());
    }
}
