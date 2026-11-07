// swift-tools-version:5.9
// Swift side of tauri-plugin-store-billing (iOS/iPadOS). `../.tauri/tauri-api` is created by the
// tauri-plugin build script (Tauri's iOS Swift API).
import PackageDescription

let package = Package(
    name: "tauri-plugin-store-billing",
    platforms: [.iOS(.v16), .macOS(.v14)],
    products: [
        .library(name: "tauri-plugin-store-billing", type: .static, targets: ["tauri-plugin-store-billing"]),
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api"),
    ],
    targets: [
        .target(
            name: "tauri-plugin-store-billing",
            dependencies: [.byName(name: "Tauri")],
            path: "Sources"
        ),
    ]
)
