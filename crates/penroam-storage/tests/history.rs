//! Version history: restore to any retained point, thinning, deletion, damage,
//! and the guarantee that history is never needed to keep working.
//! Design: module docs of `penroam_storage::history`.

use penroam_core::ops::page_diff;
use penroam_core::*;
use penroam_storage::history::*;
use penroam_storage::*;
use std::fs;
use std::time::Instant;
use tempfile::TempDir;
use uuid::Uuid;

const DAY: i64 = 86_400_000;

fn stroke(z: i64, n: usize) -> Element {
    Element::Ink(InkStroke {
        id: Uuid::new_v4(),
        z,
        tool: InkTool::Pen,
        color: "#101010".into(),
        opacity: 1.0,
        width: 2.0,
        pressure: true,
        source: "stylus".into(),
        input: Default::default(),
        points: (0..n)
            .map(|i| InkPoint {
                x: 10.0 + i as f64 * 0.8,
                y: 50.0 + (i as f64 * 0.3).sin() * 9.0 + (z % 700) as f64,
                pressure: 0.4 + (i as f64 * 0.2).sin() * 0.2,
                tilt_x: 10.0,
                tilt_y: 4.0,
                time_us: i as i64 * 4000,
                twist: None,
                flags: None,
            })
            .collect(),
        extra: Default::default(),
    })
}

fn batch(base: u64, kind: OpKind) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: base,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind,
        }],
    }
}

fn setup() -> (TempDir, Library, Uuid, Uuid) {
    let d = TempDir::new().unwrap();
    let mut l = Library::open(d.path().join("lib")).unwrap();
    let (nb, p) = l
        .create_notebook("History", PageDefaults::default(), None)
        .unwrap();
    (d, l, nb.id, p.id)
}

/// Adds `n` strokes one batch at a time and returns the page after each.
fn write(l: &mut Library, nb: Uuid, page: Uuid, n: usize) -> Vec<Page> {
    let mut out = Vec::new();
    for i in 0..n {
        let rev = l.load_page(nb, page).unwrap().revision;
        let e = stroke(rev as i64 + 1 + i as i64, 40);
        l.apply_page_operations(
            nb,
            page,
            &batch(rev, OpKind::AddElements { elements: vec![e] }),
        )
        .unwrap();
        out.push(l.load_page(nb, page).unwrap());
    }
    out
}

fn chain(d: &TempDir, nb: Uuid, page: Uuid) -> std::path::PathBuf {
    d.path()
        .join(format!("lib/notebooks/{nb}/history/{page}.phl"))
}

#[test]
fn every_edit_can_be_read_back_exactly() {
    let (_d, mut l, nb, page) = setup();
    let versions = write(&mut l, nb, page, 30);
    let points = l.page_history(nb, page).unwrap();
    assert_eq!(points.len(), 31, "the empty start plus each edit");
    for v in &versions {
        let got = l.page_version(nb, page, v.revision).unwrap();
        assert_eq!(got.elements, v.elements);
    }
    assert!(matches!(
        l.page_version(nb, page, 9999),
        Err(StorageError::NotFound(_))
    ));
}

#[test]
fn rollback_is_a_new_edit_and_can_be_undone() {
    let (_d, mut l, nb, page) = setup();
    let versions = write(&mut l, nb, page, 10);
    let latest = l.load_page(nb, page).unwrap();
    let seq_before = l.last_seq();

    l.restore_page_version(nb, page, versions[3].revision)
        .unwrap();
    let now = l.load_page(nb, page).unwrap();
    assert_eq!(now.elements, versions[3].elements);
    assert_eq!(
        now.revision,
        latest.revision + 1,
        "history is not rewritten"
    );
    assert!(l.last_seq() > seq_before, "journaled, so it syncs");

    // old versions all still there, and the rollback is one of them
    assert!(l.page_version(nb, page, latest.revision).is_ok());
    l.restore_page_version(nb, page, latest.revision).unwrap();
    assert_eq!(l.load_page(nb, page).unwrap().elements, latest.elements);

    // restoring the current state is a no-op
    let rev = l.load_page(nb, page).unwrap().revision;
    let out = l.restore_page_version(nb, page, rev).unwrap();
    assert_eq!(out.revision, rev);
}

