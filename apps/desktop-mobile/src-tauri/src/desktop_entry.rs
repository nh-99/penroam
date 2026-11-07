//! App-menu entry for the Linux AppImage.
//!
//! An AppImage is a single file the user downloads and runs, so unlike the `.deb` nothing puts
//! Penroam in the desktop's app menu. When the app runs from an AppImage (the AppImage runtime
//! sets `APPIMAGE` to the file's absolute path), [`install_on_launch`] writes a desktop entry
//! and an icon into the user's data directory (`$XDG_DATA_HOME`, by default `~/.local/share`):
//!
//! * `applications/ink.penroam.app.desktop`, named after the app identifier, which is also the
//!   Wayland app id, so the window is matched to the entry and gets its icon;
//! * `icons/hicolor/256x256/apps/ink.penroam.app.png`.
//!
//! It runs on every launch but only writes when the content would change, so moving the AppImage
//! and starting it again points the entry at the new path. `TryExec` names the AppImage, so if
//! the file is deleted the desktop hides the leftover entry.
//!
//! Only builds from the release workflow, which sets `PENROAM_RELEASE_BUILD` at compile time, use
//! the names above. Every other build (a developer's local `pnpm tauri build`) is listed as
//! "Penroam Developer" under `ink.penroam.app.developer`, with its own removal marker, so it sits
//! beside an installed release instead of replacing its entry.
//!
//! Settings → Advanced can remove the entry (for users about to delete the AppImage) and add it
//! back. Removing it leaves a marker file in the app data directory so the next launch doesn't
//! put it straight back; adding it deletes the marker. Only an entry carrying
//! [`MARKER`] is ever overwritten or deleted, so a hand-written `ink.penroam.app.desktop` is
//! left alone. Other platforms and the `.deb` report `supported: false` and do nothing.
//! Docs: `docs/platforms/linux.md`.

use serde::Serialize;
use tauri::AppHandle;

/// What Settings shows about the app-menu entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopEntryStatus {
    /// Penroam is running from an AppImage on Linux. Otherwise the UI shows nothing.
    pub supported: bool,
    /// Penroam's entry is in the app menu.
    pub installed: bool,
}

/// Key written into every entry this module creates; nothing without it is touched.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const MARKER: &str = "X-Penroam-AppImage=true";

/// Whether the app-menu entry can be managed here and whether it is installed.
#[tauri::command]
pub fn desktop_entry_status(app: AppHandle) -> DesktopEntryStatus {
    imp::status(&app)
}

/// Adds Penroam to the app menu and forgets an earlier removal. Errors are messages for the
/// user: not running from an AppImage, or the files could not be written.
#[tauri::command]
pub fn desktop_entry_install(app: AppHandle) -> Result<DesktopEntryStatus, String> {
    imp::install(&app)
}

/// Removes Penroam's entry and icon from the app menu and stops later launches from adding them
/// again. Errors are messages for the user.
#[tauri::command]
pub fn desktop_entry_remove(app: AppHandle) -> Result<DesktopEntryStatus, String> {
    imp::remove(&app)
}

