//! Developer tool: one sync profile (library + keys) per directory, driven
//! from the command line against a sync server over HTTP. Used to run
//! independent client processes in tests; it talks to the development test
//! server's `/_test/session` stub for sign-in, so it is not a production
//! client.
//!
//! ```text
//! penroam-sync-cli <profile> create <server-url> <account>        # prints the recovery code
//! penroam-sync-cli <profile> pair-begin <server-url> <account>    # prints the pairing code
//! penroam-sync-cli <profile> pair-approve <code> [label]          # prints the comparison code
//! penroam-sync-cli <profile> pair-poll                            # prints the comparison code or "waiting"
//! penroam-sync-cli <profile> recover <server-url> <account> <recovery-code>
//! penroam-sync-cli <profile> new-notebook <title>                 # prints the notebook id
//! penroam-sync-cli <profile> draw <notebook> <count> <x>          # strokes on the first page
//! penroam-sync-cli <profile> sync                                 # prints the status JSON
//! penroam-sync-cli <profile> checkpoint                           # publishes a checkpoint
//! penroam-sync-cli <profile> dump                                 # canonical JSON of the library
//! ```

use penroam_core::*;
use penroam_storage::Library;
use penroam_sync::transport::HttpTransport;
use penroam_sync::{FileSecretStore, Setup, SyncEngine};
use std::path::{Path, PathBuf};
use std::process::exit;
use uuid::Uuid;

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    exit(1)
}

fn secrets(p: &Path) -> Box<FileSecretStore> {
    Box::new(FileSecretStore::new(p.join("secrets")))
}

fn lib_dir(p: &Path) -> PathBuf {
    p.join("library")
}

fn setup(p: &Path, server: &str, account: &str) -> Setup {
    Setup {
        library_root: lib_dir(p),
        server_url: server.to_string(),
        account_id: account.to_string(),
        label: p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cli")
            .to_string(),
    }
}

fn account_transport(server: &str, account: &str) -> HttpTransport {
    let (token, _) = HttpTransport::test_session(server, account, None).unwrap_or_else(|e| die(e));
    HttpTransport::new(server, token)
}

fn open(p: &Path) -> (Library, SyncEngine<HttpTransport>) {
    let lib = Library::open(lib_dir(p)).unwrap_or_else(|e| die(e));
    let store = penroam_sync::store::SyncStore::open(&lib_dir(p)).unwrap_or_else(|e| die(e));
    let st = &store.state;
    let token =
        match HttpTransport::test_session(&st.server_url, &st.account_id, Some(&st.device_id)) {
            Ok((t, _)) => t,
            // not approved/registered for device sessions yet: account session
            Err(_) => {
                HttpTransport::test_session(&st.server_url, &st.account_id, None)
                    .unwrap_or_else(|e| die(e))
                    .0
            }
        };
    let t = HttpTransport::new(&st.server_url, token);
    drop(store);
    let e = SyncEngine::open(&lib_dir(p), t, secrets(p)).unwrap_or_else(|e| die(e));
    (lib, e)
}

