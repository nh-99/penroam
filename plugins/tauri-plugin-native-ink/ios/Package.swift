// swift-tools-version:5.9
// Swift side of tauri-plugin-native-ink (iOS/iPadOS). `../.tauri/tauri-api`
// is created by the tauri-plugin build script (Tauri's iOS Swift API). The
// platform-independent logic lives in ios/InkKit (`PencilSurface`, tested on
// Linux); this package adds the UIKit layer.
import PackageDescription

let package = Package(
    name: "tauri-plugin-native-ink",
    platforms: [.iOS(.v16), .macOS(.v14)],
    products: [
        .library(name: "tauri-plugin-native-ink", type: .static, targets: ["tauri-plugin-native-ink"]),
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api"),
        .package(name: "InkKit", path: "../../../ios/InkKit"),
    ],
    targets: [
        .target(
            name: "tauri-plugin-native-ink",
            dependencies: [
                .byName(name: "Tauri"),
                .product(name: "PencilSurface", package: "InkKit"),
                .product(name: "InkCore", package: "InkKit"),
                .product(name: "WebTuning", package: "InkKit"),
            ],
            path: "Sources"
        ),
    ]
)
