//! Export, backup and restore: the scenarios a user depends on when a device
//! is lost or a file is damaged. Format rules: `docs/formats/portability.md`.

use penroam_core::*;
use penroam_storage::*;
use std::io::{Cursor, Read, Write};
use tempfile::TempDir;
use uuid::Uuid;

fn stroke(z: i64, x: f64) -> Element {
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
        points: (0..32)
            .map(|i| InkPoint {
                x: x + i as f64,
                y: 10.0 + (i as f64 * 0.7).sin(),
                pressure: 0.25 + i as f64 / 100.0,
                tilt_x: 12.0,
                tilt_y: -4.0,
                time_us: i * 1000,
                twist: Some(30.0),
                flags: Some(point_flags::COALESCED),
            })
            .collect(),
        extra: Default::default(),
    })
}

fn add(base: u64, elements: Vec<Element>) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: base,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind: OpKind::AddElements { elements },
        }],
    }
}

fn lib() -> (TempDir, Library) {
    let d = TempDir::new().unwrap();
    let l = Library::open(d.path().join("lib")).unwrap();
    (d, l)
}

/// A notebook with two live pages holding ink, one trashed page and an asset.
fn notebook(lib: &mut Library, title: &str) -> (Uuid, Vec<Uuid>) {
    let (nb, first) = lib
        .create_notebook(title, PageDefaults::default(), None)
        .unwrap();
    let second = lib.create_page(nb.id, None, None, None).unwrap().1;
    let gone = lib.create_page(nb.id, None, None, None).unwrap().1;
    let pdf = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/pdf/mixed-sizes-rotations.pdf"
    ))
    .unwrap();
    let asset = lib
        .add_asset(nb.id, &pdf, "application/pdf", Some("source.pdf".into()))
        .unwrap();
    for (i, p) in [first.id, second.id, gone.id].iter().enumerate() {
        let mut els = vec![stroke(1, i as f64 * 20.0)];
        if i == 0 {
            els.push(Element::Image(ImageElement {
                id: Uuid::new_v4(),
                z: 2,
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                asset_hash: asset.sha256.clone(),
                extra: Default::default(),
            }));
        }
        lib.apply_page_operations(nb.id, *p, &add(0, els)).unwrap();
    }
    lib.delete_page(nb.id, gone.id).unwrap();
    (nb.id, vec![first.id, second.id, gone.id])
}

fn backup(lib: &mut Library) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    lib.export_backup(&mut out, "test").unwrap();
    out.into_inner()
}

fn pages_of(lib: &mut Library, id: Uuid) -> Vec<Page> {
    let nb = lib.notebook(id).unwrap().clone();
    let ids: Vec<Uuid> = nb.all_page_ids().collect();
    ids.iter().map(|p| lib.load_page(id, *p).unwrap()).collect()
}

