# Penroam Sync Protocol v1: implemented contract (M02.3)

**Status:** Implemented as public Rust types and fixtures. The wire *shapes* in this document (envelope header, canonical bytes, batch framing, error codes) are what `penroam-sync-protocol` encodes and what the committed fixtures pin. **HTTP route names and the HTTP transport mapping are proposed, not frozen.** The design rationale is in `agent-docs/01-SYNC-PROTOCOL-V1.md`.

| Crate | Purpose | Dependencies |
|---|---|---|
| `crates/penroam-sync-protocol` | Versions, ids, limits, envelope header and validation, canonical bytes, batch framing, DTOs, error codes, retry policy, contract manifest | `serde`, `serde_json`, `thiserror`, `sha2` only. No `penroam-core`, storage, Tauri, UI or device SDKs |
| `crates/penroam-sync-testkit` | `InMemoryServer` fake server (test scaffolding, not production), deterministic fixture generator `penroam-sync-fixtures` | the protocol crate, `serde`, `serde_json`, `thiserror`, `sha2`, `uuid` |

There is no network server, authentication or cryptography in M02. Signatures and AEAD come in M03. This document fixes only the bytes they will cover.

## 1. Version numbers

Each number is independent. None is derived from another.

| Constant | Value | Describes |
|---|---|---|
| `PROTOCOL_MAJOR` / `PROTOCOL_MINOR` | 1 / 0 | HTTP sync protocol. A different major is never compatible. A minor adds features, and each new feature is gated by a capability. |
| `ENVELOPE_VERSION` | 1 | Opaque remote container: envelope header, canonical bytes, framing |
| `OP_SCHEMA_VERSION` (min supported `OP_SCHEMA_MIN_SUPPORTED`) | 1 (1) | Encrypted semantic operation payload |
| `FILE_FORMAT_VERSION` | 1 | Local `.prp` page file |
| `JOURNAL_FORMAT_VERSION` | 1 | Local operation journal |
| `ARCHIVE_FORMAT_VERSION` | 2 | `.penroam` archive |
| `NOTEBOOK_SCHEMA_VERSION` | 2 | Notebook model schema |
| `PAGE_SCHEMA_VERSION` | 1 | Page model schema |

The local-format numbers are copied into the protocol crate on purpose, because it must not depend on storage. They are advertised in `contract-manifest.json`:

```json
{ "protocol": {"major": 1, "minor": 0}, "envelope_version": 1,
  "op_schema": {"min": 1, "max": 1},
  "formats": {"file_format": 1, "journal_format": 1, "archive_format": 2, "notebook_schema": 2, "page_schema": 1},
  "git_sha": null }
```

`git_sha` is `null` in the committed fixture. Release tooling will fill it in. A cross-check test against the real storage/core constants is planned. It is owned by the storage/core side.

## 2. Compatibility and negotiation

* `GET /v1/meta` returns `ServerMeta { min_client_protocol, max_client_protocol, capabilities, limits, server_instance_id }`.
* `negotiate(ProtocolRange::CLIENT, &meta)` picks the **highest version both sides support**. If the ranges do not overlap, it returns `Incompatible`:
  * `ClientTooOld`: the client needs an update.
  * `ServerTooOld`: the server is outdated.
  * `MalformedRange`: an advertised range has min > max.

  Every `Incompatible` maps to `upgrade_required`. The client stops syncing, keeps its outbox and local data, and shows an actionable state. It never "tries anyway".
* Minor features are **never assumed from the version number**. Use them only if `meta.has_capability(..)` is true. Current capability strings: `ops.batch.v1`, `ops.pull.v1`, `acks.v1`, `usage.v1`. Clients ignore unknown capabilities.
* Clients must call `Limits::validate()` on advertised limits. They refuse values that are 0 or above the hard maxima.
* Op schema: servers relay payloads whose `op_schema_version` is newer than they know, because the payload is opaque to them. A client that cannot apply a newer schema calls `EnvelopeHeader::check_op_schema_supported()`. On failure it pauses with `upgrade_required` and keeps the ciphertext. It never drops the operation.
* Unknown `ErrorCode` strings deserialize to `unknown`, which is treated as not retryable.
* `EnvelopeHeader` JSON uses `deny_unknown_fields`, so a header cannot carry unsigned extra data that a receiver would ignore.

