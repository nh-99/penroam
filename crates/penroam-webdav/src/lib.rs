//! Keeps Penroam backup files on a WebDAV server (Nextcloud, ownCloud, a NAS).
//!
//! This is **backup storage only**. It moves whole `.penroam-backup` files and never merges
//! anything, so two devices backing up to one folder simply leave two files. Only the
//! operation sync protocol reconciles concurrent edits: see `docs/formats/portability.md`.
//!
//! **Invariants.**
//! * An upload goes to `<name>.part` and is moved over the final name only after the server
//!   reports the full size, so an interrupted upload never replaces a good backup.
//! * Rotation only ever deletes files matching [`BACKUP_PREFIX`] and [`BACKUP_SUFFIX`], and
//!   only after a new upload has succeeded.
//! * Credentials go only to the configured origin, over `https://`. Plain `http://` is
//!   accepted for loopback addresses alone, and redirects are not followed, so they can't
//!   carry the password somewhere else.
//! * Server responses are hostile input: listing bodies are size-limited, parsed with no DTD
//!   support, and names that aren't plain file names are ignored.

#![warn(missing_docs)]

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;
use ts_rs::TS;

/// Start of the name of every backup this module manages.
pub const BACKUP_PREFIX: &str = "Penroam backup ";
/// End of the name of every backup this module manages.
pub const BACKUP_SUFFIX: &str = ".penroam-backup";
/// Largest directory listing read from a server.
const MAX_LISTING: u64 = 8 << 20;

/// Why a WebDAV request failed.
#[derive(Debug, thiserror::Error)]
pub enum WebDavError {
    /// The address can't be used.
    #[error("{0}")]
    BadUrl(String),
    /// The server refused the user name or password.
    #[error("the server didn't accept that user name and password")]
    Unauthorized,
    /// The server answered with an error.
    #[error("the server answered {0}")]
    Status(u16),
    /// The server couldn't be reached or the transfer failed.
    #[error("could not reach the server: {0}")]
    Network(String),
    /// The server reported a different size than was sent.
    #[error("the server stored {stored} bytes of {sent}, so the upload was not kept")]
    Incomplete {
        /// Bytes sent.
        sent: u64,
        /// Bytes the server reports.
        stored: u64,
    },
    /// A local file failed.
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A file in the backup folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteFile {
    /// File name.
    pub name: String,
    /// Size in bytes, if the server reported it.
    #[ts(type = "number | null")]
    pub bytes: Option<u64>,
}

/// A backup folder on a server.
pub struct WebDav {
    base: String,
    user: String,
    password: String,
    agent: ureq::Agent,
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let v = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(v >> 18) as usize & 63] as char);
        o.push(T[(v >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 {
            T[(v >> 6) as usize & 63] as char
        } else {
            '='
        });
        o.push(if c.len() > 2 {
            T[v as usize & 63] as char
        } else {
            '='
        });
    }
    o
}

fn encode_segment(name: &str) -> String {
    let mut o = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn decode_segment(s: &str) -> String {
    let b = s.as_bytes();
    let mut o = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%' && i + 2 < b.len())
            .then(|| std::str::from_utf8(&b[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                o.push(v);
                i += 3;
            }
            None => {
                o.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&o).into_owned()
}

/// Whether a host is this machine. Plain HTTP is only safe there.
fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "[::1]"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Checks a folder address and returns it with one trailing slash.
fn normalize(url: &str) -> Result<String, WebDavError> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| WebDavError::BadUrl("the address must start with https://".into()))?;
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err(WebDavError::BadUrl(
            "put the user name in its own field, not in the address".into(),
        ));
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, p)| {
        if p.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            authority
        }
    });
    if url.starts_with("http://") && !is_loopback(host) {
        return Err(WebDavError::BadUrl(
            "the address must use https://, or your password would travel unencrypted".into(),
        ));
    }
    if url.contains('?') || url.contains('#') {
        return Err(WebDavError::BadUrl(
            "the address should be a folder, without ? or #".into(),
        ));
    }
    Ok(format!("{}/", url.trim_end_matches('/')))
}

/// The last path segment of an `href`, if it is a plain file name.
fn href_name(href: &str) -> Option<String> {
    let path = href.split(['?', '#']).next()?.trim_end_matches('/');
    let name = decode_segment(path.rsplit('/').next()?);
    let plain =
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0']);
    plain.then_some(name)
}

