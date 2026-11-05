# Search index: privacy and storage

Search in Penroam is local. This page states what the search index contains, where it lives,
who can read it, and what never happens to it. Code: `crates/penroam-search` (index,
recognition) and `apps/desktop-mobile/src-tauri/src/search.rs` (worker, commands).

## What is stored

One SQLite database per library, `index.db` (plus SQLite's `-wal`/`-shm` side files):

| Table | Content |
|---|---|
| `entries` + `entries_fts` | Plain text of typed text blocks, notebook titles, the selectable text of PDF page backgrounds, and recognized handwriting (best reading, alternate readings, per-word bounds and stroke ids), each with notebook/page id, page revision, recognizer id/version/language and confidence |
| `pages`, `notebooks` | Which page revision was indexed, page positions, notebook titles |
| `pdf_text` | Text of each imported PDF, per page |
| `corrections` | Readings the user typed for handwriting regions |

It holds derived text only. It never holds ink, images or PDF bytes, and the notes are never
written to from it.

## Where it lives

`<app cache dir>/search/<library device id>/index.db`

* **Cache directory, on purpose.** The index can be rebuilt from the notes at any time, so it
  lives where the OS expects disposable data: excluded from iCloud/iTunes device backups on
  iOS and from Android Auto Backup, and in `%LOCALAPPDATA%` (not the roaming profile) on
  Windows. If the OS clears it, Penroam rebuilds it. The only loss is handwriting
  corrections, which live only here.
* **One index per library.** The file records the library it was built for. An index from
  another library, another schema version or a damaged file is deleted and rebuilt, so
  indexes never mix, even after a sync account change or a restored library.
* **Not in exports or sync.** `.penroam` archives and sync carry notes, not the index.
  Nothing from the index is uploaded to Penroam Cloud or any other server. The server never
  learns recognized text, that a search happened, or what was searched for.

## Encryption at rest

The index is **not encrypted** by Penroam. That matches the notes it is derived from, which are
also stored unencrypted in the app's private data folder: anyone who can read one can read the
other, so encrypting only the index would add no protection. Both rely on the operating
system's app sandbox and on device encryption (FileVault, BitLocker, iOS/Android data
protection). If notes ever get at-rest encryption, the index has to get the same treatment.

The plaintext copy can still leak in two ways the notes do not:

* Desktop indexing services (Windows Search, Spotlight, Tracker) normally skip application
  cache folders, but a user who adds that folder to an index or a backup tool exposes the
  recognized text there. The text is no more sensitive than the notes themselves.
* After a notebook is deleted forever, its rows leave the index at once, but SQLite may keep
  freed pages in the file until they are reused. Turning search off deletes the whole file.

## Handwriting recognition

Recognition runs on the device. Platform recognizers come first: on Windows the handwriting
recognizers that ship with Windows language features (`Windows.UI.Input.Inking`), on macOS and
iOS Apple's Vision text recognition. Elsewhere, and as a fallback, Penroam can use a
handwriting model, which the user installs from a `.penroam-model` file. Before installing, the
app shows the model's name, version, size and licence; the files are verified against the
SHA-256 checksums in the bundle before they are run, and a damaged or altered bundle is refused.
Penroam never downloads a model by itself and sends no ink anywhere. The model is optional:
removing it only drops the handwriting it recognized from search. Model format:
[`../formats/handwriting-model.md`](../formats/handwriting-model.md).

## User controls (Settings → Search)

* **Search index** off: deletes the index files and stops indexing.
* **Recognize handwriting** off: handwriting rows are removed as pages are re-indexed.
  Titles, typed text and PDF text stay searchable.
* **Pause indexing** and **Wait while on battery** (on by default; power state is known on
  Windows and Linux).
* **Rebuild**: drops derived rows and indexes everything again; corrections are kept.

## Logging and telemetry

Queries, recognized text and corrections are never logged and never sent anywhere. Errors
shown in settings carry SQLite or recognizer messages only.
