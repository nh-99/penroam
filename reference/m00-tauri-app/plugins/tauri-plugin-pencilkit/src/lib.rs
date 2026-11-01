//! Bridges the web UI to the Swift `PencilKitPlugin`, which embeds a real
//! `PKCanvasView` (shared `ios/InkKit` PencilInk backend) in the app's UIKit
//! view hierarchy above the WKWebView.
//!
//! Same shape as tauri-plugin-boox-ink: one generic `call(method, args)`
//! command forwards to the Swift method of the same name; completed strokes
//! come back as plugin events (`strokes`, `erased`, `viewport`, `status`).
//! PencilKit renders ink itself: no Pencil sample crosses into JS or Rust.

#[cfg(target_os = "ios")]
use tauri::Manager;
use tauri::{
    plugin::{Builder, TauriPlugin},
    AppHandle, Runtime,
};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_pencilkit);

#[cfg(target_os = "ios")]
struct Native<R: Runtime>(tauri::plugin::PluginHandle<R>);

#[tauri::command]
async fn call<R: Runtime>(
    app: AppHandle<R>,
    method: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "ios")]
    {
        app.state::<Native<R>>()
            .0
            .run_mobile_plugin::<serde_json::Value>(&method, args)
            .map_err(|e| format!("{method}: {e}"))
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = (&app, &args);
        Err(format!(
            "pencilkit.{method}: the native PencilKit canvas is only available on iOS/iPadOS (this is {})",
            std::env::consts::OS
        ))
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("pencilkit")
        .invoke_handler(tauri::generate_handler![call])
        .setup(|_app, _api| {
            #[cfg(target_os = "ios")]
            {
                let handle = _api.register_ios_plugin(init_plugin_pencilkit)?;
                _app.manage(Native(handle));
            }
            Ok(())
        })
        .build()
}
