# Sync threat model

**Status: `security_review_pending`.** This design and its implementation
(`crates/penroam-sync`) have **not** been reviewed by an independent security reviewer. Do not
describe vaults as audited end-to-end encryption until that review is done and its findings are
resolved, and never describe ordinary sync as end-to-end encrypted: the server holds its key. Release builds sync only with Penroam Cloud; debug builds also work
against the local development test server and other servers with the public sign-in (`auth.v1`).

## Two levels of protection

| | Ordinary notebooks | Notebooks in a vault |
|---|---|---|
| Adding a device | Sign in | Sign in, then the vault's passphrase or recovery key, once |
| Encrypted before upload | Yes | Yes |
| Who holds the key | Every device **and the server** | Devices. The server holds it only inside a passphrase lock |
| Read by a malicious or breached server | **Yes**, if it has the stored key and its sealing secret | No, unless it guesses the passphrase offline |
| Read with a stolen sign-in | **Yes** | No |
| Lost passphrase and every device | The server still has the key | Recovery key, or the content is gone |

The trade is deliberate. Ordinary sync is as simple as other note apps and offers about the same
protection from the operator. Vaults are for what the user wouldn't want the operator to read.

## What we protect

The goals below apply to vaults in full. For ordinary notebooks, an adversary holding the stored
root secret is out of reach of all of them, since it can do anything a device can.

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

