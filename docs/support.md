# Getting help

Start with the [user manual](manual/README.md) and [known issues](known-issues.md).

To report a bug, [open an issue](https://github.com/nh-99/penroam/issues/new/choose). In Penroam, go
to **Settings → About → Show diagnostic report**, read it, and paste it into the issue. It holds
version numbers, your platform and any crash reports, but no notes, file names or account details.

Nobody working on Penroam will ever ask for your recovery code, your password or the contents of
your notes, and you should not put them in an issue.

Security problems: see [SECURITY.md](../SECURITY.md).

## Common problems

**A notebook won't open and says it needs a newer version.** It was written by a newer version of
Penroam. Update the app. The notebook hasn't been changed.

**The app crashed.** Reopen it. Every stroke is saved as you write it, so your notebook comes back
with what you'd written. **Settings → About** shows what start-up recovery did.

**Something went wrong after an update.** Your notes are stored apart from the app. Install the
previous version from the [releases page](https://github.com/nh-99/penroam/releases); it will refuse
any format it can't read rather than change it.

**I want my notes out of Penroam.** Export a notebook as `.penroam`, PDF, Markdown, text, PNG or SVG,
or back up the whole library to one file. None of this needs an account.

**I invited someone and they can't see it.** No email is sent. They need to open Penroam, signed in
with the same verified email address, and accept under **Invitations** in the library. Invitations
last 14 days; you can cancel and send a new one in the share dialog.

**A shared notebook says "Access removed".** The owner removed you. The notebook stays on your
device: choose **Keep a private copy** or **Remove from this device**. See
[shared notebooks](manual/shared-notebooks.md#when-your-access-is-removed).

**I forgot my vault's passphrase.** Use the vault's recovery key: choose **Unlock** or the key icon
next to the vault, then **Use the recovery key**, and set a new passphrase. Any device that already
has the vault keeps working too. Penroam can't reset either one, so without the passphrase, the
recovery key and a device that has the vault, nobody can read what's in it. That's how a vault is
meant to work.

**My vault's notebooks are gone from the library.** The vault is locked on this device. Choose
**Unlock** next to it in the **Vaults** row and enter the passphrase. If it locks every time Penroam
starts, **Lock vaults when the app closes** is on in **Settings → Advanced**.

**A vault says "Not synced".** It stopped syncing with the cloud, on this device or another one, and
its notebooks are only on the devices that have them. Choose it, then **Restore cloud sync** and enter
the passphrase to sync it again; if another device restored it first, this one joins it. Or choose
**Remove from this device** to delete its notebooks here.

**A device was lost or stolen.** On another device, open **Settings → Sync** and remove it. It stops
syncing at once. Sign out of Penroam on the identity provider's site too. What it already downloaded
stays on it, so also use your device maker's find-and-erase service.
