// No commands: only the app's Rust side uses this plugin, so the WebView can't start a purchase
// or finish a transaction by itself.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).ios_path("ios").build();
}
