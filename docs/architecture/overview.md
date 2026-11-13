# Penroam architecture

Penroam is a Tauri 2 app: a Vue 3 + TypeScript editor in the system WebView, a Rust core that
owns all notebook data, and native ink surfaces on BOOX (Onyx Pen SDK) and iPadOS (UIKit Pencil).
Tauri was chosen over Flutter after an evaluation of both on the target devices.

## 1. Layout

```text
apps/desktop-mobile/            Tauri 2 app
  src/                          Vue 3 + TS + Vite + Tailwind 4 + @lucide/vue
    editor/                     framework-free editor: engine, renderer, session, geometry, text and Markdown layout, math, PDF.js
    components/ views/          UI (library, editor, toolbar, sidebar, dialogs)
    composables/                editor host (page loading, flushing, native surface wiring)
    stores/                     Pinia: library + UI state + device preferences (UI state only)
    platform/                   typed facades: ipc (Rust commands), files (dialogs), nativeInk, lifecycle
    generated/                  TS types generated from the Rust schema (ts-rs) — do not edit
    bench/                      Workload D benchmark harness (docs/benchmarks/protocol.md)
  src-tauri/                    Rust shell: typed commands, structured errors, flush-on-close
    gen/android/                generated Android project (+ scripts/patch-android.py)
crates/penroam-core             schema, validation, page ops, operation schema + merge model (replica), binary page codec, geometry, text and Markdown layout, migrations
crates/penroam-storage          live library: device-wide operation journal, checkpoints, trash, recovery, outbox counters, version history, .penroam archive, full-library backups
crates/penroam-pdf              PDF inspection (import) and annotated vector export (lopdf)
crates/penroam-interchange      other formats: Xournal++, Markdown and text import with loss reports; text and SVG export
crates/penroam-webdav           backup files on a WebDAV server (backup storage only, nothing merges)
crates/penroam-sync-protocol    public sync wire contract: versions, ids, limits, envelope header, canonical bytes, framing, DTOs, errors
crates/penroam-sync-testkit     development test server (in memory / persisted, HTTP, fault injection) + fixture generator
crates/penroam-sync             sync client: crypto, key log, operation log, transports, reconcile engine, dev CLI
crates/penroam-search           local search: handwriting regions, recognizers (Windows ink, Apple Vision, Penroam model), SQLite FTS5 index
plugins/tauri-plugin-native-ink NativeInkSurface: Rust bridge, android/ (Kotlin), ios/ (Swift/UIKit)
plugins/tauri-plugin-native-print system print dialogs: GTK (Rust), android/ (Kotlin), ios/ (Swift/UIKit)
android/  ios/                  native ink modules used by the plugin: BooxInkView (Kotlin, Gradle root for its tests), InkKit (Swift)
tests/fixtures/                 shared fixtures (strokes, geometry, text and Markdown layout goldens, scenes, PDF, .prp golden, legacy libraries)
fuzz/                           cargo-fuzz targets (page decoder, journal open, sync batch frame)
tools/linux-e2e/                XTest input driver + session runner for real-app E2E on Linux
tools/screenshots/              README screenshots: capture.sh (Linux, Xephyr) and capture-windows.mjs (WebView2 DevTools)
tools/  docs/                   dev tools, documentation (benchmarks/ = local, git-ignored benchmark output)
```

## 2. Data ownership

| Data | Owner | Notes |
|---|---|---|
| Notebook/page/asset content | **Rust** (`penroam-storage`) | Operation journal + snapshots on disk. Every change is journaled as typed operations and acknowledged only after an fsync |
| Open page working copy, undo/redo stacks | Vue editor (`PageSession`) | Mirror of the Rust page; edits are applied locally with the same semantics and queued to Rust in order |
| Live pen samples | The active surface (web canvas or native view) | Never cross IPC while a stroke is live; only finalised strokes do |
| Rendered caches (page bitmaps, thumbnails) | Frontend | Thumbnails are stored by Rust as derived files, never exported or synced. Installing a synced page drops its thumbnail; the library draws missing covers (`editor/pagePicture.ts`) |
| UI preferences (theme, tool colours, pen-only mode) | `localStorage` | Per device, not notebook data |
| Search index (recognized handwriting, typed/PDF text) | Rust search worker | Derived, in the app cache directory; rebuildable, never exported or synced (§5) |

