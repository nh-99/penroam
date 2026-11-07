# Known issues

Problems we know about, with a workaround where there is one. There are no known bugs that lose
notes. If you find one, please report it: see [support](support.md).

## Everywhere

* **Automatic backups only run while Penroam is open.** Each notebook is locked separately, so a
  backup isn't one single instant across the library. Close open notebooks first if you need that.
* **A backup, export or WebDAV copy isn't encrypted.** Keep these files somewhere you trust.
  Penroam warns you if the notebooks are synced.

## Importing

* Evernote PDF attachments aren't imported. Penroam lists them in the import report.
* Joplin's Markdown folder exports aren't read. Export a `.jex` file or single `.md` files instead.

## Sync

* Encryption hasn't been independently reviewed. See the
  [threat model](security/threat-model.md).
* Erased content stays in your encrypted account until the account is deleted, even after it's gone
  from every device. It is never included in exports or backups.
* Edits made before a notebook's first sync appear in the account's history as a single version.
* A removed device keeps what it already downloaded. An edit it uploaded just before removal, which
  your other devices hadn't received yet, is refused; it stays on the removed device.

## Devices

* **BOOX:** the partial screen refresh after erasing hasn't been tried on a device. If erased ink
  leaves ghosting, turn the page or zoom to force a full refresh.
* **iPad:** template lines and PDF pages can look soft at high zoom. Ink stays sharp.
* **Samsung S Pen, other Android tablets, Windows, macOS:** the pen code hasn't been run on real
  hardware. See the [release matrix](platforms/release-matrix.md).
* **Android and iOS:** file pickers for importing and backups haven't been tested on a device.
