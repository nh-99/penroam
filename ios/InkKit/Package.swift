// swift-tools-version: 6.0
// Shared Swift ink code for every iOS host (standalone UIKit host, Tauri iOS
// plugin, Flutter iOS Platform View):
//   InkCore   — canonical stroke format, shared geometry, scene generator, metrics
//               (Foundation only; tested on Linux with `swift test`)
//   PencilInk — reusable native PencilKit backend (PKCanvasView) implementing
//               the InkBackend contract + PencilKit <-> canonical conversion
//   WebTuning — WKWebView 120 Hz rendering-update unlock (private SPI, guarded)
import PackageDescription

let v5: [SwiftSetting] = [.swiftLanguageMode(.v5)]

let package = Package(
    name: "InkKit",
    platforms: [.iOS(.v16), .macOS(.v14)],
    products: [
        .library(name: "InkCore", targets: ["InkCore"]),
        .library(name: "PencilInk", targets: ["PencilInk"]),
        .library(name: "WebTuning", targets: ["WebTuning"]),
        .library(name: "PencilSurface", targets: ["PencilSurface"]),
    ],
    targets: [
        .target(name: "InkCore", swiftSettings: v5),
        .target(name: "PencilInk", dependencies: ["InkCore"], swiftSettings: v5),
        // WKWebView tweaks used by the Tauri iOS host and the WKWebView proxy.
        .target(name: "WebTuning", swiftSettings: v5),
        // M01: platform-independent core of the Penroam iPad native ink surface
        .target(name: "PencilSurface", dependencies: ["InkCore"], swiftSettings: v5),
        .testTarget(name: "InkCoreTests", dependencies: ["InkCore", "PencilInk"], swiftSettings: v5),
        .testTarget(name: "PencilSurfaceTests", dependencies: ["PencilSurface", "InkCore"], swiftSettings: v5),
    ]
)
