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
  "folder": "…folder uuid…",                    // present only when filed in a library folder
  "infiniteCanvas": true,                       // present only when the pages are infinite canvases
  "metadata": {}
}
```

`paper`: `letter` (612×792), `a4` (595.276×841.89), `custom`, `pdf`. `pageOrder` is always
sorted by (order key, page id); see [`penroam-binary-v1.md`](penroam-binary-v1.md) §10.

`folder` was added without a schema change; older builds keep it as an unknown field. Folder ids
mean nothing outside their library, so importing a `.penroam` file clears it. Restoring a
`.penroam-backup` keeps it.

`infiniteCanvas` (also added without a schema change) shows every page as an infinite canvas: an
unbounded surface where elements may sit at any coordinate, negative ones included, instead of a
sheet of `size`. It only changes how pages are shown and laid out, so turning it off loses
nothing; elements outside the sheet are then simply not on it. On a canvas of blank, grid or
dotted paper the template repeats from the origin in every direction. Ruled paper makes a column
(`Page::is_canvas_column`): the sheet's width from the origin, with its header rows and margin
line, ruled all the way down. A PDF background stays in the box `0, 0, size`. Exports cut a
canvas page to a sheet: its content and background with a 36 pt margin, the top-left corner
moved down to a whole template pitch so the lines stay put; a column is cut to the page's width
from the origin, down to its content's end plus the margin and at least the page's height,
widened for any content outside it (`Page::canvas_as_sheet`). PDF export and printing then
split that sheet top to bottom into pages of the paper's proportions, as wide as the content and
at least the paper, breaking in a gap between elements in the bottom 40 % of a page where there
is one (`Page::canvas_as_sheets`). The pages of a column after its first carry
`penroamContinuedSheet` in their `extra` and are ruled from the top, without the header; that
key is only on sheets made for export, never on stored pages. Older builds keep
`infiniteCanvas` as an unknown field and show sheets.

Folders themselves are in `<library>/folders.json` and in a backup's `extras/folders.json`, never
in a single-notebook archive. The file is an object keyed by folder id and includes deleted folders:

```jsonc
{ "…folder uuid…": { "id": "…", "name": "School", "parent": "…optional folder uuid…",
                     "createdAt": 1793630000000, "deletedAt": 1793630100000 /* only if deleted */ } }
```

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
  platform ranges live in `input` (e.g. `maxTouchPressure` from Onyx; `maximumForce` and
  `pressureCurve` from Apple Pencil, whose strokes are stored through a curve).
* `flags`: bit 1 = pressure was an estimate refined later (iOS), bit 2 = sample came from a
  coalesced event list.
* Rendering (all ports): width = `pressure ? max(0.5, width·(0.4 + 1.2·p)) : width`; midpoint
  quadratic smoothing; one point = dot. Highlighters are drawn as one constant-width path at
  `opacity`. Golden: `tests/fixtures/geometry-golden.json`.

### Text block

```jsonc
{ "type": "text", "id": "…", "z": 13, "x": 72, "y": 120, "width": 280, "height": 40,
  "text": "A **Markdown** note\nsecond line", "fontSize": 16, "color": "#111111", "align": "left",
  "font": "handwritten",  // optional
  "rowPitch": 24 }  // optional
```

`text` is Markdown source: CommonMark with GFM tables, strikethrough and task lists, plus
LaTeX formulas. Source is all that is stored, so a client that doesn't render Markdown still
shows the note, and search indexes what was typed. Rules for rendering it:

* A soft line break is a line break: Enter starts a new line in a note. Blocks are one line
  apart, and several blank lines count as one.
* HTML is shown as text. Links are underlined. The app opens http, https and mailto links
  when they are tapped; exported PDFs have no clickable links. Images show their alt text.
* Tapping a task's checkbox in the app edits the source: the character between its brackets
  becomes `x`, or a space if it was ticked.
* A table is a spreadsheet (`penroam_core::sheet`, ported in
  `apps/desktop-mobile/src/editor/sheet.ts`). Each cell is read from its source text, trimmed
  and with `\|` unescaped. A cell that starts with `=` is a formula and shows its result; one
  that is exactly `[ ]`, `[x]` or `[X]` is a checkbox drawn like a task's, and tapping it edits
  the source the same way. Columns are lettered from `A` and rows numbered from 1, the header
  row. Formulas have `+ - * / ^` (`^` binds tighter than a sign, which binds tighter than the
  rest), parentheses, references such as `B2`, ranges such as `B2:C9`, and `SUM`, `AVERAGE`
  (`AVG`), `MIN`, `MAX`, `COUNT`, `PRODUCT`, `ROUND` and `ABS`, in any case. Numbers are
  written `1,234.5`, `-3`, `$9.99` (or `€ £ ¥`) or `12%`; `TRUE`, `FALSE` and checkboxes count
  as 1 and 0 in arithmetic, and aggregates skip text, blanks and checkboxes in ranges. Money
  may also put its symbol after the digits, with or without a space (`$ 9.99`, `9.99 €`), and
  `(9.99)` is negative. A cell's value looks through emphasis wrapped around the whole cell
  (`**$5**` is still 5, `**=SUM(B2:B5)**` still a formula, whose result is then drawn bold). A
  result keeps the currency (and where its symbol goes), percent and thousands separators of the
  first number it was made from, shows currency with two decimals and anything else with at most
  six. Errors are shown as
  `#ERROR!`, `#NAME?`, `#REF!`, `#VALUE!`, `#DIV/0!`, `#NUM!`, `#CYCLE!` (the formula depends
  on itself) and `#LIMIT!` (the ranges in a table's formulas cover more than 200,000 cells in
  all). Formulas are evaluated in dependency order, so the result never depends on cell order.
  Both ports are checked against `tests/fixtures/sheet-golden.json`. Other Markdown readers
  show the formula's source.
  Containers nested past 16 levels join their parent.