/// Writes or refreshes the entry when running from an AppImage, unless the user removed it.
/// Runs on a background thread; failures are logged and never stop the app.
pub fn install_on_launch(app: &AppHandle) {
    imp::install_on_launch(app);
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{DesktopEntryStatus, MARKER};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tauri::{AppHandle, Manager};

    // 256×256, the largest size the bundle has as a PNG.
    const ICON: &[u8] = include_bytes!("../icons/128x128@2x.png");

    /// The names one kind of build uses, so a developer build never touches the release's files.
    pub(super) struct Flavour {
        /// `Name=` in the menu.
        pub name: &'static str,
        /// Base name of the desktop entry and the icon.
        pub id: &'static str,
        /// Marker file in the app data directory recording that the user removed the entry.
        /// Both builds share that directory (same identifier), so each needs its own.
        pub opted_out: &'static str,
    }

    /// Builds made by the release workflow.
    pub(super) const RELEASE: Flavour = Flavour {
        name: "Penroam",
        id: "ink.penroam.app",
        opted_out: "desktop-entry-removed",
    };

    /// Every other build.
    pub(super) const DEVELOPER: Flavour = Flavour {
        name: "Penroam Developer",
        id: "ink.penroam.app.developer",
        opted_out: "developer-desktop-entry-removed",
    };

    fn flavour() -> &'static Flavour {
        if option_env!("PENROAM_RELEASE_BUILD").is_some() {
            &RELEASE
        } else {
            &DEVELOPER
        }
    }

    /// Where the files go, worked out once per call so tests can point them elsewhere.
    pub(super) struct Paths {
        pub entry: PathBuf,
        pub icon: PathBuf,
        pub opted_out: PathBuf,
        pub flavour: &'static Flavour,
    }

    impl Paths {
        pub(super) fn new(data_home: &Path, app_data: &Path, flavour: &'static Flavour) -> Self {
            let id = flavour.id;
            Paths {
                entry: data_home.join("applications").join(format!("{id}.desktop")),
                icon: data_home
                    .join("icons/hicolor/256x256/apps")
                    .join(format!("{id}.png")),
                opted_out: app_data.join(flavour.opted_out),
                flavour,
            }
        }
    }

    /// The AppImage this process was started from, if any.
    fn appimage() -> Option<PathBuf> {
        let path = PathBuf::from(std::env::var_os("APPIMAGE")?);
        (path.is_absolute() && path.is_file()).then_some(path)
    }

    /// `$XDG_DATA_HOME`, or `~/.local/share` when it is unset or relative (as the spec says).
    fn data_home() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .map(|h| h.join(".local/share"))
            })
    }

    fn paths(app: &AppHandle) -> Option<Paths> {
        Some(Paths::new(
            &data_home()?,
            &app.path().app_data_dir().ok()?,
            flavour(),
        ))
    }

    /// `Exec` argument quoting from the Desktop Entry spec: the path goes in double quotes with
    /// `"`, `` ` ``, `$` and `\` backslash-escaped, `%` is doubled so it isn't a field code, and
    /// then the string-value escaping doubles every backslash again.
    pub(super) fn exec_arg(path: &str) -> String {
        let mut quoted = String::from("\"");
        for c in path.chars() {
            match c {
                '"' | '`' | '$' | '\\' => {
                    quoted.push('\\');
                    quoted.push(c);
                }
                '%' => quoted.push_str("%%"),
                _ => quoted.push(c),
            }
        }
        quoted.push('"');
        quoted.replace('\\', "\\\\")
    }

    /// The entry for an AppImage at `appimage`. `None` for paths a desktop file can't hold
    /// (not UTF-8, or with control characters such as a newline).
    pub(super) fn entry(appimage: &Path, flavour: &Flavour) -> Option<String> {
        let path = appimage.to_str()?;
        if path.chars().any(char::is_control) {
            return None;
        }
        Some(format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={name}\n\
             GenericName=Handwriting notebook\n\
             Comment=Offline-first handwriting notebook\n\
             Exec={exec}\n\
             TryExec={try_exec}\n\
             Icon={id}\n\
             Terminal=false\n\
             Categories=Office;\n\
             StartupWMClass=penroam-app\n\
             {MARKER}\n",
            name = flavour.name,
            id = flavour.id,
            exec = exec_arg(path),
            try_exec = path.replace('\\', "\\\\"),
        ))
    }

    /// Whether the file at `entry` is one this module wrote.
    fn ours(entry: &Path) -> bool {
        std::fs::read_to_string(entry).is_ok_and(|s| s.lines().any(|l| l.trim() == MARKER))
    }

    /// Writes `bytes` to `path` unless it already holds them, via a temporary file and a rename
    /// so the menu never reads half a file.
    fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if std::fs::read(path).is_ok_and(|old| old == bytes) {
            return Ok(());
        }
        let dir = path.parent().expect("entry paths have a parent");
        std::fs::create_dir_all(dir)?;
        let name = path.file_name().expect("entry paths have a file name");
        let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// Writes the entry and icon. Refuses to replace an entry it did not write.
    pub(super) fn write(paths: &Paths, appimage: &Path) -> Result<(), String> {
        let text = entry(appimage, paths.flavour)
            .ok_or("The AppImage's location can't be used in an app-menu entry. Move it to a folder whose name has only ordinary characters.")?;
        if paths.entry.exists() && !ours(&paths.entry) {
            return Err(format!(
                "{} already exists and wasn't made by Penroam, so it was left alone.",
                paths.entry.display()
            ));
        }
        let failed = |e: std::io::Error| format!("Could not add Penroam to the app menu: {e}");
        write_if_changed(&paths.icon, ICON).map_err(failed)?;
        write_if_changed(&paths.entry, text.as_bytes()).map_err(failed)
    }

    /// Deletes the entry (only if it is ours) and the icon.
    pub(super) fn delete(paths: &Paths) -> Result<(), String> {
        let failed = |e: std::io::Error| format!("Could not remove Penroam from the app menu: {e}");
        let gone = |r: std::io::Result<()>| match r {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        if ours(&paths.entry) {
            gone(std::fs::remove_file(&paths.entry)).map_err(failed)?;
        }
        gone(std::fs::remove_file(&paths.icon)).map_err(failed)
    }

    pub(super) fn installed(paths: &Paths) -> bool {
        ours(&paths.entry)
    }

    pub fn status(app: &AppHandle) -> DesktopEntryStatus {
        match (appimage(), paths(app)) {
            (Some(_), Some(p)) => DesktopEntryStatus {
                supported: true,
                installed: installed(&p),
            },
            _ => DesktopEntryStatus {
                supported: false,
                installed: false,
            },
        }
    }

    fn not_appimage() -> String {
        "Penroam isn't running from an AppImage, so it can't manage its app-menu entry.".into()
    }

    pub fn install(app: &AppHandle) -> Result<DesktopEntryStatus, String> {
        let image = appimage().ok_or_else(not_appimage)?;
        let p = paths(app).ok_or_else(not_appimage)?;
        write(&p, &image)?;
        // Only forget the removal once the entry is back.
        let _ = std::fs::remove_file(&p.opted_out);
        Ok(status(app))
    }

    pub fn remove(app: &AppHandle) -> Result<DesktopEntryStatus, String> {
        let p = paths(app).ok_or_else(not_appimage)?;
        // Record the choice first: if deleting fails half-way, the next launch still won't
        // re-add what the user asked to remove.
        if let Some(dir) = p.opted_out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&p.opted_out, b"").map_err(|e| e.to_string())?;
        delete(&p)?;
        Ok(status(app))
    }

    pub fn install_on_launch(app: &AppHandle) {
        let Some(image) = appimage() else { return };
        let Some(p) = paths(app) else { return };
        if p.opted_out.exists() {
            return;
        }
        std::thread::spawn(move || {
            if let Err(e) = write(&p, &image) {
                eprintln!("penroam: app-menu entry: {e}");
            }
        });
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::DesktopEntryStatus;
    use tauri::AppHandle;

    const UNSUPPORTED: &str = "Only the Linux AppImage adds itself to the app menu.";

    pub fn status(_: &AppHandle) -> DesktopEntryStatus {
        DesktopEntryStatus {
            supported: false,
            installed: false,
        }
    }

    pub fn install(_: &AppHandle) -> Result<DesktopEntryStatus, String> {
        Err(UNSUPPORTED.into())
    }

    pub fn remove(_: &AppHandle) -> Result<DesktopEntryStatus, String> {
        Err(UNSUPPORTED.into())
    }

    pub fn install_on_launch(_: &AppHandle) {}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::imp::{delete, entry, exec_arg, installed, write, Paths, DEVELOPER, RELEASE};
    use std::path::Path;

    #[test]
    fn exec_quotes_and_escapes_the_path() {
        assert_eq!(
            exec_arg("/opt/Penroam.AppImage"),
            "\"/opt/Penroam.AppImage\""
        );
        assert_eq!(exec_arg("/home/a b/P.AppImage"), "\"/home/a b/P.AppImage\"");
        assert_eq!(exec_arg("/x/100%/\"$`"), "\"/x/100%%/\\\\\"\\\\$\\\\`\"");
        // A literal backslash takes four in the file.
        assert_eq!(exec_arg("/a\\b"), "\"/a\\\\\\\\b\"");
    }

    #[test]
    fn paths_a_desktop_file_cannot_hold_are_refused() {
        assert!(entry(Path::new("/tmp/a\nb.AppImage"), &RELEASE).is_none());
        assert!(entry(Path::new("/tmp/ok.AppImage"), &RELEASE).is_some());
    }

    #[test]
    fn writes_refreshes_and_removes_only_its_own_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(
            &dir.path().join("share"),
            &dir.path().join("data"),
            &RELEASE,
        );
        let first = dir.path().join("Penroam.AppImage");

        write(&p, &first).unwrap();
        assert!(installed(&p));
        assert!(p.icon.is_file());
        let text = std::fs::read_to_string(&p.entry).unwrap();
        assert!(text.contains(&format!("TryExec={}\n", first.display())));

        // Moving the AppImage and launching again points the entry at the new file.
        let moved = dir.path().join("Apps/Penroam.AppImage");
        write(&p, &moved).unwrap();
        let text = std::fs::read_to_string(&p.entry).unwrap();
        assert!(text.contains(&format!("TryExec={}\n", moved.display())));

        delete(&p).unwrap();
        assert!(!p.entry.exists() && !p.icon.exists());
        delete(&p).unwrap(); // nothing left to remove is fine

        // An entry someone else wrote is neither replaced nor deleted.
        std::fs::write(&p.entry, "[Desktop Entry]\nName=Mine\n").unwrap();
        assert!(write(&p, &first).is_err());
        delete(&p).unwrap();
        assert!(p.entry.exists());
        assert!(!installed(&p));
    }

    #[test]
    fn developer_builds_sit_beside_the_release_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (share, data) = (dir.path().join("share"), dir.path().join("data"));
        let release = Paths::new(&share, &data, &RELEASE);
        let developer = Paths::new(&share, &data, &DEVELOPER);
        assert_ne!(release.entry, developer.entry);
        assert_ne!(release.icon, developer.icon);
        assert_ne!(release.opted_out, developer.opted_out);

        let image = dir.path().join("Penroam.AppImage");
        write(&release, &image).unwrap();
        write(&developer, &image).unwrap();
        let text = std::fs::read_to_string(&developer.entry).unwrap();
        assert!(text.contains("Name=Penroam Developer\n"));
        assert!(text.contains("Icon=ink.penroam.app.developer\n"));
        assert!(std::fs::read_to_string(&release.entry)
            .unwrap()
            .contains("Name=Penroam\n"));

        // Removing one leaves the other.
        delete(&developer).unwrap();
        assert!(installed(&release) && !installed(&developer));
    }
}
