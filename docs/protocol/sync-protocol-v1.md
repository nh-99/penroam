# Penroam Sync Protocol v1: implemented contract

**Status:** Implemented as public Rust types and fixtures. The wire *shapes* in this document (envelope header, canonical bytes, batch framing, error codes) are what `penroam-sync-protocol` encodes and what the committed fixtures pin. The HTTP mapping (§12) is frozen for protocol 1.0. Encryption, signatures and the key log (§13–§15) are implemented by the client crate `penroam-sync`; **they have not been independently reviewed** (see [`../security/threat-model.md`](../security/threat-model.md)). The design rationale is in `agent-docs/01-SYNC-PROTOCOL-V1.md`.

| Crate | Purpose | Dependencies |
|---|---|---|
| `crates/penroam-sync-protocol` | Versions, ids, limits, envelope header and validation, canonical bytes, batch framing, DTOs, error codes, retry policy, contract manifest | `serde`, `serde_json`, `thiserror`, `sha2` only. No `penroam-core`, storage, Tauri, UI or device SDKs |
| `crates/penroam-sync-testkit` | `InMemoryServer` fake server (test scaffolding, not production), deterministic fixture generator `penroam-sync-fixtures` | the protocol crate, `serde`, `serde_json`, `thiserror`, `sha2`, `uuid` |

The protocol crate itself contains no cryptography and no network code. It fixes the bytes that signatures and AEAD cover; the client crate implements them (§13).

## 1. Version numbers

Each number is independent. None is derived from another.

| Constant | Value | Describes |
|---|---|---|
| `PROTOCOL_MAJOR` / `PROTOCOL_MINOR` | 1 / 3 | HTTP sync protocol. A different major is never compatible. A minor adds features, and each new feature is gated by a capability. |
| `ENVELOPE_VERSION` | 1 | Opaque remote container: envelope header, canonical bytes, framing |
| `OP_SCHEMA_VERSION` (min supported `OP_SCHEMA_MIN_SUPPORTED`) | 1 (1) | Encrypted semantic operation payload |
| `FILE_FORMAT_VERSION` | 1 | Local `.prp` page file |
| `JOURNAL_FORMAT_VERSION` | 1 | Local operation journal |
| `ARCHIVE_FORMAT_VERSION` | 2 | `.penroam` archive |
| `NOTEBOOK_SCHEMA_VERSION` | 2 | Notebook model schema |
| `PAGE_SCHEMA_VERSION` | 1 | Page model schema |

The local-format numbers are copied into the protocol crate on purpose, because it must not depend on storage. They are advertised in `contract-manifest.json`:

