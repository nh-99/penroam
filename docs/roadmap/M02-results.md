# M02 results — binary format contract, operation algebra, durable journal, sync contracts

Date: 2026-11-02. Machine: the Linux workstation from `docs/platforms/linux.md` (rustc 1.99.0
stable). Plan: [`M02-plan.md`](M02-plan.md). Normative format: [`docs/formats/penroam-binary-v1.md`](../formats/penroam-binary-v1.md).
Protocol: [`docs/protocol/sync-protocol-v1.md`](../protocol/sync-protocol-v1.md).
**Status: implemented in the working tree on `main` (base `fba9dab`), not yet committed.**

## 1. Exit criteria

| Criterion | Status | Evidence |
|---|---|---|
| Preexisting M01 `.penroam` files reopen and migrate without byte/semantic loss | **Met** | `m01_archives_reopen_without_loss`: 5 M01-written archives (tiny, 1k, 10k strokes, mixed text, 5-page rotated PDF) import; every page equals the M01 bytes decoded; re-export reproduces every page and asset **byte for byte**. `m01_crashed_library_migrates_with_backup`: M01 library with an unflushed JSON-lines journal migrates (7 batches replayed, 13 elements, backups byte-identical, idempotent on reopen). `m01_migration_is_crash_safe`: crash at every write boundary of the migration converges to the same result. Fixtures SHA-256 pinned (`tests/fixtures/m01/SHA256SUMS`). |
| File format v1 and op schema described with fixtures, limits and migrations | **Met** | `docs/formats/penroam-binary-v1.md` (versions, `.prp`, required/optional features, extension area, limits, journal, checkpoints, op schema, notebook v1→v2); golden `.prp` hash-pinned and reproduced byte for byte by the M02 encoder. |
| `Saved locally` edits survive deterministic crash injection across write boundaries | **Met** | `crash_at_every_write_boundary_loses_no_acknowledged_edit`: a 25-step scenario (ink, text edits, page create/reorder/trash/restore, rename, cover, assets, notebook trash + purge, forced frequent checkpoints, segment deletion) crashed at **each of 380 injection points** across 15 boundary kinds (`append.write/torn/fsync`, `atomic.create/write/fsync/rename/dirsync`, `create.file/header/dirsync`, `rename`, `rename.dirsync`, `remove`, `remove.dir`). After every restart the state equals the last acknowledged step or the in-flight step, a crash is never reported as corruption, and the library keeps working through a second restart. |
| No ack before durable journal persistence; snapshot recovery after partial write | **Met** | Journal append = write + `fdatasync` before `apply_page_operations`/notebook calls return; snapshots are checkpoints. Torn tail truncated (`torn_journal_tail_recovers_previous_valid_state`); crash between snapshot and manifest harmless; damaged `.prp` rebuilt from `.prp.prev` + journal (`damaged_snapshot_falls_back_to_previous_snapshot_and_journal`); mid-log corruption keeps every readable record, quarantines, never truncates (`mid_journal_corruption_keeps_every_readable_record`); disk full rolls back cleanly (`disk_full_rolls_back_and_the_library_keeps_working`). |
| Public sync crates independently build/test; clean public clone needs no private repo | **Met locally / CI job added** | `cargo test -p penroam-sync-protocol -p penroam-sync-testkit`: 88 pass. `cargo tree -p penroam-sync-protocol -e all` contains no penroam-core/storage/pdf/cloud or tauri (deps: serde, serde_json, sha2, thiserror). No `penroam-cloud` anywhere. New CI job `sync-contract`; **CI has still never been executed** (no remote configured). |
| Performance change relative to M01 documented; native ink quality unchanged | **Met (documented, one explained regression)** | §3. Native ink adapters, the page model, the `.prp` encoder output and the TS editor are unchanged (TS 37/37, golden bytes identical). |
| CI lint/test/fuzz smoke + scheduled long fuzz; results committed | **Partly** | fmt/clippy `-D warnings`/tests/fixture checks/fuzz-target compile added to `ci.yml`; `fuzz.yml` scheduled nightly (3 targets). Deterministic fuzz smoke runs in `cargo test` (20 000 mutations + 300 random bit-exact round trips). **Not run here:** cargo-fuzz (no nightly toolchain on this machine; the fuzz crate only type-checks on stable) and GitHub CI itself. Nothing is committed yet. |

## 2. Commands and results (all run on this machine)

