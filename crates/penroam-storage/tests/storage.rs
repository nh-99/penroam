use penroam_core::migrate::stroke_doc_to_elements;
use penroam_core::*;
use penroam_storage::*;
use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use uuid::Uuid;

fn fixture_elements() -> Vec<Element> {
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample-strokes.json");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap();
    stroke_doc_to_elements(&v, 1).unwrap()
}

fn text(z: i64, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::new_v4(),
        z,
        x: 100.5,
        y: 200.25,
        width: 180.0,
        height: 40.0,
        text: s.into(),
        font_size: 14.0,
        color: "#1d4ed8".into(),
        align: TextAlign::Left,
        row_pitch: None,
        extra: Default::default(),
    })
}

fn add(base: u64, els: Vec<Element>) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: base,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind: OpKind::AddElements { elements: els },
        }],
    }
}

fn remove(base: u64, ids: Vec<Uuid>) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: base,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind: OpKind::RemoveElements { ids },
        }],
    }
}

fn new_lib() -> (tempfile::TempDir, Library) {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::open(d.path().join("library")).unwrap();
    (d, lib)
}

#[test]
fn notebook_and_page_lifecycle() {
    let (d, mut lib) = new_lib();
    let (nb, p1) = lib
        .create_notebook("Physics", PageDefaults::default(), None)
        .unwrap();
    assert_eq!(nb.page_order, vec![p1.id]);
    let (_, p2) = lib
        .create_page(
            nb.id,
            None,
            Some(Template {
                kind: TemplateKind::Grid,
                spacing: 18.0,
            }),
            Some(Paper::A4),
        )
        .unwrap();
    let (_, p0) = lib.create_page(nb.id, Some(0), None, None).unwrap();
    assert_eq!(
        lib.notebook(nb.id).unwrap().page_order,
        vec![p0.id, p1.id, p2.id]
    );
    assert_eq!(
        lib.load_page(nb.id, p2.id).unwrap().size,
        Paper::A4.size().unwrap()
    );

    lib.update_page_order(nb.id, vec![p2.id, p0.id, p1.id])
        .unwrap();
    assert!(
        lib.update_page_order(nb.id, vec![p2.id, p0.id]).is_err(),
        "order must be a permutation"
    );

    lib.rename_notebook(nb.id, "Physics II").unwrap();
    assert!(lib.rename_notebook(nb.id, "   ").is_err());

    // page trash/restore keeps position
    lib.delete_page(nb.id, p0.id).unwrap();
    assert_eq!(lib.notebook(nb.id).unwrap().page_order, vec![p2.id, p1.id]);
    lib.restore_page(nb.id, p0.id).unwrap();
    assert_eq!(
        lib.notebook(nb.id).unwrap().page_order,
        vec![p2.id, p0.id, p1.id]
    );

    // notebook trash/restore/purge
    lib.delete_notebook(nb.id).unwrap();
    assert!(
        lib.create_page(nb.id, None, None, None).is_err(),
        "trashed notebooks are read-only"
    );
    assert!(lib.list_notebooks()[0].deleted_at.is_some());
    lib.restore_notebook(nb.id).unwrap();
    assert!(lib.purge_notebook(nb.id).is_err(), "must be trashed first");

    // persisted ordering survives reopen
    drop(lib);
    let mut lib = Library::open(d.path().join("library")).unwrap();
    let nb2 = lib.notebook(nb.id).unwrap().clone();
    assert_eq!(nb2.title, "Physics II");
    assert_eq!(nb2.page_order, vec![p2.id, p0.id, p1.id]);
    assert_eq!(
        lib.load_page(nb.id, p2.id).unwrap().template.kind,
        TemplateKind::Grid
    );
    lib.delete_notebook(nb.id).unwrap();
    lib.purge_notebook(nb.id).unwrap();
    assert!(lib.list_notebooks().is_empty());
}