```json
{ "protocol": {"major": 1, "minor": 3}, "envelope_version": 1,
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
* Minor features are **never assumed from the version number**. Use them only if `meta.has_capability(..)` is true. Current capability strings: `ops.batch.v1`, `ops.pull.v1`, `acks.v1`, `usage.v1` (1.0), `keylog.v1`, `blobs.v1`, `checkpoints.v1` (1.0, §12), `auth.v1` (1.1, §12a; redirect options 1.2), `account.v1` (1.3, §12b), `models.v1` (1.4, §12c). Clients ignore unknown capabilities.
* Minor history: 1.0 is the original HTTP mapping; 1.1 adds `auth.v1` and nothing else. A 1.0 client works unchanged against a 1.1 server that also accepts its tokens. 1.2 adds two optional `AuthConfig` fields, the `loopback-ports` redirect kind and the mobile redirect (§12a). A 1.1 client works against a 1.2 server whose `redirect` is `loopback`; it refuses `loopback-ports` as an unknown kind. 1.3 adds `account.v1` (§12b) and the error code `reauth_required`; older clients never call the new routes and read the new code as `unknown`. 1.4 adds `models.v1` (§12c) and nothing else.
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
| `signature` | string | 1..=512 characters of canonical unpadded base64url: the Ed25519 signature (§13). The server does not verify it (it does not hold the membership log); receiving clients verify it before decrypting. Fixtures use a placeholder (64 × `0xA5`). |

`EnvelopeHeader::validate(&limits)` returns a typed `EnvelopeError`. Each error has a stable `kind()` and an `error_code()`:

| kind | wire code |
|---|---|
| `unsupported_envelope_version`, `unsupported_op_schema_version` | `upgrade_required` |
| `ciphertext_too_large` | `payload_too_large` |
| `invalid_op_schema_version`, `id_too_long`, `too_many_dependencies`, `duplicate_dependency`, `dependency_cycle`, `empty_ciphertext`, `invalid_ciphertext_hash`, `invalid_signature_encoding` | `invalid_envelope` |

`verify_ciphertext(header, bytes)` checks the declared length and SHA-256. A mismatch returns `integrity_mismatch`. This is an integrity check only; authenticity comes from the device signature, which receiving clients check (§13).

## 5. Canonical bytes

These are the bytes a device signs and the bytes used as AEAD associated data (the signing and encryption are done by the client, §13). The rules:

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

### 5.2 `canonical_aad_bytes`: AEAD associated-data layout

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
* Media type: `application/vnd.penroam.batch.v1`.

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

## 8. DTOs and routes

Routes are defined in `penroam_sync_protocol::routes` and frozen for protocol 1.0 (HTTP mapping in §12). Every route except `/v1/meta` needs `Authorization: Bearer <token>`.

| Route | Request | Response |
|---|---|---|
| `GET /v1/meta` | — | `ServerMeta` |
| `POST /v1/devices`, `GET /v1/devices`, `DELETE /v1/devices/{id}` | `DeviceRegistrationRequest { device_id?, signing_public_key, encryption_public_key, label? }` (keys opaque to the server; the label is server-visible, so clients send a generic one) / `DeviceListResponse { devices: [DeviceInfo] }` | `DeviceRegistrationResponse { device_id }`. Clients never trust keys from these routes; membership comes from the key log (§14) |
| `POST /v1/ops/batch` | PRSB frame of `EnvelopeHeader`s | `PushOpsResponse { acks: [{op_id, result}] }` |
| `GET /v1/ops?cursor=..&limit=..` | `PullOpsRequest { cursor?, limit }` | `PullOpsResponse { items: [{server_seq, header}], next_cursor, has_more }` plus a PRSB frame of `PulledOp`s (§8.1) |
| `POST /v1/acks` | `AckRequest { applied_cursor }` | — |
| `GET /v1/usage` | — | `UsageResponse` |
| `POST /v1/checkpoints`, `GET /v1/checkpoints/latest` | `CheckpointRecord` | 204 / `CheckpointRecord` (§15a) |
| `/v1/blobs/uploads` | constant only | not implemented (blobs use `PUT /v1/blobs/{id}`) |

The `result` of each op is tagged:

* `{"status":"accepted","server_seq":N}`
* `{"status":"duplicate","server_seq":N}` (an identical retry)
* `{"status":"rejected","error":ErrorBody}`

Acks always name exact operations, never just "the batch". A `Cursor` is opaque bounded base64url (at most 512 characters). Persist `next_cursor` only after the items have been applied durably. `has_more` says whether more items existed when the query ran.

### 8.1 Pull transport mapping

The JSON control document carries the headers. The ciphertexts travel in a PRSB frame whose items match `items` one to one. Over HTTP, the proposal is: a framed response (`Accept: application/vnd.penroam.batch.v1`) carries the pagination in the `Penroam-Next-Cursor` and `Penroam-Has-More` response headers. The testkit returns both forms as `PullPage { response, frame }`. This mapping is not frozen until a real HTTP client and server exist.

## 9. Errors

`ErrorBody { code, message, request_id, retry_after_ms? }`. `message` is safe to display and never contains secrets or plaintext.

| code | HTTP | auto-retry | client guidance |
|---|---|---|---|
| `malformed_request` | 400 | no | Bug. Report it and keep the outbox. |
| `unauthenticated` | 401 | no | Re-authenticate. Never erase the outbox. |
| `forbidden` | 403 | no | Account/device mismatch. Surface it. |
| `device_revoked` | 403 | no | Re-enroll the device. Keep local data. |
| `reauth_required` | 403 | no | The action needs a recent sign-in (§12b). Sign in again, then retry with the new account session. |
| `not_found` | 404 | no | Unknown resource |
| `conflict` | 409 | no | State conflict. Refresh and decide. |
| `op_id_conflict` | 409 | no | The op_id or device_seq was reused with different content. This is a local bug or corruption. Do not overwrite. |
| `recovery_required` | 409 | no | Run the recovery/rebase flow. Preserve unsent ops. |
| `cursor_expired` | 410 | no | Fetch the latest checkpoint, import it, then continue from the cursor it covers (§15a). Without a usable checkpoint the client stops pulling (`rebaseRequired`) and keeps uploading its own operations. |
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

## 12. HTTP mapping (frozen for 1.0)

| Route | Token | Request | Response |
|---|---|---|---|
| `GET /v1/meta` | none | — | `ServerMeta` |
| `POST /v1/devices` | account | `DeviceRegistrationRequest` | `DeviceRegistrationResponse` |
| `GET /v1/devices` | account | — | `DeviceListResponse` |
| `DELETE /v1/devices/{id}` | device | — | 204 |
| `POST /v1/ops/batch` | device | PRSB frame of `EnvelopeHeader`s (`application/vnd.penroam.batch.v1`) | `PushOpsResponse` |
| `GET /v1/ops?cursor=&limit=` | device | `Accept: application/vnd.penroam.batch.v1` | PRSB frame of `PulledOp`; `Penroam-Next-Cursor` and `Penroam-Has-More: true\|false` headers. With `Accept: application/json`: `PullOpsResponse` without ciphertexts |
| `POST /v1/acks` | device | `AckRequest` | 204 |
| `GET /v1/usage` | account | — | `UsageResponse` |
| `GET /v1/keylog?from=N` | account | — | `KeyLogPage` (at most 256 entries) |
| `POST /v1/keylog` | account | `KeyLogAppendRequest` | `KeyLogAppendResponse`; 409 `conflict` when `index` is not the current length |
| `PUT /v1/blobs/{id}` | device | raw bytes, `Penroam-Content-Sha256: <hex>` | `BlobPutResponse` |
| `GET /v1/blobs/{id}` | device | — | raw bytes |
| `POST /v1/checkpoints` | device | `CheckpointRecord` (blob uploaded first) | 204; 409 `conflict` if older than the stored one |
| `GET /v1/checkpoints/latest` | device | — | `CheckpointRecord`, or 404 `not_found` |

* "Account" tokens identify an account; "device" tokens identify a registered device of it.
  How tokens are obtained is not part of this protocol (the development test server has a
  `POST /_test/session` stub; a hosted service uses its own sign-in).
* Every response carries `Penroam-Request-Id`. Errors are an `ErrorBody` with the status of
  §9 and, when `retry_after_ms` is set, `Retry-After` in whole seconds (rounded up).
* Bodies need `Content-Length`; servers check size limits before reading the body.
* New capabilities in 1.0: `keylog.v1`, `blobs.v1`, `checkpoints.v1`. Clients use these routes only when the
  capability is listed.

## 12a. Sign-in and sessions (`auth.v1`, protocol 1.1; redirect options 1.2)

Servers with real accounts list `auth.v1`. Types are in `penroam_sync_protocol::auth`; the
client side is `penroam_sync::auth`.

| Route | Token | Request | Response |
|---|---|---|---|
| `GET /v1/auth/config` | none | — | `AuthConfig { issuer, client_id, scopes, redirect, loopback_ports?, mobile_redirect_uri? }` |
| `POST /v1/auth/session` | none | `SessionRequest { id_token }` | `SessionTokens` (account session) |
| `POST /v1/auth/refresh` | none | `RefreshRequest { refresh_token }` | `SessionTokens` (same scope) |
| `POST /v1/auth/logout` | account or device | — | 204 |
| `POST /v1/auth/device-challenge` | account or device | `DeviceChallengeRequest { device_id }` | `DeviceChallenge { challenge, expires_in_s }` |
| `POST /v1/auth/device-session` | account or device | `DeviceSessionRequest { device_id, challenge, signature }` | `SessionTokens` (device session) |

`SessionTokens { account_id, device_id?, access_token, expires_in_s, refresh_token }`. Tokens are
opaque; clients store them in the keychain and never log them.

1. **Provider sign-in (client).** Fetch `{issuer}/.well-known/openid-configuration`; its
   `issuer` must equal `AuthConfig.issuer`. Run the authorization-code flow with PKCE `S256`
   (RFC 7636) in the **system browser**, never an embedded web view. Check `state` on the
   callback and the ID token's `nonce` after the code exchange. Clients must refuse settings
   that fail `AuthConfig::validate()`. How the answer comes back:
   * **Desktop, `redirect: "loopback"`:** `http://127.0.0.1:{port}/callback` on a port chosen
     per attempt (RFC 8252 §7.3). The provider must accept any port.
   * **Desktop, `redirect: "loopback-ports"` (1.2):** the same URI on the first free port of
     `loopback_ports` (1 to 8 distinct ports, tried in order), for providers that require every
     redirect URI to be registered exactly. If all are busy, the sign-in fails; it never falls
     back to another port.
   * **Mobile (1.2):** `mobile_redirect_uri`, a private-use URI scheme in reverse domain-name
     form (RFC 8252 §7.1), e.g. `ink.penroam.app://callback`. The operating system hands the
     link to the app that claims the scheme; the app only accepts its own scheme. Without
     `mobile_redirect_uri` the mobile apps cannot sign in to that server. Fixture:
     `auth-config.loopback-ports.json`.
