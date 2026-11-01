//! Bridges the web UI to the Kotlin `BooxInkPlugin`, which hosts the shared
//! `BooxInkView` (android/boox-ink) as a native overlay above the WebView.
//!
//! A single generic `call(method, args)` command forwards to the Kotlin
//! plugin method of the same name. Strokes drawn natively come back as
//! plugin events (`stroke`, `erased`, `viewport`, `status`), never through
//! per-sample IPC: the hardware preview path does not touch JS or Rust.

#[cfg(target_os = "android")]
use tauri::Manager;
use tauri::{
    plugin::{Builder, TauriPlugin},
    AppHandle, Runtime,
};

#[cfg(target_os = "android")]
struct Native<R: Runtime>(tauri::plugin::PluginHandle<R>);

#[tauri::command]
async fn call<R: Runtime>(
    app: AppHandle<R>,
    method: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "android")]
    {
        let native = app.state::<Native<R>>();
        native
            .0
            .run_mobile_plugin::<serde_json::Value>(&method, args)
            .map_err(|e| format!("{method}: {e}"))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (&app, &args);
        Err(format!(
            "boox-ink.{method}: the native BOOX ink view is only available on Android (this is {})",
            std::env::consts::OS
        ))
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("boox-ink")
        .invoke_handler(tauri::generate_handler![call])
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle = _api.register_android_plugin("dev.opennote.booxink.tauri", "BooxInkPlugin")?;
                _app.manage(Native(handle));
            }
            Ok(())
        })
        .build()
}
