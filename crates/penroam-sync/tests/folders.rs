//! Library folders across devices: they arrive, merge without conflicts and
//! never strand a notebook.

mod common;

use common::*;
use penroam_core::*;
use penroam_sync_testkit::InMemoryServer;
use std::sync::Arc;
use uuid::Uuid;

fn server() -> Arc<InMemoryServer> {
    Arc::new(InMemoryServer::new())
}

fn shown_in(c: &Client, nb: Uuid) -> Option<Uuid> {
    c.lib
        .list_notebooks()
        .into_iter()
        .find(|n| n.id == nb)
        .unwrap()
        .folder
}

#[test]
fn folders_and_filing_reach_the_other_device() {
    let s = server();
    let acct = account(&s, "folders-1");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let f = a.lib.create_folder("School", None).unwrap();
    let (nb, _) = a
        .lib
        .create_notebook("Physics", PageDefaults::default(), None)
        .unwrap();
    a.lib.file_notebook(nb.id, Some(f.id)).unwrap();
    a.sync().unwrap();

    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    assert_eq!(b.lib.list_folders(), a.lib.list_folders());
    assert_eq!(shown_in(&b, nb.id), Some(f.id));
    // the merged notebook keeps the time of its last real change
    assert_eq!(
        b.lib.notebook(nb.id).unwrap().updated_at,
        a.lib.notebook(nb.id).unwrap().updated_at
    );
    assert!(b.status().up_to_date, "{:?}", b.status());
    // folders aren't notebooks, so they aren't offered as cloud-only ones
    assert!(b.engine.erased_notebooks(&b.lib).unwrap().is_empty());
    // the server never saw the name
    assert!(!format!("{:?}", s.list_devices(&acct).unwrap()).contains("School"));
}

#[test]
fn folders_that_existed_before_connecting_are_uploaded() {
    let s = server();
    let acct = account(&s, "folders-2");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let kept = a.lib.create_folder("Kept", None).unwrap();
    let gone = a.lib.create_folder("Gone", Some(kept.id)).unwrap();
    let (nb, _) = a
        .lib
        .create_notebook("N", PageDefaults::default(), None)
        .unwrap();
    a.lib.file_notebook(nb.id, Some(gone.id)).unwrap();
    a.lib.delete_folder(gone.id).unwrap();
    // nothing synced yet: this first reconcile seeds from the snapshots
    a.sync().unwrap();

    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    assert_eq!(b.lib.list_folders(), a.lib.list_folders());
    assert_eq!(shown_in(&b, nb.id), Some(kept.id));
}

#[test]
fn moving_two_folders_under_each_other_offline_converges_without_a_cycle() {
    let s = server();
    let acct = account(&s, "folders-3");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let x = a.lib.create_folder("X", None).unwrap();
    let y = a.lib.create_folder("Y", None).unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();

    a.offline(true);
    b.offline(true);
    a.lib.move_folder(x.id, Some(y.id)).unwrap();
    b.lib.move_folder(y.id, Some(x.id)).unwrap();
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);

    let tree = a.lib.list_folders();
    assert_eq!(tree, b.lib.list_folders());
    assert_eq!(tree.len(), 2);
    assert_eq!(
        tree.iter().filter(|f| f.parent.is_none()).count(),
        1,
        "exactly one of them is cut loose: {tree:?}"
    );
}

#[test]
fn filing_into_a_folder_deleted_elsewhere_keeps_the_notebook_reachable() {
    let s = server();
    let acct = account(&s, "folders-4");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let top = a.lib.create_folder("Top", None).unwrap();
    let inner = a.lib.create_folder("Inner", Some(top.id)).unwrap();
    let (nb, _) = a
        .lib
        .create_notebook("N", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();

    a.offline(true);
    b.offline(true);
    a.lib.delete_folder(inner.id).unwrap();
    b.lib.file_notebook(nb.id, Some(inner.id)).unwrap();
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);

    assert_eq!(shown_in(&a, nb.id), Some(top.id));
    assert_eq!(shown_in(&b, nb.id), Some(top.id));
    assert_eq!(a.lib.list_folders(), b.lib.list_folders());
}

#[test]
fn concurrent_renames_and_filings_raise_no_conflict() {
    let s = server();
    let acct = account(&s, "folders-5");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let one = a.lib.create_folder("One", None).unwrap();
    let two = a.lib.create_folder("Two", None).unwrap();
    let (nb, _) = a
        .lib
        .create_notebook("N", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();

    a.offline(true);
    b.offline(true);
    a.lib.rename_folder(one.id, "From A").unwrap();
    b.lib.rename_folder(one.id, "From B").unwrap();
    a.lib.file_notebook(nb.id, Some(one.id)).unwrap();
    b.lib.file_notebook(nb.id, Some(two.id)).unwrap();
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);

    assert_eq!(a.lib.list_folders(), b.lib.list_folders());
    assert_eq!(shown_in(&a, nb.id), shown_in(&b, nb.id));
    assert_eq!(a.status().conflicts, 0, "{:?}", a.status());
}
