//! Journaled storage: crash injection at every durable-write boundary,
//! legacy file compatibility and migration, journal corruption handling, single writer,
//! disk full, retry deduplication, outbox, snapshot fallback, page order.

use penroam_core::migrate::stroke_doc_to_elements;
use penroam_core::*;
use penroam_storage::fsutil::fault;
use penroam_storage::library::Tuning;
use penroam_storage::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn tmp() -> tempfile::TempDir {
    // fsync-heavy loops: prefer RAM-backed storage when available
    if Path::new("/dev/shm").is_dir() {
        tempfile::tempdir_in("/dev/shm").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    }
}

fn strokes(n: usize) -> Vec<Element> {
    let v: Value =
        serde_json::from_slice(&fs::read(fixtures().join("sample-strokes.json")).unwrap()).unwrap();
    let mut els = stroke_doc_to_elements(&v, 1).unwrap();
    els.truncate(n);
    els
}

fn ink(id: u128, z: i64) -> Element {
    let mut e = strokes(1).remove(0);
    if let Element::Ink(s) = &mut e {
        s.id = Uuid::from_u128(id);
        s.z = z;
    }
    e
}

fn text(id: u128, z: i64, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::from_u128(id),
        z,
        x: 10.0,
        y: 10.0,
        width: 100.0,
        height: 20.0,
        text: s.into(),
        font_size: 12.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        extra: Default::default(),
    })
}

fn batch(id: u128, base: u64, kind: OpKind) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::from_u128(id),
        base_revision: base,
        ops: vec![PageOperation {
            op_id: Uuid::from_u128(id + 1),
            kind,
        }],
    }
}

/// Library state without wall-clock values.
fn state(lib: &mut Library) -> Value {
    fn scrub(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.remove("createdAt");
                m.remove("updatedAt");
                if let Some(d) = m.get_mut("deletedAt") {
                    *d = Value::from(0);
                }
                m.values_mut().for_each(scrub);
            }
            Value::Array(a) => a.iter_mut().for_each(scrub),
            _ => {}
        }
    }
    let mut ids: Vec<Uuid> = lib.list_notebooks().iter().map(|n| n.id).collect();
    ids.sort();
    let mut out = Vec::new();
    for id in ids {
        let nb = lib.notebook(id).unwrap().clone();
        let mut pages: Vec<Uuid> = nb.all_page_ids().collect();
        pages.sort();
        let pages: Vec<Value> = pages
            .into_iter()
            .map(|p| serde_json::to_value(lib.load_page(id, p).unwrap()).unwrap())
            .collect();
        out.push(serde_json::json!({ "notebook": nb, "pages": pages }));
    }
    let mut v = Value::Array(out);
    scrub(&mut v);
    v
}

