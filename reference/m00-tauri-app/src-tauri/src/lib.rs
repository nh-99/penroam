//! Minimal Rust backend for the M00 Tauri prototype: file persistence for
//! stroke/perf JSON, system/process information for benchmarks, and the
//! BOOX ink plugin registration. Drawing never goes through Rust.

use serde::Serialize;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

static STARTED: OnceLock<Instant> = OnceLock::new();

fn data_dir(app: &AppHandle, sub: &str) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = base.join(sub);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

fn stamp() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn safe_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(format!("invalid file name: {name}"));
    }
    Ok(name)
}

#[tauri::command]
fn save_strokes(app: AppHandle, json: String) -> Result<String, String> {
    let path = data_dir(&app, "strokes")?.join(format!("strokes-{}.json", stamp()));
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

#[tauri::command]
fn list_strokes(app: AppHandle) -> Result<Vec<String>, String> {
    let dir = data_dir(&app, "strokes")?;
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    names.reverse();
    Ok(names)
}

#[tauri::command]
fn load_strokes(app: AppHandle, name: String) -> Result<String, String> {
    let path = data_dir(&app, "strokes")?.join(safe_name(&name)?);
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

/// Android: app-specific external storage (/sdcard/Android/data/<id>/files/<sub>),
/// created by the BOOX plugin at load, so `adb pull` works on release builds.
/// Same location as the native reference and Flutter apps.
#[cfg(target_os = "android")]
fn android_external_dir(app: &AppHandle, sub: &str) -> Option<PathBuf> {
    let id = &app.config().identifier;
    let base = PathBuf::from(format!("/storage/emulated/0/Android/data/{id}/files"));
    if !base.is_dir() { return None; }
    let dir = base.join(sub);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(not(target_os = "android"))]
fn android_external_dir(_app: &AppHandle, _sub: &str) -> Option<PathBuf> { None }

#[tauri::command]
fn write_perf(app: AppHandle, label: String, json: String) -> Result<String, String> {
    let label: String = label.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    // M00_RESULTS_DIR lets benchmark runs write straight into benchmarks/results.
    let dir = match std::env::var("M00_RESULTS_DIR") {
        Ok(d) if !d.is_empty() => {
            std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            PathBuf::from(d)
        }
        _ => android_external_dir(&app, "perf").map_or_else(|| data_dir(&app, "perf"), Ok)?,
    };
    let path = dir.join(format!("perf-{label}-{}.json", stamp()));
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemInfo {
    os: &'static str,
    arch: &'static str,
    family: &'static str,
    kernel: Option<String>,
    webview_version: Option<String>,
    tauri_version: &'static str,
    app_version: String,
    session_type: Option<String>,
    desktop: Option<String>,
}

#[tauri::command]
fn system_info(app: AppHandle) -> SystemInfo {
    SystemInfo {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        family: std::env::consts::FAMILY,
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|s| s.trim().to_string()),
        // On Linux this is the WebKitGTK version, on Android the WebView version.
        webview_version: tauri::webview_version().ok(),
        tauri_version: tauri::VERSION,
        app_version: app.package_info().version.to_string(),
        session_type: std::env::var("XDG_SESSION_TYPE").ok(),
        desktop: std::env::var("XDG_CURRENT_DESKTOP").ok(),
    }
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProcInfo {
    pid: u32,
    name: String,
    rss_bytes: u64,
    cpu_seconds: f64,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProcStats {
    available: bool,
    pid: u32,
    rss_bytes: u64,
    cpu_seconds: f64,
    tree_rss_bytes: u64,
    tree_cpu_seconds: f64,
    processes: Vec<ProcInfo>,
    note: Option<String>,
}

/// Reads /proc for this process and all descendants. On Linux the WebKitGTK
/// web/network/GPU processes are children of the app, so the tree total is
/// the meaningful number. On Android the WebView renderer runs in a separate
/// sandboxed process that is not visible here; only the app process is counted.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_proc_tree() -> ProcStats {
    use std::collections::HashMap;
    const CLK_TCK: f64 = 100.0;
    fn stat(pid: u32) -> Option<(String, u32, f64)> {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let open = s.find('(')?;
        let close = s.rfind(')')?;
        let name = s[open + 1..close].to_string();
        let rest: Vec<&str> = s[close + 2..].split_whitespace().collect();
        let ppid = rest.get(1)?.parse().ok()?;
        let ut: f64 = rest.get(11)?.parse().ok()?;
        let st: f64 = rest.get(12)?.parse().ok()?;
        Some((name, ppid, (ut + st) / CLK_TCK))
    }
    fn rss(pid: u32) -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).map(|l| l.to_string()))
            .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()))
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    }
    let me = std::process::id();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut names: HashMap<u32, (String, f64)> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            if let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() {
                if let Some((name, ppid, cpu)) = stat(pid) {
                    children.entry(ppid).or_default().push(pid);
                    names.insert(pid, (name, cpu));
                }
            }
        }
    }
    let mut out = ProcStats { available: true, pid: me, ..Default::default() };
    let mut stack = vec![me];
    while let Some(pid) = stack.pop() {
        let (name, cpu) = names.get(&pid).cloned().unwrap_or_default();
        let r = rss(pid);
        if pid == me {
            out.rss_bytes = r;
            out.cpu_seconds = cpu;
        }
        out.tree_rss_bytes += r;
        out.tree_cpu_seconds += cpu;
        out.processes.push(ProcInfo { pid, name, rss_bytes: r, cpu_seconds: cpu });
        if let Some(c) = children.get(&pid) {
            stack.extend(c);
        }
    }
    if cfg!(target_os = "android") {
        out.note = Some("Android: WebView renderer process memory is not included".into());
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn read_proc_tree() -> ProcStats {
    ProcStats { pid: std::process::id(), note: Some("process stats not implemented on this OS".into()), ..Default::default() }
}

#[tauri::command]
fn proc_stats() -> ProcStats {
    read_proc_tree()
}

/// Milliseconds from process start (Linux/Android: /proc, same method as
/// the Flutter app) to the first call from the frontend. Falls back to time
/// since Rust entry on other platforms.
#[tauri::command]
fn startup_ms() -> f64 {
    process_age_ms().unwrap_or_else(|| STARTED.get().map(|t| t.elapsed().as_secs_f64() * 1000.0).unwrap_or(-1.0))
}

fn process_age_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest: Vec<&str> = stat[stat.rfind(')')? + 2..].split_whitespace().collect();
    let start_ticks: f64 = rest.get(19)?.parse().ok()?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime").ok()?.split_whitespace().next()?.parse().ok()?;
    Some((uptime - start_ticks / 100.0) * 1000.0)
}

#[tauri::command]
fn autobench_requested() -> bool {
    std::env::var("M00_AUTOBENCH").map(|v| v == "1").unwrap_or(false)
}

#[tauri::command]
fn exit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    STARTED.get_or_init(Instant::now);
    tauri::Builder::default()
        .plugin(tauri_plugin_boox_ink::init())
        .plugin(tauri_plugin_pencilkit::init())
        .invoke_handler(tauri::generate_handler![
            save_strokes,
            list_strokes,
            load_strokes,
            write_perf,
            system_info,
            proc_stats,
            startup_ms,
            autobench_requested,
            exit_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        assert!(safe_name("../etc/passwd").is_err());
        assert!(safe_name("a/b.json").is_err());
        assert!(safe_name("strokes-1.json").is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_tree_includes_self() {
        let s = read_proc_tree();
        assert!(s.available);
        assert!(s.rss_bytes > 0);
        assert!(s.processes.iter().any(|p| p.pid == std::process::id()));
    }
}
