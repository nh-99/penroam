//! Crash reports and the diagnostic report.
//!
//! Penroam has no telemetry and this module adds none: it never touches the network. When the
//! app panics, or the UI hits an unhandled error, a small report goes into `crash-reports/` next
//! to the library, and it stays there. The only way a report leaves the device is the user
//! copying the diagnostic report, which they read first, and pasting it somewhere.
//!
//! **Reports must never hold note content, paths, addresses or secrets.** Panic messages can
//! quote any of those, so everything passes through [`redact`] before it is written. A report
//! keeps the source location and the shape of the message, which is what a fix needs. Library
//! identifiers, account details and keys are not read here at all.
//!
//! The panic hook has to be safe in a failing process, so it ignores its own I/O errors, and
//! [`MAX_PER_RUN`] stops an error loop from writing to disk without end. Writing a report
//! changes nothing in the library. Docs: `docs/policies/privacy.md`.

use crate::AppState;
use penroam_sync_protocol::contract::ContractManifest;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};

/// Reports kept on disk; older ones are deleted.
const MAX_KEPT: usize = 20;
/// Reports written per run.
const MAX_PER_RUN: u32 = 5;
/// Longest message kept, in characters.
const MAX_MESSAGE: usize = 300;
/// First words of the file `tools/release/release.py notices` writes.
const NOTICES_START: &str = "Penroam is free software";

static DIR: OnceLock<PathBuf> = OnceLock::new();
static WRITTEN: AtomicU32 = AtomicU32::new(0);

/// One stored report. Every field is safe to show and share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    /// Milliseconds since the epoch.
    pub at_ms: u64,
    /// `panic` or `ui`.
    pub kind: String,
    /// App version.
    pub version: String,
    /// `os arch`.
    pub platform: String,
    /// Source file and line, or the UI component.
    pub location: String,
    /// The redacted message.
    pub message: String,
}

/// Is `token` something a report must not repeat?
fn sensitive(token: &str) -> Option<&'static str> {
    if token.contains('/') || token.contains('\\') {
        return Some("<path>");
    }
    if token.contains('@') {
        return Some("<address>");
    }
    let run = token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '='))
        .count();
    (run >= 24).then_some("<id>")
}

/// Removes what a report may not contain: quoted text, paths, addresses and long identifiers
/// or tokens. Quoted text goes first because panics print user data inside quotes. An
/// unterminated quote hides the rest of the message, since that is the safe reading of it.
pub fn redact(message: &str) -> String {
    let mut unquoted = String::with_capacity(message.len());
    let mut open: Option<char> = None;
    let mut before: Option<char> = None;
    let mut escaped = false;
    for c in message.chars() {
        match open {
            // `Debug` output writes a quote inside a string as `\"`.
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(q) if c == q => {
                open = None;
                unquoted.push_str("<text>");
            }
            Some(_) => {}
            // An apostrophe inside a word (`doesn't`) doesn't open a quote.
            None if matches!(c, '"' | '`') => open = Some(c),
            None if c == '\'' && !before.is_some_and(char::is_alphanumeric) => open = Some(c),
            None => unquoted.push(c),
        }
        before = Some(c);
    }
    if open.is_some() {
        unquoted.push_str("<text>");
    }
    let cleaned = unquoted
        .split_whitespace()
        .map(|t| sensitive(t).unwrap_or(t))
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.chars().take(MAX_MESSAGE).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn write(dir: &Path, kind: &str, location: &str, message: &str) -> Option<()> {
    if WRITTEN.fetch_add(1, Ordering::Relaxed) >= MAX_PER_RUN {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let report = CrashReport {
        at_ms: now_ms(),
        kind: kind.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        location: redact(location),
        message: redact(message),
    };
    let bytes = serde_json::to_vec(&report).ok()?;
    let name = format!("crash-{}-{}.json", report.at_ms, std::process::id());
    std::fs::write(dir.join(name), bytes).ok()?;
    prune(dir);
    Some(())
}

/// The stored reports, newest first. A file that isn't a report is skipped.
fn read_all(dir: &Path) -> Vec<CrashReport> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut reports: Vec<CrashReport> = entries
        .flatten()
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    reports.sort_by_key(|r| std::cmp::Reverse(r.at_ms));
    reports
}

fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // The timestamp is the start of the name, and all current timestamps have the same length.
    let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    names.sort();
    let excess = names.len().saturating_sub(MAX_KEPT);
    for old in names.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

/// Starts recording panics in `dir`. Panics still reach the previous hook.
pub fn install(dir: PathBuf) {
    if DIR.set(dir.clone()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map_or_else(
            || "unknown".to_string(),
            |l| format!("{}:{}", file_name(l.file()), l.line()),
        );
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("panic");
        let _ = write(&dir, "panic", &location, message);
        previous(info);
    }));
}

