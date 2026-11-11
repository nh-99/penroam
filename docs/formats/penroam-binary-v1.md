# Penroam binary formats, v1 (normative)

Status: implemented contract. The JSON page/notebook model and the `.penroam` archive are
described in [`model-and-archive.md`](model-and-archive.md). Reference implementations:
`crates/penroam-core/src/binary.rs` (`.prp`), `crates/penroam-storage/src/journal.rs` (journal),
`crates/penroam-core/src/operation.rs` (operation schema), `crates/penroam-core/src/migrate.rs`.
All integers are **little-endian**. CRC-32 is IEEE (zlib `crc32`).

## 1. Versions — independent, never inferred from one another

| Version | Value | Where | Bumped when |
|---|---|---|---|
| `.prp` file format | 1 | `.prp` header u16 | byte layout changes incompatibly |
| Journal format | 1 | segment header u16 | journal byte layout changes |
| Archive format | 2 | `manifest.json#formatVersion` | ZIP layout changes (1 = JSON pages) |
| Library format | 2 | `library.json#version` | live-library layout changes (1 = per-page JSON-lines journals) |
| Notebook schema | 2 | `notebook.json#schemaVersion` | notebook JSON model changes (1 = no order keys) |
| Page schema | 1 | page JSON / `.prp` metadata `schemaVersion` | page JSON model changes |
| Operation schema | 1 | every journal op / sync envelope | operation bodies or semantics change |
| Sync protocol | 1.0 | `penroam-sync-protocol` | see `docs/protocol/sync-protocol-v1.md` |

Readers accept any version ≤ their own and run the migration chain (read old, write current).
Anything newer is **refused without modification** (`TooNew`, "please update Penroam"). The
public protocol crate duplicates these numbers for its contract manifest; a test
(`crates/penroam-storage/tests/contract_versions.rs`) keeps them equal.

## 2. `.prp` page file, format 1

```text
offset  size  field
0       4     magic "PRPG"
4       2     file format version u16 (= 1; 0 is invalid; > 1 refused)
6       2     flags u16 (§3)
8       4     M = stored metadata length u32
12      M     metadata: the page JSON (page schema 1) with every ink stroke's "points" omitted;
              zlib (RFC 1950) when flag bit 0 is set
12+M    4     S = ink stroke count u32 (must equal the ink elements of the metadata, in order)
...           S point blocks (§4), in metadata order
...           extension area if flag bit 8 (§5)
end-4   4     CRC-32 of every preceding byte
```

The encoder is deterministic: the same page yields the same bytes. Golden file
`tests/fixtures/format/sample-page-v1.prp`, SHA-256
`05716d7d5cd66973bc8524da9a3055501dfd198087778973afc984a2a7f8da64` (pinned in
`crates/penroam-core/tests/format_v1.rs`; written before the extension area existed and still
reproduced byte for byte by the current encoder). An independent Python decoder is `decode_prp` in
`tools/compare-penroam.py`.

## 3. Required and optional features

* Flag bits **0–7 are required features.** A reader that does not implement a set bit refuses the
  file (`UnsupportedFeature`). Defined: bit 0 = metadata is zlib-compressed.
* Flag bits **8–15 are optional.** Unknown optional bits are ignored; they must never change the
  layout of anything a reader that ignores them parses. Defined: bit 8 = extension area present.
* Unknown **JSON fields** in the metadata (page, element, stroke level) are preserved verbatim
  through `extra` maps and re-emitted on save.

Older readers (built before optional flags existed) rejected every unknown flag. The current
encoder only sets bit 8 when a page carries extension records, and it never creates them itself,
so its output stays readable by those older readers.

## 4. Point block

`varint n` (unsigned LEB128, ≤ 10 bytes; n ≤ 200 000 and ≤ remaining bytes); float columns `x`,
`y`, `pressure`, `tiltX`, `tiltY`; int column `timeUs`; then `twist` (float) and `flags` (int),
each as presence byte + column. Presence: 0 = none, 1 = all, 2 = bitmap of ⌈n/8⌉ bytes (bit `i%8`
of byte `i/8`), column covers only present points.

| float tag | encoding | decode |
|---|---|---|
| 0 CONST | one f64 | every value = that f64 (bit-identical) |
| 1 DECIMAL | `u8 k` (0–6), n zig-zag varint deltas | running sum `q`; value = `(f64)q / 10^k` |
| 2 RAW | n × f64 | as stored |

DECIMAL only if `(f64)q/10^k` reproduces every value bit for bit (excludes −0.0) and |q| < 2^53;
smallest such `k`. Int column: tag 0 CONST (one zig-zag varint), tag 1 DELTA (n zig-zag deltas from
0). Zig-zag: `(v << 1) ^ (v >> 63)`. Every f64 and i64 round-trips bit for bit (fuzzed with arbitrary
bit patterns incl. NaN, ±inf, −0.0).

