// swift-tools-version:5.9
// Swift side of tauri-plugin-pencilkit. `../.tauri/tauri-api` is created by
// the tauri-plugin build script (Tauri's iOS Swift API). The PencilKit logic
// itself lives in the shared ios/InkKit package (also used by the Flutter
// iOS Platform View and the standalone UIKit host).
import PackageDescription

let package = Package(
    name: "tauri-plugin-pencilkit",
    platforms: [.iOS(.v16), .macOS(.v14)],
    products: [
        .library(name: "tauri-plugin-pencilkit", type: .static, targets: ["tauri-plugin-pencilkit"]),
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api"),
        .package(name: "InkKit", path: "../../../../../ios/InkKit"),
    ],
    targets: [
        .target(
            name: "tauri-plugin-pencilkit",
            dependencies: [
                .byName(name: "Tauri"),
                .product(name: "PencilInk", package: "InkKit"),
                .product(name: "InkCore", package: "InkKit"),
                .product(name: "WebTuning", package: "InkKit"),
            ],
            path: "Sources"
        ),
    ]
)