/// Reads the files of a PROPFIND multistatus body.
fn parse_listing(xml: &str) -> Result<Vec<RemoteFile>, WebDavError> {
    let bad = |e: String| WebDavError::Network(format!("unreadable folder listing: {e}"));
    let mut r = Reader::from_str(xml);
    let mut out = Vec::new();
    let (mut href, mut bytes, mut is_dir) = (String::new(), None, false);
    let mut field = "";
    loop {
        match r.read_event().map_err(|e| bad(e.to_string()))? {
            Event::Start(e) | Event::Empty(e) => {
                let local: &str = {
                    let n = e.local_name();
                    match <_ as AsRef<str>>::as_ref(&n) {
                        "href" => "href",
                        "getcontentlength" => "len",
                        "collection" => "dir",
                        "response" => "response",
                        _ => "",
                    }
                };
                match local {
                    "response" => {
                        href.clear();
                        bytes = None;
                        is_dir = false;
                    }
                    "dir" => is_dir = true,
                    "href" | "len" => field = local,
                    _ => {}
                }
            }
            Event::Text(t) => {
                let text = t.xml10_content();
                match field {
                    "href" => href.push_str(text.trim()),
                    "len" => bytes = text.trim().parse().ok(),
                    _ => {}
                }
            }
            Event::End(e) => {
                field = "";
                let n = e.local_name();
                if <_ as AsRef<str>>::as_ref(&n) == "response" && !is_dir {
                    if let Some(name) = href_name(&href) {
                        out.push(RemoteFile { name, bytes });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

impl WebDav {
    /// Prepares a client for a folder. Makes no request.
    pub fn new(url: &str, user: &str, password: &str) -> Result<Self, WebDavError> {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // PROPFIND and MOVE aren't methods ureq knows
            .allow_non_standard_methods(true)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(20)))
            .build();
        Ok(Self {
            base: normalize(url)?,
            user: user.to_string(),
            password: password.to_string(),
            agent: config.into(),
        })
    }

    fn auth(&self) -> String {
        format!(
            "Basic {}",
            base64(format!("{}:{}", self.user, self.password).as_bytes())
        )
    }

    fn url(&self, name: &str) -> String {
        format!("{}{}", self.base, encode_segment(name))
    }

    /// Lists the files in the folder. Also serves as a connection test.
    pub fn list(&self) -> Result<Vec<RemoteFile>, WebDavError> {
        let body = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getcontentlength/><d:resourcetype/></d:prop></d:propfind>"#;
        let req = ureq::http::Request::builder()
            .method("PROPFIND")
            .uri(&self.base)
            .header("Authorization", self.auth())
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(body.to_string())
            .map_err(|e| WebDavError::BadUrl(e.to_string()))?;
        let mut res = self.agent.run(req).map_err(net)?;
        check(res.status().as_u16())?;
        let mut xml = String::new();
        res.body_mut()
            .as_reader()
            .take(MAX_LISTING)
            .read_to_string(&mut xml)
            .map_err(|e| WebDavError::Network(e.to_string()))?;
        let mut files = parse_listing(&xml)?;
        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(files)
    }

    /// The backups in the folder, oldest name first. Dated names sort by date.
    pub fn backups(&self) -> Result<Vec<RemoteFile>, WebDavError> {
        let mut f = self.list()?;
        f.retain(|x| x.name.starts_with(BACKUP_PREFIX) && x.name.ends_with(BACKUP_SUFFIX));
        Ok(f)
    }

    /// Uploads a file under `name`, replacing it only once the upload is complete.
    pub fn upload(&self, local: &Path, name: &str) -> Result<(), WebDavError> {
        let sent = local.metadata()?.len();
        let part = format!("{name}.part");
        let req = ureq::http::Request::builder()
            .method("PUT")
            .uri(self.url(&part))
            .header("Authorization", self.auth())
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", sent.to_string())
            .body(())
            .map_err(|e| WebDavError::BadUrl(e.to_string()))?;
        let body = ureq::SendBody::from_owned_reader(File::open(local)?);
        let (parts, ()) = req.into_parts();
        let res = self
            .agent
            .run(ureq::http::Request::from_parts(parts, body))
            .map_err(net)?;
        check(res.status().as_u16())?;
        let stored = self
            .list()?
            .into_iter()
            .find(|f| f.name == part)
            .and_then(|f| f.bytes);
        if stored != Some(sent) {
            let _ = self.delete(&part);
            return Err(WebDavError::Incomplete {
                sent,
                stored: stored.unwrap_or(0),
            });
        }
        let req = ureq::http::Request::builder()
            .method("MOVE")
            .uri(self.url(&part))
            .header("Authorization", self.auth())
            .header("Destination", self.url(name))
            .header("Overwrite", "T")
            .body(())
            .map_err(|e| WebDavError::BadUrl(e.to_string()))?;
        let res = self.agent.run(req).map_err(net)?;
        check(res.status().as_u16())
    }

    /// Downloads `name` into `dest`.
    pub fn download(&self, name: &str, dest: &mut impl Write) -> Result<u64, WebDavError> {
        let mut res = self
            .agent
            .get(self.url(name))
            .header("Authorization", self.auth())
            .call()
            .map_err(net)?;
        check(res.status().as_u16())?;
        io::copy(&mut res.body_mut().as_reader(), dest)
            .map_err(|e| WebDavError::Network(e.to_string()))
    }

    /// Deletes one file.
    pub fn delete(&self, name: &str) -> Result<(), WebDavError> {
        let res = self
            .agent
            .delete(self.url(name))
            .header("Authorization", self.auth())
            .call()
            .map_err(net)?;
        check(res.status().as_u16())
    }

    /// Deletes all but the newest `keep` backups. Other files are never touched.
    pub fn rotate(&self, keep: usize) -> Result<u32, WebDavError> {
        let backups = self.backups()?;
        let extra = backups.len().saturating_sub(keep);
        for f in &backups[..extra] {
            self.delete(&f.name)?;
        }
        Ok(extra as u32)
    }
}

fn net(e: ureq::Error) -> WebDavError {
    WebDavError::Network(e.to_string())
}

fn check(status: u16) -> Result<(), WebDavError> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(WebDavError::Unauthorized),
        s => Err(WebDavError::Status(s)),
    }
}