#[test]
fn erasing_moving_and_templates_round_trip() {
    let (_d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 5);
    let Element::Ink(mut moved) = vs[4].elements[1].clone() else {
        panic!()
    };
    moved.color = "#ff0000".into();
    let id = vs[4].elements[0].id();
    l.apply_page_operations(
        nb,
        page,
        &OperationBatch {
            batch_id: Uuid::new_v4(),
            base_revision: vs[4].revision,
            ops: vec![
                PageOperation {
                    op_id: Uuid::new_v4(),
                    kind: OpKind::RemoveElements { ids: vec![id] },
                },
                PageOperation {
                    op_id: Uuid::new_v4(),
                    kind: OpKind::ReplaceElements {
                        elements: vec![Element::Ink(moved)],
                    },
                },
                PageOperation {
                    op_id: Uuid::new_v4(),
                    kind: OpKind::SetTemplate {
                        template: Template {
                            kind: TemplateKind::Grid,
                            spacing: 18.0,
                        },
                    },
                },
            ],
        },
    )
    .unwrap();
    l.restore_page_version(nb, page, vs[4].revision).unwrap();
    let back = l.load_page(nb, page).unwrap();
    assert_eq!(back.elements, vs[4].elements);
    assert_eq!(back.template, vs[4].template);
    assert!(page_diff(&back, &vs[4]).is_empty());
}

#[test]
fn history_survives_restart_and_checkpoints() {
    let (d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 8);
    l.flush_all().unwrap();
    drop(l);
    let mut l = Library::open(d.path().join("lib")).unwrap();
    let more = write(&mut l, nb, page, 3);
    assert_eq!(l.page_history(nb, page).unwrap().len(), 12);
    assert_eq!(
        l.page_version(nb, page, vs[2].revision).unwrap().elements,
        vs[2].elements
    );
    assert_eq!(
        l.page_version(nb, page, more[2].revision).unwrap().elements,
        more[2].elements
    );
}

#[test]
fn a_crash_between_journal_and_history_only_costs_those_versions() {
    let (d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 6);
    l.flush_all().unwrap();
    drop(l);
    // the last append never reached the disk, in part
    let p = chain(&d, nb, page);
    let len = fs::metadata(&p).unwrap().len();
    let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_len(len - 20).unwrap();
    drop(f);

    let mut l = Library::open(d.path().join("lib")).unwrap();
    assert_eq!(l.load_page(nb, page).unwrap().elements, vs[5].elements);
    assert_eq!(
        l.page_version(nb, page, vs[3].revision).unwrap().elements,
        vs[3].elements,
        "versions before the tear still read"
    );
    let next = write(&mut l, nb, page, 2);
    assert_eq!(
        l.page_version(nb, page, next[1].revision).unwrap().elements,
        next[1].elements,
        "new versions restart from a keyframe"
    );
}

#[test]
fn damage_loses_old_versions_but_never_the_page() {
    let (d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 10);
    l.flush_all().unwrap();
    drop(l);
    let p = chain(&d, nb, page);
    let mut b = fs::read(&p).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 0xff;
    fs::write(&p, &b).unwrap();

    let mut l = Library::open(d.path().join("lib")).unwrap();
    assert_eq!(l.load_page(nb, page).unwrap().elements, vs[9].elements);
    assert!(l.page_history(nb, page).unwrap().len() < 11);
    assert_eq!(
        l.page_version(nb, page, vs[0].revision).unwrap().elements,
        vs[0].elements
    );
    // a broken chain can't be restored from, and editing still works
    assert!(l.page_version(nb, page, vs[9].revision).is_err());
    let next = write(&mut l, nb, page, 2);
    assert_eq!(l.load_page(nb, page).unwrap().elements, next[1].elements);

    // total loss of the folder changes nothing either
    fs::remove_dir_all(p.parent().unwrap()).unwrap();
    let vs = write(&mut l, nb, page, 1);
    assert_eq!(l.load_page(nb, page).unwrap().elements, vs[0].elements);
}

