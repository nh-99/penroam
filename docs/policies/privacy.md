# Privacy

This describes what the Penroam app does with your data. It is checked against the code. Penroam
Cloud, the optional paid sync service, will have its own privacy policy before it opens.

## In short

Your notes stay on your device. The app has no ads, analytics or telemetry, and needs no account.

## What leaves your device, and when

The app connects to the network only in these cases:

| What | When | What is sent | To whom |
|---|---|---|---|
| Update check | Only if you click **Check for updates** or turn on the start-up check | An ordinary download request, with your IP address | GitHub |
| Handwriting model download | Once, on devices with no built-in handwriting recognizer, and whenever you ask for it in **Advanced** | An anonymous download request, with your IP address. No notes | The Penroam model server |
| WebDAV backup | Only if you set it up | Your backup files, not encrypted by Penroam | The server you choose |
| Sync | Only if you set it up | Encrypted notebook data, the key to your ordinary notebooks, and the metadata listed in the [threat model](../security/threat-model.md#what-the-server-sees). The service could read ordinary notebooks. It can't read notebooks in a vault, unless it guesses a weak passphrase | The sync server |
| Shared notebooks | Only if you share a notebook or accept an invitation | The notebook's data, readable by the service, as for ordinary notebooks. For invitations, the invited email address (as a one-way hash for matching, and a sealed copy to show to the other people in the notebook). See [shared notebooks](../manual/shared-notebooks.md#privacy) | The sync server |
| App Store purchases | Only if you buy or restore a plan on iPhone or iPad, or in the Mac App Store version | A random token tied to your account, and your purchase, to Apple. Apple handles the payment; Penroam never sees your card | Apple, and the sync server |
| Links you open | **Get help**, **Privacy**, sign-in and billing pages | What any web page visit sends | Opens your browser |

## What stays on your device

Notebooks, PDFs and images, version history, the search index and recognized handwriting. Search
runs on the device, and recognized text is never uploaded. On Android, the app opts out of the
system's cloud backup, so your library isn't copied to Google. That also means a new phone won't
get your notes that way: use **Backup** to move them.

## Crash reports

If the app crashes it keeps a short report on your device, with the version, the platform, the place
in the code and a shortened error message. Quoted text, file paths, addresses and long identifiers
are removed before it is written. Reports are **never sent automatically**. You can read them in
**Settings → About → Show diagnostic report**, copy them into a bug report yourself, and delete them
there. Penroam keeps the latest 20.

## Deleting your data

Delete a notebook from the Trash to remove it from this device. Uninstalling the app can leave your
library behind, so you can reinstall without losing it; delete the app's data folder to remove it.
With sync on, what has been synced stays in your account until the account is deleted. See
[deleting your Penroam Cloud account](delete-account.md) for how, and what is kept.

## Shared notebooks

A shared notebook isn't end-to-end encrypted: the service can read it, and everyone you share it
with can read their copy. Verified email addresses of the people in a notebook are visible to the
others in the same notebook, and to no one else. If you remove someone, they keep what their devices
already downloaded. Deleting an account ends the shared notebooks it owns and removes it from the
ones it belongs to.

## Exporting your data

Everything can be exported without an account, in documented formats. See
[portability](../formats/portability.md).

## Changes

Changes to this page are in the repository's history. Contact: see [support](../support.md).