/// A scenario touching every kind of journaled change, checkpoints with
/// segment deletion, notebook trash and purge. Stops at the first error.
fn scenario(lib: &mut Library, mut after_step: impl FnMut(&mut Library)) -> Result<()> {
    lib.set_deterministic_ids(1 << 64);
    let (nb, p1) = lib.create_notebook("A", PageDefaults::default(), None)?;
    after_step(lib);
    lib.apply_page_operations(
        nb.id,
        p1.id,
        &batch(
            100,
            0,
            OpKind::AddElements {
                elements: vec![ink(1, 1), ink(2, 2), ink(3, 3)],
            },
        ),
    )?;
    after_step(lib);
    lib.apply_page_operations(
        nb.id,
        p1.id,
        &batch(
            102,
            1,
            OpKind::AddElements {
                elements: vec![text(4, 4, "hi")],
            },
        ),
    )?;
    after_step(lib);
    lib.apply_page_operations(
        nb.id,
        p1.id,
        &batch(
            104,
            2,
            OpKind::ReplaceElements {
                elements: vec![text(4, 4, "hi there")],
            },
        ),
    )?;
    after_step(lib);
    let (_, p2) = lib.create_page(nb.id, Some(0), None, Some(Paper::A4))?;
    after_step(lib);
    lib.rename_notebook(nb.id, "A renamed")?;
    after_step(lib);
    lib.apply_page_operations(
        nb.id,
        p2.id,
        &batch(
            106,
            0,
            OpKind::AddElements {
                elements: vec![ink(5, 1)],
            },
        ),
    )?;
    after_step(lib);
    lib.update_page_order(nb.id, vec![p1.id, p2.id])?;
    after_step(lib);
    lib.flush_all()?;
    after_step(lib);
    lib.set_tuning(Tuning {
        max_entries: 2,
        min_compact_bytes: 0,
    });
    lib.apply_page_operations(
        nb.id,
        p1.id,
        &batch(
            108,
            3,
            OpKind::RemoveElements {
                ids: vec![Uuid::from_u128(2)],
            },
        ),
    )?;
    after_step(lib);
    lib.add_asset(nb.id, b"%PDF-1.4 crash test", "application/pdf", None)?;
    after_step(lib);
    lib.delete_page(nb.id, p2.id)?;
    after_step(lib);
    lib.restore_page(nb.id, p2.id)?;
    after_step(lib);
    let (b, bp) = lib.create_notebook("B", PageDefaults::default(), None)?;
    after_step(lib);
    let mut rev = 0;
    for i in 0..5u128 {
        rev = lib
            .apply_page_operations(
                b.id,
                bp.id,
                &batch(
                    200 + 2 * i,
                    rev,
                    OpKind::AddElements {
                        elements: vec![ink(50 + i, i as i64)],
                    },
                ),
            )?
            .revision;
        after_step(lib);
    }
    lib.apply_page_operations(
        nb.id,
        p1.id,
        &batch(
            300,
            4,
            OpKind::SetTemplate {
                template: Template {
                    kind: TemplateKind::Grid,
                    spacing: 12.0,
                },
            },
        ),
    )?;
    after_step(lib);
    lib.delete_notebook(b.id)?;
    after_step(lib);
    lib.purge_notebook(b.id)?;
    after_step(lib);
    lib.set_notebook_cover(nb.id, "#7A3B3B")?;
    after_step(lib);
    lib.flush_all()?;
    after_step(lib);
    Ok(())
}

/// Crash at every injection point (fsync/rename/create/write/torn append
/// boundaries) of the scenario. After restart the library must equal the
/// state after the last acknowledged step, or after the step in flight —
/// never anything else, and never lose an acknowledged step.
#[test]
fn crash_at_every_write_boundary_loses_no_acknowledged_edit() {
    // golden states
    let d = tmp();
    let mut golden = vec![];
    {
        let mut lib = Library::open(d.path().join("lib")).unwrap();
        golden.push(state(&mut lib));
        scenario(&mut lib, |l| golden.push(state(l))).unwrap();
    }
    let steps = golden.len() - 1;
    let mut crashes = 0;
    let mut by_point: std::collections::BTreeMap<&str, u32> = Default::default();
    for k in 0.. {
        let d = tmp();
        let root = d.path().join("lib");
        let mut lib = Library::open(&root).unwrap();
        let mut done = 0;
        fault::arm(k);
        let r = scenario(&mut lib, |_| done += 1);
        let crashed = fault::crashed();
        fault::disarm();
        if r.is_ok() && !crashed {
            assert_eq!(done, steps);
            break;
        }
        crashes += 1;
        *by_point.entry(fault::last()).or_default() += 1;
        lib.crash_for_tests();
        let mut lib = Library::open(&root)
            .unwrap_or_else(|e| panic!("reopen after crash at point {k} ({}): {e}", fault::last()));
        let s = state(&mut lib);
        assert!(
            s == golden[done] || s == golden[(done + 1).min(steps)],
            "crash at point {k} ({}) during step {}: state matches neither the acknowledged nor the in-flight step\nwarnings: {:?}",
            fault::last(),
            done + 1,
            lib.recovery.warnings
        );
        assert_eq!(
            lib.recovery.corrupt_journal_records, 0,
            "point {k}: a crash is never reported as corruption"
        );
        // the recovered library keeps working and survives another restart
        let (nb, p) = lib
            .create_notebook("after", PageDefaults::default(), None)
            .unwrap();
        lib.apply_page_operations(
            nb.id,
            p.id,
            &batch(
                900,
                0,
                OpKind::AddElements {
                    elements: vec![ink(900, 1)],
                },
            ),
        )
        .unwrap();
        let s2 = state(&mut lib);
        drop(lib);
        let mut lib = Library::open(&root).unwrap();
        assert_eq!(state(&mut lib), s2, "point {k}: second restart");
    }
    eprintln!("crash points exercised: {crashes} {by_point:?}");
    assert!(
        crashes > 100,
        "the scenario passes many injection points ({crashes})"
    );
}