2. **Session exchange (server).** The server verifies the ID token's signature against the
   issuer's JWKS, `iss`, `aud` (= `client_id`), `exp` and `iat`, maps `(iss, sub)` to an
   account and returns an **account session**. An account session may use `/v1/devices`
   (register, list), `/v1/keylog`, `/v1/usage`, `/v1/account*` (§12b) and `/v1/auth/*`. Every other protected route
   answers 403 `forbidden`.
3. **Device session.** The device asks for a challenge, signs
   `canonical_device_auth_bytes(account_id, device_id, challenge, server_instance_id)` with its
   Ed25519 key, and sends the signature. Layout: domain `"penroam/device-auth/v1\0"`, then the
   four strings, each as u32 LE length + UTF-8 bytes. The server issues a **device session** only
   if the challenge is unused and unexpired, the signature verifies against the device's
   registered signing key, and the device is an **active member of the key log with that same
   key** (§14). So a stolen account session cannot read or write notes on its own. Fixture:
   `auth-device-session.signing-bytes.hex`.
4. **Refresh and revocation.** Access tokens are short-lived. Each refresh token works once and
   is replaced by the one in the response; presenting a used refresh token revokes the whole
   session. A device session stops working (refresh included) when the device is revoked or
   removed from the key log. `unauthenticated` (401) means sign in again; local notes and the
   outbox are kept.