/// Rewrites a zip, passing every entry's bytes through `f`.
fn rewrite(bytes: &[u8], f: impl Fn(&str, Vec<u8>) -> Vec<u8>) -> Vec<u8> {
    let mut zin = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..zin.len() {
        let mut e = zin.by_index(i).unwrap();
        let name = e.name().unwrap().to_string();
        let mut data = Vec::new();
        e.read_to_end(&mut data).unwrap();
        out.start_file(
            &name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        out.write_all(&f(&name, data)).unwrap();
    }
    out.finish().unwrap().into_inner()
}

#[test]
fn identical_notebooks_export_identical_bytes() {
    let (_d, mut l) = lib();
    let (id, _) = notebook(&mut l, "Same");
    let opts = ExportOptions {
        deterministic: true,
        ..Default::default()
    };
    let mut a = Cursor::new(Vec::new());
    let mut b = Cursor::new(Vec::new());
    l.export_archive_with(id, &mut a, "test", &opts).unwrap();
    l.export_archive_with(id, &mut b, "test", &opts).unwrap();
    assert_eq!(a.into_inner(), b.into_inner());
}

#[test]
fn selected_pages_export_on_their_own() {
    let (_d, mut l) = lib();
    let (id, pages) = notebook(&mut l, "Selected");
    let mut out = Cursor::new(Vec::new());
    let opts = ExportOptions {
        pages: Some(vec![pages[1]]),
        ..Default::default()
    };
    l.export_archive_with(id, &mut out, "test", &opts).unwrap();
    let (_d2, mut other) = self::lib();
    let r = other.import_archive(Cursor::new(out.into_inner())).unwrap();
    assert_eq!(r.notebook.page_order, vec![pages[1]]);
    assert!(r.notebook.order_is_consistent());
    // a trashed page is not a page you can select
    let opts = ExportOptions {
        pages: Some(vec![pages[2]]),
        ..Default::default()
    };
    let mut out = Cursor::new(Vec::new());
    assert!(matches!(
        l.export_archive_with(id, &mut out, "test", &opts),
        Err(StorageError::NotFound(_))
    ));
}

#[test]
fn losing_every_device_is_recovered_from_one_backup_file() {
    let (_d, mut l) = lib();
    let (a, _) = notebook(&mut l, "Alpha");
    let (b, _) = notebook(&mut l, "Beta");
    l.delete_notebook(b).unwrap();
    let want_a = pages_of(&mut l, a);
    let want_b = pages_of(&mut l, b);
    let want_nb = l.notebook(a).unwrap().clone();
    let file = backup(&mut l);
    drop(l);
    drop(_d);

    let (_d2, mut fresh) = lib();
    let preview = fresh.preview_backup(Cursor::new(&file)).unwrap();
    assert_eq!(preview.notebooks.len(), 2);
    assert!(preview.damaged.is_empty());
    assert!(preview.notebooks.iter().all(|n| !n.already_in_library));
    assert_eq!(fresh.list_notebooks().len(), 0, "a preview changes nothing");

    let r = fresh
        .restore_backup(Cursor::new(&file), &RestoreOptions::default())
        .unwrap();
    assert_eq!(r.restored.len(), 2);
    assert!(r.failed.is_empty() && r.skipped.is_empty());
    assert_eq!(pages_of(&mut fresh, a), want_a, "ink, raw samples, trash");
    assert_eq!(pages_of(&mut fresh, b), want_b);
    let nb = fresh.notebook(a).unwrap();
    assert_eq!(nb.assets, want_nb.assets);
    assert_eq!(nb.trashed_pages, want_nb.trashed_pages);
    assert_eq!(nb.order_keys, want_nb.order_keys);
    let hash = &nb.assets[0].sha256.clone();
    assert!(!fresh.read_asset(a, hash).unwrap().is_empty());
    assert!(fresh.notebook(b).unwrap().deleted_at.is_some());
    // a deleted page comes back from the restored trash
    let trashed = fresh.notebook(a).unwrap().trashed_pages[0].id;
    let nb = fresh.restore_page(a, trashed).unwrap();
    assert!(nb.page_order.contains(&trashed));
    // survives a restart
    drop(fresh);
    let mut again = Library::open(_d2.path().join("lib")).unwrap();
    assert_eq!(pages_of(&mut again, b), want_b);
}

#[test]
fn restore_never_overwrites_by_default() {
    let (_d, mut l) = lib();
    let (id, pages) = notebook(&mut l, "Mine");
    let file = backup(&mut l);
    // newer work than the backup
    let rev = l.load_page(id, pages[0]).unwrap().revision;
    l.apply_page_operations(id, pages[0], &add(rev, vec![stroke(2, 5.0)]))
        .unwrap();
    let want = pages_of(&mut l, id);

    let p = l.preview_backup(Cursor::new(&file)).unwrap();
    assert_eq!(
        p.notebooks[0].title, "Mine",
        "the file's title, not a copy's"
    );
    assert!(p.notebooks[0].already_in_library);
    let r = l
        .restore_backup(Cursor::new(&file), &RestoreOptions::default())
        .unwrap();
    assert_eq!(r.skipped, vec!["Mine".to_string()]);
    assert!(r.restored.is_empty());
    assert_eq!(pages_of(&mut l, id), want);

    let r = l
        .restore_backup(
            Cursor::new(&file),
            &RestoreOptions {
                on_conflict: ConflictPolicy::Copy,
                only: None,
            },
        )
        .unwrap();
    assert_eq!(r.copied, 1);
    assert_ne!(r.restored[0], id);
    assert_eq!(pages_of(&mut l, id), want);
    assert_eq!(l.list_notebooks().len(), 2);
}

#[test]
fn one_damaged_notebook_does_not_stop_the_rest() {
    let (_d, mut l) = lib();
    let (good, _) = notebook(&mut l, "Good");
    let (bad, _) = notebook(&mut l, "Bad");
    let file = backup(&mut l);
    let target = format!("notebooks/{bad}.penroam");
    let broken = rewrite(&file, |name, mut data| {
        if name == target {
            let mid = data.len() / 2;
            data[mid] ^= 0xff;
        }
        data
    });
    let (_d2, mut fresh) = lib();
    let p = fresh.preview_backup(Cursor::new(&broken)).unwrap();
    assert_eq!(p.notebooks.len(), 1);
    assert_eq!(p.damaged[0].id, bad);
    let r = fresh
        .restore_backup(Cursor::new(&broken), &RestoreOptions::default())
        .unwrap();
    assert_eq!(r.restored, vec![good]);
    assert_eq!(r.failed.len(), 1);
    assert!(r.failed[0].error.contains("checksum"));
    assert_eq!(fresh.list_notebooks().len(), 1);
    assert_eq!(
        std::fs::read_dir(fresh.staging_dir()).unwrap().count(),
        0,
        "nothing left behind in staging"
    );
}

#[test]
fn truncated_and_foreign_backups_are_refused_whole() {
    let (_d, mut l) = lib();
    notebook(&mut l, "Kept");
    let file = backup(&mut l);
    let before = l.list_notebooks().len();
    assert!(l
        .restore_backup(
            Cursor::new(&file[..file.len() / 2]),
            &RestoreOptions::default()
        )
        .is_err());
    assert!(l.preview_backup(Cursor::new(b"not a zip")).is_err());

    // a notebook archive is not a backup
    let id = l.notebook_ids()[0];
    let mut one = Cursor::new(Vec::new());
    l.export_archive(id, &mut one, "test").unwrap();
    assert!(l.preview_backup(Cursor::new(one.into_inner())).is_err());

    let newer = rewrite(&file, |name, data| {
        if name == "backup.json" {
            let mut v: serde_json::Value = serde_json::from_slice(&data).unwrap();
            v["formatVersion"] = 99.into();
            serde_json::to_vec(&v).unwrap()
        } else {
            data
        }
    });
    let e = l.preview_backup(Cursor::new(newer)).unwrap_err();
    assert!(e.to_string().contains("newer"));
    assert_eq!(l.list_notebooks().len(), before);
}

#[test]
fn backup_paths_cannot_escape() {
    let (_d, mut l) = lib();
    let id = notebook(&mut l, "Escape").0;
    let file = backup(&mut l);
    let evil = rewrite(&file, |name, data| {
        if name == "backup.json" {
            let s = String::from_utf8(data).unwrap();
            s.replace(
                &format!("notebooks/{id}.penroam"),
                "../../../../tmp/evil.penroam",
            )
            .into_bytes()
        } else {
            data
        }
    });
    assert!(l.preview_backup(Cursor::new(evil)).is_err());
}

#[test]
fn an_unreadable_notebook_is_named_in_the_backup() {
    let (d, mut l) = lib();
    let (ok, _) = notebook(&mut l, "Fine");
    let (bad, pages) = notebook(&mut l, "Broken");
    l.flush_all().unwrap();
    drop(l);
    // damage the page and its fallback
    let dir = d
        .path()
        .join("lib/notebooks")
        .join(bad.to_string())
        .join("pages");
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.file_name()
            .to_string_lossy()
            .starts_with(&pages[0].to_string())
        {
            std::fs::write(e.path(), b"garbage").unwrap();
        }
    }
    let mut l = Library::open(d.path().join("lib")).unwrap();
    let mut out = Cursor::new(Vec::new());
    let m = l.export_backup(&mut out, "test").unwrap();
    assert_eq!(
        m.notebooks.iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![ok]
    );
    assert_eq!(m.unreadable[0].id, bad);
    let p = l.preview_backup(Cursor::new(out.into_inner())).unwrap();
    assert_eq!(p.unreadable[0].title, "Broken");
}

