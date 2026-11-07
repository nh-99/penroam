# Getting help

Start with [known issues](known-issues.md) and the [usage guide](../README.md#using-penroam).

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

**I lost my recovery code (sync).** The code can't be replaced yet, but your devices still work and
you can add a new one by approving it from a device that's signed in. If you lose the code and every
device, nobody can decrypt the copy in your account, which is how the encryption is meant to work.