The development test server does not list `auth.v1`; it keeps its `/_test/session` stub.

## 12b. Plan, storage allowance and billing links (`account.v1`, protocol 1.3)

Servers that give accounts a storage allowance, and possibly sell larger ones, list
`account.v1`. Types are in `penroam_sync_protocol::account`; the client calls are inherent
methods of `penroam_sync::transport::HttpTransport`. Payment providers are **not** part of the
protocol: the app never sees a provider id or a price id, and never reports a payment.

| Route | Token | Request | Response |
|---|---|---|---|
| `GET /v1/account` | account or device | — | `AccountState` |
| `POST /v1/account/billing-link` | account or device | `BillingLinkRequest { action, offer_id? }` | `BillingLink { url, expires_in_s }` |
| `POST /v1/account/deletion` | account session from a recent sign-in | `{}` | `DeletionState { requested_at_unix_s, erase_after_unix_s }` |
| `DELETE /v1/account/deletion` | account or device | — | 204 |

`AccountState { account_id, plan, storage, writable, read_only_reason?, offers, actions,
deletion? }`:

* `plan { id, name, status, renews_at_unix_s?, ends_at_unix_s? }`. `status` is one of `free`,
  `trialing`, `active`, `canceling` (ends at `ends_at_unix_s`), `past_due` (a payment failed; the
  plan's allowance holds until `ends_at_unix_s`, the end of the grace period) or `granted`
  (operator allowance, may end at `ends_at_unix_s`).
* `storage { used_bytes, quota_bytes, free_quota_bytes? }`. `quota_bytes` is the allowance in
  force now (`null` = unlimited); `free_quota_bytes` is what every account keeps without a plan.
* `writable` is false exactly when `read_only_reason` is set: `over_quota` (stored data exceeds
  the allowance, for example after a plan ended), `suspended` (operator) or `deletion_pending`.
  A read-only account still pulls, downloads blobs and lets the user export; uploads fail with
  `quota_exceeded` and stay queued on the device. Nothing on the device is deleted or locked.
* `offers [{ id, name, price_label, quota_bytes }]`: paid plans the account can start. The
  server formats `price_label`; the billing page shows the binding price.
* `actions`: what the user can do now, from `subscribe`, `manage_billing`, `request_deletion`,
  `cancel_deletion`. Clients only call an action that is listed.

All strings except ids are open sets: clients ignore unknown statuses, reasons and actions.
Clients call `AccountState::validate()` and refuse responses that fail it.

**Billing links.** `subscribe` names an offer id from `offers`; `manage_billing` takes none. The
server creates the page (for example a hosted checkout or billing portal session) for the
session's account and returns its URL. Clients accept only what `BillingLink::validate()`
accepts (absolute `https://`, no credentials, at most 2048 characters) and open it in the
**system browser**. The app does not learn the outcome from the browser: it reads
`GET /v1/account` again when it returns to the foreground. The server derives the plan from its
payment provider's verified records, never from the client. A client build distributed through
an app store whose rules forbid outside purchase links does not show `offers` or open billing
links; that is a client decision.

**Deletion.** `POST /v1/account/deletion` needs an **account** session created by a sign-in within
the server's re-authentication window (the hosted service uses 10 minutes); otherwise it answers
403 `reauth_required`. During the cooling-off period (until `erase_after_unix_s`) the account is
read-only (`deletion_pending`), downloads keep working, and `DELETE /v1/account/deletion`
withdraws the request. Erasing the cloud copy never touches notes on devices.

The development test server lists `account.v1` in its extended mode: a free plan whose
allowance is the account's quota, no offers and no actions (the action routes answer
`forbidden`). Fixtures: `account-state.free.json`, `account-state.over-quota.json`,
`account-state.past-due.json`, `account-billing-link-request.json`, `account-billing-link.json`,
`account-deletion.json`.