#[test]
fn notebook_goes_back_to_an_earlier_time() {
    let (_d, mut l, nb, page) = setup();
    write(&mut l, nb, page, 3);
    let second = l.create_page(nb, None, None, None).unwrap().1.id;
    write(&mut l, nb, second, 2);
    let want_first = l.load_page(nb, page).unwrap();
    let want_second = l.load_page(nb, second).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let at = now_ms();
    std::thread::sleep(std::time::Duration::from_millis(5));

    // everything that happens afterwards
    write(&mut l, nb, page, 2);
    let third = l.create_page(nb, None, None, None).unwrap().1.id;
    write(&mut l, nb, third, 1);
    l.delete_page(nb, second).unwrap();
    l.rename_notebook(nb, "Changed").unwrap();

    let r = l.restore_notebook_version(nb, at).unwrap();
    assert_eq!(r.pages_reverted, 1);
    assert_eq!(r.pages_recovered, 1);
    assert_eq!(r.pages_trashed, 1);
    let n = l.notebook(nb).unwrap().clone();
    assert_eq!(n.title, "History");
    assert_eq!(n.page_order, vec![page, second]);
    assert_eq!(l.load_page(nb, page).unwrap().elements, want_first.elements);
    assert_eq!(
        l.load_page(nb, second).unwrap().elements,
        want_second.elements
    );
    assert!(n.trashed_pages.iter().any(|t| t.id == third), "not deleted");
}

#[test]
fn retention_thins_old_versions_and_keeps_the_rest_exact() {
    let (d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 40);
    let before = fs::metadata(chain(&d, nb, page)).unwrap().len();
    let now = now_ms();
    // nothing is old yet
    let r = HistoryRetention::default();
    assert_eq!(l.compact_history(nb, now, &r).unwrap(), 0);
    assert_eq!(l.page_history(nb, page).unwrap().len(), 41);

    // 400 days later only the newest version stays
    let freed = l.compact_history(nb, now + 400 * DAY, &r).unwrap();
    assert!(freed > 0);
    let pts = l.page_history(nb, page).unwrap();
    assert_eq!(pts.len(), 1);
    assert_eq!(pts[0].revision, vs[39].revision);
    assert!(fs::metadata(chain(&d, nb, page)).unwrap().len() < before);
    assert_eq!(
        l.page_version(nb, page, vs[39].revision).unwrap().elements,
        vs[39].elements
    );

    // the chain carries on afterwards
    let next = write(&mut l, nb, page, 3);
    assert_eq!(l.page_history(nb, page).unwrap().len(), 4);
    assert_eq!(
        l.page_version(nb, page, next[1].revision).unwrap().elements,
        next[1].elements
    );
}

#[test]
fn thinning_keeps_one_version_a_day_and_drops_erased_content() {
    let (d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 4);
    let secret = stroke(9999, 2000);
    let id = secret.id();
    let rev = vs[3].revision;
    l.apply_page_operations(
        nb,
        page,
        &batch(
            rev,
            OpKind::AddElements {
                elements: vec![secret],
            },
        ),
    )
    .unwrap();
    l.apply_page_operations(
        nb,
        page,
        &batch(rev + 1, OpKind::RemoveElements { ids: vec![id] }),
    )
    .unwrap();
    let before = fs::metadata(chain(&d, nb, page)).unwrap().len();
    // all six edits fall on one day, inside the daily band
    let r = HistoryRetention {
        keep_all_days: 0,
        daily_days: 30,
        weekly_days: 365,
    };
    l.compact_history(nb, now_ms() + 2 * DAY, &r).unwrap();
    assert!(l.page_history(nb, page).unwrap().len() <= 2);
    assert!(
        fs::metadata(chain(&d, nb, page)).unwrap().len() < before / 2,
        "the erased stroke's samples are gone from disk"
    );
}

#[test]
fn delete_including_history() {
    let (d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 4);
    let hist = chain(&d, nb, page);
    assert!(hist.exists());
    assert!(l.history_stats(nb).unwrap().bytes > 0);
    l.clear_history(nb).unwrap();
    assert!(!hist.exists());
    assert_eq!(l.history_stats(nb).unwrap(), HistoryStats::default());
    assert!(l.page_history(nb, page).unwrap().is_empty());
    assert_eq!(l.load_page(nb, page).unwrap().elements, vs[3].elements);
    write(&mut l, nb, page, 1);
    assert!(hist.exists(), "history starts again");

    // purging a notebook takes its history with it
    l.delete_notebook(nb).unwrap();
    l.purge_notebook(nb).unwrap();
    assert!(!hist.exists());
}

