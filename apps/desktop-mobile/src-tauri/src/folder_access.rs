//! Keeps access to folders the user chose, across restarts, in the Mac App Store build.
//!
//! Store apps run in the App Sandbox. A folder picked in the open panel is reachable only until
//! the app quits, so the automatic-backup folder would stop working after a restart. The fix
//! Apple provides is a security-scoped bookmark: made while access is granted, saved, and resolved
//! at the next launch to get access back.
//!
//! [`remember`] is called right after the folder picker returns, and saves a bookmark per folder
//! in `folder-bookmarks.json` in the app's data folder. [`open`] resolves the saved bookmark and
//! starts access, which lasts until the returned guard is dropped. A bookmark that has gone stale
//! (the folder was moved or renamed) is resolved anyway when macOS can, and replaced with a fresh
//! one.
//!
//! In every other build there is no sandbox, so both are no-ops and paths are used as they are.

use tauri::AppHandle;

/// Saves access to `dir`, which the user has just chosen. Does nothing outside the Mac App Store
/// build.
///
/// # Errors
/// A readable message if the bookmark couldn't be made or saved. The folder still works until
/// the app quits.
#[tauri::command(async)]
pub fn remember_folder(app: AppHandle, dir: String) -> Result<(), String> {
    imp::remember(&app, &dir)
}

/// Access to a remembered folder, held until this is dropped.
pub struct Access(#[allow(dead_code)] imp::Guard);

/// Starts access to `dir` if it was remembered. Without a bookmark (or outside the Mac App Store
/// build) this still succeeds, and the folder is used as it is: then reading or writing it fails
/// in the sandbox with an ordinary permission error.
pub fn open(app: &AppHandle, dir: &str) -> Access {
    Access(imp::open(app, dir))
}

#[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
mod imp {
    use tauri::AppHandle;

    pub struct Guard;

    pub fn remember(_app: &AppHandle, _dir: &str) -> Result<(), String> {
        Ok(())
    }

    pub fn open(_app: &AppHandle, _dir: &str) -> Guard {
        Guard
    }
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_foundation::{
        NSData, NSString, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tauri::{AppHandle, Manager};

    const FILE: &str = "folder-bookmarks.json";

    /// Stops access when dropped.
    pub struct Guard(Option<Retained<NSURL>>);

    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(url) = &self.0 {
                // SAFETY: balanced with the successful start in `open`.
                unsafe { url.stopAccessingSecurityScopedResource() };
            }
        }
    }

    fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
        Ok(app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join(FILE))
    }

    /// Folder path to hex-encoded bookmark bytes.
    fn load(app: &AppHandle) -> BTreeMap<String, String> {
        store_path(app)
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(app: &AppHandle, map: &BTreeMap<String, String>) -> Result<(), String> {
        let path = store_path(app)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(map).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(s: &str) -> Option<Vec<u8>> {
        (0..s.len())
            .step_by(2)
            .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
            .collect()
    }

    fn bookmark(url: &NSURL) -> Result<Vec<u8>, String> {
        url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
            NSURLBookmarkCreationOptions::WithSecurityScope,
            None,
            None,
        )
        .map(|d| d.to_vec())
        .map_err(|e| e.localizedDescription().to_string())
    }

    pub fn remember(app: &AppHandle, dir: &str) -> Result<(), String> {
        let url = NSURL::fileURLWithPath(&NSString::from_str(dir));
        let data = bookmark(&url)?;
        let mut map = load(app);
        map.insert(dir.to_owned(), hex(&data));
        save(app, &map)
    }

    pub fn open(app: &AppHandle, dir: &str) -> Guard {
        let mut map = load(app);
        let Some(bytes) = map.get(dir).and_then(|h| unhex(h)) else {
            return Guard(None);
        };
        let data = NSData::with_bytes(&bytes);
        let mut stale = Bool::NO;
        // SAFETY: `stale` outlives the call, which only writes a BOOL through the pointer.
        let url = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &data,
                NSURLBookmarkResolutionOptions::WithSecurityScope,
                None,
                &mut stale,
            )
        };
        let Ok(url) = url else {
            log::warn!("a saved folder bookmark no longer resolves");
            return Guard(None);
        };
        // SAFETY: plain message send; balanced by `Guard::drop` when it returns true.
        if !unsafe { url.startAccessingSecurityScopedResource() } {
            return Guard(None);
        }
        if stale.as_bool() {
            // Access is held now, so a fresh bookmark can be made for next time.
            if let Ok(fresh) = bookmark(&url) {
                map.insert(dir.to_owned(), hex(&fresh));
                let _ = save(app, &map);
            }
        }
        Guard(Some(url))
    }
}