It also holds, for an ordinary account, the root secret (sealed at rest with a service secret, so a
database backup alone doesn't expose it). For a vault it sees that the vault exists, its owner, its
size and activity, its passphrase lock and its encrypted name, but not which notebooks are in it. A
device's name is its host name by default, which the server can read in the key log.

## Sign-in (servers with `auth.v1`)

The identity provider and the sync server decide who may use an account. For ordinary notebooks,
sign-in is also what gives access to the keys. For a vault it isn't: neither ever receives its
passphrase, recovery key or a content key.

* The app signs in in the system browser (authorization code + PKCE), checks `state` and the
  ID token's `nonce`, and gives the ID token to the sync server, which verifies it. No password
  passes through the app. Desktop receives the code on a loopback redirect; with registered
  fixed ports, another local program could hold the port first and receive the code. Mobile
  receives it as an `ink.penroam.app:` link, and another app could claim that scheme. Neither
  can redeem the code, because only the app holds the PKCE verifier; the real sign-in then
  times out and the user tries again.
* An account session can fetch the account's root secret, register devices and read or append the
  key log, so **a stolen account session reads every ordinary notebook**. That's why it is only
  created by a sign-in, and why the app ends it as soon as it has a device session: a device session
  can't fetch the root secret, so a stolen or removed device holds no way to it.
* Pushing, pulling, blobs and checkpoints need a device session, which the server issues only to a
  device that signs a fresh challenge with a key that is an active member of the key log.
* A vault's content keys are wrapped only to its member devices and its recovery key. No session,
  from any account, gives access to them.
* Session tokens: the app keeps only refresh tokens, in the OS keychain next to the device keys
  (access tokens stay in memory). Refresh tokens are single-use; a copied one ends the session
  when either copy is used twice. The renderer never sees any token.
* For a vault, losing account access and losing the keys are different problems. Signing in again
  restores access to its storage, not to its keys.

## Plans and billing (servers with `account.v1`)

* The app never sends a price, a payment result or a provider id. It asks for a billing link
  for an offer id the server listed, and the server alone decides what that offer costs and
  whether the account has paid. A modified app can therefore not grant itself storage.
* Billing links are opened only in the system browser and only if they are absolute
  `https://` URLs without credentials (`BillingLink::validate`). A malicious server could still
  send the user to a page of its choosing; it cannot make the app open anything but `https://`.
* Account deletion needs an account session from a sign-in a few minutes old: a stolen device
  or a copied refresh token is not enough to erase an account. Deletion erases the cloud copy
  only; notes on devices stay.
* Plan state reaches the server but tells it nothing about note content. The server learns
  usage sizes and timing, as before.

## Key hierarchy

An account and each vault have one of these, independent of each other.

```text
root secret (256 bit, not stored by devices)
  account: stored on the server, sealed     vault: passphrase lock + recovery key shown once
 ├─ account signing key (Ed25519)      signs genesis and enrollments in the key log
 ├─ recovery key (X25519)              every epoch key is wrapped to it
 └─ vault name key                     encrypts a vault's name (vaults only)

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

* **Confidentiality of vault content** against the server: payloads and blobs are encrypted with
  keys the server never receives outside the passphrase lock
  (`a_vault_syncs_between_unlocked_devices_and_stays_out_of_the_main_account`). The server refuses
  to store an account key for a vault, and the client's account path refuses a vault scope
  (`a_vault_can_never_hold_a_stored_key`).
* **Confidentiality of ordinary content** against the network and object storage, and against a
  database backup without the service's sealing secret. Not against the service.
* **Authenticity before use:** a pulled envelope is applied only after its signer is found in
  the verified key log, its Ed25519 signature verifies, its ciphertext hash matches, AEAD
  decryption succeeds, and the decrypted operation matches the signed header. Anything else is
  quarantined and reported (`tampered_envelopes_are_quarantined_and_never_applied`).
* **No server-injected devices in a vault:** membership changes are signed by the account key or an
  active member and hash-chained, and a device joining a vault verifies the whole key log against
  the root it opened, so a swapped lock fails (`a_swapped_lock_is_caught_by_the_vault_key_log`,
  `server_registered_device_without_a_key_log_entry_gets_nothing`). This holds for ordinary accounts
  only against a server that doesn't use the stored key.
* **No history rewrite:** clients pin the verified key-log prefix; a log that changes it is
  rejected (`server_cannot_inject_or_rewrite`).
* **Replay/duplication safety:** operations are deduplicated by id; an id reused with different
  content is an integrity alert, never applied.
* **No loss through sync:** local saves never wait for the network; queued operations survive
  restarts, offline periods, sign-in failures, quota errors and revocation; nothing is deleted
  locally because of a server response.

## Known limitations and risks

* `.penroam` files and `.penroam-backup` files are plaintext by design, whatever sync does. They
  are as private as the place they are saved, including a WebDAV server, and with sync on the app
  warns before an export.
* Version history is plaintext on the device, like the notebook itself, and keeps erased content
  until retention drops it or the user deletes it. Those chains are never uploaded, but the
  account's encrypted operation log holds the same edits and isn't thinned, so erasing something
  doesn't remove it from the account. See [`../formats/portability.md`](../formats/portability.md).
* The WebDAV password is in the OS keychain with the sync keys, and a device that can read those can
  read it.

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
* **Vault passphrases can be guessed offline.** Whoever holds a lock, which includes the server and
  anyone who breaches it, can try passphrases as fast as they can run Argon2id (64 MiB, 3 iterations).
  There is no attempt limit, because nothing but the client enforces anything. A passphrase's
  strength is the vault's strength. The app requires 12 characters and says why, but doesn't measure
  strength. A short numeric PIN was rejected for this reason.
* **A vault isn't a device lock.** Its notebooks are plaintext on the device like any others, in the
  library, search index and version history, and it stays unlocked once a device has opened it.
  Device loss is left to the operating system's disk encryption. Backups and exports of vault
  notebooks are plaintext, and a notebook restored from a backup into a library without its
  assignment is an ordinary notebook, which sync uploads as one.
* **Moving a notebook into a vault doesn't remove its past.** What the main account already holds
  stays there, readable by anyone with the stored key, until the account is deleted. Only what's
  created in a vault was always private.
* **Assignment is local.** Which notebooks are in a vault is recorded on each device, and a device
  that loses `vault-notebooks.json` treats them as ordinary. A damaged file stops sync rather than
  being read as empty.
* **Removing a device from a vault that the remover hasn't unlocked:** the server ends the device's
  access, but the vault's key isn't rotated until a device that has it unlocked removes it. A removed
  device can always decrypt what it could before. A device sign-out on the identity provider's side
  is the user's to do: the app can't reach it.
* **Recovery key loss:** with the passphrase, the recovery key and every device lost, nobody can
  decrypt a vault, including Penroam. With the recovery key exposed, an attacker with server access
  can enroll a device. Neither the recovery key nor a vault's root can be rotated yet; changing the
  passphrase re-wraps the same root, and a device or server that kept the old lock can still try the
  old passphrase.
* **Stored account key:** the sealing secret is in the service's environment, so a breach of the
  running service reads every ordinary notebook. Losing the secret strands accounts: only one with a
  recovery code can come back, and accounts created now have none.
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

## The WebView

Note content never runs as script: nothing in the app inserts it as HTML, and the content security
policy refuses inline and remote scripts. This matters, because many commands take a file path that
came from a system dialog, and the backend doesn't check a path against that dialog. Script running
in the WebView could therefore read and write files as the app. Treat any way of running script
from note content as critical.

## Before calling vaults end-to-end encrypted

1. Independent review of this document, §12e–§15 of the protocol, and `penroam-sync`.
2. Verify the keychain stores on macOS, iOS, Windows and Android hardware (Linux verified).
3. Recovery-key rotation and a tested recovery drill on real devices.
4. Cross-language vectors for third-party clients.
