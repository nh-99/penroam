// swift-tools-version: 6.0
// iPad benchmark harness, built on Linux with xtool (https://xtool.sh).
// Standalone UIKit/PencilKit test host. Shared code lives in ../InkKit
// (InkCore + PencilInk) and is reused by the Tauri and Flutter iOS hosts.
import PackageDescription

let v5: [SwiftSetting] = [.swiftLanguageMode(.v5)]

let package = Package(
    name: "BenchHost",
    platforms: [.iOS("17.5"), .macOS(.v14)],
    products: [
        // xtool: exactly one library product = the app.
        .library(name: "BenchHost", targets: ["BenchHost"]),
    ],
    dependencies: [.package(path: "../InkKit")],
    targets: [
        .target(
            name: "BenchHost",
            dependencies: [.product(name: "InkCore", package: "InkKit"), .product(name: "PencilInk", package: "InkKit"), .product(name: "WebTuning", package: "InkKit")],
            resources: [.copy("web"), .copy("fixtures"), .copy("autorun")],
            swiftSettings: v5
        ),
    ]
)
