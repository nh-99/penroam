# Sync threat model

**Status: `security_review_pending`.** This design and its implementation
(`crates/penroam-sync`) have **not** been reviewed by an independent security reviewer. Do not
describe Penroam sync as audited end-to-end encryption until that review is done and its
findings are resolved. Sync is a developer preview. It works against the local development
test server and against servers that implement the public sign-in (`auth.v1`).

## What we protect

| Asset | Protection goal |
|---|---|
| Note content: titles, ink coordinates, typed text, page templates, PDF and image bytes | Confidential and authentic against the server and the network |
| Which device wrote what | Authentic: signed by a device key the account certified |
| Account membership (which devices can read and write) | Server cannot add a device or swap its keys |
| Local notes | Never lost because of sync, sign-out, revocation or server errors |

## Adversaries

1. **The sync server** (honest-but-curious or actively malicious): reads everything it
   stores, can drop, delay, replay, reorder or duplicate data, can return forged responses,
   and can register devices for the account.
2. **The network** between device and server (TLS is expected on real deployments; the
   design does not rely on it for confidentiality).
3. **A removed device** that kept its keys and downloaded data.
4. **Other local apps or users** on a device, within what the OS keychain/file permissions allow.

Out of scope: a compromised operating system or Penroam binary on a trusted device, physical
attacks on an unlocked device, and traffic analysis beyond what is listed under "What the
server sees".

## What the server sees

The server sees account and device identifiers, envelope headers (operation ids, device
sequences, causal references, epochs, ciphertext lengths and hashes, signatures),
opaque **notebook routing aliases** (not notebook ids), opaque blob ids and sizes, key-log
entries (public keys, wrapped keys, device labels inside signed entries: key-log entries are
signed but not encrypted, so clients keep labels generic), request times,
IP addresses and storage usage. Sync is **not metadata-private**: activity patterns, sizes
and the number of notebooks are visible.

## Sign-in (servers with `auth.v1`)

Sign-in and encryption are separate. The identity provider and the sync server decide who may
use an account's storage; they never receive the recovery code or a content key.

* The app signs in in the system browser (authorization code + PKCE, loopback redirect), checks
  `state` and the ID token's `nonce`, and gives the ID token to the sync server, which verifies
  it. No password passes through the app.
* An account session only registers devices and reads/appends the key log. Pushing, pulling,
  blobs and checkpoints need a device session, which the server issues only to a device that
  signs a fresh challenge with a key that is an active member of the key log. A stolen account
  session therefore gives no access to ciphertext; even a malicious server gains nothing for
  confidentiality, because content keys are wrapped only to devices.
* Session tokens: the app keeps only refresh tokens, in the OS keychain next to the device keys
  (access tokens stay in memory). Refresh tokens are single-use; a copied one ends the session
  when either copy is used twice. The renderer never sees any token.
* Losing account access (provider account, session) and losing the decryption keys are
  different problems. Signing in again restores access to storage, not to keys.

## Key hierarchy

```text
recovery code (root secret, 256 bit, shown once, not stored by devices)
 ├─ account signing key (Ed25519)      signs genesis / recovery enrollments in the key log
 └─ recovery key (X25519)              every epoch key is wrapped to it

epoch key e (random, 256 bit; new epoch whenever a device is removed)
 ├─ per-notebook payload key   HKDF(e, salt = routing alias)
 ├─ per-notebook blob key      HKDF(e, salt = routing alias)
 └─ routing alias / blob ids   HMAC-based, opaque to the server

device keys (per device, in the device's secret store)
 ├─ Ed25519 signing key        signs every envelope and its own key-log entries
 └─ X25519 key                 receives wrapped epoch keys
```

Primitives: Ed25519 (`ed25519-dalek`), X25519 (`x25519-dalek`), XChaCha20-Poly1305
(`chacha20poly1305`), HKDF/HMAC-SHA256 (`hkdf`, `hmac`, `sha2`), OS randomness (`getrandom`).
Exact constructions: [`../protocol/sync-protocol-v1.md`](../protocol/sync-protocol-v1.md)
§13–§15. Vectors: `crates/penroam-sync/tests/vectors/crypto-vectors.json` (test keys only).

## Guarantees (as designed and tested)

* **Confidentiality of content** against the server: payloads and blobs are encrypted with
  keys the server never receives.