* `$…$` is an inline formula: it closes on its line, opens before a non-space, and closes after
  a non-space and not before a digit. `$$…$$` is a display formula on its own line and may span
  lines up to a blank line. `\$`, code spans and fences are left alone. A formula that can't be
  drawn shows its source in the code face.

Layout is deterministic and lives in `penroam_core::markdown`, with a TypeScript port in
`apps/desktop-mobile/src/editor/markdown.ts`: widths from
`crates/penroam-core/data/text-metrics.json` (Liberation Sans and Mono = Helvetica and Courier
metrics, and Excalifont); greedy wrapping at spaces; over-long words break between characters; trailing spaces
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

`font` is `"formatted"` (Liberation Sans) or `"handwritten"` (Excalifont). It is written only
for handwritten blocks: a missing field means formatted, which is what every block written
before the field existed uses, so their layout doesn't change. A client that predates the field
keeps it as an unknown key and draws the block formatted. The font applies to text, headings,
list bullets and numbers. Code, and formulas shown as source, always use the monospace face;
checkboxes, link underlines, strikethrough, rules and frames are drawn lines. They are the
same in both fonts except checkboxes, which handwritten text sketches: four slightly bowed
strokes that overshoot their corners and a tick that flicks past the box, 1.5 times as heavy.
The strokes are fixed points, not random, so every client and export draws the same marks.
Rules for handwritten text:

* Excalifont has one face. Bold also strokes the glyph outlines, 0.045 em wide; italic shears
  them by 0.2 (x moves 0.2 to the right per unit above the baseline). Both keep the regular
  widths.
* A character Excalifont has no glyph for is set in the formatted face of the same style, one
  character at a time. A list label is set in one face: Excalifont if it has every character.
* No kerning and no ligatures, as in the PDF.
* Line height and first baseline are the formatted font's. `bottom` reaches Excalifont's
  descender (0.374 em) below the last baseline.

Goldens: `tests/fixtures/markdown-layout-golden.json` (Markdown) and
`tests/fixtures/text-layout-golden.json` (plain wrapping).

Formulas aren't stored. MathJax renders them in the webview to outlines in em, for the canvas and
for each PDF export or print, which passes them to Rust as `RenderedMath` values
(`apps/desktop-mobile/src/editor/math.ts`, `crates/penroam-pdf`). The PDF exporter uses the
standard Helvetica and Courier fonts, embeds Excalifont as a TrueType font when an exported page
has handwritten text, and fills formula outlines as vector paths.

### Image (minimal)

```jsonc
{ "type": "image", "id": "…", "z": 3, "x": 0, "y": 0, "width": 100, "height": 80, "assetHash": "…" }
```

An image may instead show one page of a PDF asset, stretched over its box. This is how PDF pages
sit on an infinite canvas, where one page can hold a whole document:

```jsonc
{ "type": "image", "id": "…", "z": -2, "x": 0, "y": 840, "width": 612, "height": 792,
  "assetHash": "…sha256…",
  "pdfPage": { "assetHash": "…same sha256…", "pageIndex": 1,
               "mediaBox": [0, 0, 612, 792], "cropBox": [0, 0, 612, 792], "rotation": 0 } }
```

`pdfPage` has the shape of a page `background` and must name the element's own asset. It merges
with the content field group. The editor and the PDF exporter draw these under the ink, the PDF
exporter from the source page as vector content; SVG and text exports leave them out with a
warning. Added without a schema change: older builds keep `pdfPage` as an unknown field and draw
a placeholder, since the asset is not an image.

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
* A normal export is a snapshot of live pages; trashed pages, thumbnails, journals and version
  history are not included. Only assets referenced by exported pages are included.
* Backups write trashed pages as well, as `pages/<uuid>.prp` entries listed under an extra
  `trashedPages` manifest key, and keep the notebook's `deletedAt`. Older builds ignore both and
  import the live pages. Without the restore option, import drops the trash with a warning.
* An export can be limited to selected pages, and can be made reproducible by using the
  notebook's `updatedAt` as `exportedAt`. See [`portability.md`](portability.md).
* Import checks: entry names are whitelisted (`mimetype`, `manifest.json`,
  `pages/<uuid>.prp`, `pages/<uuid>.json` (format 1), `assets/<sha256>`; anything with `..`, `\`, `:`, NUL or a leading `/` is
  rejected; other safe names are ignored with a warning); ≤ 25 000 entries; per-entry limits
  enforced on the *decompressed* byte count (manifest 32 MiB, page 512 MiB, asset 1 GiB, total
  4 GiB); compression ratio ≤ 200:1 for entries over 1 MiB; every page/asset SHA-256 verified;
  manifest ↔ notebook page order must match; every asset reference must resolve; pages are
  validated (finite numbers, ranges, unique ids). Work happens in a staging directory and is
  moved into the library with one rename. If the notebook id already exists, the import
  becomes a copy with a new id.
