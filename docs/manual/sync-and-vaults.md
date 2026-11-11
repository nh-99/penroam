# Sync and vaults

[Manual](README.md) › Sync and vaults

Penroam works fully without an account. Sync is optional, for when you want the same notebooks on
several devices.

## Sync

Sync works through Penroam Cloud. Open **Settings → Sync** and sign in with your web browser. That's
all: do the same on each new device and your notebooks arrive. Folders sync too.

Each device names itself after your computer or tablet, and you can rename or remove devices in
**Settings → Sync**. Sync can be paused or disconnected at any time, and your notes stay on the
device either way.

Notebooks are encrypted before they're uploaded, but Penroam keeps a copy of the key so that
signing in is enough, which means Penroam could read them. For notes nobody else can read, use a
vault.

## Vaults

A vault holds notebooks that only you can read. To make one, sign in to sync, then choose
**New vault** in the **Vaults** row above your notebooks. Choose a name and a passphrase of at
least 12 characters, and save the recovery key, which is shown once. Then choose the vault under
**Keep in** when you create a notebook, or use **Move to vault…** on an existing one.

**On another device**, sign in, choose the vault in the **Vaults** row and enter the passphrase
once.

**Locking.** Choose an open vault in the **Vaults** row to lock it. Locking hides its notebooks and
encrypts them on that device until you unlock it with the passphrase again, even without a
connection. To lock every vault whenever Penroam closes, turn on **Lock vaults when the app
closes** in **Settings → Advanced**; phones often stop apps without closing them, so there the
vaults lock the next time Penroam starts. While a vault is unlocked, its notebooks are stored on
the device like any others.

**Changing the passphrase.** Choose the open vault in the **Vaults** row.

**Stopping cloud sync.** **Stop syncing with the cloud** erases the vault's copy in your account but
keeps its notebooks on every device that has them, marked **Not synced**. On any of them,
**Restore cloud sync** puts the vault back in your account (the other devices join it the same
way), and **Remove from this device** deletes its notebooks there.

Things to know:

* Penroam can't read a vault or reset its passphrase. If you lose both the passphrase and the
  recovery key, what's in the vault can only be read on devices already holding it.
* When you move a notebook into a vault, what had already synced stays in your account's history,
  so create sensitive notebooks in the vault to begin with.
* Vault notebooks are left out of backups, and stay outside folders.
* Vault encryption hasn't been independently reviewed yet.

## Cloud storage

Every account includes some free storage. For more, choose a plan in **Settings → Sync**.

* On a computer, plans open a payment page in your browser.
* On iPhone and iPad they're App Store subscriptions, billed by Apple: manage or cancel them with
  **Manage subscription**, and use **Restore purchases** if a plan doesn't appear.

One account has one plan, whichever way you bought it. Deleting your account stops a plan bought on
a computer, but only you can cancel an App Store subscription, so do that too or you'll keep being
charged.

## Related

* [Forgotten vault passphrase, lost devices and other problems](../support.md#common-problems)
* [Privacy](../policies/privacy.md)