#[test]
fn archives_and_backups_carry_no_history() {
    let (_d, mut l, nb, page) = setup();
    write(&mut l, nb, page, 3);
    let mut out = std::io::Cursor::new(Vec::new());
    l.export_backup(&mut out, "t").unwrap();
    let raw = out.into_inner();
    let (_d2, mut other) = {
        let d = TempDir::new().unwrap();
        let l = Library::open(d.path().join("lib")).unwrap();
        (d, l)
    };
    other
        .restore_backup(std::io::Cursor::new(raw), &RestoreOptions::default())
        .unwrap();
    assert!(other.page_history(nb, page).unwrap().is_empty());
    // the first edit after a restore starts a chain from the restored page
    let vs = write(&mut other, nb, page, 1);
    assert_eq!(other.page_history(nb, page).unwrap().len(), 2);
    assert_eq!(other.load_page(nb, page).unwrap().elements, vs[0].elements);
}

#[test]
fn sync_installs_become_versions() {
    let (_d, mut l, nb, page) = setup();
    let vs = write(&mut l, nb, page, 3);
    let merged_nb = l.notebook(nb).unwrap().clone();
    let mut merged = l.load_page(nb, page).unwrap();
    merged.elements.push(stroke(500, 10));
    let want = merged.elements.clone();
    l.install_merged(merged_nb.clone(), vec![merged]).unwrap();
    assert_eq!(l.load_page(nb, page).unwrap().elements, want);
    let pts = l.page_history(nb, page).unwrap();
    assert_eq!(pts.len(), 5);
    // back to before the remote change, then forwards again
    l.restore_page_version(nb, page, vs[2].revision).unwrap();
    assert_eq!(l.load_page(nb, page).unwrap().elements, vs[2].elements);
    let rev = pts.last().unwrap().revision;
    l.restore_page_version(nb, page, rev).unwrap();
    assert_eq!(l.load_page(nb, page).unwrap().elements, want);
    // an install that changes nothing adds nothing
    let same = l.load_page(nb, page).unwrap();
    let n = l.page_history(nb, page).unwrap().len();
    l.install_merged(merged_nb, vec![same]).unwrap();
    assert_eq!(l.page_history(nb, page).unwrap().len(), n);
}

/// `cargo test -p penroam-storage --release --test history -- --ignored --nocapture`
#[test]
#[ignore = "measurement, run by hand"]
fn measure_history_size_and_restore_time() {
    for strokes in [1_000usize, 10_000] {
        let (d, mut l, nb, page) = setup();
        for i in 0..strokes {
            // one eighth of edits erase an earlier stroke, as real notes do
            let kind = if i % 8 == 7 {
                let p = l.load_page(nb, page).unwrap();
                OpKind::RemoveElements {
                    ids: vec![p.elements[i / 16].id()],
                }
            } else {
                OpKind::AddElements {
                    elements: vec![stroke(i as i64 + 1, 60)],
                }
            };
            l.apply_page_operations(nb, page, &batch(i as u64, kind))
                .unwrap();
        }
        l.flush_all().unwrap();
        let hist = l.history_stats(nb).unwrap().bytes;
        let snap = fs::metadata(
            d.path()
                .join(format!("lib/notebooks/{nb}/pages/{page}.prp")),
        )
        .unwrap()
        .len();
        let points = l.page_history(nb, page).unwrap();
        let t = Instant::now();
        l.page_version(nb, page, points[points.len() / 2].revision)
            .unwrap();
        let mid = t.elapsed();
        let t = Instant::now();
        l.page_version(nb, page, points.last().unwrap().revision)
            .unwrap();
        let last = t.elapsed();
        let t = Instant::now();
        l.restore_page_version(nb, page, points[1].revision)
            .unwrap();
        let rollback = t.elapsed();
        println!(
            "{strokes} edits: history {:.1} MiB ({:.2}x snapshot of {:.1} MiB), {} versions, read mid {mid:?}, read newest {last:?}, restore oldest {rollback:?}",
            hist as f64 / 1048576.0,
            hist as f64 / snap as f64,
            snap as f64 / 1048576.0,
            points.len(),
        );
    }
}
