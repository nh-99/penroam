# Portability: exports, backups, imports and history

What each way of getting notes in and out of Penroam promises. Byte formats live in
[`model-and-archive.md`](model-and-archive.md) and [`penroam-binary-v1.md`](penroam-binary-v1.md).

## The five boundaries

| | What it is | Editable afterwards | Loses | Encrypted |
|---|---|---|---|---|
| **Native export** (`.penroam`) | One notebook or selected pages, live pages only | Yes | Trash, history | No |
| **Archive backup** (`.penroam-backup`) | Every notebook, trash and search corrections included | Yes | History, the search index | No |
| **PDF / print** | Flattened copy for reading | No | Raw samples, editable text | No |
| **Sync ciphertext** | Encrypted operations on a server | Only through a client with the content key | Nothing | Yes |
| **Third-party import** | A new notebook made from another format | Yes, as Penroam content | Listed in a report each time | n/a |

Text, PNG and SVG exports are PDF's kind of copy, not backups. The UI must never present a lossy
export as a way to keep a notebook.

Every native path works offline and without an account. Native export and backups write plaintext
to a location the user picks. With sync on, the app warns before any export that the file won't be
encrypted; the user can go ahead, and can turn the question off. A backup is not a substitute for
the content key: it doesn't contain one, and restoring from one doesn't need one.

## Native export

Format 2 archives, unchanged, plus three options:

* **Selected pages**: only those pages and the assets they use.
* **Reproducible**: `exportedAt` is the notebook's `updatedAt`, entries are in page order and ZIP
  timestamps are fixed, so the same content gives the same bytes.
* **Include trash**: see below.

Format version policy: import accepts every format up to `FORMAT_VERSION` and refuses newer ones
with a message asking for an update. Additions that older builds can safely ignore don't bump the
version. Archives with trash are such an addition: an older build reads the live pages and warns.

Golden fixtures: `tests/fixtures/linux-export.penroam` and the format fixtures must keep importing.

## `.penroam-backup`

```text
mimetype                  "application/vnd.penroam.library-backup+zip" (first entry, stored)
backup.json               { format: "penroam-backup", formatVersion: 1, generator, createdAt,
                            notebooks:  [{ id, title, path, sha256, bytes }],
                            unreadable: [{ id, title, error }] }
notebooks/<uuid>.penroam  a format 2 archive with trash, reproducible, stored without compression
extras/search-corrections.json  optional: [[notebook, page, region, text], ...]
```

* Each nested archive is an ordinary `.penroam`, so one notebook can be unzipped and imported alone.
  The defensive import is the only code that reads notebook content.
* `path` must be exactly `notebooks/<id>.penroam`, and ids must be unique. Otherwise the backup is
  refused whole. A file that isn't a ZIP, a truncated ZIP, a wrong `mimetype` and a newer
  `formatVersion` are refused whole too.
* `sha256` and `bytes` are checked on the unpacked nested archive, with the size enforced while
  copying. A nested archive may be at most 4 GiB. Failures here affect only that notebook.
* A notebook that couldn't be read at backup time is listed in `unreadable`. The backup is never
  silently incomplete. A full disk or other I/O failure fails the backup.
* The app reads each new backup back through the restore checks before writing it to the destination.
* Backups and restores take the library lock one notebook at a time and report progress, so a long
  one doesn't stop saving. Each notebook is consistent; notebooks aren't a snapshot of one instant.

**Restore** shows a preview, verifying everything without changing the library. It never overwrites:
a notebook whose id is already in the library is skipped unless the user asks for a copy, which gets
a new id and "(imported)" in its title. Notebooks are verified and installed one at a time, each
with an atomic rename, so a failure leaves the others restored and the library as it was.

Search is derived data: restored notebooks are queued for indexing, and handwriting is read again.
Corrections typed into search are restored with their notebooks and attach when pages are re-read.

## Importers

An import always makes a new notebook, reports what it kept and lost, and either arrives whole or
leaves nothing behind.

