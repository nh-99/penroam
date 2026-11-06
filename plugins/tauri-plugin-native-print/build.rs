// No commands: only the app's Rust side uses this plugin, so the WebView cannot name a file to
// print.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .ios_path("ios")
        .build();
}