#[test]
fn disk_full_rolls_back_and_the_library_keeps_working() {
    let d = tmp();
    let root = d.path().join("lib");
    let mut lib = Library::open(&root).unwrap();
    let (nb, p) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            10,
            0,
            OpKind::AddElements {
                elements: vec![ink(1, 1)],
            },
        ),
    )
    .unwrap();
    let before = state(&mut lib);
    for n in 0..3 {
        // fail the n-th I/O point of the next append once: before the write,
        // half-way through it (torn), before the fsync
        fault::arm_transient(n);
        let r = lib.apply_page_operations(
            nb.id,
            p.id,
            &batch(
                20 + 2 * n as u128,
                1,
                OpKind::AddElements {
                    elements: vec![ink(2, 2)],
                },
            ),
        );
        fault::disarm();
        let err = r.expect_err("append must fail");
        assert_eq!(err.code(), ErrorCode::StorageFull, "{err}");
        assert_eq!(state(&mut lib), before, "nothing applied in memory");
    }
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            40,
            1,
            OpKind::AddElements {
                elements: vec![ink(2, 2)],
            },
        ),
    )
    .unwrap();
    let after = state(&mut lib);
    lib.crash_for_tests();
    let mut lib = Library::open(&root).unwrap();
    assert_eq!(state(&mut lib), after);
    assert_eq!(
        lib.recovery.corrupt_journal_records, 0,
        "{:?}",
        lib.recovery.warnings
    );
    assert_eq!(
        lib.recovery.truncated_journals, 0,
        "failed appends were rolled back"
    );
}

#[test]
fn second_writer_is_refused() {
    let d = tmp();
    let root = d.path().join("lib");
    let lib = Library::open(&root).unwrap();
    match Library::open(&root) {
        Err(StorageError::Conflict(m)) => assert!(m.contains("already open")),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("second writer was allowed"),
    }
    drop(lib);
    Library::open(&root).unwrap();
}

#[test]
fn retried_batch_is_applied_once() {
    let d = tmp();
    let mut lib = Library::open(d.path().join("lib")).unwrap();
    let (nb, p) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    let b = batch(
        10,
        0,
        OpKind::AddElements {
            elements: vec![ink(1, 1)],
        },
    );
    let o1 = lib.apply_page_operations(nb.id, p.id, &b).unwrap();
    let o2 = lib.apply_page_operations(nb.id, p.id, &b).unwrap();
    assert_eq!(o1.revision, o2.revision);
    assert_eq!(lib.load_page(nb.id, p.id).unwrap().elements.len(), 1);
    // also after a restart (rebuilt from the journal)
    lib.crash_for_tests();
    let mut lib = Library::open(d.path().join("lib")).unwrap();
    assert_eq!(
        lib.apply_page_operations(nb.id, p.id, &b).unwrap().revision,
        1
    );
    assert_eq!(lib.load_page(nb.id, p.id).unwrap().revision, 1);
}

