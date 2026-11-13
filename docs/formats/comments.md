# Comment operations and anchors

Comments on shared notebooks are operations in a space's **comment stream**, separate from the
notebook's operation log (protocol §12h in [sync-protocol-v1.md](../protocol/sync-protocol-v1.md)).
This document describes what is inside a comment payload, how threads are merged, how anchors are
placed on the notebook, and the comment log a device keeps. Code: `penroam_core::comment` (model,
encoding, merge, anchors) and `penroam_sync::comments` (the stream on a device).

## Operations

A comment operation (`CommentOperation`) is one change plus its identity and causal context:

| Field | Meaning |
|---|---|
| `id` | random UUID, stable across retries |
| `deviceId` | the writing device (must be the envelope's signing device) |
| `sequence` | 1, 2, 3, … per device, on this space's comment stream |
| `schemaVersion` | comment schema, currently 1 |
| `causalDependencies` | frontier when written: `[{deviceId, deviceSeq}]`, sorted by device, one entry each |
| `author` | account id of the member who wrote it |
| `moderation` | set only by the owner, only on `commentDeleted` |
| `at` | wall-clock milliseconds; shown to people and used to sort lists, never to merge |
| `op` | the change, tagged by `type` |

The changes:

| `type` | Fields | |
|---|---|---|
| `threadCreated` | `threadId`, `anchor`, `body` | `threadId` equals the operation id; the first comment's id is the thread id |
| `replied` | `threadId`, `commentId`, `body`, `replyTo?` | `commentId` equals the operation id; `replyTo` is a comment of the same thread |
| `commentEdited` | `commentId`, `body` | author only |
| `commentDeleted` | `commentId` | author, or the owner with `moderation` |
| `threadResolved` | `threadId` | |
| `threadReopened` | `threadId` | |
| `anchorReattached` | `threadId`, `anchor` | |

Tying new thread and comment ids to operation ids means two operations can never claim the same
thread or comment.

The `author` and `deviceId` in a payload are only trusted after the receiver has checked them
against the verified envelope: the signing device, and the member the server attributes that
device to. The receiver also refuses `moderation` from anyone but the owner.

A payload keeps the author's original account id for good. When an account is erased, the server
replaces its id with an `erased-…` tombstone in what it stores beside the comments (attribution,
participants, member list), but it can't change the signed, encrypted payloads, and it can read
them, since it keeps the space's root secret. Erasure therefore unlinks the server-side metadata
only. A device that pulls an erased member's comment afterwards accepts the payload's author on
its signature from a device of the key log, as long as that author isn't a current member, and
shows it under the tombstone. The original id never reaches the app's views.

The key log's cutoff for a removed device counts its document-stream sequences, not its comment
sequences, so it isn't applied to comments. A removed device's comments are accepted on their
signature alone, until the key log records a comment-stream cutoff too.

## Bodies and mentions

A body is plain text of at most 8 KiB (UTF-8). It may not be empty or only whitespace, and may not
contain `\r`, control characters other than newline and tab, or the bidirectional overrides and
isolates U+202A–U+202E and U+2066–U+2069. The composer cleans input into that form; a received
body that isn't in it is refused, not cleaned, so every device shows the same text. Bodies are
rendered escaped, never as HTML or Markdown.

A mention is the token `@[accountId]`, where the account id is 1 to 128 characters from
`[A-Za-z0-9_-]`. Anything else (`@name`, `@[` with other characters) is plain text. A body may
mention at most 20 distinct accounts; repeats count once. The app shows each token as the member's
current name. The sync client copies the mentioned active members, other than the author, into
the push's plaintext side-car, which the server uses only to create notifications.
The side-car always names the operation's thread, and says what kind of push it is: `comment`
for a new comment (a thread start or a reply), `update` for anything else (edits, deletes,
resolve, reopen, reattach, fillers). The server notifies the thread's earlier participants only of
`comment` pushes, so resolving or deleting notifies no one. A mention an edit adds still notifies,
under the edit's operation id, and a thread mute applies to it as to any other notification of
that thread.

The side-car isn't signed, so a member's client could name another thread or mention someone the
comment doesn't. A client therefore shows a notification only once it has pulled and verified the
comment operation it names, and only if that operation is a comment by the named actor, in the
named thread, that mentions the recipient (for a mention) or replies in a thread the recipient
commented in (for a reply). Until the operation is pulled the notification waits; one that the
comment contradicts is dropped and reported as a security event.

## Anchors

Anchors are in document coordinates: points, origin at the top-left of the page as displayed, y
down. Coordinates may be negative, since infinite canvases extend in every direction. They must be
finite and within ±1 000 000. Rectangles are `{x, y, w, h}` with `w` and `h` not negative. JSON is
tagged by `kind`:

| `kind` | Fields |
|---|---|
| `point` | `pageId`, `x`, `y` |
| `rect` | `pageId`, `rect` |
| `element` | `pageId`, `elementId`, `fallbackRect?` |
| `text` | `pageId`, `elementId`, `start`, `end`, `quote`, `quoteHash`, `fallbackRect?` |
| `pdf` | `pageId`, `pdfPageIndex`, `normalizedRect`, `rotation`, `pdfAssetSha256?` |

* `start`/`end` (`text`) are the text CRDT's sticky indexes, unpadded base64url, 1 to 256 bytes
  each. `quote` is the selected text, or its first 1 KiB if longer. `quoteHash` is the SHA-256 of
  the UTF-8 bytes of the whole selection, as 64 lowercase hex digits.
* `normalizedRect` (`pdf`) is in fractions (0 to 1) of the PDF page as displayed when the comment
  was made, origin top-left. `rotation` (0, 90, 180 or 270, clockwise) is the page rotation it was
  measured at. `pdfAssetSha256` is the SHA-256 of the PDF asset, as lowercase hex.

### Placing an anchor

An anchor is placed on its own page only, and the page must be live. On a trashed or deleted page
it is **orphaned**, and it never moves to another page.

* `point` and `rect` are placed as given.
* `element` takes the element's current bounds, so it follows moves and resizes. It uses
  `fallbackRect` only if the element has no bounds (a stroke without points). A deleted element
  orphans it.
* `text` resolves the two sticky indexes to UTF-16 offsets in the block's current text. It is
  placed only if the text between them still hashes to `quoteHash`; otherwise it is orphaned with
  its quote, so it never points at different text.
* `pdf` uses the page's PDF background with that page index or, failing that, the one image
  element showing that PDF page on an infinite canvas. With `pdfAssetSha256`, only that PDF
  counts. The rectangle is turned from the rotation it was measured at to the PDF page's current
  rotation, then scaled to the page or image box. If no PDF page matches, or two images show it,
  the anchor is orphaned rather than guessed.

Orphan reasons: `pageTrashed`, `pageDeleted`, `pageUnavailable` (content not loaded),
`elementDeleted`, `textRangeLost`, `quoteMismatch`, `pdfPageMissing`.

## Merge rules

Operations are applied in causal order: each device's sequences in turn, and every dependency
first. The resulting threads are the same on every device whatever the delivery order.

* An operation may only refer to threads and comments created in its causal past. A comment may
  be edited only by its author, and deleted by its author or by the owner with `moderation`. An
  operation that breaks these rules is **rejected**: it is recorded, so later operations of its
  device still apply, but has no effect. Every device rejects the same operations.
* A comment's text is a multi-value register. Concurrent edits are all kept; the one with the
  highest operation id is shown and the others are listed as alternates (at most 16, those with
  the highest operation ids). A thread's anchor works the same way. A register holds at most one
  write per writing device, since one device's writes are always ordered; it is never truncated,
  because dropping a write could change which one wins once a later write supersedes the rest.
* A delete is a tombstone and wins over concurrent edits. It hides the comment's text in the
  merged view, but the comment keeps its place in the thread, so replies stay attached. It reads
  "deleted" when its author deleted it, and "removed by owner" otherwise. Deleting doesn't erase
  anything: the text stays in the comment stream's operations, on every member's devices and on
  the server, which can read it (it keeps the space's root secret). Only erasing the shared
  notebook removes it.
* Open or resolved is a register written by resolve, reopen and **reply**. If any concurrent head
  is a reply the thread is open; otherwise the head with the highest operation id decides. So a
  reply made concurrently with or after a resolve reopens the thread, and a resolve made after
  seeing the reply closes it. Edits and deletes don't change it.
* A retried operation (same id, same content) changes nothing. An id reused with other content, or
  a (device, sequence) used twice, is an error.

## Binary encoding

A payload is the operation's binary encoding. It starts with the magic `PRCM`, which no other
Penroam payload uses (journal records start `PRJR`, checkpoints `PRCP`, pages `PRPG`), so a comment
never decodes as a document operation or the reverse. Integers are little-endian; `varint` is
unsigned LEB128, `zz` a zigzag varint, `bytes` a varint length followed by the bytes, `uuid` 16
bytes, `f64` the IEEE 754 bits, and `rect` four `f64` (`x`, `y`, `w`, `h`).

```text
"PRCM" | u16 schema | uuid id | uuid device | varint sequence | zz at
| u8 flags (bit 0 moderation) | bytes author
| varint n | n × (uuid device, varint seq) | u8 op | op fields | u32 crc32

op 1 threadCreated:    uuid thread, anchor, bytes body
op 2 replied:          uuid thread, uuid comment, bytes body, u8 has_reply_to [uuid]
op 3 commentEdited:    uuid comment, bytes body
op 4 commentDeleted:   uuid comment
op 5 threadResolved:   uuid thread
op 6 threadReopened:   uuid thread
op 7 anchorReattached: uuid thread, anchor

anchor 1 point:   uuid page, f64 x, f64 y
anchor 2 rect:    uuid page, rect
anchor 3 element: uuid page, uuid element, u8 has_fallback [rect]
anchor 4 text:    uuid page, uuid element, bytes start, bytes end, bytes quote,
                  32 quote_hash, u8 has_fallback [rect]
anchor 5 pdf:     uuid page, varint pdf_page_index, rect, u16 rotation,
                  u8 has_asset [32 sha256]
```

The CRC-32 covers everything before it. Only the canonical encoding is accepted (minimal varints,
presence flags 0 or 1, no trailing bytes), so decoding then encoding gives back the same bytes. A
newer schema version is reported as such and kept, never dropped. Every length is checked against
its limit before anything is allocated.

| Limit | Value |
|---|---|
| encoded operation | 64 KiB |
| body | 8 KiB |
| mentions per body | 20 |
| quote | 1 KiB |
| sticky index | 256 bytes |
| author or mentioned account id | 128 bytes |
| causal dependencies | 1024 |

`crates/penroam-core/tests/vectors/comments.json` holds golden vectors (operations with their
encodings and the threads they merge into, and anchors with their expected placements) that the
tests check.

## Sealing

A comment payload is sealed like a document payload (XChaCha20-Poly1305, the envelope header as
associated data, a nonce derived from the operation id and plaintext, an Ed25519 signature over
the header), but under its own key:
`HKDF-SHA256(ikm = epoch key, salt = the notebook's routing alias in that epoch, info =
"penroam/comment/v1")`. A receiver quarantines a comment-stream envelope that doesn't open with
that key or doesn't decode as a comment operation.

## The comment log on a device

Each space's comment stream is kept in its own folder:

```text
comments.log    append-only records after an 8-byte header ("PRCL", u16 version 1, 2 reserved)
comments.json   the pull cursor and the last acknowledged cursor (atomic writes)
```

Each record is `u32 len ‖ u32 crc32 ‖ JSON`, tagged by `t`: `local` (an operation of this device),
`draft` (written while commenting wasn't allowed; never uploaded, takes no sequence), `remote` (a
verified operation from the stream), `parked` (an envelope waiting for a device, key or upgrade),
`unparked`, `quarantined` (refused, kept), `status` (a local operation was sent, failed,
drafted or discarded), `filler`, `sealed` (the epoch fixed for an upload) and `pin` (the member a
device is attributed to). The log is fsynced before the cursor that skips its envelopes is saved,
and before a new comment is reported as saved. A torn last record is cut off on open; damage
anywhere else stops the open and nothing is deleted.

A device's sequences on the stream must have no gaps, or other devices would wait forever. So a
queued operation that becomes a draft, or that the server refuses for good, is replaced on the
server by a **filler**: an operation with a derived id that reopens a thread that doesn't exist,
which every device rejects identically. The draft or failed comment stays on this device.
