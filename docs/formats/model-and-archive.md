# Penroam data model and `.penroam` archive

This document describes the JSON notebook/page model, editor operation batches and the portable
`.penroam` archive. The binary contract — versions, the `.prp` page file, the operation journal,
checkpoints, migrations and limits — is [`penroam-binary-v1.md`](penroam-binary-v1.md).
Current versions: notebook schema 2, page schema 1, archive format 2.

Penroam stores notebooks as plain JSON plus original attachment bytes, so they stay readable and
editable without Penroam. The Rust types in `crates/penroam-core/src/model.rs` are normative;
TypeScript types are generated from them (`apps/desktop-mobile/src/generated`).

## 1. Conventions

* JSON, UTF-8, camelCase keys. Unknown keys are preserved on load/save where the type allows
  (`Notebook`, `Page`, `InkStroke`, `TextBlock`, `ImageElement` keep extra fields).
* Ids are UUIDs (lowercase, hyphenated). Timestamps are milliseconds since the Unix epoch (UTC).
* Geometry is in **points** (1/72 inch) in page space: origin at the top-left of the page *as
  displayed*, y down. At zoom 1 a point is one CSS pixel.
* Colours are `#rrggbb`.
* Every canonical file has `schemaVersion`. Readers run the migration chain in
  `penroam-core/src/migrate.rs`; files newer than the reader are refused, never truncated.

## 2. Notebook (`notebook.json`, `manifest.json#notebook`)

```jsonc
{
  "schemaVersion": 2,
  "id": "…uuid…",
  "title": "Physics lecture",
  "createdAt": 1793630000000,
  "updatedAt": 1793630400000,
  "pageOrder": ["…page uuid…", "…"],          // live pages in reading order
  "orderKeys": { "…page uuid…": "V", "…": "k" }, // fractional order key for every live and trashed page
  "trashedPages": [{ "id": "…", "deletedAt": 1793630100000, "formerIndex": 2 }],
  "deletedAt": 1793630500000,                   // present only while the notebook is in the trash
  "defaults": { "paper": "letter", "template": { "kind": "ruled", "spacing": 24 } },
  "cover": "#3b5b7a",
  "assets": [{ "sha256": "…64 hex…", "mediaType": "application/pdf", "byteSize": 123456, "originalName": "slides.pdf" }],
  "metadata": {}
}
```

`paper`: `letter` (612×792), `a4` (595.276×841.89), `custom`, `pdf`. `pageOrder` is always
sorted by (order key, page id); see [`penroam-binary-v1.md`](penroam-binary-v1.md) §10.

## 3. Page (logical model; stored as binary `pages/<uuid>.prp`)

The page is shown here as JSON — the form used over IPC, in operations, and inside the
`.prp` metadata section. On disk the ink points live in the compact binary column block
([`penroam-binary-v1.md`](penroam-binary-v1.md) §2–§4).

```jsonc
{
  "schemaVersion": 1,
  "id": "…",
  "paper": "letter",
  "size": { "width": 612, "height": 792 },      // displayed size (PDF: rotated crop box)
  "template": { "kind": "blank|ruled|grid|dotted", "spacing": 24 },
  "background": {                                // optional: PDF page under the elements
    "assetHash": "…sha256…", "pageIndex": 0,
    "mediaBox": [0, 0, 612, 792], "cropBox": [0, 0, 612, 792], "rotation": 90
  },
  "revision": 17,                                // +1 per applied operation batch
  "createdAt": 0, "updatedAt": 0,
  "elements": [ /* ink | text | image, sorted by z (stable) */ ]
}
```

### Ink stroke

```jsonc
{
  "type": "ink", "id": "…", "z": 12,
  "tool": "pen" | "highlighter",
  "color": "#111111", "opacity": 1,
  "width": 2.2,                  // nominal width (pt)
  "pressure": true,              // width modulated by pressure when drawn
  "source": "stylus" | "mouse" | "touch" | "fixture",
  "input": { "api": "web-pointer" | "onyx-touchhelper" | "uikit-pencil" | …, "device": "pen", … },
  "points": [ { "x": 120.5, "y": 240.25, "pressure": 0.62, "tiltX": 0, "tiltY": 0, "timeUs": 0,
                "twist": 30, "flags": 2 } ]
}
```

* `points` are the original input samples mapped to page points (rounded to 0.001 pt; pressure
  to 1e-4). Predicted samples are never stored. `pressure` is normalised 0..1 (mouse 0.5); raw
  platform ranges live in `input` (e.g. `maxTouchPressure` from Onyx).
* `flags`: bit 1 = pressure was an estimate refined later (iOS), bit 2 = sample came from a
  coalesced event list.
* Rendering (all ports): width = `pressure ? max(0.5, width·(0.4 + 1.2·p)) : width`; midpoint
  quadratic smoothing; one point = dot. Highlighters are drawn as one constant-width path at
  `opacity`. Golden: `tests/fixtures/geometry-golden.json`.

### Text block

```jsonc
{ "type": "text", "id": "…", "z": 13, "x": 72, "y": 120, "width": 280, "height": 40,
  "text": "A **Markdown** note\nsecond line", "fontSize": 16, "color": "#111111", "align": "left",
  "rowPitch": 24 }  // optional
```