#[test]
fn detached_reads_see_journaled_edits_without_touching_the_cache() {
    let d = tmp();
    let root = d.path().join("lib");
    let mut lib = Library::open(&root).unwrap();
    let (nb, p) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            10,
            0,
            OpKind::AddElements {
                elements: vec![ink(1, 1)],
            },
        ),
    )
    .unwrap();
    // cached page
    let cached = lib.read_page_detached(nb.id, p.id).unwrap();
    assert_eq!((cached.revision, cached.elements.len()), (1, 1));

    // after a crash the edit lives only in the journal: snapshot + pending batch
    lib.crash_for_tests();
    let mut lib = Library::open(&root).unwrap();
    let replayed = lib.recovery.replayed_batches;
    let detached = lib.read_page_detached(nb.id, p.id).unwrap();
    assert_eq!((detached.revision, detached.elements.len()), (1, 1));
    // nothing was loaded into the cache: the batch is replayed only by the real load
    assert_eq!(lib.recovery.replayed_batches, replayed);
    assert_eq!(detached, lib.load_page(nb.id, p.id).unwrap());
    assert_eq!(lib.recovery.replayed_batches, replayed + 1);
    assert!(matches!(
        lib.read_page_detached(nb.id, Uuid::from_u128(999)),
        Err(StorageError::NotFound(_))
    ));
}

fn journal_segments(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root.join("journal"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "pjl"))
        .collect();
    v.sort();
    v
}

#[test]
fn mid_journal_corruption_keeps_every_readable_record() {
    let d = tmp();
    let root = d.path().join("lib");
    let mut lib = Library::open(&root).unwrap();
    let (nb, p) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    for i in 0..6u64 {
        lib.apply_page_operations(
            nb.id,
            p.id,
            &batch(
                10 + 2 * i as u128,
                i,
                OpKind::AddElements {
                    elements: vec![text(100 + i as u128, i as i64, "x")],
                },
            ),
        )
        .unwrap();
    }
    lib.crash_for_tests();
    // damage one byte inside the 3rd page record
    let seg = journal_segments(&root).pop().unwrap();
    let mut bytes = fs::read(&seg).unwrap();
    let starts: Vec<usize> = (0..bytes.len() - 4)
        .filter(|i| &bytes[*i..*i + 4] == b"PRJR")
        .collect();
    let victim = starts[3] + 40; // record 0 = notebook creation
    bytes[victim] ^= 0xff;
    fs::write(&seg, &bytes).unwrap();

    let mut lib = Library::open(&root).unwrap();
    assert_eq!(lib.recovery.corrupt_journal_records, 1);
    assert_eq!(lib.recovery.truncated_journals, 0, "nothing truncated");
    assert_eq!(
        fs::read(&seg).unwrap(),
        bytes,
        "damaged segment left byte-identical"
    );
    assert!(
        fs::read_dir(root.join("journal/quarantine"))
            .unwrap()
            .count()
            == 1
    );
    // records before the damage are applied; the page stops at the gap
    let page = lib.load_page(nb.id, p.id).unwrap();
    assert_eq!(page.revision, 2, "{:?}", lib.recovery.warnings);
    // later valid records are kept: the checkpoint floor never passes them
    lib.flush_all().unwrap();
    assert!(
        journal_segments(&root).contains(&seg),
        "segment with unapplied records is retained"
    );
    // new appends go to a new segment
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            99,
            2,
            OpKind::AddElements {
                elements: vec![ink(7, 9)],
            },
        ),
    )
    .unwrap();
    assert!(journal_segments(&root).len() >= 2);
}