#[test]
fn archives_with_trash_import_as_plain_notebooks_by_default() {
    let (_d, mut l) = lib();
    let (id, pages) = notebook(&mut l, "Trash");
    let mut out = Cursor::new(Vec::new());
    let opts = ExportOptions {
        include_trash: true,
        ..Default::default()
    };
    l.export_archive_with(id, &mut out, "test", &opts).unwrap();
    let (_d2, mut other) = lib();
    let r = other.import_archive(Cursor::new(out.into_inner())).unwrap();
    assert!(r.notebook.trashed_pages.is_empty());
    assert_eq!(r.notebook.page_order, pages[..2]);
    assert!(r.notebook.order_is_consistent());
    assert!(!r.warnings.is_empty(), "the dropped trash is reported");
}

#[test]
fn the_oldest_supported_archive_still_restores() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/linux-export.penroam"
    ))
    .unwrap();
    let (_d, l) = lib();
    let s = l.verify_archive(Cursor::new(bytes)).unwrap();
    assert!(s.pages > 0);
    assert!(s.format_version <= archive::FORMAT_VERSION);
}

#[test]
fn mutated_backups_never_panic_or_harm_the_library() {
    let (_d, mut l) = lib();
    notebook(&mut l, "Fuzz");
    let valid = backup(&mut l);
    let (_d2, mut target) = lib();
    let mut seed = 0xdead_beef_cafe_f00du64;
    for round in 0..300 {
        let mut b = valid.clone();
        for _ in 0..1 + round % 6 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let i = seed as usize % b.len();
            b[i] = (seed >> 40) as u8;
        }
        let _ = target.preview_backup(Cursor::new(&b));
        let opts = RestoreOptions {
            on_conflict: ConflictPolicy::Copy,
            only: None,
        };
        let _ = target.restore_backup(Cursor::new(&b), &opts);
    }
    // whatever got through was fully valid
    for s in target.list_notebooks() {
        pages_of(&mut target, s.id);
    }
    assert_eq!(std::fs::read_dir(target.staging_dir()).unwrap().count(), 0);
}

