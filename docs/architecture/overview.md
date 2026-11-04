# Penroam architecture

Penroam is a Tauri 2 app: a Vue 3 + TypeScript editor in the system WebView, a Rust core that
owns all notebook data, and native ink surfaces on BOOX (Onyx Pen SDK) and iPadOS (UIKit Pencil).
Tauri was chosen over Flutter after an evaluation of both on the target devices.

## 1. Layout

```text
apps/desktop-mobile/            Tauri 2 app
  src/                          Vue 3 + TS + Vite + Tailwind 4 + @lucide/vue
    editor/                     framework-free editor: engine, renderer, session, geometry, text layout, PDF.js
    components/ views/          UI (library, editor, toolbar, sidebar, dialogs)
    composables/                editor host (page loading, flushing, native surface wiring)
    stores/                     Pinia: library + UI state + device preferences (UI state only)
    platform/                   typed facades: ipc (Rust commands), files (dialogs), nativeInk, lifecycle
    generated/                  TS types generated from the Rust schema (ts-rs) — do not edit
    bench/                      Workload D benchmark harness (docs/benchmarks/protocol.md)
  src-tauri/                    Rust shell: typed commands, structured errors, flush-on-close
    gen/android/                generated Android project (+ scripts/patch-android.py)
crates/penroam-core             schema, validation, page ops, operation schema + merge model (replica), binary page codec, geometry, text layout, migrations
crates/penroam-storage          live library: device-wide operation journal, checkpoints, trash, recovery, outbox counters, .penroam archive
crates/penroam-pdf              PDF inspection (import) and annotated vector export (lopdf)
crates/penroam-sync-protocol    public sync wire contract: versions, ids, limits, envelope header, canonical bytes, framing, DTOs, errors
crates/penroam-sync-testkit     development test server (in memory / persisted, HTTP, fault injection) + fixture generator
crates/penroam-sync             sync client: crypto, key log, operation log, transports, reconcile engine, dev CLI
plugins/tauri-plugin-native-ink NativeInkSurface: Rust bridge, android/ (Kotlin), ios/ (Swift/UIKit)
android/  ios/                  native ink modules used by the plugin: BooxInkView (Kotlin, Gradle root for its tests), InkKit (Swift)
tests/fixtures/                 shared fixtures (strokes, geometry and text-layout goldens, scenes, PDF, .prp golden, legacy libraries)
fuzz/                           cargo-fuzz targets (page decoder, journal open, sync batch frame)
tools/linux-e2e/                XTest input driver + session runner for real-app E2E on Linux
tools/  docs/                   dev tools, documentation (benchmarks/ = local, git-ignored benchmark output)
```

## 2. Data ownership

| Data | Owner | Notes |
|---|---|---|
| Notebook/page/asset content | **Rust** (`penroam-storage`) | Operation journal + snapshots on disk. Every change is journaled as typed operations and acknowledged only after an fsync |
| Open page working copy, undo/redo stacks | Vue editor (`PageSession`) | Mirror of the Rust page; edits are applied locally with the same semantics and queued to Rust in order |
| Live pen samples | The active surface (web canvas or native view) | Never cross IPC while a stroke is live; only finalised strokes do |
| Rendered caches (page bitmaps, thumbnails) | Frontend | Thumbnails are stored by Rust as derived files, never exported |
| UI preferences (theme, tool colours, pen-only mode) | `localStorage` | Per device, not notebook data |

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
* **No database**: snapshots plus journal are the source of truth, so no index can drift out of
  sync. `list_notebooks` reads small `notebook.json` files.
* **Merge model.** `penroam_core::replica::Replica` merges operations from several devices
  order-independently: multi-value registers with version-vector dominance, a deterministic
  visible winner (highest op id) with every alternate retained as a conflict, tombstones,
  add-wins restore, fractional page order keys. The sync engine applies operations to it with
  `apply_delivered` after delivering them in causal order across the whole account.
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
  mobile builds show the plan and storage only: the backend drops offers and refuses billing
  links there (`PURCHASES_ALLOWED` in `src-tauri/src/sync.rs`). A banner over the library and
  the editor (`CloudBanner.vue`, text from `cloudNotice` in `src/platform/sync.ts`) warns at
  90 % storage, when uploads pause, and after a failed payment; on desktop it opens the plan
  picker or the billing portal. When the account accepts uploads again (focus after checkout),
  the app syncs at once instead of waiting for the next attempt. Account deletion needs a fresh
  browser sign-in and only ever erases the cloud copy.
* **Sync engine (developer preview).** `penroam-sync` keeps its own state under
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
* **Live stroke**: screen-space `active` canvas for stable pieces drawn
  incrementally and `tail` canvas for the provisional piece and predicted samples (50 % alpha,
  never stored). One sample stream (`pointerrawupdate` when present, else `pointermove`) plus
  `getCoalescedEvents()`.
* **Hand-off**: on pen-up the stroke is committed to the session, which paints it into the
  page cache synchronously; the preview is cleared in the same task, so both changes reach the
  same frame (no flash, no double). Native surfaces use an explicit acknowledgement (§6).
* Text blocks are rendered from a deterministic layout (`textLayout.ts` = `penroam-core::text`,
  same metrics file, golden-tested) with the bundled Liberation Sans (metric-compatible with
  Helvetica, which the PDF export uses). Editing uses a textarea overlay positioned in the same
  page coordinate system; the canvas copy is hidden while editing.
* Paint order is ink → images → text (same in the PDF exporter).
* Input rules: pen eraser button erases; palm-sized touches are ignored; touches are ignored
  while the pen is down; "pen-only" mode (auto after the first pen event) makes fingers pan and
  zoom; `pointercancel`, lost capture, window blur and app backgrounding finish or cancel
  gestures so no stroke is ever left stuck.

## 5. NativeInkSurface

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

## 6. Platform constraints

* **Linux/Windows/macOS desktop**: WebKitGTK / WebView2 / WKWebView. Web canvas only.
* **Android**: system WebView (Chromium). Release builds drop the `INTERNET` permission.
  BOOX Onyx SDK pinned (pen 1.5.2 / device 1.3.3 / base 1.8.4) from the official Onyx Maven repo.
* **iOS/iPadOS**: Tauri iOS builds require macOS + Xcode (not available in this environment).
  WKWebView renders rAF at 60 Hz unless the private `PreferPageRenderingUpdatesNear60FPSEnabled`
  flag is lifted (`WebTuning` hook in `ios/InkKit`; App Store risk) — hence the native preview layer.
* **Offline**: strict CSP (`connect-src ipc:` only), PDF.js data files bundled, no network code.