#[test]
fn damaged_snapshot_falls_back_to_previous_snapshot_and_journal() {
    let d = tmp();
    let root = d.path().join("lib");
    let mut lib = Library::open(&root).unwrap();
    let (nb, p) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            10,
            0,
            OpKind::AddElements {
                elements: vec![ink(1, 1)],
            },
        ),
    )
    .unwrap();
    lib.flush_all().unwrap();
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            12,
            1,
            OpKind::AddElements {
                elements: vec![ink(2, 2)],
            },
        ),
    )
    .unwrap();
    lib.flush_all().unwrap();
    let expected = lib.load_page(nb.id, p.id).unwrap();
    drop(lib);
    let prp = root.join(format!("notebooks/{}/pages/{}.prp", nb.id, p.id));
    assert!(prp.with_extension("prp.prev").exists());
    let mut b = fs::read(&prp).unwrap();
    let n = b.len() / 2;
    b[n] ^= 1; // bit rot
    fs::write(&prp, b).unwrap();
    let mut lib = Library::open(&root).unwrap();
    assert_eq!(lib.load_page(nb.id, p.id).unwrap(), expected);
    assert_eq!(lib.recovery.fallback_snapshots, 1);
}

#[test]
fn newer_library_or_journal_is_refused_without_changes() {
    let d = tmp();
    let root = d.path().join("lib");
    drop(Library::open(&root).unwrap());
    fs::write(
        root.join("library.json"),
        br#"{"format":"penroam-library","version":3}"#,
    )
    .unwrap();
    assert!(matches!(Library::open(&root), Err(StorageError::Corrupt(m)) if m.contains("newer")));
    assert_eq!(
        fs::read(root.join("library.json")).unwrap(),
        br#"{"format":"penroam-library","version":3}"#
    );
}

#[test]
fn outbox_is_derivable_and_persists_acks() {
    let d = tmp();
    let root = d.path().join("lib");
    let mut lib = Library::open(&root).unwrap();
    let (nb, p) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            10,
            0,
            OpKind::AddElements {
                elements: vec![ink(1, 1), ink(2, 2)],
            },
        ),
    )
    .unwrap();
    let st = lib.outbox_status();
    assert_eq!(st.enqueued_through, 3);
    assert_eq!(st.pending, 3);
    let ops = lib.operations_after(0).unwrap();
    assert_eq!(
        ops.iter().map(|o| o.local_sequence).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(ops.iter().all(|o| o.device_id == lib.device_id()));
    assert_eq!(
        ops[2].id,
        Uuid::from_u128(11),
        "editor op id is the stable operation id"
    );
    lib.set_sync_enabled(true).unwrap();
    lib.mark_uploaded(2).unwrap();
    assert!(lib.mark_uploaded(99).is_err());
    let device = lib.device_id();
    lib.crash_for_tests();
    let mut lib = Library::open(&root).unwrap();
    assert_eq!(lib.device_id(), device, "device id is stable");
    assert_eq!(lib.outbox_status().uploaded_through, 2);
    // with sync enabled, checkpoints keep un-uploaded operations
    lib.flush_all().unwrap();
    lib.flush_all().unwrap();
    assert_eq!(lib.operations_after(2).unwrap().len(), 1);
    drop(lib);
    // a damaged outbox file is rebuilt: unknown upload state => re-upload, no loss
    fs::write(root.join("outbox.json"), b"garbage").unwrap();
    let lib = Library::open(&root).unwrap();
    let st = lib.outbox_status();
    assert_eq!((st.enqueued_through, st.uploaded_through), (3, 0));
}

#[test]
fn page_reorders_use_order_keys_and_restore_returns_between_neighbours() {
    let d = tmp();
    let mut lib = Library::open(d.path().join("lib")).unwrap();
    let (nb, p0) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    let mut ids = vec![p0.id];
    for _ in 0..7 {
        let (_, p) = lib.create_page(nb.id, None, None, None).unwrap();
        ids.push(p.id);
    }
    let mut rng = 12345u64;
    for _ in 0..50 {
        // random permutation
        let mut order = lib.notebook(nb.id).unwrap().page_order.clone();
        for i in (1..order.len()).rev() {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            order.swap(i, (rng % (i as u64 + 1)) as usize);
        }
        let got = lib.update_page_order(nb.id, order.clone()).unwrap();
        assert_eq!(got.page_order, order);
        assert!(got.order_is_consistent());
    }
    let order = lib.notebook(nb.id).unwrap().page_order.clone();
    lib.delete_page(nb.id, order[3]).unwrap();
    let nb2 = lib.restore_page(nb.id, order[3]).unwrap();
    assert_eq!(nb2.page_order, order);
    assert!(
        nb2.order_keys.values().all(|k| k.len() < 32),
        "keys stay short"
    );
    let reopened = {
        drop(lib);
        Library::open(d.path().join("lib")).unwrap()
    };
    assert_eq!(reopened.notebook(nb.id).unwrap().page_order, order);
}