Rust is the only writer. The renderer never sees library paths; import/export pass a
path/URI chosen in a system dialog straight back to Rust (`tauri-plugin-fs` resolves Android
`content://` URIs). No `fs` commands are exposed to the WebView (`capabilities/default.json`).

## 3. Document model and persistence

Specs: [`../formats/model-and-archive.md`](../formats/model-and-archive.md) (JSON model, archive) and
[`../formats/penroam-binary-v1.md`](../formats/penroam-binary-v1.md) (`.prp`, journal, checkpoints, operations).

* Units: **points** (1/72 in), page-local, y down. At zoom 1 one point is one CSS px, so the
  shared geometry golden (`tests/fixtures/geometry-golden.json`) applies unchanged.
* `Page.elements` = `ink | text | image`; the PDF page is `Page.background` (`PdfBackgroundRef`).
* Stroke points keep `x, y, pressure, tiltX, tiltY, timeUs` plus optional `twist`/`flags`; the
  stroke `input` map keeps platform metadata verbatim (Onyx stream counts, raw pressure range,
  coalesced counts…). Smoothing is derived at render time.
* **Operations.** Every change becomes `LocalOperation`s (`penroam_core::operation`): a stable
  op id, the device id, a per-device `local_sequence` (never a wall clock), causal dependencies,
  and a typed body (notebook create/rename/cover/trash/restore/purge, page create/trash/restore/
  reorder/template, element add/remove/replace, asset reference). Editor batches
  (`OperationBatch`) are translated one-to-one, so local replay is bit-identical to applying
  the batch.
* **Journal.** One device-wide, segmented, append-only binary log in `<library>/journal/`. A
  transaction is fsynced before the call returns — that is what *Saved locally* means.
  Snapshots (`notebooks/<id>/notebook.json`, `pages/<id>.prp` + one `.prp.prev` fallback) are
  checkpoints that may lag; `checkpoint.json` records which sequences they cover. Start-up
  recovery truncates a torn tail, quarantines (never deletes) mid-log damage, replays everything
  after the checkpoint floor, removes temp files and moves unreferenced files to `orphans/`.
  An exclusive file lock prevents a second writer.
* **No database for notes**: snapshots plus journal are the source of truth. `list_notebooks`
  reads small `notebook.json` files. The search index (§5) is a separate, disposable SQLite
  file derived from them, never a source of truth.
* **Permanent delete.** With sync on, a purge is an operation like any other, so it is queued
  offline and every device erases its copy. Its operations stay in the account, which
  is what makes it recoverable: a `notebookRestored` from any device brings it back everywhere.
  `Replica::is_purged` decides what a device does, not the library's purge list, which only lasts
  as long as the journal retains it. Notebooks the account holds that aren't on this device are
  found by comparing the operation log with the library: the shelf offers those the account has
  live, and the trash those it has deleted. A purge frees space on devices, not in the account,
  and without sync it is final. Connecting a device clears its purges, since it starts over with
  what the account holds. A later install supersedes the purge, which the journal may still hold
  (`Library::replay`).
* **Merge model.** `penroam_core::replica::Replica` merges operations from several devices
  order-independently: multi-value registers with version-vector dominance, a deterministic
  visible winner (highest op id) with every alternate retained as a conflict, tombstones,
  add-wins restore, fractional page order keys. The sync engine applies operations to it with
  `apply_delivered` after delivering them in causal order across the whole account.
* **Folders.** Library folders (`penroam_core::folder`) have no notebook to live in, so their
  operations use a reserved stream id. The journal, routing, keys and causal delivery treat it like
  any notebook, and `Replica` holds the folders instead of a notebook. Each notebook stores its own
  folder. Deleted folders stay as tombstones and cycles from concurrent moves are cut when reading,
  so neither needs a repair write and a notebook can't be stranded. Only the main account has
  folders: the stream belongs to no vault, and notebooks in a vault can't be filed.