## 3. Identifiers

`DeviceId`, `OpId`, `NotebookRoutingId`, `AccountId` and `BlobId` are opaque strings. They are validated when constructed and when deserialized:

* non-empty
* at most 128 bytes (`ID_MAX_LEN`). A server may advertise a lower `max_id_length`.
* charset `[A-Za-z0-9_-]`

The client generates op ids (for example a hyphenated UUIDv4/v7) and keeps each one stable across retries. A sortable id never implies causal order. Notebook routing ids must not be titles.

## 4. Envelope header (`envelope_version = 1`)

```json
{
  "envelope_version": 1,
  "op_id": "0190c6f2-7b1a-7cc0-9d1e-000000000002",
  "device_id": "dev-a-3b1f",
  "device_seq": 2,
  "notebook_id": "nb-7f3c2a9e",
  "depends_on": [{"device_id": "dev-b-c94e", "device_seq": 9}, {"device_id": "dev-a-3b1f", "device_seq": 1}],
  "key_epoch": 1,
  "op_schema_version": 1,
  "ciphertext_sha256": "<64 lowercase hex>",
  "ciphertext_length": 1962,
  "signature": "<unpadded base64url>"
}
```

| Field | Type | Rule |
|---|---|---|
| `envelope_version` | u16 | Must be 1 |
| `op_id`, `device_id`, `notebook_id` | id | See §3. Length must be ≤ `limits.max_id_length` |
| `device_seq` | u64 | Per-device, strictly increasing |
| `depends_on` | `[{device_id, device_seq}]` | ≤ `max_depends_on`, no duplicates. A reference to the **own** device must have `device_seq < own device_seq` (no self-reference or cycle). Order on the wire does not matter. |
| `key_epoch` | u32 | Content key epoch |
| `op_schema_version` | u16 | ≥ 1 (0 is invalid). Newer values are relayed (§2). |
| `ciphertext_sha256` | string | Exactly 64 **lowercase** hex characters |
| `ciphertext_length` | u64 | `1..=max_ciphertext_bytes_per_op` |
| `signature` | string | 1..=512 characters of canonical unpadded base64url. Not verified in M02. Fixtures use a placeholder (64 × `0xA5`). |

`EnvelopeHeader::validate(&limits)` returns a typed `EnvelopeError`. Each error has a stable `kind()` and an `error_code()`:

| kind | wire code |
|---|---|
| `unsupported_envelope_version`, `unsupported_op_schema_version` | `upgrade_required` |
| `ciphertext_too_large` | `payload_too_large` |
| `invalid_op_schema_version`, `id_too_long`, `too_many_dependencies`, `duplicate_dependency`, `dependency_cycle`, `empty_ciphertext`, `invalid_ciphertext_hash`, `invalid_signature_encoding` | `invalid_envelope` |

`verify_ciphertext(header, bytes)` checks the declared length and SHA-256. A mismatch returns `integrity_mismatch`. This is an integrity check only. Authentication comes from the device signature in M03.

## 5. Canonical bytes

These are the bytes a device will sign, and the bytes that will be AEAD associated data, in M03. **No crypto is implemented.** The rules:

* A fixed ASCII domain prefix that ends in `\0`. The prefix has no length prefix.
* Fields always appear in the same fixed order.
* **All integers are u64 little-endian**, whatever their Rust width.
* **All strings are a u32 little-endian byte length followed by the UTF-8 bytes.**
* `depends_on` is a u32 LE count, then the entries **sorted by (device_id bytes, device_seq)**.
* No JSON and no string concatenation.

Always validate a header before signing or verifying it.

### 5.1 `canonical_signing_bytes`: everything except `signature`

| # | Offset | Field | Encoding | Size |
|---|---|---|---|---|
| 0 | 0 | domain | `"penroam/envelope/v1\0"` (`70656e726f616d2f656e76656c6f70652f763100`) | 20 |
| 1 | 20 | envelope_version | u64 LE | 8 |
| 2 | 28 | op_id | u32 LE len ‖ bytes | 4 + n |
| 3 | … | device_id | u32 LE len ‖ bytes | 4 + n |
| 4 | … | device_seq | u64 LE | 8 |
| 5 | … | notebook_id | u32 LE len ‖ bytes | 4 + n |
| 6 | … | depends_on | u32 LE count ‖ sorted (u32 LE len ‖ device_id ‖ u64 LE device_seq)* | 4 + Σ(12 + n) |
| 7 | … | key_epoch | u64 LE | 8 |
| 8 | … | op_schema_version | u64 LE | 8 |
| 9 | … | ciphertext_sha256 | u32 LE len (= 64) ‖ ASCII lowercase hex | 68 |
| 10 | … | ciphertext_length | u64 LE | 8 |

