# Penroam file formats (schema 1, archive format 1)

Penroam stores notebooks as plain JSON plus original attachment bytes, so they stay readable and
editable without Penroam. The Rust types in `crates/penroam-core/src/model.rs` are normative;
TypeScript types are generated from them (`apps/desktop-mobile/src/generated`).

## 1. Conventions

* JSON, UTF-8, camelCase keys. Unknown keys are preserved on load/save where the type allows
  (`Notebook`, `Page`, `InkStroke`, `TextBlock`, `ImageElement` keep extra fields).
* Ids are UUIDs (lowercase, hyphenated). Timestamps are milliseconds since the Unix epoch (UTC).
* Geometry is in **points** (1/72 inch) in page space: origin at the top-left of the page *as
  displayed*, y down. At zoom 1 a point is one CSS pixel (this equals M00's "document units").
* Colours are `#rrggbb`.
* Every canonical file has `schemaVersion`. Readers run the migration chain in
  `penroam-core/src/migrate.rs`; files newer than the reader are refused, never truncated.

## 2. Notebook (`notebook.json`, `manifest.json#notebook`)

```jsonc
{
  "schemaVersion": 1,
  "id": "…uuid…",
  "title": "Physics lecture",
  "createdAt": 1793630000000,
  "updatedAt": 1793630400000,
  "pageOrder": ["…page uuid…", "…"],          // live pages in reading order
  "trashedPages": [{ "id": "…", "deletedAt": 1793630100000, "formerIndex": 2 }],
  "deletedAt": 1793630500000,                   // present only while the notebook is in the trash
  "defaults": { "paper": "letter", "template": { "kind": "ruled", "spacing": 24 } },
  "cover": "#3b5b7a",
  "assets": [{ "sha256": "…64 hex…", "mediaType": "application/pdf", "byteSize": 123456, "originalName": "slides.pdf" }],
  "metadata": {}
}
```

`paper`: `letter` (612×792), `a4` (595.276×841.89), `custom`, `pdf`.

## 3. Page (logical model; stored as binary `pages/<uuid>.prp`, §6)

The page is shown here as JSON — the form used over IPC, in operations, and inside the
`.prp` metadata section. On disk the ink points live in the compact binary column block.

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
  "width": 2.2,                  // nominal width (pt) — M00 brush.width
  "pressure": true,              // pressure modulated width when drawn — M00 brush.pressure
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
  "text": "Plain multiline text\nsecond line", "fontSize": 16, "color": "#111111", "align": "left" }
```

Layout is deterministic: widths from `crates/penroam-core/data/text-metrics.json` (Liberation
Sans = Helvetica metrics); explicit `\n` breaks; greedy wrapping at spaces; over-long words
break between characters; trailing spaces don't count. Line height 1.25 em, first baseline
0.97 em below `y`. Golden: `tests/fixtures/text-layout-golden.json`. `height` is the laid-out
content height (renderers never clip).

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

A batch applies atomically or not at all; `baseRevision` must equal the page revision.
The live library journals batches as JSON lines `{"revision", "at", "batch"}` next to the page
snapshot; journals are an implementation detail and are never exported.

## 5. Portable archive (`.penroam`, ZIP)

```text
mimetype            "application/vnd.penroam.notebook+zip" (first entry, stored)
manifest.json       { format: "penroam-notebook", formatVersion: 2, schemaVersion: 1,
                      generator, exportedAt, notebook,
                      pages:  [{ id, path: "pages/<id>.prp", sha256, bytes }],
                      assets: [{ sha256, path: "assets/<sha256>", mediaType, byteSize }] }
pages/<uuid>.prp    canonical page, binary encoding (§6), journal folded in; byte-identical on import
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

## 6. Binary page encoding (`.prp`, version 1)

Used for live-library page snapshots (`notebooks/<nb>/pages/<page>.prp`) and archive pages.
Reference implementations: `crates/penroam-core/src/binary.rs` (Rust, encoder + decoder) and
`decode_prp` in `tools/compare-penroam.py` (independent Python decoder). Golden file:
`tests/fixtures/format/sample-page-v1.prp`. All integers little-endian.

```text
offset  size  field
0       4     magic "PRPG"
4       2     version u16 (= 1). Newer versions are refused, never guessed at.
6       2     flags u16: bit 0 = metadata is zlib-compressed (RFC 1950). Other bits must be 0.
8       4     M = stored metadata length, u32
12      M     metadata: the Page JSON (§3) with every ink stroke's "points" key omitted
12+M    4     S = number of ink strokes, u32 (must equal the ink elements in metadata order)
...           S point blocks, in the order the ink elements appear in metadata
end-4   4     CRC-32 (IEEE, as zlib's crc32) of all preceding bytes
```

**Point block:** `varint n`; float columns `x`, `y`, `pressure`, `tiltX`, `tiltY`; int column
`timeUs`; then `twist` and `flags`, each as *presence* + column. Presence byte: 0 = no point
has it (no column follows), 1 = every point has it, 2 = a bitmap of ⌈n/8⌉ bytes follows (bit
`i%8` of byte `i/8`), and the column covers only the points that have it (`twist` is a float
column, `flags` an int column).

**Varints** are unsigned LEB128 (≤ 10 bytes). **Zig-zag**: `(v << 1) ^ (v >> 63)`.

**Float column**, one tag byte:

| tag | encoding | decode |
|---|---|---|
| 0 CONST | one f64 | every value = that f64 (bit-identical) |
| 1 DECIMAL | `u8 k` (0–6), then n zig-zag varint deltas | running sum `q`, value = `(f64)q / 10^k` |
| 2 RAW | n × f64 | as stored |

An encoder may use DECIMAL only if `(f64)q / 10^k` reproduces every value **bit-for-bit**
(this excludes −0.0) and |q| < 2^53; otherwise RAW. Encoders pick the smallest such `k`
(deterministic output).

**Int column**: tag 0 CONST (one zig-zag varint), tag 1 DELTA (n zig-zag varint deltas,
first from 0).

Decoders must reject: bad magic or checksum, unknown version/flags/tags, `n` above the
per-stroke point limit or larger than the remaining bytes, metadata that inflates beyond
256 MiB, stroke-count mismatch, trailing bytes.

Measured on the M00 10k-stroke scene (275 503 points): JSON 24.5 MB (89 B/point), `.prp`
2.5 MB (9 B/point, including all per-stroke metadata); decode 27 ms vs 183 ms for JSON
(`cargo run --release -p penroam-core --example page_codec_bench`). Lossless: every f64 and
i64 round-trips bit-for-bit (`crates/penroam-core/tests/binary.rs`).
