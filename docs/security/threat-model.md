# Sync threat model

**Status: `security_review_pending`.** This design and its implementation
(`crates/penroam-sync`) have **not** been reviewed by an independent security reviewer. Do not
describe vaults as audited end-to-end encryption until that review is done and its findings are
resolved, and never describe ordinary sync as end-to-end encrypted: the server holds its key. Release builds sync only with Penroam Cloud; debug builds also work
against the local development test server and other servers with the public sign-in (`auth.v1`).

## Two levels of protection

| | Ordinary notebooks | Shared notebooks | Notebooks in a vault |
|---|---|---|---|
| Adding a device | Sign in | Sign in (as a member) | Sign in, then the vault's passphrase or recovery key, once |
| Encrypted before upload | Yes | Yes | Yes |
| Who holds the key | Every device **and the server** | Every member's devices **and the server** | Devices. The server holds it only inside a passphrase lock |
| Read by a malicious or breached server | **Yes**, if it has the stored key and its sealing secret | **Yes**, the same way | No, unless it guesses the passphrase offline |
| Read with a stolen sign-in | **Yes** | **Yes**, any member's | No |
| Lost passphrase and every device | The server still has the key | The server still has the key | Recovery key, or the content is gone |

The trade is deliberate. Ordinary sync is as simple as other note apps and offers about the same
protection from the operator. Shared notebooks follow ordinary sync: they are **not** end-to-end
encrypted. Vaults are for what the user wouldn't want the operator to read, and a vault notebook
can't be shared (the app refuses, in the backend as well as the UI).

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
4. **A member of a shared notebook**, or one who was removed: they hold the space's root and the
   keys of every epoch they were part of, and may run a modified client.
5. **Other local apps or users** on a device, within what the OS keychain/file permissions allow.

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

For a shared notebook it holds the space's root secret, as for an ordinary account, so it can read
the notebook. It knows who the members are, their roles and verified emails, and which member
registered each device. The notebook's title travels sealed under a key derived from the space's
root (`sealed_meta`), which the server could open with that root. Invitations are matched by the
invitee's verified email; their responses don't say whether an email belongs to an account, and a
space the caller isn't a member of answers `not_found`.

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

An account, each vault and each shared notebook space have one of these, independent of each
other. A space's root is stored on the server like an account's, and members fetch it with their
own session; it also derives the space's title key.

