//! Developer tool: one sync profile (library + keys) per directory, driven
//! from the command line against a sync server over HTTP. Used to run
//! independent client processes in tests. It is not a production client.
//!
//! Sign-in: against a server that advertises `auth.v1`, run `login` first
//! (system browser with PKCE, or `--id-token-file` for automated tests); the
//! sessions are kept in `<profile>/sessions.json` (mode 0600, refreshed on
//! every run) and `-` may be given as the account to use the signed-in one.
//! Against the development test server, its `/_test/session` stub is used.
//!
//! ```text
//! penroam-sync-cli <profile> login <server-url> [--id-token-file <file>]
//! penroam-sync-cli <profile> logout
//! penroam-sync-cli <profile> connect <server-url> <account>       # first device or a later one
//! penroam-sync-cli <profile> create <server-url> <account>        # prints the recovery code; stores no key
//! penroam-sync-cli <profile> recover <server-url> <account> <recovery-code>
//! penroam-sync-cli <profile> new-notebook <title>                 # prints the notebook id
//! penroam-sync-cli <profile> draw <notebook> <count> <x>          # strokes on the first page
//! penroam-sync-cli <profile> sync                                 # prints the status JSON
//! penroam-sync-cli <profile> checkpoint                           # publishes a checkpoint
//! penroam-sync-cli <profile> dump                                 # canonical JSON of the library
//! ```

use penroam_core::*;
use penroam_storage::Library;
use penroam_sync::auth;
use penroam_sync::transport::HttpTransport;
use penroam_sync::{FileSecretStore, Setup, SyncEngine, SyncTransport};
use penroam_sync_protocol::dto::capabilities;
use penroam_sync_protocol::SessionTokens;
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
        scope: penroam_sync::vault::Scope::Account,
        device_id: None,
        server_url: server.to_string(),
        account_id: account.to_string(),
        label: p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cli")
            .to_string(),
    }
}

/// Sessions from `login` (servers with `auth.v1`). Tokens are secrets: the
/// file is created with mode 0600 and never printed.
#[derive(serde::Serialize, serde::Deserialize)]
struct Sessions {
    server_url: String,
    account: SessionTokens,
    #[serde(default)]
    device: Option<SessionTokens>,
}

fn sessions_path(p: &Path) -> PathBuf {
    p.join("sessions.json")
}

fn load_sessions(p: &Path) -> Option<Sessions> {
    let bytes = std::fs::read(sessions_path(p)).ok()?;
    Some(serde_json::from_slice(&bytes).unwrap_or_else(|e| die(e)))
}

fn save_sessions(p: &Path, s: &Sessions) {
    let path = sessions_path(p);
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp).unwrap_or_else(|e| die(e));
    std::io::Write::write_all(&mut f, &serde_json::to_vec(s).unwrap()).unwrap_or_else(|e| die(e));
    f.sync_all().unwrap_or_else(|e| die(e));
    std::fs::rename(&tmp, &path).unwrap_or_else(|e| die(e));
}

/// Refreshes the stored account session (refresh tokens are single-use, so
/// the rotated pair is saved before it is used).
fn account_session(p: &Path, server: &str) -> Option<SessionTokens> {
    let mut s = load_sessions(p)?;
    if s.server_url != server {
        die("this profile is signed in to another server; run logout first");
    }
    s.account = auth::refresh(server, &s.account.refresh_token).unwrap_or_else(|e| die(e));
    save_sessions(p, &s);
    Some(s.account)
}

/// Resolves the account argument: `-` means the signed-in account.
fn account_arg(p: &Path, account: &str) -> String {
    if account != "-" {
        return account.to_string();
    }
    load_sessions(p)
        .map(|s| s.account.account_id.to_string())
        .unwrap_or_else(|| die("`-` needs a prior login"))
}

fn account_transport(p: &Path, server: &str, account: &str) -> HttpTransport {
    if let Some(s) = account_session(p, server) {
        if s.account_id.as_str() != account {
            die("the account argument differs from the signed-in account");
        }
        return HttpTransport::new(server, s.access_token);
    }
    let (token, _) = HttpTransport::test_session(server, account, None).unwrap_or_else(|e| die(e));
    HttpTransport::new(server, token)
}