fn view(lib: &mut Library) -> serde_json::Value {
    let mut out = Vec::new();
    for id in lib.notebook_ids() {
        let nb = lib.notebook(id).unwrap().clone();
        let mut ids = nb.page_order.clone();
        let mut t: Vec<Uuid> = nb.trashed_pages.iter().map(|t| t.id).collect();
        t.sort();
        ids.extend(t);
        let mut pages = Vec::new();
        for pid in ids {
            let p = lib.load_page(id, pid).unwrap();
            let mut els = p.elements.clone();
            els.sort_by_key(|e| (e.z(), e.id()));
            pages.push(serde_json::json!({ "id": pid, "elements": els }));
        }
        out.push(serde_json::json!({ "id": id, "title": nb.title, "order": nb.page_order, "pages": pages }));
    }
    serde_json::Value::Array(out)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        die("usage: penroam-sync-cli <profile> <command> …");
    }
    let p = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&p).unwrap_or_else(|e| die(e));
    let arg = |i: usize| {
        args.get(i)
            .cloned()
            .unwrap_or_else(|| die("missing argument"))
    };
    match args[2].as_str() {
        "create" => {
            let (server, account) = (arg(3), arg(4));
            Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            let (_, code) = SyncEngine::create_account(
                setup(&p, &server, &account),
                account_transport(&server, &account),
                secrets(&p),
            )
            .unwrap_or_else(|e| die(e));
            println!("{code}");
        }
        "recover" => {
            let (server, account, code) = (arg(3), arg(4), arg(5));
            Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            SyncEngine::enroll_with_recovery(
                setup(&p, &server, &account),
                account_transport(&server, &account),
                secrets(&p),
                &code,
            )
            .unwrap_or_else(|e| die(e));
            println!("enrolled");
        }
        "pair-begin" => {
            let (server, account) = (arg(3), arg(4));
            Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            let (_, code) = SyncEngine::begin_pairing(
                setup(&p, &server, &account),
                account_transport(&server, &account),
                secrets(&p),
            )
            .unwrap_or_else(|e| die(e));
            println!("{code}");
        }
        "pair-approve" => {
            let (_lib, mut e) = open(&p);
            let sas = e
                .approve_device(&arg(3), args.get(4).map_or("device", |s| s))
                .unwrap_or_else(|e| die(e));
            println!("{sas}");
        }
        "pair-poll" => {
            let (_lib, mut e) = open(&p);
            match e.poll_enrollment().unwrap_or_else(|e| die(e)) {
                Some(sas) => println!("{sas}"),
                None => println!("waiting"),
            }
        }
        "new-notebook" => {
            let mut lib = Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            let (nb, _) = lib
                .create_notebook(&arg(3), PageDefaults::default(), None)
                .unwrap_or_else(|e| die(e));
            println!("{}", nb.id);
        }
        "draw" => {
            let mut lib = Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            let nb: Uuid = arg(3).parse().unwrap_or_else(|e| die(e));
            let n: u64 = arg(4).parse().unwrap_or_else(|e| die(e));
            let x: f64 = arg(5).parse().unwrap_or_else(|e| die(e));
            let page = lib.notebook(nb).unwrap_or_else(|e| die(e)).page_order[0];
            for i in 0..n {
                let rev = lib.load_page(nb, page).unwrap().revision;
                let el = Element::Ink(InkStroke {
                    id: Uuid::new_v4(),
                    z: i as i64 + 1,
                    tool: InkTool::Pen,
                    color: "#111111".into(),
                    opacity: 1.0,
                    width: 2.0,
                    pressure: true,
                    source: "cli".into(),
                    input: Extra::new(),
                    points: (0..16)
                        .map(|k| InkPoint {
                            x: x + k as f64,
                            y: 20.0 + i as f64 * 3.0,
                            pressure: 0.5,
                            tilt_x: 0.0,
                            tilt_y: 0.0,
                            time_us: k * 4000,
                            twist: None,
                            flags: None,
                        })
                        .collect(),
                    extra: Extra::new(),
                });
                lib.apply_page_operations(
                    nb,
                    page,
                    &OperationBatch {
                        batch_id: Uuid::new_v4(),
                        base_revision: rev,
                        ops: vec![PageOperation {
                            op_id: Uuid::new_v4(),
                            kind: OpKind::AddElements { elements: vec![el] },
                        }],
                    },
                )
                .unwrap_or_else(|e| die(e));
            }
            println!("drew {n}");
        }
        "sync" => {
            let (mut lib, mut e) = open(&p);
            let r = e.reconcile_owned(&mut lib);
            let st = e.status(lib.last_seq());
            lib.flush_all().unwrap_or_else(|e| die(e));
            println!("{}", serde_json::to_string(&st).unwrap());
            if let Err(err) = r {
                die(err);
            }
        }
        "checkpoint" => {
            let (mut lib, mut e) = open(&p);
            e.reconcile_owned(&mut lib).unwrap_or_else(|e| die(e));
            match e.create_checkpoint().unwrap_or_else(|e| die(e)) {
                Some(r) => println!("checkpoint covers server_seq {}", r.covers_server_seq),
                None => println!("not published (envelopes still parked)"),
            }
        }
        "dump" => {
            let mut lib = Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            println!("{}", serde_json::to_string(&view(&mut lib)).unwrap());
        }
        other => die(format!("unknown command {other}")),
    }
}