#[test]
fn acknowledged_batches_survive_a_crash_without_flush() {
    let (d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Crash", PageDefaults::default(), None)
        .unwrap();
    let els = fixture_elements();
    let t = text(100, "typed\ntext");
    let o1 = lib
        .apply_page_operations(nb.id, page.id, &add(0, els.clone()))
        .unwrap();
    assert!(o1.durable);
    let o2 = lib
        .apply_page_operations(nb.id, page.id, &add(1, vec![t.clone()]))
        .unwrap();
    lib.apply_page_operations(nb.id, page.id, &remove(2, vec![els[0].id()]))
        .unwrap();
    assert_eq!(o2.revision, 2);
    let expected = lib.load_page(nb.id, page.id).unwrap();
    lib.crash_for_tests(); // no flush, no compaction

    let mut lib = Library::open(d.path().join("library")).unwrap();
    let got = lib.load_page(nb.id, page.id).unwrap();
    assert_eq!(got, expected);
    assert_eq!(got.revision, 3);
    assert_eq!(lib.recovery.replayed_batches, 3);
}

fn journal_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root.join("library/journal"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "pjl"))
        .collect();
    v.sort();
    v
}

#[test]
fn torn_journal_tail_recovers_previous_valid_state() {
    let (d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Torn", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(nb.id, page.id, &add(0, vec![text(1, "kept")]))
        .unwrap();
    let good = lib.load_page(nb.id, page.id).unwrap();
    lib.crash_for_tests();
    let journal = journal_files(d.path()).pop().unwrap();
    let mut f = fs::OpenOptions::new().append(true).open(&journal).unwrap();
    // crash mid-append: record header promising 4000 bytes, 9 arrived
    f.write_all(b"PRJR\xa0\x0f\x00\x00\x01\x02\x03\x04partial..")
        .unwrap();
    drop(f);
    let len_before = fs::metadata(&journal).unwrap().len();

    let mut lib = Library::open(d.path().join("library")).unwrap();
    assert_eq!(lib.load_page(nb.id, page.id).unwrap(), good);
    assert_eq!(lib.recovery.truncated_journals, 1);
    assert_eq!(lib.recovery.corrupt_journal_records, 0);
    assert!(fs::metadata(&journal).unwrap().len() < len_before);
    // and the page keeps working afterwards
    lib.apply_page_operations(nb.id, page.id, &add(1, vec![text(2, "after")]))
        .unwrap();
}

#[test]
fn crash_between_snapshot_and_manifest_is_harmless() {
    let (d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Compact", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(nb.id, page.id, &add(0, vec![text(1, "a")]))
        .unwrap();
    lib.apply_page_operations(nb.id, page.id, &add(1, vec![text(2, "b")]))
        .unwrap();
    let manifest = d.path().join("library/checkpoint.json");
    let saved = fs::read(&manifest).ok();
    lib.flush_all().unwrap(); // snapshot at rev 2, manifest advanced
    let expected = lib.load_page(nb.id, page.id).unwrap();
    drop(lib);
    // pretend the manifest write never happened: the journal is replayed
    // over a snapshot that already contains it
    match saved {
        Some(b) => fs::write(&manifest, b).unwrap(),
        None => fs::remove_file(&manifest).unwrap(),
    }
    let mut lib = Library::open(d.path().join("library")).unwrap();
    assert_eq!(lib.load_page(nb.id, page.id).unwrap(), expected);
    assert!(
        lib.recovery.warnings.is_empty(),
        "{:?}",
        lib.recovery.warnings
    );
}

#[test]
fn temp_files_and_orphans_are_cleaned_up() {
    let (d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Orphans", PageDefaults::default(), None)
        .unwrap();
    drop(lib);
    let dir = d.path().join(format!("library/notebooks/{}", nb.id));
    fs::write(dir.join("notebook.json.tmp-deadbeef"), b"partial").unwrap();
    let orphan = Uuid::new_v4();
    fs::write(dir.join(format!("pages/{orphan}.json")), b"{}").unwrap();
    let mut lib = Library::open(d.path().join("library")).unwrap();
    assert_eq!(lib.recovery.removed_temp_files, 1);
    assert_eq!(lib.recovery.orphaned_files, 1);
    assert!(
        dir.join(format!("orphans/{orphan}.json")).exists(),
        "orphans are moved, never deleted"
    );
    assert!(lib.load_page(nb.id, page.id).is_ok());
}

#[test]
fn corrupt_notebook_is_reported_and_left_untouched() {
    let (d, lib) = new_lib();
    drop(lib);
    let id = Uuid::new_v4();
    let dir = d.path().join(format!("library/notebooks/{id}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("notebook.json"), b"{ not json").unwrap();
    let lib = Library::open(d.path().join("library")).unwrap();
    assert!(lib.list_notebooks().is_empty());
    assert_eq!(lib.recovery.warnings.len(), 1);
    assert_eq!(fs::read(dir.join("notebook.json")).unwrap(), b"{ not json");
}

#[test]
fn many_batches_trigger_compaction_and_stay_consistent() {
    let (d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Big", PageDefaults::default(), None)
        .unwrap();
    let base = fixture_elements();
    let mut rev = 0;
    for i in 0..600 {
        let mut e = base[i % base.len()].clone();
        if let Element::Ink(s) = &mut e {
            s.id = Uuid::new_v4();
            s.z = i as i64;
        }
        rev = lib
            .apply_page_operations(nb.id, page.id, &add(rev, vec![e]))
            .unwrap()
            .revision;
    }
    // 512 entries triggered a checkpoint; a final one releases the journal
    let m: serde_json::Value =
        serde_json::from_slice(&fs::read(d.path().join("library/checkpoint.json")).unwrap())
            .unwrap();
    assert!(m["floor"].as_u64().unwrap() >= 512, "{m}");
    lib.flush_all().unwrap();
    assert!(lib.journal_segments() <= 1, "old segments deleted");
    let expected = lib.load_page(nb.id, page.id).unwrap();
    lib.crash_for_tests();
    let mut lib = Library::open(d.path().join("library")).unwrap();
    assert_eq!(lib.load_page(nb.id, page.id).unwrap(), expected);
}

fn sample_pdf_hash_asset(lib: &mut Library, nb: Uuid) -> Asset {
    lib.add_asset(
        nb,
        b"%PDF-1.4 fake bytes for storage tests",
        "application/pdf",
        Some("x.pdf".into()),
    )
    .unwrap()
}

#[test]
fn archive_round_trip_is_lossless() {
    let (_d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook(
            "Portable",
            PageDefaults {
                paper: Paper::A4,
                template: Template {
                    kind: TemplateKind::Dotted,
                    spacing: 20.0,
                },
            },
            None,
        )
        .unwrap();
    let mut els = fixture_elements();
    els.push(text(50, "Multiline\ntyped text ✓"));
    lib.apply_page_operations(nb.id, page.id, &add(0, els))
        .unwrap();
    let asset = sample_pdf_hash_asset(&mut lib, nb.id);
    let bg = PdfBackgroundRef {
        asset_hash: asset.sha256.clone(),
        page_index: 0,
        media_box: [0.0, 0.0, 300.0, 400.0],
        crop_box: [0.0, 0.0, 300.0, 400.0],
        rotation: 90,
    };
    let (_, pdf_pages) = lib
        .create_pages(
            nb.id,
            None,
            vec![(
                Paper::Pdf,
                bg.display_size(),
                Template {
                    kind: TemplateKind::Blank,
                    spacing: 24.0,
                },
                Some(bg),
            )],
        )
        .unwrap();
    let (_, trashed) = lib.create_page(nb.id, None, None, None).unwrap();
    lib.delete_page(nb.id, trashed.id).unwrap();

    let mut buf = Cursor::new(Vec::new());
    let manifest = lib.export_archive(nb.id, &mut buf, "test").unwrap();
    assert_eq!(manifest.pages.len(), 2);

    // import into a *different* library (stands in for another device)
    let (_d2, mut other) = new_lib();
    buf.set_position(0);
    let rep = other.import_archive(&mut buf).unwrap();
    assert!(!rep.renamed_copy);
    let nb2 = rep.notebook;
    assert_eq!(nb2.id, nb.id);
    assert_eq!(nb2.page_order, vec![page.id, pdf_pages[0].id]);
    assert!(nb2.trashed_pages.is_empty());
    for pid in &nb2.page_order {
        assert_eq!(
            other.load_page(nb.id, *pid).unwrap(),
            lib.load_page(nb.id, *pid).unwrap()
        );
    }
    assert_eq!(
        other.read_asset(nb.id, &asset.sha256).unwrap(),
        lib.read_asset(nb.id, &asset.sha256).unwrap()
    );

    // importing the same archive again makes a copy instead of overwriting
    buf.set_position(0);
    let again = other.import_archive(&mut buf).unwrap();
    assert!(again.renamed_copy);
    assert_ne!(again.notebook.id, nb.id);
    assert_eq!(other.list_notebooks().len(), 2);
}

fn zip_with(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (n, b) in entries {
        z.start_file(*n, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b).unwrap();
    }
    let mut c = z.finish().unwrap();
    c.set_position(0);
    c
}

fn exported() -> (Vec<(String, Vec<u8>)>, Uuid) {
    let (_d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Victim", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(nb.id, page.id, &add(0, vec![text(1, "x")]))
        .unwrap();
    let mut buf = Cursor::new(Vec::new());
    lib.export_archive(nb.id, &mut buf, "test").unwrap();
    buf.set_position(0);
    let mut z = zip::ZipArchive::new(buf).unwrap();
    let mut out = Vec::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut b).unwrap();
        out.push((f.name().unwrap().to_string(), b));
    }
    (out, page.id)
}

fn rebuild(entries: &[(String, Vec<u8>)]) -> Cursor<Vec<u8>> {
    let v: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    zip_with(&v)
}

#[test]
fn hostile_archives_are_rejected() {
    let (entries, page_id) = exported();
    let (_d, mut lib) = new_lib();
    assert!(
        lib.import_archive(rebuild(&entries)).is_ok(),
        "control: untouched export imports"
    );

    let check = |lib: &mut Library, c: Cursor<Vec<u8>>, what: &str| {
        let r = lib.import_archive(c);
        assert!(r.is_err(), "{what} should be rejected");
        r.err().unwrap()
    };
    let (_d, mut lib) = new_lib();

    // path traversal
    let mut e = entries.clone();
    e.push(("../../evil.sh".into(), b"rm -rf /".to_vec()));
    check(&mut lib, rebuild(&e), "traversal");
    let mut e = entries.clone();
    e.push(("/etc/passwd".into(), b"x".to_vec()));
    check(&mut lib, rebuild(&e), "absolute path");
    let mut e = entries.clone();
    e.push(("pages\\..\\x".into(), b"x".to_vec()));
    check(&mut lib, rebuild(&e), "backslash");

    // decompression bomb (highly compressible entry)
    let mut e = entries.clone();
    e.push((format!("assets/{}", "a".repeat(64)), vec![0u8; 64 << 20]));
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (n, b) in &e {
        z.start_file(
            n.as_str(),
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        z.write_all(b).unwrap();
    }
    let mut c = z.finish().unwrap();
    c.set_position(0);
    let err = check(&mut lib, c, "bomb");
    assert!(err.to_string().contains("compression ratio"), "{err}");

    // tampered page bytes -> checksum mismatch
    let mut e = entries.clone();
    let i = e.iter().position(|(n, _)| n.starts_with("pages/")).unwrap();
    let mid = e[i].1.len() / 2;
    e[i].1[mid] ^= 0x01;
    check(&mut lib, rebuild(&e), "checksum");

    // missing page
    let e: Vec<_> = entries
        .iter()
        .filter(|(n, _)| !n.starts_with("pages/"))
        .cloned()
        .collect();
    check(&mut lib, rebuild(&e), "missing page");

    // newer format version
    let mut e = entries.clone();
    let i = e.iter().position(|(n, _)| n == "manifest.json").unwrap();
    let mut m: serde_json::Value = serde_json::from_slice(&e[i].1).unwrap();
    m["formatVersion"] = 99.into();
    e[i].1 = serde_json::to_vec(&m).unwrap();
    let err = check(&mut lib, rebuild(&e), "future version");
    assert!(err.to_string().contains("newer"));

    // page references an asset that is not in the archive
    let mut e = entries.clone();
    let pi = e.iter().position(|(n, _)| n.starts_with("pages/")).unwrap();
    let mut p = penroam_core::binary::decode_page(&e[pi].1).unwrap();
    p.background = Some(PdfBackgroundRef {
        asset_hash: "b".repeat(64),
        page_index: 0,
        media_box: [0.0, 0.0, 1.0, 1.0],
        crop_box: [0.0, 0.0, 1.0, 1.0],
        rotation: 0,
    });
    e[pi].1 = penroam_core::binary::encode_page(&p);
    let mi = e.iter().position(|(n, _)| n == "manifest.json").unwrap();
    let mut m: serde_json::Value = serde_json::from_slice(&e[mi].1).unwrap();
    m["pages"][0]["sha256"] = sha256_hex(&e[pi].1).into();
    e[mi].1 = serde_json::to_vec(&m).unwrap();
    let err = check(&mut lib, rebuild(&e), "broken reference");
    assert!(err.to_string().contains("missing asset"), "{err}");

    // not a zip at all
    check(
        &mut lib,
        Cursor::new(b"PK\x03\x04garbage".to_vec()),
        "garbage",
    );
    // failures leave nothing behind
    assert!(lib.list_notebooks().is_empty());
    assert_eq!(fs::read_dir(lib.staging_dir()).unwrap().count(), 0);
    let _ = page_id;
}

#[test]
fn assets_are_content_addressed_and_verified() {
    let (d, mut lib) = new_lib();
    let (nb, _) = lib
        .create_notebook("Assets", PageDefaults::default(), None)
        .unwrap();
    let a = sample_pdf_hash_asset(&mut lib, nb.id);
    let b = sample_pdf_hash_asset(&mut lib, nb.id);
    assert_eq!(a, b);
    assert_eq!(lib.notebook(nb.id).unwrap().assets.len(), 1);
    let path = d
        .path()
        .join(format!("library/notebooks/{}/assets/{}", nb.id, a.sha256));
    fs::write(&path, b"tampered").unwrap();
    assert!(matches!(
        lib.read_asset(nb.id, &a.sha256),
        Err(StorageError::Corrupt(_))
    ));
}

#[test]
fn stale_revision_is_a_conflict_not_an_overwrite() {
    let (_d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Rev", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(nb.id, page.id, &add(0, vec![text(1, "a")]))
        .unwrap();
    let err = lib
        .apply_page_operations(nb.id, page.id, &add(0, vec![text(2, "b")]))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Conflict);
    assert_eq!(lib.load_page(nb.id, page.id).unwrap().elements.len(), 1);
}

/// Archives written before the binary page encoding (format 1, JSON pages)
/// and live libraries with JSON snapshots keep working.
#[test]
fn legacy_json_archives_and_snapshots_still_load() {
    let (_d, mut lib) = new_lib();
    let (nb, page) = lib
        .create_notebook("Legacy", PageDefaults::default(), None)
        .unwrap();
    lib.apply_page_operations(nb.id, page.id, &add(0, fixture_elements()))
        .unwrap();
    let mut buf = Cursor::new(Vec::new());
    lib.export_archive(nb.id, &mut buf, "test").unwrap();
    buf.set_position(0);
    let expected = lib.load_page(nb.id, page.id).unwrap();

    // rewrite the archive as format 1: pages/<id>.json
    let mut z = zip::ZipArchive::new(buf).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut b).unwrap();
        entries.push((f.name().unwrap().to_string(), b));
    }
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &entries
            .iter()
            .find(|(n, _)| n == "manifest.json")
            .unwrap()
            .1,
    )
    .unwrap();
    manifest["formatVersion"] = 1.into();
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for (n, b) in &entries {
        if let Some(id) = n
            .strip_prefix("pages/")
            .and_then(|r| r.strip_suffix(".prp"))
        {
            let json = serde_json::to_vec(&penroam_core::binary::decode_page(b).unwrap()).unwrap();
            for mp in manifest["pages"].as_array_mut().unwrap() {
                if mp["id"] == id {
                    mp["path"] = format!("pages/{id}.json").into();
                    mp["sha256"] = sha256_hex(&json).into();
                    mp["bytes"] = json.len().into();
                }
            }
            out.push((format!("pages/{id}.json"), json));
        } else if n != "manifest.json" {
            out.push((n.clone(), b.clone()));
        }
    }
    out.push((
        "manifest.json".into(),
        serde_json::to_vec(&manifest).unwrap(),
    ));
    let (d2, mut other) = new_lib();
    let rep = other.import_archive(rebuild(&out)).unwrap();
    assert_eq!(other.load_page(rep.notebook.id, page.id).unwrap(), expected);

    // a live JSON snapshot (pre-binary library) loads and is converted on the next write
    drop(other);
    let pages = d2
        .path()
        .join(format!("library/notebooks/{}/pages", rep.notebook.id));
    assert!(pages.join(format!("{}.json", page.id)).exists());
    let mut other = Library::open(d2.path().join("library")).unwrap();
    assert_eq!(other.load_page(rep.notebook.id, page.id).unwrap(), expected);
    other
        .apply_page_operations(
            rep.notebook.id,
            page.id,
            &add(expected.revision, vec![text(500, "new")]),
        )
        .unwrap();
    other.flush_all().unwrap();
    assert!(pages.join(format!("{}.prp", page.id)).exists());
    assert!(!pages.join(format!("{}.json", page.id)).exists());
}