/// The last path component. Source paths in a build can contain the builder's home directory.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Records an unhandled UI error.
#[tauri::command]
pub fn diagnostics_ui_error(location: String, message: String) {
    if let Some(dir) = DIR.get() {
        let _ = write(dir, "ui", &location, &message);
    }
}

/// The text a user can read and choose to share: versions, platform, recovery counts and
/// redacted crash reports. `ink_path` is the name of the pen path in use.
#[tauri::command]
pub fn diagnostics_report(state: State<'_, AppState>, ink_path: Option<String>) -> String {
    let recovery = state.lib.lock().ok().map(|l| {
        (
            l.recovery.replayed_batches,
            l.recovery.truncated_journals,
            l.recovery.orphaned_files,
        )
    });
    let reports = DIR.get().map(|d| read_all(d)).unwrap_or_default();
    report_text(recovery, ink_path.as_deref().map(redact), &reports)
}

fn report_text(
    recovery: Option<(u32, u32, u32)>,
    ink_path: Option<String>,
    reports: &[CrashReport],
) -> String {
    let c = ContractManifest::current();
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut out = format!(
        "Penroam {} ({build})\nPlatform: {} {}\nProtocol: {}.{}\nFormats: page {}, journal {}, archive {}, notebook {}, page model {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        c.protocol.major,
        c.protocol.minor,
        c.formats.file_format,
        c.formats.journal_format,
        c.formats.archive_format,
        c.formats.notebook_schema,
        c.formats.page_schema,
    );
    if let Some(ink) = ink_path {
        out.push_str(&format!("Pen path: {ink}\n"));
    }
    if let Some((replayed, truncated, orphaned)) = recovery {
        out.push_str(&format!(
            "Last start-up recovery: {replayed} replayed, {truncated} discarded, {orphaned} set aside\n"
        ));
    }
    out.push_str(&format!("Crash reports: {}\n", reports.len()));
    for r in reports {
        out.push_str(&format!(
            "- {} {} {} {} {}: {}\n",
            r.at_ms, r.version, r.platform, r.kind, r.location, r.message
        ));
    }
    out
}

/// Deletes the stored crash reports.
#[tauri::command]
pub fn diagnostics_clear() -> Result<(), String> {
    let Some(dir) = DIR.get() else { return Ok(()) };
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

/// The notices for the third-party software in this build. Release builds generate them; a
/// development build has none, and the UI then hides its link.
#[tauri::command]
pub fn third_party_notices(app: AppHandle) -> Option<String> {
    let asset = app.asset_resolver().get("third-party-notices.txt".into())?;
    notices_text(asset.bytes)
}

/// The notices, if `bytes` are them. The asset resolver answers an unknown path with
/// `index.html`, so finding an asset doesn't mean the file is in this build.
fn notices_text(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes)
        .ok()
        .filter(|text| text.starts_with(NOTICES_START))
}

/// A page the app may open in the system browser. The renderer picks one of these and never
/// passes a URL.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HelpPage {
    /// Getting help and reporting a problem.
    Support,
    /// The privacy statement.
    Privacy,
    /// The App Store's subscription settings.
    Subscriptions,
    /// The terms of use for store purchases: Apple's standard licence agreement.
    Terms,
}