| Command | Result |
|---|---|
| `cargo test --workspace` | 194 pass, 0 fail (core 64, storage 36, pdf 6, sync protocol 67, testkit 21) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `pnpm typecheck && pnpm lint && pnpm test` | clean; 37/37 (TS bindings regenerated: `Notebook.orderKeys`, `RecoveryReport` counters) |
| `cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures -- --check crates/penroam-sync-protocol/fixtures/v1` | up to date (generator output byte-identical across runs) |
| `cargo check --manifest-path fuzz/Cargo.toml` | compiles (stable) |
| Release app smoke test | see §4 |

New tests worth naming: `operation.rs` — convergence of every interleaving of concurrent
operations (independent strokes, same-block text edits with retained alternate, move + edit of one
block merging, delete vs. concurrent edit, delete vs. restore, concurrent renames, concurrent page
inserts/moves), retries idempotent, op-id and sequence reuse rejected, newer schema refused,
missing dependencies not applied, journal replay bit-identical to M01 batches over 400 random
batches, notebook suffix replay idempotent for every (prefix, suffix). `format_v1.rs` — extension
preserve / read-only / refuse, required/optional flags, notebook v1→v2.

## 3. Performance (M01 baseline vs M02)

`crates/penroam-storage/examples/library_bench.rs` (public `Library` API only — the same file ran
against the untouched M01 code before any change). Median of 3 processes per size, ext4 NVMe, warm
page cache. Raw: `benchmarks/results/m02/baseline-m01-storage.jsonl`, `m02-storage.jsonl`.

| Metric | 100 | 1 000 | 5 000 | 10 000 strokes |
|---|---|---|---|---|
| durable stroke append median (ms) M01 → M02 | 5.59 → 5.50 | 5.50 → 5.48 | 5.31 → 5.73 | 6.32 → 5.99 |
| durable stroke append p95 (ms) | 6.42 → 6.30 | 6.13 → 6.22 | 5.72 → 6.59 | 7.21 → 6.83 |
| text edit median (ms) | 5.57 → 5.60 | 5.14 → 5.55 | 5.30 → 5.81 | 6.36 → 6.04 |
| bulk persist of the whole scene (ms) | 7.1 → 6.5 | 49.7 → 13.3 | 112 → 130 | 205 → 201 |
| cold page load (ms) | 1.2 → 0.9 | 6.0 → 3.6 | 18.7 → 18.9 | 36.3 → 36.8 |
| crash recovery: open + replay 50 batches (ms) | 3.0 → 4.6 | 7.3 → 10.8 | 27.9 → 29.7 | 54.7 → 56.4 |
| open library (ms) | 0.26 → 2.1 | 0.20 → 4.0 | 0.18 → 1.3 | 0.20 → 1.2 |
| checkpoint (ms) | 30 → 60 | 39 → 67 | 62 → 97 | 103 → 137 |
| disk after checkpoint (bytes) | 76 k → 329 k | 309 k → 982 k | 1.31 M → 2.78 M | 2.53 M → 5.21 M |
| peak RSS (MiB) | 7.6 → 8.6 | 39 → 38 | 189 → 193 | 374 → 382 |

The user-visible path (pen-up → *Saved locally*) is unchanged within noise (5000-stroke p95 +15 %
is 0.9 ms and inverts at 10 000). Regressions above 20 %, explained:

* **Checkpoint +33–100 % (+30–35 ms absolute)** — about five more fsyncs per checkpoint: the
  previous snapshot is kept (rename + dir sync), the new snapshot is read back, and the checkpoint
  manifest is rotated and written. Checkpoints are off the acknowledgement path (page eviction,
  every 512 batches / journal size, app close). An earlier version also rewrote `library.json`
  on every open and wrote the manifest twice; both were fixed after the first run (raw numbers of
  that run were overwritten).
* **Disk ≈ 2×** — by design: one fallback snapshot per page (`.prp.prev`) plus one retained journal
  generation, so a damaged snapshot can always be rebuilt. Bounded at roughly 2× the live data.
* **Open +1–4 ms** — validating and indexing the retained journal generation. Small-size
  percentages look large because M01's open did almost nothing.

Not measured in M02 (no change to those paths): rendering, WebView RSS, BOOX/iPad.

## 4. Real app

`pnpm tauri build --no-bundle` (release) built. Run on GNOME/Wayland, dpr 2, with
`PENROAM_LIBRARY_DIR` pointing at a **copy of the M01 crashed-library fixture** and
`PENROAM_AUTOBENCH=m02-smoke,quick,exit` (the CI smoke mode):