* **Sync contract.** `penroam-sync-protocol` defines the wire format (envelope header, canonical
  signing/AEAD bytes, `PRSB` batch framing, DTOs, errors, retry policy). `penroam-sync-testkit`
  is the development test server (in memory or persisted to a folder, with an HTTP front end
  and fault injection). Neither depends on core or storage. See
  [`../protocol/sync-protocol-v1.md`](../protocol/sync-protocol-v1.md) and
  [`../protocol/openapi.yaml`](../protocol/openapi.yaml).
* **Sign-in (`auth.v1`).** Servers with real accounts advertise `auth.v1`. `penroam_sync::auth`
  signs in with the server's OpenID Connect provider in the system browser (authorization code
  + PKCE, `state` and `nonce` checks; desktop gets the answer on a loopback redirect, on any
  port or on one of a few registered ports, and Android and iOS get it as an `ink.penroam.app:`
  link), exchanges the ID token for an account
  session, and gets a device session by signing a server challenge with the device key; the
  server only grants that to active key-log members. The hosted Penroam Cloud server is a
  separate, private codebase that implements the same public protocol; nothing in this
  repository depends on it, and the development test server keeps its stub sign-in.
* **Plan and storage (`account.v1`).** Servers that give accounts a storage allowance advertise
  `account.v1`: the account's plan, allowance, whether uploads are accepted, what the user can do
  next, and a server-made `https://` link to the server's own billing pages, which the app opens
  in the system browser. No payment provider, price id or payment result passes through the
  app; the server derives the plan from its provider's verified records. A read-only account
  still downloads and exports, and nothing on the device is locked or deleted. Store-distributed
  mobile builds never open those pages: the backend drops Stripe offers and refuses billing links
  there (`PURCHASES_ALLOWED` in `src-tauri/src/sync.rs`). A banner over the library and
  the editor (`CloudBanner.vue`, text from `cloudNotice` in `src/platform/sync.ts`) warns at
  90 % storage, when uploads pause, and after a failed payment; on desktop it opens the plan
  picker or the billing portal. When the account accepts uploads again (focus after checkout),
  the app syncs at once instead of waiting for the next attempt. Account deletion needs a fresh
  browser sign-in and only ever erases the cloud copy.
* **Plans in the app store.** Store builds (iOS, and the Mac App Store build) sell through
  `tauri-plugin-store-billing` instead of the browser. The plugin only talks to the store. The server looks every purchase up with the store
  itself, and the app finishes a store transaction only after the server has answered, so a crash
  or an outage can't lose a purchase.
* **Accounts and vaults.** Connecting needs only a sign-in: the first device stores the account's
  root secret on the server (`account-key.v1`), and later devices fetch it and add themselves to the
  key log, so the server could read ordinary notebooks. A vault (`penroam_sync::vault`) holds
  notebooks it couldn't: on the wire it's a hidden second account, and its root secret reaches the
  server only wrapped with a passphrase. The app runs one more `SyncEngine` per vault unlocked on the
  device, with its data under `<library>/sync/vaults/<id>/`. Keeping vaults as separate accounts
  means a locked vault can't stall ordinary sync, whose causal delivery spans the whole account, and
  a main-account checkpoint can't contain vault content. Each engine's `Scope` limits it to its own
  notebooks, by `<library>/vault-notebooks.json`. That file is written under the library lock in the
  same critical section that creates a notebook, because the main engine uploads whatever isn't
  assigned elsewhere; it sits outside `sync/` so that disconnecting can't release vault notebooks to
  the main account. The library releases a journal entry only when every engine has copied it.
* **Locking a vault on a device.** `penroam_sync::locker` takes a vault's notebooks out of the
  library (`Library::detach_notebooks`, which also drops their journal transactions), encrypts them
  with the vault's sync folder and its own search index under a key derived from the vault's root,
  and deletes the plaintext and the device's keys for the vault. A committed pack means the vault is
  locked; start-up deletes any plaintext found next to one. Unlocking decrypts into a staging folder
  before moving anything, then the notebooks rejoin the library (`Library::attach_notebooks`) and the
  engine resumes reading the journal at the current position. Format:
  [`../formats/locked-vault.md`](../formats/locked-vault.md).