* **Authenticity before use:** a pulled envelope is applied only after its signer is found in
  the verified key log, its Ed25519 signature verifies, its ciphertext hash matches, AEAD
  decryption succeeds, and the decrypted operation matches the signed header. Anything else is
  quarantined and reported (`tampered_envelopes_are_quarantined_and_never_applied`).
* **No server-injected devices:** membership changes are signed by the account key or an active
  member and hash-chained; the new device's keys are bound by the pairing code (fingerprint) and
  confirmed by a 6-digit comparison code on both screens
  (`server_registered_device_without_approval_gets_nothing`, key-log unit tests).
* **No history rewrite:** clients pin the verified key-log prefix; a log that changes it is
  rejected (`server_cannot_inject_or_rewrite`).
* **Replay/duplication safety:** operations are deduplicated by id; an id reused with different
  content is an integrity alert, never applied.
* **No loss through sync:** local saves never wait for the network; queued operations survive
  restarts, offline periods, sign-in failures, quota errors and revocation; nothing is deleted
  locally because of a server response.

## Known limitations and risks

* **Not reviewed** (see status). Constructions are standard but their combination is custom.
* **Removed devices** keep everything they downloaded and every key of earlier epochs. Removal
  protects only *new* writes (new epoch). Re-encrypting history is not implemented.
* **Writes by a removed device:** the removal entry records the removed device's last sequence
  the remover had received (it pulls first), and clients refuse anything above it, so a
  misbehaving server cannot deliver writes made after removal
  (`writes_from_a_removed_device_are_refused_even_if_the_server_delivers_them`). The cost: an
  operation the device uploaded just before removal, but which the remover had not pulled yet,
  is refused too. The removed device keeps it locally.
* **Key-log freeze:** the server can withhold new key-log entries (for example, hide a
  removal) from some devices. Detection needs out-of-band comparison or future gossip.
* **First pairing trust:** a new device learns the account key from the server; the comparison
  code is what detects substitution. Users who skip the comparison lose that protection.
  Enrollment with the recovery code needs no such trust.
* **Recovery code loss:** with the code lost *and* every device lost, encrypted server data
  cannot be decrypted by anyone, including Penroam. With the code exposed, an attacker with
  server access can enroll a device. There is no code rotation yet.
* **Secret storage:** device private keys live in the OS credential store
  (`KeychainSecretStore`, scoped per library):

  | Platform | Store | Verified |
  |---|---|---|
  | Linux / BSD | Secret Service (GNOME Keyring, KWallet, …) over D-Bus | yes, on GNOME (round trip, migration, removal) |
  | macOS | login Keychain | compiles only |
  | iOS | Data Protection keychain (needs a code-signed app) | compiles only |
  | Windows | Credential Manager | compiles only |
  | Android | `SharedPreferences` encrypted with a non-exportable Android Keystore AES key | compiles only |

  Protection is whatever the OS gives: code running as the same user in an unlocked session
  can usually ask the keychain for the key (macOS may prompt; the Linux Secret Service does
  not). Without a keychain, sync refuses to start; `PENROAM_SYNC_ALLOW_FILE_SECRETS=1` (headless
  development only) falls back to a 0600 file and the UI says so. Keys from the older file store
  are moved into the keychain on first launch and the file is deleted (not securely wiped).
* **Deterministic nonces:** nonces derive from key, operation id and plaintext digest, so an
  identical operation re-sealed under the same key gives identical ciphertext (needed for
  idempotent retries). This reveals equality of retried operations only.
* **Metadata:** see "What the server sees".
* **Checkpoints** are vouched for by the member that signed them. A recovering device checks the
  signer is a current member, plus each operation's sequence and removal cutoff. It cannot
  check the original author's signature on each operation, because checkpoints carry
  plaintext records, not the original envelopes. A malicious *current member* could therefore
  insert forged operations into a checkpoint; member devices are trusted not to. Without any
  usable checkpoint, a device whose cursor expired stops pulling (`rebaseRequired`) but keeps
  its data and keeps uploading.

## Before calling it end-to-end encrypted

1. Independent review of this document, §13–§15 of the protocol, and `penroam-sync`.
2. Verify the keychain stores on macOS, iOS, Windows and Android hardware (Linux verified).
3. Recovery-code rotation and a tested recovery drill on real devices.
4. Cross-language vectors for third-party clients.