#[test]
fn read_only_page_with_unknown_extension_refuses_edits_and_keeps_bytes() {
    let d = tmp();
    let root = d.path().join("lib");
    let mut lib = Library::open(&root).unwrap();
    let (nb, p) = lib
        .create_notebook("A", PageDefaults::default(), None)
        .unwrap();
    lib.flush_all().unwrap();
    drop(lib);
    let prp = root.join(format!("notebooks/{}/pages/{}.prp", nb.id, p.id));
    let mut page = penroam_core::binary::decode_page(&fs::read(&prp).unwrap()).unwrap();
    page.binary_extensions.push(BinaryExtension {
        tag: 0x42,
        data: vec![1, 2],
    });
    let bytes = penroam_core::binary::encode_page(&page);
    fs::write(&prp, &bytes).unwrap();
    let mut lib = Library::open(&root).unwrap();
    let err = lib
        .apply_page_operations(
            nb.id,
            p.id,
            &batch(
                10,
                0,
                OpKind::AddElements {
                    elements: vec![ink(1, 1)],
                },
            ),
        )
        .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
    lib.flush_all().unwrap();
    assert_eq!(
        fs::read(&prp).unwrap(),
        bytes,
        "never rewritten or stripped"
    );
}

// ------------------------------------------------ legacy compatibility --

fn sha256(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Every legacy fixture is unchanged (SHA-256 pinned in SHA256SUMS).
#[test]
fn legacy_fixtures_are_hash_pinned() {
    let dir = fixtures().join("legacy");
    let sums = fs::read_to_string(dir.join("SHA256SUMS")).unwrap();
    let mut n = 0;
    for line in sums.lines() {
        let (hash, path) = line.split_once("  ").unwrap();
        assert_eq!(sha256(&fs::read(dir.join(path)).unwrap()), hash, "{path}");
        n += 1;
    }
    assert!(n >= 9);
}

fn zip_entries(path: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            (f.name().to_string(), b)
        })
        .collect()
}