## 12c. Handwriting model distribution (`models.v1`, protocol 1.4)

Servers that distribute handwriting models list `models.v1`. Types are in
`penroam_sync_protocol::models`; signature checking and the HTTP calls are in
`penroam_sync::models`. Models have nothing to do with the notes in an account: both routes take
no token, and clients send none.

| Route | Response |
|---|---|
| `GET /v1/models/catalog` | `SignedCatalog`, or 404 `not_found` when nothing is published |
| `GET /v1/models/files/{sha256}` | The `.penroam-model` archive with that SHA-256 (raw bytes), or 404 `not_found` |

`SignedCatalog` is `{ key_id, catalog, signature }`. `catalog` is the `ModelCatalog` as JSON
**text**, and `signature` is the base64url Ed25519 signature over the bytes
`penroam/model-catalog/v1\0` followed by that text. The signature covers the text as sent, so
nothing is re-serialized to check it.

```json
{
  "format": "penroam-model-catalog",
  "format_version": 1,
  "sequence": 12,
  "models": [{
    "id": "penroam-handwriting-en",
    "version": "2026.11.0",
    "name": "Penroam handwriting model (English)",
    "license": "…",
    "languages": ["en"],
    "sha256": "<64 lowercase hex digits>",
    "bytes": 41234567,
    "model_format_version": 1,
    "min_app_version": "0.2.0",
    "access": "free",
    "recommended": true
  }]
}
```

Client rules:

1. Catalogs are signed by a release process, with a key the API server does not hold. The API
   server is a mirror and is not trusted: a client accepts a catalog only if `key_id` names a key
   built into the app and the signature verifies, and it checks the signature before parsing the
   text. A client with no trusted key makes no request.
2. A client refuses a catalog whose `sequence` is below the highest it has accepted, so an old
   signed catalog cannot be replayed. Publishers increase `sequence` with every catalog.
