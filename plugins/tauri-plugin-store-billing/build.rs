// No commands: only the app's Rust side uses this plugin, so the WebView can't start a purchase
// or finish a transaction by itself.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).ios_path("ios").build();

    // The Mac App Store build links the StoreKit bridge in `macos/`. Tauri's plugin builder only
    // handles iOS and Android projects, so the Swift package is compiled and linked here.
    #[cfg(feature = "mac-app-store")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        swift_rs::SwiftLinker::new("14.0")
            .with_package("StoreBillingMac", "macos")
            .link();
    }
}