/// Legacy (archive format 1) `.penroam` archives import losslessly; re-export reproduces every
/// page byte for byte and every asset.
#[test]
fn legacy_archives_reopen_without_loss() {
    for name in [
        "tiny",
        "strokes-1000",
        "strokes-10000",
        "mixed-text",
        "pdf-annotated",
    ] {
        let src = fixtures().join(format!("legacy/{name}.penroam"));
        let d = tmp();
        let mut lib = Library::open(d.path().join("lib")).unwrap();
        let report = lib.import_archive(fs::File::open(&src).unwrap()).unwrap();
        let nb = report.notebook;
        assert_eq!(nb.schema_version, NOTEBOOK_SCHEMA_VERSION);
        assert!(nb.order_is_consistent());
        let orig = zip_entries(&src);
        for pid in &nb.page_order {
            let page = lib.load_page(nb.id, *pid).unwrap();
            let bytes = &orig[&format!("pages/{pid}.prp")];
            assert_eq!(
                page,
                penroam_core::binary::decode_page(bytes).unwrap(),
                "{name}"
            );
        }
        let out = d.path().join("re.penroam");
        lib.export_archive(nb.id, fs::File::create(&out).unwrap(), "test")
            .unwrap();
        let again = zip_entries(&out);
        for (k, v) in &orig {
            if k.starts_with("pages/") || k.starts_with("assets/") {
                assert_eq!(&again[k], v, "{name}: {k} changed on re-export");
            }
        }
        // the re-exported manifest is schema 2: schema-1-only builds refuse it (fail safe)
        let m: Value = serde_json::from_slice(&again["manifest.json"]).unwrap();
        assert_eq!(m["schemaVersion"], 2);
        assert_eq!(m["notebook"]["schemaVersion"], 2);
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// A legacy library left after a crash (snapshot + unflushed JSON-lines
/// journal) migrates: backups first, journal folded in, schema 2 written.
#[test]
fn legacy_crashed_library_migrates_with_backup() {
    let d = tmp();
    let root = d.path().join("lib");
    copy_dir(&fixtures().join("legacy/crashed-library"), &root);
    let nb_dir = fs::read_dir(root.join("notebooks"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let legacy_journal = fs::read_dir(nb_dir.join("pages"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "journal"))
        .unwrap();
    let journal_bytes = fs::read(&legacy_journal).unwrap();

    let mut lib = Library::open(&root).unwrap();
    assert_eq!(lib.recovery.migrated_notebooks, 1);
    assert_eq!(
        lib.recovery.replayed_batches, 7,
        "{:?}",
        lib.recovery.warnings
    );
    let nb = lib.list_notebooks()[0].clone();
    let page = lib.load_page(nb.id, nb.first_page_id.unwrap()).unwrap();
    assert_eq!(
        page.elements.len(),
        13,
        "12 strokes + text typed after the last legacy checkpoint"
    );
    assert_eq!(page.revision, 13);
    assert!(
        !legacy_journal.exists(),
        "legacy journal removed after commit"
    );
    let on_disk: Value =
        serde_json::from_slice(&fs::read(nb_dir.join("notebook.json")).unwrap()).unwrap();
    assert_eq!(on_disk["schemaVersion"], 2);
    // original bytes are in the backup
    let backups: Vec<PathBuf> = walk(&root.join("migration-backups"));
    assert!(backups
        .iter()
        .any(|p| fs::read(p).unwrap() == journal_bytes));
    let expected = state(&mut lib);
    drop(lib);
    // idempotent: a second open changes nothing
    let mut lib = Library::open(&root).unwrap();
    assert_eq!(lib.recovery.migrated_notebooks, 0);
    assert_eq!(state(&mut lib), expected);
}

fn walk(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in fs::read_dir(p).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

/// Interrupting the migration at any write boundary leaves a library that
/// a later open migrates to the same result.
#[test]
fn legacy_migration_is_crash_safe() {
    let golden = {
        let d = tmp();
        copy_dir(
            &fixtures().join("legacy/crashed-library"),
            &d.path().join("lib"),
        );
        let mut lib = Library::open(d.path().join("lib")).unwrap();
        state(&mut lib)
    };
    for k in 0.. {
        let d = tmp();
        let root = d.path().join("lib");
        copy_dir(&fixtures().join("legacy/crashed-library"), &root);
        fault::arm(k);
        let r = Library::open(&root);
        let crashed = fault::crashed();
        fault::disarm();
        match r {
            Ok(lib) if !crashed => {
                drop(lib);
                break;
            }
            Ok(lib) => lib.crash_for_tests(),
            Err(_) => {}
        }
        let mut lib = Library::open(&root).unwrap();
        assert_eq!(
            state(&mut lib),
            golden,
            "crash at point {k} ({})",
            fault::last()
        );
    }
}

#[test]
fn legacy_fixture_sums_file_matches_generator_output_names() {
    // guard against the SHA256SUMS file silently dropping entries
    let sums = fs::read_to_string(fixtures().join("legacy/SHA256SUMS")).unwrap();
    for f in [
        "tiny.penroam",
        "strokes-1000.penroam",
        "strokes-10000.penroam",
        "mixed-text.penroam",
        "pdf-annotated.penroam",
    ] {
        assert!(sums.contains(f), "{f}");
    }
    let _ = std::io::stdout().flush();
}
