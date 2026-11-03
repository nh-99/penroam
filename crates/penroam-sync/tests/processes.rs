//! Independent client *processes* (separate libraries, keys and secret
//! stores) syncing through the HTTP test server, including a client killed
//! mid-sync and a server restart from its data directory.

use penroam_sync_testkit::{http, InMemoryServer};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const CLI: &str = env!("CARGO_BIN_EXE_penroam-sync-cli");

fn run(profile: &Path, args: &[&str]) -> String {
    let out = Command::new(CLI).arg(profile).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn post(base: &str, path: &str, body: &str) {
    // tiny HTTP/1.1 client for the test-control routes
    use std::io::{Read, Write};
    let addr = base.trim_start_matches("http://");
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut r = String::new();
    let _ = s.read_to_string(&mut r);
    assert!(r.starts_with("HTTP/1.1 2"), "{path}: {r}");
}

fn strokes(dump: &str) -> usize {
    dump.matches("\"tool\":\"pen\"").count()
}

#[test]
fn two_processes_pair_and_converge_through_the_http_test_server() {
    let data = tempfile::tempdir().unwrap();
    let server = Arc::new(InMemoryServer::open_persistent(data.path()).unwrap());
    let mut handle = Some(http::spawn("127.0.0.1:0", server).unwrap());
    let base = handle.as_ref().unwrap().base_url.clone();
    let dirs = tempfile::tempdir().unwrap();
    let a = dirs.path().join("device-a");
    let b = dirs.path().join("device-b");

    let code = run(&a, &["create", &base, "acct-proc"]);
    assert!(code.starts_with("PRK1-"));
    let nb = run(&a, &["new-notebook", "Two processes"]);
    run(&a, &["draw", &nb, "20", "100"]);
    run(&a, &["sync"]);

    let pairing = run(&b, &["pair-begin", &base, "acct-proc"]);
    assert_eq!(run(&b, &["pair-poll"]), "waiting");
    let sas_a = run(&a, &["pair-approve", &pairing, "device-b"]);
    let sas_b = run(&b, &["pair-poll"]);
    assert_eq!(sas_a, sas_b, "comparison codes match");
    run(&b, &["sync"]);
    assert_eq!(strokes(&run(&b, &["dump"])), 20);

    // both draw while not syncing, then converge
    run(&a, &["draw", &nb, "15", "200"]);
    run(&b, &["draw", &nb, "15", "300"]);

    // B is killed in the middle of a sync (the server delays its responses)
    post(&base, "/_test/delay-next", r#"{"n":3,"ms":1500}"#);
    let mut child = Command::new(CLI)
        .arg(&b)
        .arg("sync")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    child.kill().unwrap(); // SIGKILL
    child.wait().unwrap();

    // the server restarts from its data directory
    handle.take().unwrap().shutdown();
    let server = Arc::new(InMemoryServer::open_persistent(data.path()).unwrap());
    let h2 = http::spawn(base.trim_start_matches("http://"), server).unwrap();
    assert_eq!(h2.base_url, base);

    for _ in 0..2 {
        run(&a, &["sync"]);
        run(&b, &["sync"]);
    }
    let da = run(&a, &["dump"]);
    let db = run(&b, &["dump"]);
    assert_eq!(strokes(&da), 50);
    assert_eq!(da, db, "both processes hold the same notebook");
    let status: serde_json::Value = serde_json::from_str(&run(&b, &["sync"])).unwrap();
    assert_eq!(status["upToDate"], true, "{status}");
    assert_eq!(status["securityReviewPending"], true);

    // a third process joins with the recovery code alone
    let c = dirs.path().join("device-c");
    run(&c, &["recover", &base, "acct-proc", &code]);
    run(&c, &["sync"]);
    assert_eq!(run(&c, &["dump"]), da);
    drop(h2);
}