* start-up recovery: `migrated_notebooks: 1, replayed_batches: 7`, no corruption, backup written,
  legacy journal removed, `notebook.json` schema 2 with consistent order keys;
* Workload D quick (1k strokes) completed and exited 0: full-page redraw median 57 ms, visible-first
  22 ms, replay interval median 16 ms / p95 19 ms, pen-up → durable 12 ms (M01 reported 8 ms for
  one sample on the 10k page; single samples, IPC included), tree RSS 622 MiB;
* after exit the journal directory is empty (clean shutdown checkpointed and released everything).

Raw: not committed (scratch run). Not re-run: the full 10k Workload D, X11, BOOX, iPad.

## 5. Changed paths

* `crates/penroam-core`: `model.rs` (notebook schema 2 + `orderKeys`, page schema 1, preserved
  `BinaryExtension`s, read-only reason), `binary.rs` (required/optional flags, extension area,
  public point-block codec), `migrate.rs` (independent chains, notebook v1→v2), `validate.rs`,
  `ops.rs` (crate-visible helpers), **new** `operation.rs`, `replica.rs`, `order_key.rs`; tests
  `format_v1.rs`, `operation.rs`.
* `crates/penroam-storage`: **new** `journal.rs`, `state.rs`, `legacy.rs`; `library.rs` rewritten
  on the journal (public API kept; added `device_id`, `last_seq`, `outbox_status`,
  `operations_after`, `mark_uploaded`, `set_sync_enabled`; retry dedup by batch id; exclusive lock);
  `fsutil.rs` (crash/disk-full injection behind the `fault-injection` feature); `archive.rs`
  (schema 2 manifests, purged-id conflicts); examples `library_bench.rs`, `m01_fixtures.rs`; tests
  `m02_storage.rs`, `contract_versions.rs`, journal-layout updates in `storage.rs`.
* **New** `crates/penroam-sync-protocol`, `crates/penroam-sync-testkit` (+ fixtures `v1/`),
  `docs/protocol/sync-protocol-v1.md`.
* `fuzz/` (cargo-fuzz: `decode_page`, `journal_open`, `sync_batch_frame`), `.github/workflows/ci.yml`,
  `.github/workflows/fuzz.yml`.
* `tests/fixtures/m01/`, `benchmarks/results/m02/`, `docs/formats/penroam-binary-v1.md`,
  `docs/format/penroam-format.md` (pointer), `apps/desktop-mobile/src/generated/*` (regenerated).

## 6. Decisions needing owner sign-off (ADR-003)

1. Notebook schema bump to 2 — intentionally makes M01 builds refuse M02 libraries/archives
   (fail-safe); **downgrade to M01 is unsupported**.
2. Journal is device-wide and binary; snapshots keep one fallback generation (≈ 2× disk).
3. Outbox is a small derivable JSON file, not SQLite (rebuild = re-upload, never loss).
4. Merge: highest operation id wins visibly, alternates retained; restore beats delete; element
   order ties broken by id (local editing keeps M01 insertion order exactly).
5. `notebookPurged` is local-only and not merged; hosted deletion policy belongs to M05.
6. Sync crate choices made by the implementer (see its doc): newer op schemas are relayed by
   servers and refused by applying clients; pull pagination carried in response headers; extra
   normalized error codes (`op_id_conflict`, `device_revoked`, `integrity_mismatch`, …).
   Open: should `device_seq`/`key_epoch` start at 1; must signed `depends_on` be sorted on the wire.

## 7. Known issues / not done

* Coverage-guided fuzzing and GitHub CI have not been executed (no nightly toolchain, no remote).
* Only Linux was exercised; no second OS build (Windows/macOS CI jobs exist from M01, unrun).
* No BOOX/iPad run in M02 (no device-specific code changed; `.prp` bytes are identical).
* The editor UI does not yet surface `RecoveryReport.corruptJournalRecords`/`fallbackSnapshots`
  beyond the existing recovery warnings list, nor conflicts (no sync yet — M03).
* `Replica` (merge model) is not wired into storage; M03 applies remote operations through it.
* Version history with rollback was considered and **deferred to M09.5** (owner decision: compact
  operation diffs + sparse snapshots, slow restore accepted, storage is the cost driver). The
  `.prp.prev` fallback stays as the corruption safety net meanwhile.
* Protocol crate is not tagged/released (`sync-protocol-v0.1.0` tag is a release action for the
  owner).