`text` is Markdown source: CommonMark with GFM tables, strikethrough and task lists, plus
LaTeX formulas. Source is all that is stored, so a client that doesn't render Markdown still
shows the note, and search indexes what was typed. Rules for rendering it:

* A soft line break is a line break: Enter starts a new line in a note. Blocks are one line
  apart, and several blank lines count as one.
* HTML is shown as text. Links are underlined and don't open. Images show their alt text.
  Containers nested past 16 levels join their parent.
* `$…$` is an inline formula: it closes on its line, opens before a non-space, and closes after
  a non-space and not before a digit. `$$…$$` is a display formula on its own line and may span
  lines up to a blank line. `\$`, code spans and fences are left alone. A formula that can't be
  drawn shows its source in the code face.

Layout is deterministic and lives in `penroam_core::markdown`, with a TypeScript port in
`apps/desktop-mobile/src/editor/markdown.ts`: widths from
`crates/penroam-core/data/text-metrics.json` (Liberation Sans and Mono = Helvetica and Courier
metrics); greedy wrapping at spaces; over-long words break between characters; trailing spaces
don't count. Line height 1.25 em, first baseline 0.97 em below `y`. `height` is the laid-out
content height (renderers never clip).

`rowPitch` locks a block to the paper's rows. Every line then takes
`ceil(tallest / rowPitch)` rows, where `tallest` is its largest font size or formula, and its
baseline sits on the last of them. Block gaps are one row, and `y` is a whole number of pitches,
so baselines stay on the paper's rows. Ruled paper's first rule is the third row, so locked text
starts at `y = 2 × spacing`; dotted paper's is the first, so it can start at 0. When a page's
template changes to ruled or dotted paper, locked blocks take its spacing in the same operation
batch. Without `rowPitch` a block uses the natural line height. A client that predates the field
keeps it as an unknown key and draws the block with natural line height.

Goldens: `tests/fixtures/markdown-layout-golden.json` (Markdown) and
`tests/fixtures/text-layout-golden.json` (plain wrapping).

Formulas aren't stored. MathJax renders them in the webview to outlines in em, for the canvas and
for each PDF export or print, which passes them to Rust as `RenderedMath` values
(`apps/desktop-mobile/src/editor/math.ts`, `crates/penroam-pdf`). The PDF exporter uses the
standard Helvetica and Courier fonts and fills formula outlines as vector paths.

### Image (minimal)

```jsonc
{ "type": "image", "id": "…", "z": 3, "x": 0, "y": 0, "width": 100, "height": 80, "assetHash": "…" }
```

## 4. Operations (journal and IPC)

```jsonc
{ "batchId": "…", "baseRevision": 16,
  "ops": [ { "opId": "…", "op": "addElements", "elements": [ … ] },
           { "opId": "…", "op": "removeElements", "ids": [ "…" ] },
           { "opId": "…", "op": "replaceElements", "elements": [ … ] },
           { "opId": "…", "op": "setTemplate", "template": { … } } ] }
```

This is the batch the editor sends over IPC. A batch applies atomically or not at all;
`baseRevision` must equal the page revision. The live library turns every change into typed
operations in a device-wide binary journal ([`penroam-binary-v1.md`](penroam-binary-v1.md) §7–§9);
libraries that still have the older per-page JSON-lines journals are migrated on first open.
Journals are never exported.

## 5. Portable archive (`.penroam`, ZIP)

```text
mimetype            "application/vnd.penroam.notebook+zip" (first entry, stored)
manifest.json       { format: "penroam-notebook", formatVersion: 2, schemaVersion: 2,
                      generator, exportedAt, notebook,
                      pages:  [{ id, path: "pages/<id>.prp", sha256, bytes }],
                      assets: [{ sha256, path: "assets/<sha256>", mediaType, byteSize }] }
pages/<uuid>.prp    canonical page, binary encoding, journal folded in; byte-identical on import
assets/<sha256>     original attachment bytes (PDFs are never rasterised)
```

* Format 1 archives (pages as `pages/<uuid>.json`) are still imported.
* The archive is a snapshot of live pages; trashed pages, thumbnails and journals are not
  included. Only assets referenced by exported pages are included.
* Import checks: entry names are whitelisted (`mimetype`, `manifest.json`,
  `pages/<uuid>.prp`, `pages/<uuid>.json` (format 1), `assets/<sha256>`; anything with `..`, `\`, `:`, NUL or a leading `/` is
  rejected; other safe names are ignored with a warning); ≤ 25 000 entries; per-entry limits
  enforced on the *decompressed* byte count (manifest 32 MiB, page 512 MiB, asset 1 GiB, total
  4 GiB); compression ratio ≤ 200:1 for entries over 1 MiB; every page/asset SHA-256 verified;
  manifest ↔ notebook page order must match; every asset reference must resolve; pages are
  validated (finite numbers, ranges, unique ids). Work happens in a staging directory and is
  moved into the library with one rename. If the notebook id already exists, the import
  becomes a copy with a new id.