```text
root secret (256 bit, not stored by devices)
  account: stored on the server, sealed     vault: passphrase lock + recovery key shown once
 ├─ account signing key (Ed25519)      signs genesis and enrollments in the key log
 ├─ recovery key (X25519)              every epoch key is wrapped to it
 ├─ vault name key                     encrypts a vault's name (vaults only)
 ├─ space title key                    seals a shared notebook's id and title (spaces only)
 └─ vault local key                    encrypts a vault locked on a device (vaults only, never sent)

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
* **Shared notebooks stay in their space:** a shared notebook is synced only by its space's engine,
  never by the account engine, and a space's engine installs only the one notebook it was assigned,
  so a member can't push a second notebook, or folders, into anyone's library. Someone who isn't
  a member gets nothing, the same answer as for a space that doesn't exist
  (`a_shares_with_b_who_sees_everything_and_c_sees_nothing`). A space's root is accepted only if
  the space's whole key log verifies against it
  (`a_space_root_is_only_accepted_if_it_verifies_the_key_log`).
* **Viewers can't change a shared notebook:** a viewer's engine never uploads, the app refuses every
  changing command, and the server refuses a viewer's pushes, blob uploads and checkpoints, including
  hand-made envelopes (`a_viewer_uploads_nothing_and_forged_writes_are_refused`).
* **Removing a member starts a new epoch:** an owner device takes the removed member's devices out
  of the space's key log, so the new epoch isn't granted to their device keys
  (`removing_a_member_rotates_their_devices_out_of_the_key_log`). Every device of a space stores the
  space's root next to its own keys, so this only helps against someone who extracted a removed
  device's signing and encryption keys but not the stored root. It does **not** keep a removed
  member, or anyone with a removed device's full storage, out of later content: see "Removed
  members keep what they had, including the space's root" below.
* **Every remaining device gets each new epoch.** In an account or a vault, a key-log entry that
  removes a device must grant the new epoch to every device that stays and to the recovery key, and
  a new device must receive the current epoch, or the log fails verification
  (`a_removal_must_grant_the_new_epoch_to_everyone_who_stays`); existing clients have always
  granted that much. In a shared notebook space every member holds the root, so a stricter and
  simpler rule applies, and **a member can't lock the others out of writing or split them onto
  different keys**:
  * an epoch's key is whatever its recovery grant (the first, in the entry that started the epoch)
    opens to with the space's root. Every member derives the same key. Device grants that disagree
    are ignored and logged as a security event; they don't stop sync;
  * an epoch whose recovery grant is missing or doesn't open is void. Devices keep sealing under
    the last epoch that has a key, with that epoch's number, and open each envelope by the epoch it
    names. The removal in that entry still takes effect, and the next proper removal starts a fresh
    epoch. Until then the remaining devices stay on a key the removed device holds; that costs
    nothing extra, because a space device stores the root, so rotation in a space never protected
    against a removed device in the first place (see the paragraph on removed members);
  * grants that are missing or misaddressed never fail a space's key log, so a bad entry can't put
    every member into an integrity alert either. Signatures, the hash chain, who may sign and who
    is a member are checked as strictly as anywhere.

  Removals this client writes always grant every remaining device and the recovery key
  (`junk_grants_in_a_removal_void_the_epoch_and_everyone_goes_on_writing`,
  `split_device_grants_still_give_every_member_the_same_key`,
  `a_recovery_only_removal_locks_nobody_out`, also across restarts).
* **Access loss loses nothing:** a member whose access is removed keeps the notebook and every
  unsent edit, read-only, and uploads nothing more; a private copy keeps the edits
  (`removing_a_member_keeps_their_unsent_edits_and_uploads_nothing_more`).
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
* **An unlocked vault is plaintext on the device.** While a vault is unlocked on a device, its
  notebooks are stored like any others, in the library, its own search index and version history.
  Locking it there ([`../formats/locked-vault.md`](../formats/locked-vault.md)) encrypts them under
  the vault's local key, removes them and their journal entries, and deletes the device's keys for
  the vault until the passphrase or recovery key is entered again
  (`a_locked_vault_leaves_no_plaintext_and_picks_up_where_it_left_off`). It is not a secure erase:
  deleted files may stay readable on the storage underneath, which only disk encryption covers.
  "Lock vaults when the app closes" locks at exit, or at the next launch when the app was stopped
  without one, so until then a stopped app leaves an unlocked vault in plaintext. A device keeps a
  copy of the passphrase lock for unlocking offline; it is open to the same offline guessing as the
  server's copy. Vault notebooks are left out of backups; exports of them are plaintext, and a
  notebook restored from an export into a library without its assignment is an ordinary notebook,
  which sync uploads as one.
* **Stopping a vault's cloud sync never erases local copies.** It erases the vault's account on the
  server; every device keeps the notebooks and marks the vault not synced until the user restores
  sync or removes it there. Otherwise a server could erase a vault's notebooks on every device just
  by no longer listing it. Restoring creates the vault again with the same root and passphrase lock
  (so the recovery key stays valid), and a device restoring after another one joins the vault whose
  sealed name opens with that root. A server can withhold a restored vault from the list, which only
  makes a device create a second one.
* **Moving a notebook into a vault doesn't remove its past.** What the main account already holds
  stays there, readable by anyone with the stored key, until the account is deleted. Only what's
  created in a vault was always private.
* **Assignment is local.** Which notebooks are in a vault or shared is recorded on each device, and a
  device that loses `vault-notebooks.json` treats them as ordinary. A damaged file stops sync rather
  than being read as empty; local editing goes on meanwhile, and nothing is uploaded until the file
  reads again. An imported `.penroam` file or restored backup of a notebook that is or was shared on
  the device gets a new id, so it can't be taken for the shared notebook
  (`a_reimported_shared_notebook_gets_a_new_id`).
* **Removing a device from a vault that the remover hasn't unlocked:** the server ends the device's
  access, but the vault's key isn't rotated until a device that has it unlocked removes it. A removed
  device can always decrypt what it could before. A device sign-out on the identity provider's side
  is the user's to do: the app can't reach it.
* **Shared notebooks are readable by the server**, like ordinary ones: it stores the space's root.
  Sharing an existing notebook shares its **history** too (the earlier versions in the account log
  are re-published into the space), and the app says so before the first invitation.
* **Removed members keep what they had, including the space's root.** A removed member (or one who
  left) keeps every downloaded copy, and nothing can erase those remotely. They also keep the
  space's **root secret**, because every member receives it so that they can join and add devices
  without the owner being online (the server holds it, as for an ordinary account). Every epoch key
  is wrapped to the recovery key that the root derives, so with the root they can unwrap **every
  future epoch** from the key log, too. All they lack is the ciphertext: the server refuses all
  their requests from the moment of removal, so reading later content needs the server's data by
  other means (a server breach, a leak, or another member passing it on). Rotating the epoch on
  removal therefore only helps against someone who holds a removed device's keys but not the root.
  Older history is never re-encrypted, and the root of a space is never changed.
* **Admitted again after removal.** A removed member's devices stay revoked in the space for good.
  If they are invited and accept again, their devices join with new ids, and only after the copy
  from the earlier membership was kept privately or removed; its unsent edits never go into the
  new membership on their own.
* **Low: a device enrolled in a space under a fresh id isn't reached by removing it from the
  account.** A device normally uses its main-account id in a space, so removing it from the account
  removes it there too. After being re-admitted, or when its main id was already registered in the
  space by someone else, it uses a random id instead; removing the device from the person's own
  account then doesn't reach it in the space until its space session expires (the owner can still
  remove the member).
* **Low: a member can use up a space's device allowance** by registering many devices; servers are
  adding a cap per member.
* **Live presence in shared notebooks.** Awareness frames (page, caret, selection, viewport) are
  sealed under a key derived from the space's root, with the space and sender device as
  associated data; the server relays them to the space's other sessions without reading them, but
  sees who is connected and when. A member who turns "share presence" off sends none. The live
  channel never carries notebook content or writes anything: content still travels only as
  encrypted operations over HTTP.
* **Which devices are revoked is the server's word.** Owner devices rotate out the devices the
  server reports as revoked in a space, so a hostile server can make an owner rotate a member out;
  it could refuse that member service anyway. It can't add a device to a space without the root.
* **Checkpoints in a space come only from the owner.** A checkpoint is vouched for by the member who
  signed it (see "Checkpoints" below), so an editor could publish one that leaves out other members'
  operations, and a device recovering from it would never see them. In a space only the owner's
  devices publish checkpoints: clients never attempt one as editor or viewer
  (`only_the_owners_devices_publish_checkpoints_in_a_space`), and servers refuse a checkpoint from
  anyone but the owner. A corrupt or wrongly signed checkpoint is refused, and a device joining
  from it stays joining (`a_corrupt_checkpoint_is_refused_and_the_join_waits`).
* **Members are trusted like an account's devices** within the space: an editor can write anything
  to the notebook, including into its history. A space's sealed title is written only by its owner
  (servers refuse anyone else). The notebook id inside it is used only for an invitee's first join;
  the owner's own devices take the notebook from their share marker, and once a device has joined,
  the id is fixed and the title only renames. A join is refused when the id is nil, already names a
  notebook in the library or a vault, or belongs to another space, and a space maps to exactly one
  notebook (`a_rewritten_sealed_title_can_never_pull_another_notebook_into_a_space`). A forged
  title can still make an invitee's first join wait forever, which the owner fixes by renaming.
* **The removed-device cutoff covers document operations only.** When a member's devices are
  removed from a space's key log, their later document operations are refused by every client
  (the cutoff is the last sequence the key log allows). Comments are not covered yet: a comment
  from a removed device is accepted on its signature, and only the server's refusal stops a removed
  device from pushing new comments (the server already holds the space's root). A cutoff for the
  comment stream is a planned key-log addition.
* **Hostile text edits are checked before they are decoded.** In a shared notebook a text edit
  (`textEdited`) is a Yrs update from another member. The Yrs decoder reserves memory for the
  counts an update declares, so a few crafted bytes could make a device allocate and clear
  gigabytes. Every remote edit is first walked by a strict pre-scan of the encoding
  (`penroam_core::textcrdt`): one client at most, only plain-text structures, every count and
  length within the bytes that remain, no trailing bytes. Only then is it decoded, on a copy, with
  panics contained; it must write only under its author's client id and leave the text plain. A
  failure while applying an edit that passed rebuilds that block's text without it
  (`updates_declaring_huge_counts_are_refused_before_decoding`,
  `a_panic_inside_yrs_mid_sequence_leaves_the_text_as_if_that_edit_never_came`).
* **An export with authors names accounts, not people.** "Include authors" writes each element's
  authors as account ids and kinds only; members' email addresses never leave the app this way.
  Account ids are stable, so whoever gets the file can tell authors apart and match them with
  other exports.
* **Older clients and downgrades.** A client that doesn't know operation schema 5 parks the
  `notebookShared` marker and pauses with "needs an update" (nothing is lost), but until it is
  updated it keeps uploading its own edits of that notebook to the account, where only an updated
  device of the account moves them into the space. A client downgraded to such a version ignores the
  `shared` map in `vault-notebooks.json`, treats shared notebooks as ordinary and uploads them to its
  own account. Assignment is local, as for vaults.
* **Two owner devices sharing one notebook at once** each create a space. Every device picks the
  same winner; the losing device moves its own edits there and removes its space locally, but the
  losing space stays on the server (no route removes one), and anyone invited to it in the meantime
  is not moved over.
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