fn imported_page(elements: Vec<Element>) -> NewPage {
    NewPage {
        paper: Paper::Custom,
        size: Size {
            width: 300.0,
            height: 400.0,
        },
        template: Template::default(),
        background: None,
        elements,
    }
}

#[test]
fn imported_content_becomes_an_ordinary_notebook() {
    let (d, mut l) = lib();
    let (existing, _) = notebook(&mut l, "Untouched");
    let want = pages_of(&mut l, existing);
    let seq = l.last_seq();
    let asset = NewAsset {
        bytes: b"\x89PNG not really".to_vec(),
        media_type: "image/png".into(),
        name: None,
    };
    let nb = l
        .create_notebook_with(
            "From Xournal",
            vec![asset],
            vec![imported_page(vec![stroke(1, 0.0)]), imported_page(vec![])],
        )
        .unwrap();
    assert_eq!(nb.page_order.len(), 2);
    assert_eq!(nb.assets.len(), 1);
    assert!(nb.order_is_consistent());
    assert!(l.last_seq() > seq, "journaled, so it syncs");
    let first = l.load_page(nb.id, nb.page_order[0]).unwrap();
    assert_eq!(first.size.width, 300.0);
    assert_eq!(first.elements.len(), 1);
    assert_eq!(pages_of(&mut l, existing), want);

    // it survives a restart, and exports like anything else
    l.flush_all().unwrap();
    drop(l);
    let mut l = Library::open(d.path().join("lib")).unwrap();
    assert_eq!(l.load_page(nb.id, nb.page_order[0]).unwrap(), first);
    let mut out = Cursor::new(Vec::new());
    l.export_archive(nb.id, &mut out, "test").unwrap();
}