Size and speed on the 10k-stroke benchmark scene (275 503 points): JSON 24.5 MB (89 B/point),
`.prp` 2.5 MB (9 B/point, including all per-stroke metadata); decode 27 ms vs 183 ms for JSON
(`cargo run --release -p penroam-core --example page_codec_bench`).

## 5. Extension area (flag bit 8)

```text
u32 count (≤ 1024)
count × { u32 tag, u32 len, len bytes }      total ≤ 64 MiB
```

Tag bit 31 = **required** (unknown → refuse the file). Tag bit 30 = **copy-safe**. No tags are
defined in v1. Behaviour for unknown tags:

| Kind | Read | Edit | Re-save |
|---|---|---|---|
| required | refused (`UnsupportedFeature`) | — | — |
| optional, copy-safe | opens | allowed | record kept verbatim |
| optional, not copy-safe | opens **read-only** | refused with a clear message | file never rewritten |

Data is never silently stripped.

## 6. Hostile-input limits

| Limit | Value |
|---|---|
| decompressed metadata | 256 MiB |
| points per stroke | 200 000 |
| elements per page | 500 000 |
| pages per notebook | 10 000 |
| text per block | 1 MiB |
| coordinates | finite, \|v\| ≤ 1e6 pt; page side ≤ 14 400 pt |
| extension area | 1024 records, 64 MiB |
| journal record | 1 GiB, 1 048 576 ops, 4096 causal deps per op |
| archive | 25 000 entries; page 512 MiB, asset 1 GiB, total 4 GiB; ratio ≤ 200:1 over 1 MiB |
| order key | 1..1024 chars |

Decoders reject: bad magic/CRC, unknown version/required flags/tags, counts above limits or above
the remaining bytes (checked before allocating), metadata that inflates past its limit,
stroke-count mismatch, trailing bytes. Asset references are SHA-256 (lowercase hex) of immutable
files and must resolve inside the notebook.

## 7. Operation journal, format 1

Device-wide, append-only, in `<library>/journal/`. Segment files are named by their first
sequence (`%020u.pjl`) and rolled at 8 MiB or after a checkpoint covers them.

```text
segment = header record*
header  = "PRJL" u16 version(=1) u16 flags(=0) u64 first_seq u32 CRC-32(previous 16 bytes)
record  = "PRJR" u32 len u32 CRC-32(payload) payload[len]
payload = u8 target
          target 0 (notebook): uuid notebook
          target 1 (page):     uuid notebook, uuid page, u64 page revision after, uuid batch id
          u32 op count (≥ 1), then per operation:
            uuid op id, uuid device id, u64 local sequence, u16 op schema, i64 at (ms, informational)
            u32 n, n × (uuid device, u64 seq)          causal dependencies
            u32 len, JSON body (§9) with ink "points" emptied
            u32 S, S point blocks (§4) for the body's ink strokes, in order
```

A record is one **atomic transaction** (all operations of one editor batch, or one notebook-level
change) with consecutive sequences. It counts as committed only after `write + fsync` returned;
only then does the app show *Saved locally*. A failed append (e.g. disk full) is truncated back to
the previous record boundary.

Recovery on open:

1. A short/damaged header in the **last** segment with no readable record → crash during segment
   creation → removed.
2. A record that fails its magic/length/CRC/decoding with **no valid record after it** at the end
   of the last segment → torn write → truncated (fsync). Only this is ever discarded.
3. A damaged record **followed by valid records** (or in an older segment) → corruption: the
   segment is copied to `journal/quarantine/`, left byte-identical in place, every readable record
   before and after is returned, the condition is reported, new records go to a new segment, and
   the checkpoint floor never passes records that could not be applied.
4. A record with an operation schema newer than the build → the library refuses to open
   (`upgrade required`); nothing is modified.

Order is the device's `local_sequence` (persisted high-water in the checkpoint manifest), never a
wall clock.

## 8. Checkpoints and fallback

* Page snapshot: `pages/<id>.prp`. Writing one renames the current file to `<id>.prp.prev`
  (fallback), writes the new file atomically (temp + fsync + rename + dir fsync) and verifies it by
  reading it back.
* `notebook.json` (schema 2) written atomically.
* `checkpoint.json` `{format:"penroam-checkpoint", version:1, deviceId, lastSeq, floor, prevFloor}`:
  every operation with sequence ≤ `floor` is in the snapshots. Written by renaming the current
  manifest to `checkpoint.prev.json` and atomically writing the new one; a damaged current manifest
  falls back to the previous one (lower floor = more idempotent replay).
* Journal segments are deleted only when every record is ≤ `prevFloor` (covered by this **and** the
  previous checkpoint) — and, once sync is enabled, ≤ the server-acknowledged sequence.
