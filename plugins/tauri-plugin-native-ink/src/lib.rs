//! `NativeInkSurface` bridge (see `docs/architecture/overview.md` §5).
//!
//! The Vue editor owns document semantics; a platform view owns *live* pen
//! rendering where the WebView cannot match native latency:
//!
//! * Android: Kotlin `NativeInkPlugin` hosting the shared `BooxInkView` (android/boox-ink)
//!   (Onyx SDK raw drawing, "Mode B") as a transparent overlay above the
//!   WebView.
//! * iOS/iPadOS: Swift `NativeInkPlugin` with a UIKit Pencil preview layer
//!   above the WKWebView (coalesced + predicted touches, estimated force
//!   updates, commit hand-off acknowledged by the web renderer).
//!
//! One generic `call(method, args)` command forwards to the native method of
//! the same name. Finalised strokes come back as plugin events (`stroke`,
//! `erased`, `viewport`, `status`, `error`); pen samples never cross IPC
//! while the stroke is live.

#[cfg(mobile)]
use tauri::Manager;
use tauri::{
    plugin::{Builder, TauriPlugin},
    AppHandle, Runtime,
};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_native_ink);

/// Handle to the registered Kotlin/Swift plugin instance.
#[cfg(mobile)]
struct Native<R: Runtime>(tauri::plugin::PluginHandle<R>);

/// Forwards `method(args)` to the native plugin and returns its JSON result.
/// On desktop there is no native surface: every call fails with a message
/// and the editor keeps using the web canvas.
#[tauri::command]
async fn call<R: Runtime>(
    app: AppHandle<R>,
    method: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[cfg(mobile)]
    {
        app.state::<Native<R>>()
            .0
            .run_mobile_plugin::<serde_json::Value>(&method, args)
            .map_err(|e| format!("{method}: {e}"))
    }
    #[cfg(not(mobile))]
    {
        let _ = (&app, &args);
        Err(format!(
            "native-ink.{method}: no native ink surface on {} (the web canvas is used)",
            std::env::consts::OS
        ))
    }
}

/// The plugin (`native-ink`): registers the `call` command and, on Android and
/// iOS, the native plugin class.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("native-ink")
        .invoke_handler(tauri::generate_handler![call])
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle =
                    _api.register_android_plugin("dev.penroam.nativeink", "NativeInkPlugin")?;
                _app.manage(Native(handle));
            }
            #[cfg(target_os = "ios")]
            {
                let handle = _api.register_ios_plugin(init_plugin_native_ink)?;
                _app.manage(Native(handle));
            }
            Ok(())
        })
        .build()
}