fn open(p: &Path) -> (Library, SyncEngine<HttpTransport>) {
    let lib = Library::open(lib_dir(p)).unwrap_or_else(|e| die(e));
    let store = penroam_sync::store::SyncStore::open(&lib_dir(p)).unwrap_or_else(|e| die(e));
    let (server, account, device) = (
        store.state.server_url.clone(),
        store.state.account_id.clone(),
        store.state.device_id.clone(),
    );
    drop(store);
    if load_sessions(p).is_some() {
        // auth.v1: reuse the device session if it still refreshes, else sign
        // the server's challenge again with the device key (possible once the
        // device is an active key-log member)
        let mut s = load_sessions(p).unwrap();
        if let Some(d) = s.device.take() {
            if let Ok(t) = auth::refresh(&server, &d.refresh_token) {
                s.device = Some(t.clone());
                save_sessions(p, &s);
                let tr = HttpTransport::new(&server, t.access_token);
                let e = SyncEngine::open(&lib_dir(p), tr, secrets(p)).unwrap_or_else(|e| die(e));
                return (lib, e);
            }
        }
        save_sessions(p, &s);
        let acct = account_session(p, &server).unwrap();
        let tr = HttpTransport::new(&server, acct.access_token.clone());
        let mut e = SyncEngine::open(&lib_dir(p), tr, secrets(p)).unwrap_or_else(|e| die(e));
        if let Ok(t) = e.device_session(&server, &acct.access_token) {
            e.transport_mut().set_token(t.access_token.clone());
            let mut s = load_sessions(p).unwrap();
            s.device = Some(t);
            save_sessions(p, &s);
        }
        return (lib, e);
    }
    let token = match HttpTransport::test_session(&server, &account, Some(&device)) {
        Ok((t, _)) => t,
        // not approved/registered for device sessions yet: account session
        Err(_) => {
            HttpTransport::test_session(&server, &account, None)
                .unwrap_or_else(|e| die(e))
                .0
        }
    };
    let t = HttpTransport::new(&server, token);
    let e = SyncEngine::open(&lib_dir(p), t, secrets(p)).unwrap_or_else(|e| die(e));
    (lib, e)
}

/// `login`: signs in with the server's OpenID Connect provider and stores
/// the account session.
fn login(p: &Path, server: &str, id_token_file: Option<&str>) {
    let meta = HttpTransport::new(server, String::new())
        .meta()
        .unwrap_or_else(|e| die(e));
    if !meta.has_capability(capabilities::AUTH_V1) {
        die("this server has no sign-in (auth.v1); the test server needs no login");
    }
    let id_token = match id_token_file {
        Some(f) => std::fs::read_to_string(f)
            .unwrap_or_else(|e| die(e))
            .trim()
            .to_string(),
        None => {
            let cfg = auth::auth_config(server).unwrap_or_else(|e| die(e));
            let disc = auth::discover(&cfg.issuer).unwrap_or_else(|e| die(e));
            let l = auth::BrowserLogin::start(&cfg, &disc).unwrap_or_else(|e| die(e));
            eprintln!("Open this URL to sign in:\n{}", l.authorize_url());
            for opener in ["xdg-open", "open"] {
                if std::process::Command::new(opener)
                    .arg(l.authorize_url())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .is_ok()
                {
                    break;
                }
            }
            l.finish(std::time::Duration::from_secs(300))
                .unwrap_or_else(|e| die(e))
        }
    };
    let account = auth::session_from_id_token(server, &id_token).unwrap_or_else(|e| die(e));
    println!("{}", account.account_id);
    save_sessions(
        p,
        &Sessions {
            server_url: server.to_string(),
            account,
            device: None,
        },
    );
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
        "login" => {
            let file = match args.get(4).map(String::as_str) {
                Some("--id-token-file") => Some(arg(5)),
                Some(other) => die(format!("unknown option {other}")),
                None => None,
            };
            login(&p, &arg(3), file.as_deref());
        }
        "logout" => {
            if let Some(s) = load_sessions(&p) {
                for t in [Some(&s.account), s.device.as_ref()].into_iter().flatten() {
                    let _ = auth::refresh(&s.server_url, &t.refresh_token)
                        .and_then(|t| auth::logout(&s.server_url, &t.access_token));
                }
                std::fs::remove_file(sessions_path(&p)).unwrap_or_else(|e| die(e));
            }
            println!("signed out");
        }
        "create" => {
            let (server, account) = (arg(3), account_arg(&p, &arg(4)));
            Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            let (_, code) = SyncEngine::create_account(
                setup(&p, &server, &account),
                account_transport(&p, &server, &account),
                secrets(&p),
            )
            .unwrap_or_else(|e| die(e));
            println!("{code}");
        }
        "recover" => {
            let (server, account, code) = (arg(3), account_arg(&p, &arg(4)), arg(5));
            Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            SyncEngine::enroll_with_recovery(
                setup(&p, &server, &account),
                account_transport(&p, &server, &account),
                secrets(&p),
                &code,
            )
            .unwrap_or_else(|e| die(e));
            println!("enrolled");
        }
        "connect" => {
            let (server, account) = (arg(3), account_arg(&p, &arg(4)));
            Library::open(lib_dir(&p)).unwrap_or_else(|e| die(e));
            SyncEngine::connect(
                setup(&p, &server, &account),
                account_transport(&p, &server, &account),
                secrets(&p),
            )
            .unwrap_or_else(|e| die(e));
            println!("connected");
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