Fixtures: `envelope-header.valid.json` produces `envelope-header.valid.signing-bytes.hex` (245 bytes). Its `depends_on` is deliberately unsorted on the wire.

### 5.2 `canonical_aad_bytes`: AEAD associated-data layout (M03 will use it)

This layout binds the payload to its identity, versions, key epoch, length and causal dependencies. It excludes `ciphertext_sha256` and `signature`. `ciphertext_length` is known before encryption for a fixed-tag AEAD.

| # | Offset | Field | Encoding | Size |
|---|---|---|---|---|
| 0 | 0 | domain | `"penroam/aead/v1\0"` | 16 |
| 1 | 16 | envelope_version | u64 LE | 8 |
| 2 | 24 | op_schema_version | u64 LE | 8 |
| 3 | 32 | key_epoch | u64 LE | 8 |
| 4 | 40 | op_id | u32 LE len ‖ bytes | 4 + n |
| 5 | … | device_id | u32 LE len ‖ bytes | 4 + n |
| 6 | … | device_seq | u64 LE | 8 |
| 7 | … | notebook_id | u32 LE len ‖ bytes | 4 + n |
| 8 | … | ciphertext_length | u64 LE | 8 |
| 9 | … | depends_on | as in 5.1 | 4 + Σ(12 + n) |

Fixture: `envelope-header.valid.aad-bytes.hex` (173 bytes).

## 6. Batch framing (`PRSB`)

JSON control headers travel next to the raw binary ciphertext. Ciphertext is never base64-encoded inside JSON. All integers are little-endian:

| Part | Encoding |
|---|---|
| magic | `"PRSB"` (4 bytes) |
| version | u16 = 1 |
| count | u32 |
| item × count | u32 header_len ‖ header JSON (UTF-8, compact) ‖ u32 ct_len ‖ ciphertext |
| end | no trailing bytes allowed |

* Push body (`POST /v1/ops/batch`): each item header is an `EnvelopeHeader`.
* Pull frame: each item header is a `PulledOp { server_seq, header }`, in the same order as `PullOpsResponse.items`.
* Proposed media type: `application/vnd.penroam.batch.v1`.

The decoder rejects all of the following **before allocating**:

* a body larger than `max_batch_bytes`
* bad magic or version
* truncation at any byte
* trailing bytes
* `count > max_batch_envelopes`
* a count that cannot fit in the remaining bytes (8 bytes minimum per item)
* an empty header
* a header larger than `max_header_json_bytes`
* ciphertext larger than `max_ciphertext_bytes_per_op`

The encoder enforces the same limits. If any header does not parse, the server rejects the whole batch, because the op id is unknown and a per-op ack is impossible. Size failures map to `payload_too_large`. Everything else maps to `malformed_batch`.

## 7. Limits

| Field | Default | Hard max |
|---|---|---|
| `max_batch_envelopes` | 256 | 1024 |
| `max_batch_bytes` | 8 MiB | 64 MiB |
| `max_ciphertext_bytes_per_op` | 1 MiB | 16 MiB |
| `max_header_json_bytes` | 16 KiB | 64 KiB |
| `max_blob_bytes` | 64 MiB | 1 GiB |
| `max_depends_on` | 64 | 256 |
| `max_pull_page_size` (larger requests are clamped) | 500 | 1000 |
| `max_id_length` | 128 | 128 |

Quotas and pricing are server policy (`UsageResponse.quota_bytes`). They are never permanent client limits.

## 8. DTOs and proposed routes

All routes below are **proposed, not frozen**. They are defined in `penroam_sync_protocol::routes`. Every route except `/v1/meta` is authenticated.