| Format | Kept | Lost, and reported |
|---|---|---|
| PDF | The original file, unchanged; page sizes and rotation | Nothing |
| Xournal++ `.xopp` (gzipped or packaged ZIP), Xournal `.xoj` | Page sizes up to `fileversion` 5; pens and highlighters with colour, opacity and per-segment width; whiteout strokes as white ink; text position, size, colour, wrapping and alignment; hyperlinks; LaTeX as live formulas; PNG and JPEG images; PDF backgrounds packaged in the file; ruled, graph, dotted and plain paper | PDFs and image backgrounds stored elsewhere (paths are never followed), coloured paper, other paper patterns, layers (merged), fills, dash styles, audio links, fonts, rotation of text and images (placed straight) |
| Evernote `.enex` | PDF attachments up to 64 MiB, as pages after their note; One notebook for the file, each note starting a new page with its title as a heading; bold, italic, strikethrough, headings, lists, code, quotes and rules as Markdown; `http`, `https` and `mailto` links; task boxes; PNG and JPEG images, after the note's text or on the pages following it | Tags, created and updated dates, note details such as author, the position of images in the text, other attachments (audio, other image types), attachments over the size limit, encrypted sections, links to other addresses (their text stays), underline and colour, table widths and merged cells (tables become Markdown tables with the first row as header) |
| Joplin `.jex` | Notes as Markdown, one notebook in folder order; PNG and JPEG images, moved to the end of their note; PDFs a note links to, as pages after it | Folders, tags, dates, encrypted notes, other attachments. Tested with hand-written files |
| Markdown | Text, paginated between blocks; links as text | Image references |
| Plain text | Text and line breaks, escaped to read literally | Unreadable bytes, replaced with U+FFFD |

Only documented or openly specified formats are read. Goodnotes and OneNote have no such format, so
the route for those is their own PDF export. Importers are checked against real files, which aren't kept here because of
their licences: Xournal++'s own test documents (47,620 strokes, every element accounted for), and
drawn output compared with Xournal++ 1.2.2's own PDF export; Evernote exports from the test suites
of Joplin, evernote2md, Trilium and OneMore; and a `.jex` written by the Joplin 3.7.1 command line.
To repeat a run, see "Testing importers on real files" in `docs/development.md`.

### Hostile input

Everything imported is treated as attacker-controlled.

* **Paths**: nothing inside a file is opened. Archive and backup entry names are whitelisted.
* **Decompression**: limits apply to bytes actually produced, not to headers: XML 256 MiB, text
  64 MiB, plus the archive limits in `model-and-archive.md`.
* **Allocation**: 16 million ink samples per document, the model's page and element limits, images
  at most 64 MiB and 100 megapixels, read from the header without decoding. An `.enex` file may be
  at most 1 GiB and one note's text 64 MiB. An attachment over 64 MiB is skipped and counted, without
  being held in memory.
* **XML**: no DTD processing. An unknown entity is an error, so entity bombs and external entities
  do nothing. XML that doesn't parse or ends early is refused whole; a single bad element is dropped
  and counted. The body of an Evernote note is the exception, because real ones are often not
  well-formed: it is read tolerantly, and a note that still can't be read, or that uses an unknown
  entity, is kept as plain text and reported, with the entity left as literal text.
* **Content**: images must carry a PNG or JPEG signature. Text is inert: notes run no scripts and
  links are not followed. Evernote `script` and `style` contents are dropped, and resource file names
  are ignored. Attachments are matched to notes by MD5 only because that is Evernote's own id. SVG
  export escapes everything and embeds only PNG and JPEG.
* **Output**: everything an importer returns passes the model's validation.

Real files also shaped what counts as damage. Xournal++ pressure lists contain `0` and `nan`, so a
bad sample becomes the lightest touch rather than costing the stroke. Evernote exports contain
nested CDATA, bare `&` and stray closing tags; those are tolerated, but a file that ends early is
still refused.

Fuzz targets: `import_xournal`, `import_jex`, `import_text`, `import_backup`, `import_enex`, plus byte-mutation tests in
`crates/penroam-interchange/tests/import.rs` that run with `cargo test`.

## Lossy exports

* **Markdown / plain text**: typed text in reading order, a heading per page, then the text read
  from that page's handwriting (with the user's corrections) if search has read it, flagged as
  possibly wrong. Handwriting not read yet, images and PDF content are left out and counted.
