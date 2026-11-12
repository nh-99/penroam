// swift-tools-version:5.9
// Swift side of tauri-plugin-store-billing for the Mac App Store build. Tauri's mobile plugin API
// doesn't exist on macOS, so this is a plain static library with one C entry point that the
// plugin's Rust side calls (see `src/macos.rs`). It is built only with the `mac-app-store` feature.
import PackageDescription

let package = Package(
    name: "StoreBillingMac",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "StoreBillingMac", type: .static, targets: ["StoreBillingMac"]),
    ],
    targets: [
        .target(name: "StoreBillingMac", path: "Sources"),
    ]
)