3. A client offers the first entry with `recommended: true` that it can use: `access` is `free`,
   its app version is at least `min_app_version`, and it reads `model_format_version`.
   **An unknown `access` value means the entry is not for this client.** That is how entries
   needing something more, such as a paid plan, can be added without older clients trying them.
4. `version` is compared for equality only. An installed model that differs from the recommended
   one is offered as an update, which lets a catalog replace a model with one of another id, or
   move back to an earlier version.
5. A client discards a download whose size or SHA-256 differs from the entry, and then installs
   it as described in [`../formats/handwriting-model.md`](../formats/handwriting-model.md).
6. Unknown JSON fields are ignored. A higher `format_version` is refused and the client offers
   nothing.

Servers may answer 429 and 503 like any route. Nothing about models affects sync.

## 13. Payloads, encryption and signatures (client rules)

These rules are implemented in `penroam-sync` (`crypto.rs`, `engine.rs`). **Not independently
reviewed.**

* **Payload.** The plaintext of one envelope is one operation encoded exactly like a journal
  record (`penroam_storage::journal::encode_txn` of a single-operation notebook transaction:
  magic, length, CRC-32, binary body with column-encoded ink points). Receivers decode it with
  `decode_txn` and reject trailing bytes.
* **Header ↔ payload binding.** After decryption the operation's id, device, sequence, schema
  version and dependency set must equal the signed header, and the notebook's routing alias
  (below) must equal `notebook_id`. Any mismatch quarantines the envelope.
* **Identities.** `device_id` and `op_id` are hyphenated lowercase UUIDs. The *sync device* is
  created at enrollment and is distinct from the local library's device id. A device's local
  operations are re-sequenced into its own contiguous `device_seq`.
* **Causality.** `depends_on` carries the full version vector the device's local view had
  absorbed when the operation was made (plus its own previous sequence). Receivers deliver an
  operation only when every dependency and the device's previous sequence are delivered;
  concurrency in the merge is decided from this vector, never from server order or clocks.
* **Keys.** Each epoch has a random 32-byte content key (§14). With HKDF-SHA256:
  * notebook routing alias = `"nb-" + base64url(HMAC-SHA256(HKDF(epoch, "penroam/routing/v1"), notebook_uuid)[..16])`
  * notebook payload key = `HKDF(epoch, salt = alias, info = "penroam/notebook-content/v1")`
  * blob key = `HKDF(epoch, salt = alias, info = "penroam/blob/v1")`; blob id = `"b-" + base64url(HMAC(HKDF(epoch, "penroam/blob-id/v1"), alias ‖ asset_sha256)[..20])`