* **PDF**: ink as vector paths, images at full resolution, then text. A JPEG keeps its original
  bytes, and its EXIF orientation is applied by how it is placed. A PNG becomes 8-bit RGB with
  transparency in a soft mask. Each image is stored once however many pages use it. Image assets
  are checked again on export: one that is missing, damaged, over 100 megapixels or in a JPEG
  variant PDF can't show is left out with a warning naming the page, and the export carries on.
* **PNG**: drawn by the same renderer as the screen at twice the page's size in points, including
  any PDF background.
* **SVG**: vector ink from the shared stroke geometry and live text in Liberation faces with
  fallbacks. No PDF background, and formulas appear as LaTeX source. Both are reported.
* **Clipboard**: a page's typed text.

The PDF exporter's text overlay stays limited to typed text: recognised handwriting isn't written
into PDFs, because a search hit on a misread word would be wrong in a way the user can't see.

## External storage

Third-party storage is backup-only. Automatic backups write one whole, dated `.penroam-backup` a
day while the app is open, to a folder (desktop) or a WebDAV server (`crates/penroam-webdav`), and
keep the newest seven. Rotation only removes files named `Penroam backup <date>.penroam-backup`.
Nothing is merged: two devices leave two files, and concurrent edits of one `.penroam` file aren't
reconciled. Only the operation sync protocol merges concurrent work.

A WebDAV upload goes to `<name>.part` and moves over the final name once the server reports the
full size, so an interrupted upload can't replace a good backup. The password is in the OS keychain,
is sent only to the configured origin, and only over HTTPS (plain HTTP is accepted for loopback
addresses). Redirects aren't followed. These files are as unencrypted as any other backup.

## Version history

Module docs: `crates/penroam-storage/src/history.rs`.

```text
notebooks/<nb>/history/<page>.phl       chain = entry*
  delta    = a journal record for one page revision (penroam-binary-v1.md section 7)
  keyframe = "PRHK" u32 len, u32 crc32(payload), payload (binary page encoding)
notebooks/<nb>/history/notebook.jsonl   { at, title, pageOrder } when either changes
```

History is built for size. A chain starts from a keyframe and then holds one delta per page
revision; a new keyframe is written only when deltas exceed the larger of 8 MiB and four times the
last keyframe. Reading an old version replays from the keyframe before it.

* **Rollback** computes the difference between the current page and the old version and applies it
  as an ordinary batch. Nothing is rewritten, so it syncs, and the replaced version stays restorable.
* **Restoring a notebook** to a time reverts each page, recovers trashed pages from then, moves
  newer pages to the trash, then sets the order and title. Each step is its own operation.
* **Chains are per device.** They aren't in archives, backups or sync, and revision numbers aren't
  comparable between devices. Sync changes are recorded as versions on pages a device already had.
* **History in the account.** With sync on, the account already stores every operation from every
  device, end-to-end encrypted, and each device keeps the full log (`sync/oplog.pjs`). So nothing
  extra is uploaded: `SyncEngine::page_versions`, `page_at` and `notebook_at` replay a notebook's log
  up to a point, and the app offers those versions for times older than this device's own chain.
  A replacement device therefore sees the history of devices that no longer exist. It works offline,
  needs no server support, and is already counted in the account's storage. Any version restored
  this way is an ordinary forward edit.
  A notebook's first sync uploads what it holds as one snapshot, so edits from before then aren't
  separate versions in the account. They remain in the chains of the device that made them.
* **Retention** (`HistoryRetention`, default 7/30/365 days): every version for a week, the last of
  each day for a month, the last of each week for a year, then nothing. Thinning runs after launch.
  It rewrites a chain with the oldest kept version as a keyframe and each later kept version as a
  diff from the one before, so content added and erased between two kept versions leaves the disk.
* **Deletion.** Erased content stays in a device's chains until retention drops it. Clearing a
  notebook's history removes them at once, and purging a notebook removes its folder, history
  included. Neither reaches the journal or `.prev` snapshots, which age out on their own.
* **The account's log is not thinned or cleared.** Retention limits what the app *offers* from it,
  but the operations stay, encrypted, in the account and on synced devices until the account is
  erased, because merging depends on them. Clearing history says so when sync is on.
* **Not a safety net.** Appends aren't fsynced and a failure never fails an edit. A crash can lose
  the newest versions, and damage costs versions up to the next keyframe. Opening a library,
  recovery and `.prp.prev` never depend on history.