/// Opens `page` in the system browser. Errors mean the browser could not be started.
#[tauri::command]
pub fn open_help(app: AppHandle, page: HelpPage) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = match page {
        HelpPage::Support => "https://github.com/nh-99/penroam/blob/main/docs/support.md",
        HelpPage::Privacy => "https://github.com/nh-99/penroam/blob/main/docs/policies/privacy.md",
        HelpPage::Subscriptions => "https://apps.apple.com/account/subscriptions",
        HelpPage::Terms => "https://www.apple.com/legal/internet-services/itunes/dev/stdeula/",
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_removes_quoted_text_paths_addresses_and_tokens() {
        let message = r#"called `Result::unwrap()` on an `Err` value: Io("my secret note") at /home/ada/notes/diary.prp for ada@example.com with token eyJhbGciOiJIUzI1NiIsInR5cCI6"#;
        let out = redact(message);
        for leaked in ["secret", "ada", "diary", "example", "eyJhbG", "unwrap"] {
            assert!(!out.contains(leaked), "{leaked} leaked: {out}");
        }
        assert!(out.contains("<path>") && out.contains("<address>") && out.contains("<id>"));
    }

    #[test]
    fn redaction_fails_closed_on_an_open_quote() {
        assert_eq!(
            redact("bad value: \"never closed secret"),
            "bad value: <text>"
        );
        assert_eq!(
            redact("bad value: 'never closed secret"),
            "bad value: <text>"
        );
    }

    #[test]
    fn escaped_quotes_stay_inside_the_quoted_text() {
        let out = redact(r#"Io("say \"dear diary entry\" now") failed"#);
        assert_eq!(out, "Io(<text>) failed");
    }

    #[test]
    fn redaction_keeps_ordinary_messages_and_limits_length() {
        assert_eq!(
            redact("index 3 out of range, doesn't fit"),
            "index 3 out of range, doesn't fit"
        );
        assert_eq!(redact(&"word ".repeat(200)).chars().count(), MAX_MESSAGE);
    }

    #[test]
    fn the_app_page_is_not_mistaken_for_the_notices() {
        assert_eq!(notices_text(b"<!doctype html><html>".to_vec()), None);
        assert_eq!(notices_text(vec![0xff, 0xfe]), None);
        let text = format!("{NOTICES_START} under the MPL");
        assert_eq!(notices_text(text.clone().into_bytes()), Some(text));
    }

    #[test]
    fn source_locations_lose_the_build_directory() {
        assert_eq!(file_name("/home/runner/work/src/sync.rs"), "sync.rs");
        assert_eq!(file_name(r"C:\build\src\sync.rs"), "sync.rs");
    }

    #[test]
    fn reports_round_trip_and_old_ones_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_KEPT + 3) {
            let name = format!("crash-{:013}-1.json", i);
            let r = CrashReport {
                at_ms: i as u64,
                kind: "panic".into(),
                version: "1".into(),
                platform: "linux".into(),
                location: "a.rs:1".into(),
                message: "m".into(),
            };
            std::fs::write(dir.path().join(name), serde_json::to_vec(&r).unwrap()).unwrap();
        }
        std::fs::write(dir.path().join("zz-not-a-report"), b"x").unwrap();
        prune(dir.path());
        let kept = read_all(dir.path());
        // the stray file counts towards the limit and survives, sorting last
        assert_eq!(kept.len(), MAX_KEPT - 1);
        assert_eq!(kept[0].at_ms, (MAX_KEPT + 2) as u64);
    }

    #[test]
    fn the_report_text_holds_only_redacted_fields() {
        let r = CrashReport {
            at_ms: 5,
            kind: "ui".into(),
            version: "1".into(),
            platform: "linux x86_64".into(),
            location: "Editor".into(),
            message: redact("failed on \"private\""),
        };
        let text = report_text(Some((1, 2, 3)), Some("Web canvas".into()), &[r]);
        assert!(text.contains("1 replayed, 2 discarded, 3 set aside"));
        assert!(text.contains("Pen path: Web canvas"));
        assert!(text.contains("Crash reports: 1"));
        assert!(!text.contains("private"));
    }

    /// Runs a real panic through the hook. This is the only test that installs it.
    #[test]
    fn a_panic_leaves_a_redacted_report() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path().to_path_buf());
        let quiet = std::panic::catch_unwind(|| panic!("broke on {:?}", "secret words"));
        assert!(quiet.is_err());
        let reports = read_all(dir.path());
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].kind, "panic");
        assert!(reports[0].location.starts_with("diagnostics.rs:"));
        assert!(!reports[0].message.contains("secret"));
    }
}
