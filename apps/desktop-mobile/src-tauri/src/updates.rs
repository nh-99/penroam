//! In-app updates for desktop builds, on top of `tauri-plugin-updater`.
//!
//! Each channel has one signed `latest.json` on GitHub Releases, and the update it names is
//! checked against the minisign public key in `tauri.conf.json` before anything is installed.
//! GitHub and TLS are therefore not what makes an update trustworthy. The signature also has to
//! carry the version it was made for (`requireSignedVersion`), so a forged `latest.json` can't
//! pair a high version number with an old, genuinely signed build to force a downgrade.
//!
//! **Until a public key is committed this module does nothing:** [`update_support`] reports
//! `enabled: false` and the app shows no update controls. Mobile builds never use it, because
//! their updates come from the stores or from a new sideloaded APK.
//!
//! The app makes no update request unless the user clicks "Check for updates" or has turned on
//! the automatic check. The order in [`update_install`] matters: on Windows the installer ends
//! this process, so the library is flushed after the download and before the install. The
//! frontend flushes the editor before it calls the command.
//!
//! Local notes don't depend on this. They live outside the app bundle, and a build that meets a
//! newer format refuses it rather than rewriting it, so installing an older build by hand is a
//! safe way back. Docs: `docs/development.md#releases`.

use serde::Serialize;
use tauri::AppHandle;

/// What this build can do about updates.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSupport {
    /// This is a desktop build with an update key. Without it the UI shows no update controls.
    pub enabled: bool,
}

/// An update the channel offers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferedUpdate {
    /// Version on offer.
    pub version: String,
    /// Release notes, as published.
    pub notes: Option<String>,
}

/// `true` when `tauri.conf.json` holds a usable public key. Pure so it can be tested.
#[cfg(any(desktop, test))]
fn has_key(updater_config: Option<&serde_json::Value>) -> bool {
    updater_config
        .and_then(|c| c.get("pubkey"))
        .and_then(|k| k.as_str())
        .is_some_and(|k| !k.trim().is_empty())
}

/// Whether this build can update itself.
#[tauri::command]
pub fn update_support(app: AppHandle) -> UpdateSupport {
    UpdateSupport {
        enabled: enabled(&app),
    }
}

#[cfg(desktop)]
fn enabled(app: &AppHandle) -> bool {
    has_key(app.config().plugins.0.get("updater"))
}

#[cfg(not(desktop))]
fn enabled(_app: &AppHandle) -> bool {
    false
}

/// Asks the channel (`stable` or `beta`) for a newer version. `None` means up to date. Errors
/// are messages for the user: no key in this build, an unknown channel, or a failed request.
#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    channel: String,
) -> Result<Option<OfferedUpdate>, String> {
    desktop::check(app, channel).await
}

/// Downloads and verifies the channel's update, flushes the library, installs and restarts.
/// Nothing is installed unless the signature verifies. Errors are messages for the user.
#[tauri::command]
pub async fn update_install(app: AppHandle, channel: String) -> Result<(), String> {
    desktop::install(app, channel).await
}

#[cfg(desktop)]
mod desktop {
    use super::{enabled, OfferedUpdate};
    use serde::Serialize;
    use tauri::{AppHandle, Emitter};
    use tauri_plugin_updater::{Update, UpdaterExt};
    use url::Url;

    /// Download progress, sent as `updates://progress`.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Progress {
        received: u64,
        total: Option<u64>,
    }

    /// The beta feed. The stable feed is the endpoint in `tauri.conf.json`.
    const BETA_FEED: &str =
        "https://github.com/nh-99/penroam/releases/download/updates-beta/latest.json";

    async fn find(app: &AppHandle, channel: &str) -> Result<Option<Update>, String> {
        if !enabled(app) {
            return Err("This build of Penroam doesn't update itself.".into());
        }
        let mut builder = app.updater_builder();
        match channel {
            "stable" => {}
            "beta" => {
                let feed = Url::parse(BETA_FEED).map_err(|e| e.to_string())?;
                builder = builder.endpoints(vec![feed]).map_err(|e| e.to_string())?;
            }
            other => return Err(format!("Unknown update channel {other:?}.")),
        }
        let updater = builder.build().map_err(|e| e.to_string())?;
        updater.check().await.map_err(|e| e.to_string())
    }

    pub async fn check(app: AppHandle, channel: String) -> Result<Option<OfferedUpdate>, String> {
        Ok(find(&app, &channel).await?.map(|u| OfferedUpdate {
            version: u.version.clone(),
            notes: u.body.clone(),
        }))
    }

    pub async fn install(app: AppHandle, channel: String) -> Result<(), String> {
        let update = find(&app, &channel)
            .await?
            .ok_or("Penroam is already up to date.")?;
        let progress = app.clone();
        let mut received = 0u64;
        let bytes = update
            .download(
                move |chunk, total| {
                    received += chunk as u64;
                    let _ = progress.emit("updates://progress", Progress { received, total });
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())?;
        // On Windows `install` doesn't return: the installer replaces this process.
        crate::commands::flush(&app);
        update.install(bytes).map_err(|e| e.to_string())?;
        app.restart();
    }
}

#[cfg(not(desktop))]
mod desktop {
    use super::OfferedUpdate;
    use tauri::AppHandle;

    const STORE: &str = "Updates for this device come from where you installed Penroam.";

    pub async fn check(_: AppHandle, _: String) -> Result<Option<OfferedUpdate>, String> {
        Err(STORE.into())
    }

    pub async fn install(_: AppHandle, _: String) -> Result<(), String> {
        Err(STORE.into())
    }
}

#[cfg(test)]
mod tests {
    use super::has_key;
    use serde_json::json;

    #[test]
    fn updates_stay_off_without_a_key() {
        assert!(!has_key(None));
        assert!(!has_key(Some(&json!({}))));
        assert!(!has_key(Some(&json!({ "pubkey": "" }))));
        assert!(!has_key(Some(&json!({ "pubkey": "   " }))));
        assert!(has_key(Some(&json!({ "pubkey": "dW50cnVzdGVk" }))));
    }
}
