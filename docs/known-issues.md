# Known issues

Problems we know about, with a workaround where there is one. There are no known bugs that lose
notes. If you find one, please report it: see [support](support.md).

## Everywhere

* **Automatic backups only run while Penroam is open.**
* **A backup or export isn't one single instant.** You can keep writing while one runs, so each
  page is saved as it was when the backup or export reached it. A page you edit at the same time
  may be in it from just before or just after that edit. Close open notebooks first if you need
  a fixed moment.
* **A backup, export or WebDAV copy isn't encrypted.** Keep these files somewhere you trust.
  Penroam warns you if the notebooks are synced.

## Infinite canvas

* Older versions of Penroam show canvas notebooks as ordinary sheets, cut off at the page edge, and
  PDF pages placed on a canvas as empty boxes. Once a notebook is switched to or from a canvas on a
  synced device, devices on older versions stop syncing it and ask to be updated.
* A long canvas is split across several PDF pages where its content has gaps. Writing or a
  drawing taller than a page, with no gap to break in, is cut at the page edge and continues on
  the next page.
* SVG export and text export leave out PDF pages placed on a canvas, with a warning. PNG and
  annotated PDF keep them.

## Importing

* Evernote PDF attachments aren't imported. Penroam lists them in the import report.
* Joplin's Markdown folder exports aren't read. Export a `.jex` file or single `.md` files instead.

## Sync

* Encryption hasn't been independently reviewed. See the
  [threat model](security/threat-model.md).
* A notebook moved into a vault leaves its earlier versions in your account's history until the
  account is deleted, and that history isn't private. Create sensitive notebooks in the vault.
* An unlocked vault's notebooks are stored on the device like any others. Lock the vault to encrypt
  them there; deleted files can still be recovered from the disk underneath unless the device's own
  encryption is on. Exports of vault notebooks aren't encrypted, and one imported on another device is
  an ordinary notebook again, which sync uploads as one.
* "Lock vaults when the app closes" can't run when a phone stops the app in the background; the
  vaults then lock the next time Penroam starts.
* A vault whose passphrase was changed on another device unlocks offline only with the old
  passphrase or the recovery key, until this device is online again.
* Erased content stays in your account until the account is deleted, even after it's gone
  from every device. It is never included in exports or backups.
* Edits made before a notebook's first sync appear in the account's history as a single version.
* A removed device keeps what it already downloaded. An edit it uploaded just before removal, which
  your other devices hadn't received yet, is refused; it stays on the removed device.

## Shared notebooks

Shared notebooks need a sync server that offers them. Penroam Cloud doesn't offer sharing yet; it
will be turned on after testing. The items below apply once it does.

* Shared notebooks aren't end-to-end encrypted, and a person you remove keeps whatever their
  devices already downloaded. See [shared notebooks](manual/shared-notebooks.md#privacy).
* An editor's uploads count against the owner's storage allowance, and removing the editor doesn't
  give that space back. When the allowance is full, edits to the shared notebook and to the
  owner's own notebooks stop uploading until space is freed or the plan is raised. They stay saved
  on each device.
* Once a notebook is shared, a version of Penroam from before sharing stops syncing and asks to be
  updated. If you downgrade a device anyway, it may show a shared notebook as an ordinary one. Don't
  edit it there; update instead.
* Ownership of a shared notebook can't be transferred.
* Invitations and notifications (mentions and replies) appear in the app only. No email is sent.
* When two people replace all of a text block's text at the same moment in a shared notebook, both
  new texts are kept, one after the other; delete the part you don't want.
* When two people change the same emoji or accented character at the same moment in a shared
  notebook (for example, each picks a different skin tone), both versions are kept side by side;
  delete the one you don't want.
* Every device needs an up-to-date Penroam to edit text in a shared notebook. An older version
  pauses that notebook and says it needs an update.
* A deleted comment's text isn't erased: it stays in the notebook's comment history and on the
  server.

## Devices

* **BOOX:** the partial screen refresh after erasing hasn't been tried on a device. If erased ink
  leaves ghosting, turn the page or zoom to force a full refresh.
* **BOOX:** while the ruler is shown, ink is drawn by the standard pen layer instead of the device's
  low-latency one, so it refreshes more slowly. It returns when you hide the ruler. Show the ruler
  with the pen lifted: turning it on in the middle of a stroke may lose that stroke.
* **iPad:** while the ruler is shown, the Apple Pencil goes through the web path rather than the
  native capture, so pressure and latency differ until you hide it.
* **iPad:** template lines and PDF pages can look soft at high zoom. Ink stays sharp.
* **Samsung S Pen, other Android tablets, Windows, macOS:** the pen code hasn't been run on real
  hardware. See the [release matrix](platforms/release-matrix.md).
* **Android and iOS:** file pickers for importing and backups haven't been tested on a device.
