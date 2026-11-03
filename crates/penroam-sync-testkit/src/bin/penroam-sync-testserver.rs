//! `penroam-sync-testserver`: the **development test server** for the
//! Penroam sync protocol over HTTP. It has a stub login and no TLS, rate
//! limiting or hardening. Use it on loopback for tests and local development
//! only.
//!
//! ```text
//! penroam-sync-testserver [--listen 127.0.0.1:PORT] [--data DIR] [--allow-non-loopback]
//! ```
//!
//! * `--listen`: address to bind. The default is `127.0.0.1:0`, an ephemeral
//!   port. The first stdout line is `listening on http://ADDR`.
//! * `--data`: keep state in `DIR` so it survives restarts. Without it, state
//!   is in memory only.
//! * `--allow-non-loopback`: required to bind anything other than a loopback
//!   address.

use std::io::Write;
use std::net::ToSocketAddrs;
use std::sync::Arc;

use penroam_sync_testkit::{http, InMemoryServer};

fn usage() -> ! {
    eprintln!(
        "usage: penroam-sync-testserver [--listen 127.0.0.1:PORT] [--data DIR] [--allow-non-loopback]"
    );
    std::process::exit(2);
}

fn main() {
    let mut listen = "127.0.0.1:0".to_string();
    let mut data: Option<String> = None;
    let mut allow_remote = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().unwrap_or_else(|| usage()),
            "--data" => data = Some(args.next().unwrap_or_else(|| usage())),
            "--allow-non-loopback" => allow_remote = true,
            _ => usage(),
        }
    }
    let addrs: Vec<_> = match listen.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            eprintln!("invalid --listen address {listen}: {e}");
            std::process::exit(2);
        }
    };
    if !allow_remote && (addrs.is_empty() || addrs.iter().any(|a| !a.ip().is_loopback())) {
        eprintln!(
            "refusing to listen on {listen}: this development test server has no real \
             authentication. Use a loopback address or pass --allow-non-loopback."
        );
        std::process::exit(2);
    }
    let server = match &data {
        Some(dir) => match InMemoryServer::open_persistent(std::path::Path::new(dir)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cannot open state in {dir}: {e}");
                std::process::exit(1);
            }
        },
        None => InMemoryServer::new(),
    };
    let handle = match http::spawn(&listen, Arc::new(server)) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("cannot listen on {listen}: {e}");
            std::process::exit(1);
        }
    };
    let mut out = std::io::stdout();
    let _ = writeln!(out, "listening on {}", handle.base_url);
    let _ = out.flush();
    loop {
        std::thread::park();
    }
}
