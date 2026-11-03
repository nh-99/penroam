// swift-tools-version: 6.0
// Swift ink code used by the app's iOS native ink plugin
// (plugins/tauri-plugin-native-ink/ios):
//   InkCore       — canonical stroke format, shared geometry, scene generator, metrics
//                   (Foundation only; tested on Linux with `swift test`)
//   PencilSurface — platform-independent core of the iPad Pencil preview surface
//   WebTuning     — WKWebView 120 Hz rendering-update unlock (private SPI, guarded)
import PackageDescription

let v5: [SwiftSetting] = [.swiftLanguageMode(.v5)]

let package = Package(
    name: "InkKit",
    platforms: [.iOS(.v16), .macOS(.v14)],
    products: [
        .library(name: "InkCore", targets: ["InkCore"]),
        .library(name: "WebTuning", targets: ["WebTuning"]),
        .library(name: "PencilSurface", targets: ["PencilSurface"]),
    ],
    targets: [
        .target(name: "InkCore", swiftSettings: v5),
        // WKWebView tweaks used by the app's iOS host.
        .target(name: "WebTuning", swiftSettings: v5),
        // platform-independent core of the Penroam iPad native ink surface
        .target(name: "PencilSurface", dependencies: ["InkCore"], swiftSettings: v5),
        .testTarget(name: "InkCoreTests", dependencies: ["InkCore"], swiftSettings: v5),
        .testTarget(name: "PencilSurfaceTests", dependencies: ["PencilSurface", "InkCore"], swiftSettings: v5),
    ]
)
