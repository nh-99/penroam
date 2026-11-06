//! The client against a small in-process WebDAV server that keeps files in memory.

use penroam_webdav::*;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

const AUTH: &str = "Basic dXNlcjpwYXNz"; // user:pass

#[derive(Default)]
struct Dav {
    files: BTreeMap<String, Vec<u8>>,
    /// Store only this many bytes of an upload, as a failing server might.
    truncate_to: Option<usize>,
    /// Header lines of every request.
    seen: Vec<String>,
}

fn serve(state: Arc<Mutex<Dav>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let st = state.clone();
            std::thread::spawn(move || {
                let _ = handle(s, st);
            });
        }
    });
    format!("http://127.0.0.1:{port}/dav/backups")
}

fn handle(mut s: TcpStream, st: Arc<Mutex<Dav>>) -> std::io::Result<()> {
    loop {
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if s.read(&mut b)? == 0 {
                return Ok(());
            }
            head.push(b[0]);
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        st.lock().unwrap().seen.push(head.clone());
        let mut lines = head.lines();
        let mut first = lines.next().unwrap().split(' ');
        let (method, path) = (first.next().unwrap(), first.next().unwrap().to_string());
        let header = |n: &str| {
            head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(n).then(|| v.trim().to_string())
            })
        };
        let len: usize = header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; len];
        s.read_exact(&mut body)?;
        let name = path.rsplit('/').next().unwrap().to_string();
        let mut d = st.lock().unwrap();
        let (status, out): (u16, Vec<u8>) = if header("authorization").as_deref() != Some(AUTH) {
            (401, vec![])
        } else {
            match method {
                "PUT" => {
                    let keep = d.truncate_to.unwrap_or(body.len()).min(body.len());
                    body.truncate(keep);
                    d.files.insert(name, body);
                    (201, vec![])
                }
                "GET" => match d.files.get(&name) {
                    Some(b) => (200, b.clone()),
                    None => (404, vec![]),
                },
                "DELETE" => (
                    if d.files.remove(&name).is_some() {
                        204
                    } else {
                        404
                    },
                    vec![],
                ),
                "MOVE" => {
                    let dest = header("destination").unwrap();
                    let to = dest.rsplit('/').next().unwrap().to_string();
                    match d.files.remove(&name) {
                        Some(b) => {
                            d.files.insert(to, b);
                            (201, vec![])
                        }
                        None => (404, vec![]),
                    }
                }
                "PROPFIND" => {
                    let mut x = String::from(
                        r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/backups/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>"#,
                    );
                    for (n, b) in &d.files {
                        x.push_str(&format!(
                            "<d:response><d:href>/dav/backups/{n}</d:href><d:propstat><d:prop><d:getcontentlength>{}</d:getcontentlength><d:resourcetype/></d:prop></d:propstat></d:response>",
                            b.len()
                        ));
                    }
                    x.push_str("</d:multistatus>");
                    (207, x.into_bytes())
                }
                _ => (405, vec![]),
            }
        };
        drop(d);
        write!(
            s,
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n",
            out.len()
        )?;
        s.write_all(&out)?;
    }
}

fn file(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f
}

#[test]
fn upload_list_download_round_trip() {
    let st = Arc::new(Mutex::new(Dav::default()));
    let dav = WebDav::new(&serve(st.clone()), "user", "pass").unwrap();
    let f = file(b"backup bytes");
    let name = "Penroam backup 2026-11-06.penroam-backup";
    dav.upload(f.path(), name).unwrap();
    let list = dav.backups().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, name);
    assert_eq!(list[0].bytes, Some(12));
    let mut out = Vec::new();
    dav.download(name, &mut out).unwrap();
    assert_eq!(out, b"backup bytes");
    assert!(st
        .lock()
        .unwrap()
        .files
        .keys()
        .all(|k| !k.ends_with(".part")));
}

#[test]
fn a_short_upload_never_replaces_a_good_backup() {
    let st = Arc::new(Mutex::new(Dav::default()));
    let dav = WebDav::new(&serve(st.clone()), "user", "pass").unwrap();
    let name = "Penroam backup 2026-11-06.penroam-backup";
    st.lock()
        .unwrap()
        .files
        .insert(encoded(name), b"good".to_vec());
    st.lock().unwrap().truncate_to = Some(3);
    let f = file(b"new longer backup");
    assert!(matches!(
        dav.upload(f.path(), name),
        Err(WebDavError::Incomplete {
            sent: 17,
            stored: 3
        })
    ));
    let d = st.lock().unwrap();
    assert_eq!(d.files[&encoded(name)], b"good");
    assert_eq!(d.files.len(), 1, "the partial file was cleaned up");
}

fn encoded(n: &str) -> String {
    n.replace(' ', "%20")
}

#[test]
fn rotation_only_touches_our_backups() {
    let st = Arc::new(Mutex::new(Dav::default()));
    {
        let mut d = st.lock().unwrap();
        for day in 1..=5 {
            d.files.insert(
                format!("Penroam%20backup%202026-11-0{day}.penroam-backup"),
                vec![1],
            );
        }
        d.files.insert("thesis.pdf".into(), vec![2]);
        d.files
            .insert("Penroam%20backup%20notes.txt".into(), vec![3]);
    }
    let dav = WebDav::new(&serve(st.clone()), "user", "pass").unwrap();
    assert_eq!(dav.rotate(2).unwrap(), 3);
    let d = st.lock().unwrap();
    assert!(d.files.contains_key("thesis.pdf"));
    assert!(d.files.contains_key("Penroam%20backup%20notes.txt"));
    assert!(d
        .files
        .contains_key("Penroam%20backup%202026-11-05.penroam-backup"));
    assert!(d
        .files
        .contains_key("Penroam%20backup%202026-11-04.penroam-backup"));
    assert_eq!(d.files.len(), 4);
}

#[test]
fn wrong_password_is_reported_plainly() {
    let st = Arc::new(Mutex::new(Dav::default()));
    let dav = WebDav::new(&serve(st), "user", "nope").unwrap();
    assert!(matches!(dav.list(), Err(WebDavError::Unauthorized)));
}

#[test]
fn passwords_never_travel_in_the_clear_to_another_machine() {
    for bad in [
        "http://example.com/dav",
        "ftp://example.com/dav",
        "example.com/dav",
        "https://me:pw@example.com/dav",
        "http://127.0.0.1.example.com/dav",
    ] {
        assert!(WebDav::new(bad, "u", "p").is_err(), "{bad}");
    }
    assert!(WebDav::new(
        "https://cloud.example.com/remote.php/dav/files/me/",
        "u",
        "p"
    )
    .is_ok());
    assert!(WebDav::new("http://localhost:8080/dav", "u", "p").is_ok());
}

#[test]
fn hostile_listings_are_ignored_or_refused() {
    let st = Arc::new(Mutex::new(Dav::default()));
    {
        let mut d = st.lock().unwrap();
        d.files
            .insert("..%2F..%2Fetc%2Fpasswd.penroam-backup".into(), vec![1]);
    }
    let dav = WebDav::new(&serve(st), "user", "pass").unwrap();
    assert!(
        dav.list().unwrap().is_empty(),
        "a decoded path isn't a file name"
    );
}