#[test]
fn a_failed_import_leaves_nothing_behind() {
    let (d, mut l) = lib();
    let (existing, _) = notebook(&mut l, "Untouched");
    let bad = Element::Image(ImageElement {
        id: Uuid::new_v4(),
        z: 1,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        asset_hash: "a".repeat(64),
        extra: Default::default(),
    });
    let r = l.create_notebook_with(
        "Half",
        vec![],
        vec![
            imported_page(vec![stroke(1, 0.0)]),
            imported_page(vec![bad]),
        ],
    );
    assert!(r.is_err());
    let live: Vec<_> = l.list_notebooks().into_iter().map(|n| n.id).collect();
    assert_eq!(live, vec![existing]);
    assert_eq!(
        std::fs::read_dir(d.path().join("lib/notebooks"))
            .unwrap()
            .count(),
        1
    );
    assert!(l.create_notebook_with("Empty", vec![], vec![]).is_err());
}

#[test]
fn erased_content_is_not_in_exports_or_backups() {
    let (_d, mut l) = lib();
    let (nb, p) = l
        .create_notebook("Clean", PageDefaults::default(), None)
        .unwrap();
    let mut secret = stroke(1, 0.0);
    if let Element::Ink(s) = &mut secret {
        s.color = "#abcdef".into();
    }
    let id = secret.id();
    l.apply_page_operations(nb.id, p.id, &add(0, vec![secret, stroke(2, 40.0)]))
        .unwrap();
    let erase = OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: 1,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind: OpKind::RemoveElements { ids: vec![id] },
        }],
    };
    l.apply_page_operations(nb.id, p.id, &erase).unwrap();

    let needles: [&[u8]; 2] = [b"abcdef", id.as_bytes()];
    let has = |bytes: &[u8], needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    // history does hold it, so the check below means something
    let hist = std::fs::read(
        l.root()
            .join(format!("notebooks/{}/history/{}.phl", nb.id, p.id)),
    )
    .unwrap();
    assert!(has(&hist, b"abcdef"));

    let mut archive = Cursor::new(Vec::new());
    l.export_archive(nb.id, &mut archive, "test").unwrap();
    let raw = unzip_all(&archive.into_inner());
    let file = backup(&mut l);
    let mut nested = unzip_all(&file);
    nested.extend(unzip_all_nested(&file));
    for n in needles {
        assert!(!has(&raw, n), "not in a notebook export");
        assert!(!has(&nested, n), "not in a backup");
    }
}

/// Every entry of a zip, inflated and joined, so searches see the real bytes.
fn unzip_all(bytes: &[u8]) -> Vec<u8> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = Vec::new();
    for i in 0..z.len() {
        z.by_index(i).unwrap().read_to_end(&mut out).unwrap();
    }
    out
}

/// The same, one level down, for the archives inside a backup.
fn unzip_all_nested(bytes: &[u8]) -> Vec<u8> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = Vec::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        if e.name().unwrap().ends_with(".penroam") {
            let mut inner = Vec::new();
            e.read_to_end(&mut inner).unwrap();
            out.extend(unzip_all(&inner));
        }
    }
    out
}