| Route | Request | Response |
|---|---|---|
| `GET /v1/meta` | — | `ServerMeta` |
| `POST /v1/devices`, `GET /v1/devices`, `DELETE /v1/devices/{id}` | `DeviceRegistrationRequest` (placeholder: public keys are opaque base64url, optional `label` is visible to the server) | `DeviceRegistrationResponse { device_id }` |
| `POST /v1/ops/batch` | PRSB frame of `EnvelopeHeader`s | `PushOpsResponse { acks: [{op_id, result}] }` |
| `GET /v1/ops?cursor=..&limit=..` | `PullOpsRequest { cursor?, limit }` | `PullOpsResponse { items: [{server_seq, header}], next_cursor, has_more }` plus a PRSB frame of `PulledOp`s (§8.1) |
| `POST /v1/acks` | `AckRequest { applied_cursor }` | — |
| `GET /v1/usage` | — | `UsageResponse` |
| `/v1/blobs/uploads`, `/v1/blobs/{id}`, `/v1/checkpoints` | constants only | not implemented |

The `result` of each op is tagged:

* `{"status":"accepted","server_seq":N}`
* `{"status":"duplicate","server_seq":N}` (an identical retry)
* `{"status":"rejected","error":ErrorBody}`

Acks always name exact operations, never just "the batch". A `Cursor` is opaque bounded base64url (at most 512 characters). Persist `next_cursor` only after the items have been applied durably. `has_more` says whether more items existed when the query ran.

### 8.1 Pull transport mapping (proposed)

The JSON control document carries the headers. The ciphertexts travel in a PRSB frame whose items match `items` one to one. Over HTTP, the proposal is: a framed response (`Accept: application/vnd.penroam.batch.v1`) carries the pagination in the `Penroam-Next-Cursor` and `Penroam-Has-More` response headers. The testkit returns both forms as `PullPage { response, frame }`. Freeze this in M03.

## 9. Errors

`ErrorBody { code, message, request_id, retry_after_ms? }`. `message` is safe to display and never contains secrets or plaintext.

| code | HTTP | auto-retry | client guidance |
|---|---|---|---|
| `malformed_request` | 400 | no | Bug. Report it and keep the outbox. |
| `unauthenticated` | 401 | no | Re-authenticate. Never erase the outbox. |
| `forbidden` | 403 | no | Account/device mismatch. Surface it. |
| `device_revoked` | 403 | no | Re-enroll the device. Keep local data. |
| `not_found` | 404 | no | Unknown resource |
| `conflict` | 409 | no | State conflict. Refresh and decide. |
| `op_id_conflict` | 409 | no | The op_id or device_seq was reused with different content. This is a local bug or corruption. Do not overwrite. |
| `recovery_required` | 409 | no | Run the recovery/rebase flow. Preserve unsent ops. |
| `cursor_expired` | 410 | no | Fetch a checkpoint, then the tail (M03/M04). |
| `checkpoint_required` | 410 | no | Same as `cursor_expired` |
| `payload_too_large` | 413 | no | Split the batch or respect the limits. |
| `invalid_envelope` | 422 | no | The header failed validation. |
| `malformed_batch` | 422 | no | Framing error |
| `integrity_mismatch` | 422 | no | Length/SHA-256 mismatch. Re-read the ciphertext and retry once as a new request. |
| `invalid_cursor` | 422 | no | The cursor was tampered with or belongs to another account. Restart from the last applied cursor. |
| `validation_failed` | 422 | no | Other semantic validation failure |
| `upgrade_required` | 426 | no | Pause sync and prompt the user to update. Keep all data. |
| `rate_limited` | 429 | **yes** | Back off, and wait at least `retry_after_ms` |
| `quota_exceeded` | 507 | no | Show a quota state. Ops stay queued. |
| `internal_error` | 500 | **yes** | Back off |
| `service_unavailable` | 503 | **yes** | Back off, and wait at least `retry_after_ms` |
| `unknown` (any unrecognized code) | — | no | Surface it |

`error-codes.json` pins this table (code, status, retryable).

**Retry policy.** `RetryPolicy { base_delay_ms = 500, max_delay_ms = 60000, max_attempts = 8, max_retry_after_ms = 15 min }` uses exponential backoff with **full jitter**:

* `delay = random % (min(cap, base·2^attempt) + 1)`
* The random `u64` is an input, so tests are deterministic.
* `delay_for(error, attempt, random)` returns `None` when the error is not retryable or the attempts are used up.
* Otherwise it returns `max(jitter, min(retry_after_ms, max_retry_after_ms))`.

