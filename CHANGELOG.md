# Changelog

User-visible changes for each release. The release workflow copies a version's section into its
release notes and stops if the version has no entry.

## 0.1.0-beta.6

0.1.0-beta.5 reached TestFlight and Google Play testing but never got a GitHub release, so if you
install from GitHub, its changes below are new to you as well.

* Pages scroll continuously: scroll past the end of a page and the next one follows, and a press
  on another page opens it. The switch at the bottom right of the canvas goes back to one page at a
  time.
* New: folders in the library. Make one with the folder button at the top, then use a notebook's
  **⋮** menu and **Move to folder…**. Folders can hold folders and sync with your account. Deleting
  a folder moves what's inside it up one level. Devices still on an older version keep the folder
  changes and ask to be updated.
* Text boxes have formatting buttons while you edit (bold, italic, headings, lists, checkboxes,
  links, tables and more), and `Ctrl+B`, `Ctrl+I` and `Ctrl+K` work. Formatting shows as you type,
  with the Markdown symbols only on the line you're on.
* Tap a checkbox in a task list to tick or untick it.
* The palette button after the toolbar colours picks any colour, and **Settings → General → Tool
  colours** changes which colours the toolbar offers.
* When the toolbar doesn't fit the window, arrows at each end move along it, and so does the mouse
  wheel.

## 0.1.0-beta.5

* The Linux AppImage adds Penroam to your app menu. **Settings → Advanced** can remove it before you
  delete the AppImage.
* Vaults moved from Settings to a row above your notebooks on the library screen, where you create,
  unlock and manage them.
* Vaults can be locked on a device: their notebooks disappear from the library and search and are
  encrypted there until you enter the passphrase again, which works offline. **Settings → Advanced**
  can lock every vault when the app closes.
* Vault notebooks are no longer included in backups.
* **Delete vault** is now **Stop syncing with the cloud**: the cloud copy is erased and the notebooks
  stay on your devices, marked **Not synced**, where you can **Restore cloud sync** or remove them.
* Dialogs have room below their buttons again, and messages no longer sit on the bottom edge of the
  window.
* A long press while drawing no longer selects the text in the top bar and toolbar.
* Penroam no longer crashes on Android when the handwriting model finishes downloading or loads at
  startup.
* If a handwriting model ever crashes Penroam while loading, Penroam stops using it so the app still
  opens, and says so in **Settings → Advanced**.

## 0.1.0-beta.4

The first public test build. It's for testers: expect rough edges, and keep a backup of notebooks
you care about.

* Adding a device now takes a sign-in. There's no pairing code, comparison code or recovery code, and
  a device names itself after your computer or tablet.
* New: vaults. Notebooks in a vault are readable only with your passphrase. Ordinary notebooks are
  encrypted too, but Penroam now holds their key, so Penroam could read them.
