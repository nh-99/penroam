# Locked vaults on a device

A vault can be locked on one device. Its notebooks then leave the library and are kept encrypted
on that device until the vault is unlocked there again. This page specifies the files involved.
Code: `crates/penroam-sync/src/locker.rs`; the app side is in
`apps/desktop-mobile/src-tauri/src/sync.rs`.

Locking is per device and never reaches the server. Other devices, and the vault's cloud copy,
don't change.

## Files

```text
<library>/locked-vaults/<vault id>/locked.json    marker (plaintext)
<library>/locked-vaults/<vault id>/manifest.bin   sealed manifest
<library>/locked-vaults/<vault id>/blobs/<n>      sealed file contents
<library>/vault-locks/<vault id>.json             copy of the vault's passphrase lock
```

A vault id becomes a folder name, so only ASCII letters, digits, `-` and `_` are accepted (at most
128 characters), as for the vault's sync folder.

`locked.json`:

```json
{ "format": "penroam-locked-vault", "version": 1, "vaultId": "<vault id>" }
```

A pack whose version is newer than the reader's is refused without changes.

The manifest is JSON, sealed as below:

```json
{
  "vaultId": "<vault id>",
  "folders": [
    { "label": "notebook/<uuid>", "dirs": ["pages", "assets"],
      "files": [{ "path": "pages/<uuid>.prp", "size": 1234, "blob": 0 }] }
  ],
  "secrets": [["vault-<vault id>-sync-device-keys", "<device keys>"]]
}
```

* `label` names a folder: `notebook/<uuid>` (a notebook folder from `notebooks/`), `sync` (the
  vault's sync folder, `sync/vaults/<vault id>/`) or `search` (the vault's search index folder in
  the app's cache folder).
* `dirs` lists every sub-folder, so empty ones come back. `files` lists every regular file, with a
  path relative to its folder (`/`-separated) and its plaintext size. A path with an empty, `.` or
  `..` component, a backslash or a colon is refused when reading.
* `secrets` holds the device's sync keys for the vault, which leave the secret store while the vault
  is locked.

## Encryption

Key: `K = HKDF-SHA256(ikm = vault root secret, salt = "penroam", info = "penroam/vault-local/v1")`,
32 bytes. A device keeps `K` in its secret store (`vault-<vault id>-local-key`) only while the vault
is unlocked there; locking deletes it. Unlocking derives it again from the root, which the
passphrase opens from the cached lock, or the recovery key gives directly.

Every sealed item is `nonce (24) ‖ ciphertext ‖ tag (16)` from XChaCha20-Poly1305 with key `K`.
Nonces are `HMAC-SHA256(K, "penroam/nonce/v1" ‖ context ‖ SHA-256(plaintext))[..24]`, the
construction sync payloads use, with the associated data as context.

* **Manifest:** associated data `"penroam/locked-vault/manifest/v1" ‖ 0x00 ‖ vault id`.
* **File contents:** a file is cut into chunks of 1 MiB of plaintext (the last one shorter; an empty
  file is one empty chunk). Chunk `i` of blob `n` is sealed with associated data
  `"penroam/locked-vault/chunk/v1" ‖ 0x00 ‖ vault id ‖ 0x00 ‖ u64le(n) ‖ u64le(i) ‖ u8(last)`, and
  the chunks are concatenated into `blobs/<n>`. The reader computes chunk boundaries from the
  manifest's size, so a blob that is cut short, extended, reordered, swapped with another or moved to
  another vault fails to open.

A wrong key and a tampered pack are reported the same way; nothing is written outside the
unlocking staging folder in either case.

## Cached lock

`vault-locks/<vault id>.json` is the vault's passphrase lock as the server stores it (protocol §12e:
Argon2id parameters, salt, wrapped root). The app refreshes it whenever it lists the account's
vaults, and after a passphrase change. It lets a device unlock a locked vault without a connection.
If the passphrase was changed on another device, the cached copy still opens with the old one until
the device is online; the recovery key works either way.

## Locking

With the library lock held:

1. The vault's sync engine copies the journal into its operation log.
2. The library takes the vault's notebooks out (`Library::detach_notebooks`): their snapshots are
   brought up to date, their transactions are removed from the journal (each segment rewritten
   atomically), and the library forgets them.
3. Every folder is read and sealed into `locked-vaults/<vault id>.partial/`, every file fsynced,
   then the marker is written and the folder renamed to `locked-vaults/<vault id>/`. **This rename
   commits the lock.**
4. The plaintext folders and the vault's secrets are deleted.

An error before the commit puts the notebooks back into the library, so nothing changed.

## Unlocking

1. The manifest is opened; a wrong key stops here.
2. Every file is decrypted into `locked-vaults/<vault id>.restore/` and checked.
3. Each folder is moved into place (renamed, or copied and deleted across file systems).
4. The secrets go back into the secret store, with `K`.
5. The pack is renamed to `locked-vaults/<vault id>.trash` and deleted. **This rename commits the
   unlock.** The notebooks then rejoin the library, and the vault's engine resumes reading the
   journal from the current position: nothing of its notebooks was written in between.

## Start-up

Before the library opens, the app deletes `*.partial`, `*.restore` and `*.trash` folders, and for
every committed pack deletes that vault's plaintext folders (its notebooks by
`vault-notebooks.json`, and its sync folder). After the app state exists it also deletes the vault's
secrets and search index folder. A committed pack is always complete, so any plaintext found next
to one is a leftover of an interrupted lock or unlock.

## What this does not protect

* An unlocked vault's notebooks are plaintext on the device.
* Deleted files may stay readable on the storage underneath; this is not a secure erase. Device
  encryption covers that.
* Journal bytes that were already damaged can't be attributed to a notebook and are left as they
  are, as is any copy in `journal/quarantine/`.
* The marker names the vault, and `vault-notebooks.json` lists which notebook ids belong to it and
  the vault's name, so the device shows a locked vault's name and how many notebooks it holds.
  Titles and content are sealed.