* **Shared notebooks.** `penroam_sync::shared` syncs one notebook among several accounts. Each
  shared notebook lives in a *sharing space* (`sharing.v1`, protocol §12f): a hidden account like a
  vault that the sharing account owns and that holds exactly that notebook, with its own root
  secret, key log, operation log, blobs and checkpoints. Members are other accounts with a role
  (owner, editor, viewer). The server keeps the space's root like an ordinary account key, and every
  member device fetches it with its own session and adds itself to the space's key log, which must
  verify against that root. Each device runs one more `SyncEngine` per space (`Scope::Shared`, data
  under `<library>/sync/shared/<id>/`), so cursors, acknowledgements and device sequences never
  mix between spaces, accounts or people. Its sync uses a device session on the space, traded for
  the account session on the space that the device's own session obtains. In a space a device
  uses its id from the main account, unless that id was in the space before (the account was
  removed and admitted again), in which case it gets a new one. `vault-notebooks.json` gains a `shared` map (notebook →
  space, role, state). The account engine neither ingests nor installs a notebook listed there, and a
  space's engine installs only its one notebook, and only once joining finished (everything pulled,
  every referenced asset downloaded). Where a member files a shared notebook stays local, whatever
  their role, and so does erasing it on one device. A notebook that is or was shared here can't be
  imported or restored under its own id (`Library::set_reserved_ids`). In a space only the owner's
  devices publish checkpoints.

  Sharing an existing notebook keeps its id and its history. The app creates the space (idempotent,
  with the root kept in the secret store first), stores the root, and re-publishes the notebook's
  operations from the account log into the space in log order, as operations of the sharing device
  with the same bodies and times and fixed derived ids. Then, under the library lock, it adds a
  corrective diff so the space's notebook equals the library's, writes a `notebookShared` operation
  (operation schema 5) into the account log and assigns the notebook. Each step is persisted
  (`<library>/sync/shares/<notebook>.json`) and can be repeated after a crash. The owner's other
  devices assign the notebook when they apply that marker, before installing anything of it, then
  join the space. Edits they made without having seen the marker are re-published into the space by
  the device that made them, before that device installs from the space. An older client parks the
  marker and pauses with "needs an update" instead of guessing. If two devices of one account share
  a notebook at the same time, the marker with the higher id wins on every device. The losing device
  moves its edits from its own space into the winner and removes its own space locally; that space
  stays on the server, empty of anything the winner lacks.

  A viewer's engine never uploads, and every app command that changes a notebook refuses viewers and
  notebooks whose access was removed (`require_writable` in `src-tauri/src/sharing.rs`); the server
  refuses a viewer's writes as well. The one exception is page edits to a notebook whose access was
  removed: they still reach the library, so batches in flight at that moment aren't lost, and they
  stay on the device because that space's engine has stopped. A damaged owners file doesn't block
  local edits; every engine refuses to sync with it instead. When access is removed (the server answers `access_revoked`, or
  lists the membership as revoked or not at all), the space's engine copies the journal entries it
  hasn't yet, stops for good and marks the notebook *access removed*. The notebook and its unsent
  edits stay on the device, read-only; the user can keep a private copy (a duplicate under a new id
  in the personal library) or remove the device's copy. Accepting a new invitation to the same
  space waits until one of those happened: the old copy and its unsent edits are never merged into
  the new membership. An owner device that sees a removed member's
  devices still active in the space's key log removes them there, which starts a new key epoch
  without their device keys (the member still holds the space's root, which can open every epoch;
  only the server's refusal keeps them from new content). Each epoch's key in a space is
  what its recovery grant opens to with the root, the same for every member; an epoch whose
  recovery grant doesn't open is void, and devices keep using the last good one, so a member's bad
  key-log entry can't lock the others out. Only the owner's devices publish
  checkpoints and refresh the sealed title, and the notebook a space maps to is fixed: on the
  owner's devices by the share marker, elsewhere at the first join, never later from the title. The
  app's worker reconciles spaces after the main engine and the vaults, rereads the
  lists of shared notebooks and invitations each pass, and emits `sync://shared` when what the
  library shows changed. Security: [`../security/threat-model.md`](../security/threat-model.md).
* **Sync engine.** `penroam-sync` keeps its own state under
  `<library>/sync/`: an append-only operation log of every synced operation (local and remote),
  parked and quarantined envelopes, and the verified key log pin. One reconcile has three phases
  so the library lock is never held during network I/O:
  1. *prepare* (locked): on first use, synthesize create operations from the current
     snapshots; then copy new journal operations into the operation log, re-sequenced as the
     sync device's operations whose dependencies are the version vector the library view had
     absorbed;
  2. *exchange* (unlocked): verify the key log, upload blobs, push sealed and signed envelopes
     (per-operation acks), pull pages, verify signature and AEAD before anything else, deliver
     causally or park, persist the cursor only after that is durable;
  3. *apply* (locked): merge every notebook that received remote operations
     (`Replica::apply_delivered`) and install the result with `Library::install_merged`, which
     writes snapshots and records how far the install covered the journal so replay never
     re-applies an edit the merged snapshot already contains.

  In the app a worker thread runs reconciles (debounced after local edits, every minute when
  idle, exponential backoff with jitter after failures) and emits `sync://status` and
  `sync://changed`. Crypto and membership: [`../security/threat-model.md`](../security/threat-model.md).
* Undo/redo are semantic: each change carries forward + inverse ops; undo enqueues the inverse
  as a new batch with fresh op ids. Page revisions guard against stale batches (a mismatch is a
  conflict, never a merge); a retried batch id returns the original outcome.

## 4. Editor and ink pipeline

* **Page-sized cached layers** (`renderer.ts`): `bg` (paper, PDF bitmap, template), `ink`
  (strokes, images), `text`. They live inside a CSS-transformed element, so pan and zoom never
  redraw; the backing resolution is re-rendered once a zoom settles (160 ms), capped by pixel
  area (16 Mpx desktop, 8 Mpx mobile — WebKit's iOS canvas limit). Erase/undo/move redraw only
  the dirty rectangle; a new stroke is appended incrementally.
* **Infinite canvas** (`Notebook.infiniteCanvas`): the same layers cover an *extent* of the
  unbounded page instead of the sheet: the content and the view with half a view's margin,
  snapped to 512 pt. When a pan or zoom settles outside it, the extent grows, or becomes a
  window around the view when growing would pass the pixel budget at the lowest backing scale,
  so memory stays bounded however far a canvas reaches. Panning is unconstrained and fit fits the
  content. On ruled paper the canvas is a column the page's width, with the sheet's header and
  margin line: the extent spans that width and grows only downward, the view is kept on the
  column to a page's length past its content, and text is placed and sized as on a sheet. PDF pages on a canvas are image elements with `pdfPage`, rendered by PDF.js sharp
  near the view and at a low resolution elsewhere. Exports cut a canvas page to a sheet around its
  content (`Page::canvas_as_sheet`), which PDF export and printing split into paper-shaped
  pages at gaps in the content (`Page::canvas_as_sheets`); the PDF exporter draws `pdfPage` elements from their source
  page as form XObjects, so they stay vector. Combining a notebook's pages onto one canvas
  (`Library::collapse_pages_into_canvas`) is ordinary journaled operations: content is added to
  the first page before the others go to the trash.
* **Live stroke**: screen-space `active` canvas for stable pieces drawn
  incrementally and `tail` canvas for the provisional piece and predicted samples (50 % alpha,
  never stored). One sample stream (`pointerrawupdate` when present, else `pointermove`) plus
  `getCoalescedEvents()`.
* **Hand-off**: on pen-up the stroke is committed to the session, which paints it into the
  page cache synchronously; the preview is cleared in the same task, so both changes reach the
  same frame (no flash, no double). Native surfaces use an explicit acknowledgement (§6).
* The page itself must never zoom. WebView2's pinch zoom is off and WKWebView doesn't magnify
  unless asked to, but WebKitGTK zooms the page natively on a touchpad pinch and tells nobody. On
  Linux `src-tauri/src/pinch.rs` takes pinches from GDK before GTK dispatches them and forwards
  them to `InkEditor.platformPinch`, which zooms the canvas.
* Text blocks are Markdown, rendered from a deterministic layout (`markdown.ts` =
  `penroam-core::markdown`, built on the metrics in `textLayout.ts` = `penroam-core::text`), so
  wrapping is identical on every OS and in the PDF. The two ports use different parsers
  (markdown-it and pulldown-cmark) and are held together by a shared golden file; formulas are
  scanned out before either parser runs, so they can't disagree about TeX. Rendering uses bundled
  Liberation fonts, metric-compatible with the Helvetica and Courier that the PDF export uses, or
  for handwritten blocks Excalifont, which the PDF export embeds. Excalifont has one face, so its
  bold and italic are a stroke and a shear, applied the same way by the canvas (`drawRun` in
  `textLayout.ts`), the PDF and the SVG export. A
  layout is cached by its inputs, because hit tests and dirty-rect redraws need a block's bounds.
  Editing uses a CodeMirror overlay in the same coordinates (`markdownEditor.ts`). Its document
  is the Markdown source, with decorations that show the formatting and hide the markers away
  from the caret (`markdownPreview.ts`). That preview parses with Lezer and is approximate: the
  finished block is always drawn by the layout above. Text locked to the paper's rows uses the
  row pitch as its line height there too.
* Tables are spreadsheets. The formula engine is `penroam-core::sheet`, ported in `sheet.ts`
  and held to it by `tests/fixtures/sheet-golden.json`; both layouts read each cell's source and
  draw a formula's result, or a checkbox for `[ ]`/`[x]`. While a box is edited, a table at the
  top level of its Markdown shows as a grid (`sheetGrid.ts`), a CodeMirror block widget over the
  table's lines. Every grid edit is a transaction that replaces those lines with the table
  printed back as GFM (`sheetTable.ts`, which also sorts, filters, inserts and deletes and moves
  formula references with the cells), so undo, the page history and sync only ever see text.
  Filters are the grid's own state and never reach the note. The table shows as source while
  the caret is on it after typing there, so a new table can be written by hand.
* Formulas come from MathJax, loaded on first use because it is the largest dependency. It runs
  only in the webview, so PDF export and printing first ask Rust which formulas a notebook has,
  render them, and pass the outlines back. Until MathJax is ready a formula shows its source.
* Text blocks align with each other as they are dragged or placed (`snap.ts`). This only moves
  them: it adds nothing to the page model. Row-locked text snaps across only, since its rows
  are already aligned.
* Paint order is ink → images → text (same in the PDF exporter).
* Input rules: pen eraser button erases; palm-sized touches are ignored (not on iOS, where
  fingertips report palm-like sizes); touches are ignored
  while the pen is down; "pen-only" mode (auto after the first pen use on the device, remembered
  across launches; native surfaces report it themselves) makes fingers pan and
  zoom; `pointercancel`, lost capture, window blur and app backgrounding finish or cancel
  gestures so no stroke is ever left stuck.

**Sharp past the canvas limit.** Page canvases are capped by pixel area (8 Mpx on mobile, about
200 % zoom on an iPad with a Letter page). Beyond that, two detail canvases cover the viewport plus
a margin at full resolution, and a clip-path cuts their area out of the page layers. They sit in the
viewport rather than the scaled page element, shown 1:1 with device pixels, because a compositor can
stretch a `will-change: transform` element from a fixed scale. On iOS they are used for any zoom past
fit width. Selection outlines and the text editor sit in the page element, so ink draws above them
while this layer is up. The page layers stay complete underneath, so a pan past the detail area shows
softer ink until the next settle, never missing ink. The template and PDF background stay on the
page layer and soften past the limit.

**Redraws never hold up the pen.** A settled zoom draws the visible region into a detached canvas
in slices of about 8 ms and swaps it in with the other layers in one task; the rest of the page
follows in bands, nearest first, and shows the old pixels scaled until then. A stroke added
meanwhile is queued on the job, and any other ink change restarts it. Only the open page has
canvases and redraw jobs (pictures of other pages are flat images, below), so a notebook's size does
not affect this. There is no separate tile cache: the page-sized canvas
already covers pan, and the cost is in drawing, not in finding what to draw.

**Continuous scrolling** (`pageStrip.ts`, on by default, toggled at the bottom of the canvas). Only the
open page is live. The other pages are laid out in one column around it, in its coordinates, and
move with the same CSS transform. Each is a flat picture: the sidebar thumbnail at first, then a
canvas the host renders off-screen for the pages within a screen of the view, at most a quarter of
a page layer's pixels each and six kept. A page whose size is not loaded yet takes the open page's.
When the middle of the view reaches another page, the editor asks the host to open it with the view
kept, so the column doesn't move. The page left becomes a picture of itself as last drawn. A press
on another page opens it rather than drawing on the open one, and nothing switches during a stroke,
drag or text edit. The host keeps the sessions of the last eight pages opened in the notebook, so
undo history survives scrolling back and forth. A session is dropped when its page changes elsewhere.
The sidebar, the page arrows and `PgUp` / `PgDn` jump to a page and fit its width.

## 5. Search

Search is local and offline. `penroam-search` builds a derived SQLite FTS5 index; the app's
search worker (`src-tauri/src/search.rs`) keeps it current. Storage and privacy rules:
[`../security/search-index.md`](../security/search-index.md).

* **Sources.** Notebook titles, typed text blocks, the selectable text of PDF page backgrounds
  (`penroam_pdf::page_texts`, cached per PDF asset) and recognized handwriting.
* **Handwriting regions.** `segment` groups pen strokes into lines from vector geometry only
  (bounding boxes, vertical overlap, gaps relative to the median stroke height, writing order
  from timestamps). A region's key is a hash of its stroke ids (survives a move; corrections
  attach to it); its fingerprint also covers the points. Each region is recognized once:
  re-indexing a page reuses every result whose fingerprint and recognizer
  (`id/model version/language`) are unchanged, so an edit re-recognizes only the lines it
  touched, and a recognizer upgrade re-recognizes in the background while old rows stay
  searchable.
* **Providers.** `Recognizer::recognize(region, language) -> Vec<RecognitionCandidate>`
  (text, alternates, confidence, source stroke ids and bounds). Platform recognizers: the
  Windows handwriting recognizers (`windows-ink` feature; stroke based) and Apple Vision on
  macOS / iOS (image based: the region is rendered by `raster.rs`; not yet run on Apple
  hardware). The optional **handwriting model** (`model.rs`, feature `onnx-model`) is a
  bundle kept outside the app: a CTC line recognizer in ONNX, run with ONNX Runtime on the CPU over the same
  rendering. The bundle declares how a line is rendered and fed to its graph, so models with
  different inputs fit
  ([`../formats/handwriting-model.md`](../formats/handwriting-model.md)).
  `ChainRecognizer` puts the platform first and the model behind it: the model handles every
  region where there is no platform recognizer (Android, BOOX, Linux) and any region the platform
  recognizer fails on. Settings → Advanced can restrict search to one of the two.
  The model comes from outside this repository; the app only verifies and runs the bundle.
  Devices with no recognizer of their own download it once at first run, and any device can
  download or update it on request (`src-tauri/src/models.rs`, `penroam_sync::models`), from a
  signed catalog, so models change without an app release. ONNX Runtime stays in the app.
  Without any recognizer, handwriting is not indexed and everything else is.
* **Freshness.** Page revisions only grow, also across sync installs, so the index stores the
  revision each page was built from. The worker is told about edits, notebook changes and pages
  installed by sync; at launch it checks every live page (`Library::read_page_detached`,
  which reads snapshot + journal without disturbing the editor's three-page cache).
* **Scheduling.** One background-priority thread. It takes the library lock only to copy a page
  (and PDF bytes not yet cached). Nothing is indexed until the pen has been idle for 1.5 s;
  recognition checks for cancellation between regions; a cancelled page is stored as
  incomplete and resumed. Pause and "wait while on battery" are user settings.
* **Queries.** User input is split into words and quoted for FTS5 (no query syntax reaches
  SQLite). Words match as prefixes, quoted text as a phrase. If few results come back, words of
  four or more letters also match indexed words within a small edit distance or containing
  them (recognition errors, words run together); those hits are marked fuzzy. Ranking is BM25
  over text, alternates and corrections, weighted by source kind and recognizer confidence.
* **Results** carry notebook, page, the source bounds and the bounds of the matching
  handwritten words. The editor outlines them in a DOM layer inside the transformed page
  element (`InkEditor.showMatches`) and pans to them; the ink is untouched.

## 6. NativeInkSurface

`src/platform/nativeInk.ts` defines the interface (`mount/unmount`, `setBounds`,
`setViewportTransform`, `setTool`, `setVisible`, `loadCommittedInk`, `addStrokes`,
`removeStrokes`, `commitAck`, events `stroke/erased/viewport/status/error`). Coordinates:
the native view covers exactly the editor element's CSS rect; both sides use
`viewPx = (doc * scale + offset) * dpr`.

| | Android / BOOX ("owner" surface) | iOS / iPadOS ("preview" surface) |
|---|---|---|
| Implementation | `NativeInkPlugin.kt` hosting `BooxInkView` (Onyx `TouchHelper` raw drawing, Mode B) | `NativeInkPlugin.swift`: UIKit `PencilPreviewView` + Pencil-only gesture recogniser on the WKWebView |
| Live ink | ePaper hardware pen layer (Onyx) | UIKit `draw(_:)` at display rate (120 Hz on ProMotion in the UIKit reference) |
| Committed ink while visible | Rendered by the native view (transparent background); the web ink layer is hidden to avoid doubled edges | Rendered by the web canvas; native keeps the stroke until `commitAck` (2 s timeout) |
| Pen-up | Native commits into its own bitmap (`PEN_UP_REFRESH` hand-off), then emits `stroke`; web adds it to the model and persists | Native emits `stroke`; web commits + paints, then acks two frames later |
| Shown for | pen, highlighter, eraser | pen, highlighter |
| Hidden for | text, select, pan tools; any open dialog/menu/drawer (`ui.overlays`) | same |
| Sync when web edits | `addStrokes`/`removeStrokes` (undo, delete, move) | not needed |

Non-BOOX Android devices use the web canvas (the generalised native Mode C exists in
`BooxInkView` but is not enabled by default until validated).

Android and iOS also serve `getPower`, since Rust cannot read power state there. The webview passes
the answer to `power_report` (`src-tauri/src/power.rs`) for the search and sync workers.

## 7. Platform constraints

* **Linux/Windows/macOS desktop**: WebKitGTK / WebView2 / WKWebView. Web canvas only.
* **Android**: system WebView (Chromium). The app opts out of Android Auto Backup, because the library is not encrypted on disk.
  BOOX Onyx SDK pinned (pen 1.5.9 / device 1.3.7 / base 1.8.10, BSD-3-Clause per their POMs) from the official Onyx Maven repo.
* **iOS/iPadOS**: Tauri iOS builds require macOS + Xcode (not available in this environment).
  WKWebView renders rAF at 60 Hz unless the private `PreferPageRenderingUpdatesNear60FPSEnabled`
  flag is lifted (`WebTuning` hook in `ios/InkKit`; App Store risk) — hence the native preview layer.
* **Offline**: strict CSP (`connect-src ipc:` only), PDF.js data files bundled. Search and
  handwriting recognition make no network calls.

## 8. Printing

Printing reuses the annotated PDF export, so a printout matches an exported PDF: ink stays
vector, and whatever the export replaces or leaves out is replaced or left out on paper too. The
`print_notebook` command renders the PDF, releases the library lock, writes the PDF to the
library's staging folder and passes it on. The renderer never sees that path.

| Platform | Route | Who applies a page range |
|---|---|---|
| Linux | GTK print dialog, then CUPS | Penroam (`penroam_pdf::keep_pages`) |
| Android | `PrintManager` system UI | The print spooler |
| iOS, iPadOS | `UIPrintInteractionController` | The system |
| Windows, macOS | Default PDF viewer, via the opener plugin | The viewer |

* **No WebView access.** `tauri-plugin-native-print` has no commands; only Rust calls it. The
  generic `call` command of the ink plugin is reachable from the WebView, which is why printing
  is not a method there: a renderer must not be able to name a file to print.
* **Page ranges on Linux.** A job made from a finished PDF is not cut down by GTK, and the range
  is not sent to CUPS. So the file is reduced to the chosen pages, after the dialog closes and off
  the main thread. `keep_pages` prunes unreferenced objects, so pages that were not chosen do not
  reach the print system. Copies, collation, reverse order and pages per sheet stay with CUPS.
* **Temporary file.** The plugin deletes the file when the print system has finished with it or
  the dialog is cancelled. Because the staging folder is emptied every time the library opens, a
  file left by a crash or still held for a PDF viewer goes on the next launch.