* **Encryption.** XChaCha20-Poly1305. Ciphertext = `nonce(24) ‖ ciphertext ‖ tag(16)`, so
  `ciphertext_length = 40 + payload length`. Associated data = `canonical_aad_bytes(header)`.
  The nonce is `HMAC-SHA256(key, "penroam/nonce/v1" ‖ op_id ‖ SHA-256(payload))[..24]`, so a
  retried operation produces the same ciphertext (required by the server's idempotency check)
  and different plaintexts never share a nonce.
* **Signature.** Ed25519 by the sync device over `canonical_signing_bytes(header)`. Receivers
  look up the signer in the verified key log, verify the signature, check the ciphertext hash,
  and only then decrypt. Failures are quarantined, never applied, and surfaced as a security
  alert.
* **Unknown schema.** A header with a newer `op_schema_version` is kept encrypted and parked;
  the client reports `needsUpgrade` and loses nothing.

## 14. Key log

An append-only list of opaque entries per account (`/v1/keylog`). The server only enforces
"append at the current length". Each entry is JSON
`{"body": base64url(body_json), "signer": "account"|<device_id>, "sig": base64url(ed25519)}`,
signed over `"penroam/keylog/v1\0" ‖ body_json`. The body is
`{"v": 1, "index": n, "prev": hex(SHA-256(previous entry bytes)), "entry": …}` with:

| `entry.kind` | Signed by | Content |
|---|---|---|
| `genesis` (index 0 only) | account key | account public key, recovery public key, first device certificate, epoch 1 key wrapped to that device and to the recovery key |
| `addDevice` | an active member, or the account key | device certificate (id, Ed25519 and X25519 public keys, label), every epoch key wrapped to it |
| `removeDevice` | an active member, or the account key | device id, `new_epoch = current + 1`, the new key wrapped to every remaining device and to the recovery key, and `lastSeq`: the removed device's highest sequence the remover had received (it pulls first). Clients refuse that device's operations above `lastSeq`, so a misbehaving server cannot deliver writes it made after removal. Entries without `lastSeq` impose no cutoff |

* The **recovery code** encodes a 32-byte root secret (`PRK1-` + Crockford base32 with a
  2-byte checksum). It derives the account signing key and the recovery X25519 key. Devices do
  not store it.
* Key wrapping: X25519 ephemeral-static, HKDF-SHA256 (salt = ephemeral ‖ recipient public
  keys, info = `"penroam/wrap/v1" ‖ context`), XChaCha20-Poly1305 with the context as
  associated data; context = `"penroam/epoch-grant/v1/<epoch>/<recipient>"`.
* Clients verify the whole chain, pin `(account key, length, head hash)` and reject a log that
  rewrites a verified prefix. The server can withhold new entries but cannot inject a device
  or change history.
* **Pairing.** The new device registers its keys and shows a pairing code: the first 8 hex
  digits of its id and its 64-bit key fingerprint. A trusted device finds the registration
  whose keys match that fingerprint, appends `addDevice`, and both devices show a 6-digit
  comparison code derived from the account key and the new device's fingerprint. Matching
  codes rule out key substitution in both directions.

## 15. Blobs

Assets are uploaded **before** any operation that references them, encrypted with the blob key
(nonce derived as in §13, associated data = routing alias), under the opaque blob id. Uploads
are idempotent (same id and bytes → `duplicate`). Receivers download, decrypt, verify the
asset's SHA-256, write it atomically, and only then make it visible.

## 15a. Checkpoints

A checkpoint lets a device that fell behind recover after the server compacted history
(capability `checkpoints.v1`).

* **Content:** the creator's whole operation log in log order (which is causal order), encoded
  as `"PRCP" ‖ u16 version (1) ‖ u32 count ‖ (u32 meta-json length ‖ meta json ‖ u32 record
  length ‖ record)*`. The meta names the operation id, device, sequence, notebook, epoch and the
  SHA-256 its envelope ciphertext has; the record is the §13 payload.
* **Encryption:** XChaCha20-Poly1305 with `HKDF(epoch, info = "penroam/checkpoint/v1")`, nonce
  derived as in §13, associated data `"penroam/checkpoint-blob/v1"`, uploaded as a blob.
* **Record:** `CheckpointRecord { covers_cursor, covers_server_seq, blob_id, blob_sha256,
  blob_length, key_epoch, created_by, signature }`, signed with Ed25519 over
  `canonical_checkpoint_bytes` (domain `"penroam/checkpoint/v1\0"`, then every field except the
  signature, same integer/string encoding as §5). The server keeps the latest record per
  account, checks that the blob exists with that hash and length, that the cursor matches
  `covers_server_seq`, and that records never move backwards. It should compact only up to the
  latest checkpoint.
* **Recovery:** on `cursor_expired`/`checkpoint_required` the client fetches the latest record,
  requires its signer to be a *current* member, verifies the signature, the blob hash and the
  AEAD, then imports each operation it lacks in order. Each must continue its device's sequence
  and pass the removal cutoff (§14). The cursor then moves to `covers_cursor`. Local operations
  are untouched; they were pushed before pulling.
* **Creation:** clients publish a checkpoint after their log grew by 500 operations (and on
  demand), but not while envelopes are parked, since those would not be covered.

## 16. Not implemented yet

* **Server retention policy:** real compaction and retention windows belong to the hosted
  server. The testkit only *simulates* compaction (`/_test/compact`, `/_test/expire-cursors`).
* **Production server:** the hosted server is private and not part of this repository. The
  public side of its sign-in is §12a; the OpenAPI description is
  [`openapi.yaml`](openapi.yaml) (hand-written; a test checks that it lists every route).
* **Independent security review** of §13–§14 and published cross-language `crypto-vectors.json`
  (the Rust crate pins its own vectors in tests).
* **Release tooling:** filling `git_sha` in the contract manifest, and the cross-check test of
  the duplicated format constants against storage/core.