This prevents retry storms. HTTP success means the server accepted the request. It does not mean the remote change was applied.

## 10. Fake server (`InMemoryServer`, testkit only)

* **Accounts and devices.** Accounts and devices are created through admin calls. Authentication is simulated: callers pass the account and device.
  * A pushed envelope's `device_id` must equal the pushing device. Otherwise the push fails with `forbidden`.
  * Revoked devices get `device_revoked`.
* **Push.**
  * Each envelope is validated, then its length and SHA-256 are checked.
  * Idempotency is keyed on `(account, op_id)`. An identical header returns `duplicate`. The same op_id with different content returns `op_id_conflict`. A `(device_id, device_seq)` reused by another op_id also returns `op_id_conflict`.
  * A quota check runs before acceptance.
  * One lock covers assigning the next `server_seq` and storing the op, so every accepted op gets a strictly increasing, gap-free sequence number.
* **Pull.**
  * The cursor encodes the last delivered `server_seq`, so pagination stays stable while other clients append.
  * Pages are clamped to `max_pull_page_size` and `max_batch_bytes`.
  * Cursors are `base64url(payload ‖ HMAC-SHA256(secret, payload)[..16])`. The payload binds account, stream and protocol major. The secret is random per server. **This is testkit-only and not a production crypto design.**
  * `expire_cursors_before(account, seq)` simulates compaction: an older cursor gets `cursor_expired`, and a request with no cursor gets `checkpoint_required`.
* **Acks.** Each device's applied cursor is tracked separately and only moves forward.
* **Usage.** Op count and bytes are counted. Blob counters stay 0.
* **Fault injection.**
  * `fail_next_requests(n, code)`: the next n requests fail without being processed.
  * `drop_next_responses(n)`: the request is processed and persisted, but the caller gets `RequestError::ResponseLost`. The retry must then come back as `duplicate`.

## 11. Fixtures and the compatibility check

`crates/penroam-sync-protocol/fixtures/v1/` holds the generated fixtures. They are deterministic: fixed ids, fixed pseudo-ciphertext (`SHA-256(label ‖ counter)` blocks), a fixed server secret and a placeholder signature. They include:

* meta and contract manifest
* the error-code table
* a valid header with its signing/AAD hex
* nine invalid headers, each with its expected `kind` and code
* two push batches (`.bin`) and their responses (accepted; and duplicate / op_id_conflict / integrity_mismatch / accepted)
* two pull pages (JSON and `.bin`)
* ack, usage and device registration
* error bodies

```sh
cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures -- crates/penroam-sync-protocol/fixtures/v1          # write
cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures -- --check crates/penroam-sync-protocol/fixtures/v1  # verify (exit 1 + diff summary)
```

`crates/penroam-sync-protocol/tests/fixtures.rs` is the public schema compatibility check. Every fixture must:

* deserialize into the current types
* re-serialize **byte-identically** (pretty JSON plus a trailing newline)
* if it is binary or hex, be reproduced exactly by the current encoders

The fixture set itself is also pinned. `crates/penroam-sync-testkit/tests/fixtures.rs` runs the `--check` comparison as a test.

Changing a committed fixture is a wire-visible change. It needs an explicit version decision.

## 12. Not yet implemented (M03/M04)

* **Cryptography:**
  * Ed25519 device signatures over `canonical_signing_bytes`, and signature verification before accepting a write
  * AEAD encryption with `canonical_aad_bytes`
  * key epochs and rotation, recovery codes
  * `crypto-vectors.json`
* **Authentication and sessions:** login, token handling, the device enrollment and pairing flow, and real device revocation semantics. `DeviceRegistration*` are placeholders.
* **Blobs:** upload intents, signed URLs, blob storage and verification. Only the route constants and `max_blob_bytes` exist.
* **Checkpoints/snapshots:** `covers_server_seq`, the checkpoint + tail recovery path, and real compaction/retention. The testkit only *simulates* expiry.
* **Real HTTP:** server (private repo, Axum) and client transport, OpenAPI document, freezing route names and the pull transport mapping (§8.1), rate limiting, request size enforcement at the HTTP layer.
* **Client sync engine:** outbox, state machine, merge, and the operation algebra integration (M03).
* **Release tooling:** filling `git_sha` in the contract manifest, and the cross-check test of the duplicated format constants against storage/core.