* Replay applies every transaction after `floor`: page transactions only when their revision is
  exactly the page revision + 1 (gaps stop replay for that page and are reported); notebook
  operations are idempotent sets. If a `.prp` is unreadable the page is rebuilt from `.prp.prev`
  plus the journal.
* `outbox.json` `{enqueuedThrough, uploadedThrough, remoteSeen, syncEnabled, deviceId}` is derivable:
  if lost it is rebuilt with `uploadedThrough = 0` (re-upload; the server deduplicates by op id).
* `.lock`: exclusive OS file lock while a library is open (second writer refused).

## 9. Operation schema 4

`LocalOperation {id, deviceId, localSequence, schemaVersion, causalDependencies, at, body}`.
`id` is stable across retries (page operations reuse the editor's `PageOperation.opId`).
Body (`type` tag, camelCase): `notebookCreated`, `notebookRenamed`, `notebookCoverChanged`,
`notebookDeleted`, `notebookRestored`, `notebookPurged` (schema 2; all other bodies are written
as schema 1, so older clients only stop at a purge), `pageCreated`
(with `orderKey`), `pageDeleted`, `pageRestored`, `pageReordered` (`orderKey`), `pageTemplateSet`,
`elementsAdded` (re-adding a removed id = explicit restore), `elementsRemoved`, `elementsReplaced`
(`changed[i]` = field-group mask: 1 placement, 2 content, 4 style), `assetReferenced`,
`notebookFiled` (`folder`, or null for the top level), `folderCreated` (`folderId`, `name`,
`parent`), `folderRenamed`, `folderMoved`, `folderDeleted`. The last five are written as schema 3.
`notebookCanvasSet` (`infinite`: whether the pages show as infinite canvases) is written as
schema 4. Unknown types or newer schemas are never applied or dropped.

Folder operations belong to no notebook. They are journaled and routed under the reserved stream
id `70656e72-6f61-4d00-8000-666f6c646572`, which no notebook may use, and applied to `folders.json`
instead of a notebook. `notebookFiled` belongs to its notebook but leaves `updatedAt` alone.

Merge semantics (implemented in `penroam_core::replica`, tested for convergence over every
interleaving): multi-value registers ordered by causal dominance (version vectors); concurrent
writes keep a deterministic winner (highest op id) and retain every alternate as a conflict;
deletes are tombstones; a concurrent restore beats a delete; an edit the deleting device had not
seen is reported (`editedWhileDeleted`) with its value; page order = (order key, page id);
element order = (z, element id); duplicate op ids with identical bodies are no-ops, reused ids
with different bodies are rejected. A folder's name, parent and deletion and a notebook's folder are
registers too, and so is whether a notebook shows infinite canvases, but concurrent writes to them
show the winner without being retained as conflicts.
A deleted folder stays as a tombstone. What's inside shows at its nearest live ancestor, and a
parent cycle made by concurrent moves is cut at its highest folder id; both are computed when
reading (`penroam_core::folder`), so no operation is written to repair them.

## 10. Notebook schema 2 and the legacy library migration

Schema 2 adds `orderKeys: {pageId: key}` for every live and trashed page (§9; keys are base-62
strings `0-9A-Za-z` not ending in `0`, compared bytewise). `pageOrder` is always sorted by
(key, id); an inconsistent file is repaired from `pageOrder` with a warning.

v1 → v2 (in memory) assigns evenly spaced keys reproducing `pageOrder`; trashed pages get keys at
their former index. On disk, a legacy notebook (schema 1 with per-page JSON-lines journals) is
migrated by: (1) copying `notebook.json` and every legacy file to
`<library>/migration-backups/<ts>-legacy/<notebook>/`; (2) folding legacy
`pages/<id>.journal` JSON lines into `.prp` (records ≤ the snapshot revision skipped); (3) writing
`notebook.json` v2 — the commit point; (4) removing the legacy files. Every step is atomic and the
sequence is idempotent (crash-tested at every write boundary).

Builds that only know notebook schema 1 refuse schema 2 and leave it untouched, which is what
makes them fail safely on a journal-based library (they would otherwise ignore the journal).
Downgrading a library to such a build is not supported.

## 11. Fixtures

| Fixture | Purpose |
|---|---|
| `tests/fixtures/format/sample-page-v1.prp` | byte-exact `.prp` golden |
| `tests/fixtures/legacy/*.penroam` | archives written before the journal existed (archive format 2, notebook schema 1): tiny, 1k, 10k strokes, mixed text, 5-page PDF |
| `tests/fixtures/legacy/crashed-library/` | legacy library (format 1) with an unflushed JSON-lines journal |
| `tests/fixtures/legacy/SHA256SUMS` | pins all of the above |
| `crates/penroam-sync-protocol/fixtures/v1/` | sync contract vectors (`penroam-sync-fixtures --check`) |
