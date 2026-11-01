// swift-tools-version: 6.0
// M00 iPad harness, built on Linux with xtool (https://xtool.sh).
// Standalone UIKit/PencilKit test host. Shared code lives in ../InkKit
// (InkCore + PencilInk) and is reused by the Tauri and Flutter iOS hosts.
import PackageDescription

let v5: [SwiftSetting] = [.swiftLanguageMode(.v5)]

let package = Package(
    name: "M00iOS",
    platforms: [.iOS("17.5"), .macOS(.v14)],
    products: [
        // xtool: exactly one library product = the app.
        .library(name: "M00iOS", targets: ["M00iOS"]),
    ],
    dependencies: [.package(path: "../InkKit")],
    targets: [
        .target(
            name: "M00iOS",
            dependencies: [.product(name: "InkCore", package: "InkKit"), .product(name: "PencilInk", package: "InkKit"), .product(name: "WebTuning", package: "InkKit")],
            resources: [.copy("web"), .copy("fixtures"), .copy("autorun")],
            swiftSettings: v5
        ),
    ]
)
