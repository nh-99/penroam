//! Measurement helpers carried over from the M00 harness (same methods, so
//! M01 numbers are comparable with `benchmarks/results/linux/*`).

use serde::Serialize;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

static STARTED: OnceLock<Instant> = OnceLock::new();

pub fn mark_start() {
    STARTED.get_or_init(Instant::now);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    os: &'static str,
    arch: &'static str,
    kernel: Option<String>,
    webview_version: Option<String>,
    tauri_version: &'static str,
    app_version: String,
    session_type: Option<String>,
    desktop: Option<String>,
}

#[tauri::command]
pub fn system_info(app: AppHandle) -> SystemInfo {
    SystemInfo {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|s| s.trim().to_string()),
        webview_version: tauri::webview_version().ok(),
        tauri_version: tauri::VERSION,
        app_version: app.package_info().version.to_string(),
        session_type: std::env::var("XDG_SESSION_TYPE").ok(),
        desktop: std::env::var("XDG_CURRENT_DESKTOP").ok(),
    }
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProcStats {
    available: bool,
    pid: u32,
    rss_bytes: u64,
    /// This process plus all descendants (WebKitGTK web/GPU/network processes on Linux).
    tree_rss_bytes: u64,
    processes: u32,
    note: Option<String>,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_proc_tree() -> ProcStats {
    use std::collections::HashMap;
    fn ppid(pid: u32) -> Option<u32> {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest: Vec<&str> = s[s.rfind(')')? + 2..].split_whitespace().collect();
        rest.get(1)?.parse().ok()
    }
    fn rss(pid: u32) -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()))
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    }
    let me = std::process::id();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            if let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() {
                if let Some(p) = ppid(pid) {
                    children.entry(p).or_default().push(pid);
                }
            }
        }
    }
    let mut out = ProcStats { available: true, pid: me, ..Default::default() };
    let mut stack = vec![me];
    while let Some(pid) = stack.pop() {
        let r = rss(pid);
        if pid == me {
            out.rss_bytes = r;
        }
        out.tree_rss_bytes += r;
        out.processes += 1;
        if let Some(c) = children.get(&pid) {
            stack.extend(c);
        }
    }
    if cfg!(target_os = "android") {
        out.note = Some("Android: the WebView renderer process is not included".into());
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn read_proc_tree() -> ProcStats {
    ProcStats { pid: std::process::id(), note: Some("process stats not implemented on this OS".into()), ..Default::default() }
}

#[tauri::command]
pub fn proc_stats() -> ProcStats {
    read_proc_tree()
}

/// Milliseconds from process start (/proc, as in M00) to this call.
#[tauri::command]
pub fn startup_ms() -> f64 {
    process_age_ms().unwrap_or_else(|| STARTED.get().map(|t| t.elapsed().as_secs_f64() * 1000.0).unwrap_or(-1.0))
}

fn process_age_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest: Vec<&str> = stat[stat.rfind(')')? + 2..].split_whitespace().collect();
    let start_ticks: f64 = rest.get(19)?.parse().ok()?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime").ok()?.split_whitespace().next()?.parse().ok()?;
    Some((uptime - start_ticks / 100.0) * 1000.0)
}

/// Writes a benchmark result file to `PENROAM_RESULTS_DIR` (or the app data
/// `perf/` directory; on Android the app's external files dir so `adb pull` works).
#[tauri::command]
pub fn write_perf(app: AppHandle, label: String, json: String) -> Result<String, String> {
    let label: String = label.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let dir = match std::env::var("PENROAM_RESULTS_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => android_external(&app).unwrap_or(app.path().app_data_dir().map_err(|e| e.to_string())?.join("perf")),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let path = dir.join(format!("perf-{label}-{stamp}.json"));
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

fn android_external(app: &AppHandle) -> Option<PathBuf> {
    if !cfg!(target_os = "android") {
        return None;
    }
    let base = PathBuf::from(format!("/storage/emulated/0/Android/data/{}/files", app.config().identifier));
    base.is_dir().then(|| base.join("perf"))
}

/// `PENROAM_AUTOBENCH=1` runs the scripted benchmark on start-up and exits.
#[tauri::command]
pub fn autobench_requested() -> Option<String> {
    std::env::var("PENROAM_AUTOBENCH").ok().filter(|v| !v.is_empty() && v != "0")
}
