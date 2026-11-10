//! Library folders on one device: persistence, journal recovery and how
//! notebooks follow a deleted folder.

use penroam_core::*;
use penroam_storage::*;
use std::fs;
use std::io::Cursor;

fn open(d: &tempfile::TempDir) -> Library {
    Library::open(d.path().join("library")).unwrap()
}

fn notebook(lib: &mut Library, title: &str) -> Notebook {
    lib.create_notebook(title, PageDefaults::default(), None)
        .unwrap()
        .0
}

fn summary(lib: &Library, id: uuid::Uuid) -> NotebookSummary {
    lib.list_notebooks()
        .into_iter()
        .find(|n| n.id == id)
        .unwrap()
}

#[test]
fn folders_and_filing_survive_a_restart() {
    let d = tempfile::tempdir().unwrap();
    let (school, physics, nb);
    {
        let mut lib = open(&d);
        school = lib.create_folder(" School ", None).unwrap();
        physics = lib.create_folder("Physics", Some(school.id)).unwrap();
        nb = notebook(&mut lib, "Mechanics");
        lib.file_notebook(nb.id, Some(physics.id)).unwrap();
    }
    let lib = open(&d);
    let tree = lib.list_folders();
    assert_eq!(
        tree.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["Physics", "School"]
    );
    assert_eq!(tree[0].parent, Some(school.id));
    assert_eq!(summary(&lib, nb.id).folder, Some(physics.id));
}

#[test]
fn folder_changes_replay_from_the_journal_if_the_snapshot_is_lost() {
    let d = tempfile::tempdir().unwrap();
    let f;
    {
        let mut lib = open(&d);
        f = lib.create_folder("Before", None).unwrap();
        lib.rename_folder(f.id, "After").unwrap();
    }
    fs::remove_file(d.path().join("library/folders.json")).unwrap();
    let lib = open(&d);
    assert_eq!(lib.list_folders()[0].id, f.id);
    assert_eq!(lib.list_folders()[0].name, "After");
}

#[test]
fn a_damaged_folders_file_is_set_aside_and_reported() {
    let d = tempfile::tempdir().unwrap();
    {
        let mut lib = open(&d);
        lib.create_folder("Kept", None).unwrap();
        lib.flush_all().unwrap();
    }
    fs::write(d.path().join("library/folders.json"), b"{not json").unwrap();
    let lib = open(&d);
    assert!(lib.list_folders().is_empty());
    assert!(lib.recovery.warnings.iter().any(|w| w.contains("folders")));
    assert!(d.path().join("library/folders.json.damaged").exists());
}

#[test]
fn deleting_a_folder_moves_its_contents_up_and_deletes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let mut lib = open(&d);
    let top = lib.create_folder("Top", None).unwrap();
    let mid = lib.create_folder("Mid", Some(top.id)).unwrap();
    let low = lib.create_folder("Low", Some(mid.id)).unwrap();
    let nb = notebook(&mut lib, "Inside");
    lib.file_notebook(nb.id, Some(mid.id)).unwrap();

    lib.delete_folder(mid.id).unwrap();

    assert_eq!(summary(&lib, nb.id).folder, Some(top.id));
    assert!(lib.notebook(nb.id).unwrap().deleted_at.is_none());
    let tree = lib.list_folders();
    assert!(tree.iter().all(|f| f.id != mid.id));
    assert_eq!(
        tree.iter().find(|f| f.id == low.id).unwrap().parent,
        Some(top.id)
    );
    // nothing further can be done to a deleted folder
    assert!(lib.rename_folder(mid.id, "Again").is_err());
    assert!(lib.file_notebook(nb.id, Some(mid.id)).is_err());
}

#[test]
fn a_folder_cannot_be_moved_inside_itself() {
    let d = tempfile::tempdir().unwrap();
    let mut lib = open(&d);
    let a = lib.create_folder("A", None).unwrap();
    let b = lib.create_folder("B", Some(a.id)).unwrap();
    assert!(lib.move_folder(a.id, Some(b.id)).is_err());
    assert!(lib.move_folder(a.id, Some(a.id)).is_err());
    lib.move_folder(b.id, None).unwrap();
    lib.move_folder(a.id, Some(b.id)).unwrap();
}

#[test]
fn filing_keeps_the_notebooks_place_in_the_list() {
    let d = tempfile::tempdir().unwrap();
    let mut lib = open(&d);
    let f = lib.create_folder("F", None).unwrap();
    let nb = notebook(&mut lib, "N");
    let before = lib.notebook(nb.id).unwrap().updated_at;
    std::thread::sleep(std::time::Duration::from_millis(5));
    lib.file_notebook(nb.id, Some(f.id)).unwrap();
    assert_eq!(lib.notebook(nb.id).unwrap().updated_at, before);
}

#[test]
fn an_imported_notebook_arrives_at_the_top_level() {
    let d = tempfile::tempdir().unwrap();
    let mut lib = open(&d);
    let f = lib.create_folder("F", None).unwrap();
    let nb = notebook(&mut lib, "N");
    lib.file_notebook(nb.id, Some(f.id)).unwrap();
    let mut buf = Cursor::new(Vec::new());
    lib.export_archive(nb.id, &mut buf, "test").unwrap();
    buf.set_position(0);
    let copy = lib.import_archive(buf).unwrap().notebook;
    assert_ne!(copy.id, nb.id);
    assert_eq!(copy.folder, None);
}

#[test]
fn a_notebook_cannot_take_the_reserved_stream_id() {
    let mut nb = Notebook::new(
        folder::LIBRARY_STREAM_ID,
        "N".into(),
        PageDefaults::default(),
        "#112233".into(),
        0,
    );
    assert!(validate::validate_notebook(&nb).is_err());
    nb.id = uuid::Uuid::new_v4();
    assert!(validate::validate_notebook(&nb).is_ok());
}

#[test]
fn restoring_folders_adds_only_the_missing_ones() {
    let d = tempfile::tempdir().unwrap();
    let mut source = open(&d);
    let kept = source.create_folder("Kept", None).unwrap();
    let gone = source.create_folder("Gone", None).unwrap();
    source.delete_folder(gone.id).unwrap();
    let saved = source.folders().clone();

    let d2 = tempfile::tempdir().unwrap();
    let mut target = open(&d2);
    target.restore_folders(&saved).unwrap();
    target.restore_folders(&saved).unwrap();
    assert_eq!(target.list_folders().len(), 1);
    assert_eq!(target.list_folders()[0].id, kept.id);
    assert!(target.folders().0.contains_key(&gone.id));
}
